// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The PortalSwitch Authors

//! Host-side cursor parking experiments for a background (agent) app.
//!
//! `park <secs> [bg] [hide] [decouple] [swallow] [edgewarp]`
//!   bg        set the connection property that lets a background app control the cursor
//!   hide      hide the cursor on the main display (then re-associate mouse and cursor)
//!   decouple  dissociate mouse motion from the cursor
//!   swallow   active HID tap that drops pointer, click, scroll and gesture events
//!   edgewarp  when the drawn cursor gets within EDGE_MARGIN px of a display edge, warp it to
//!             the centre once; the next event's delta is flagged and excluded from stats

use std::collections::BTreeMap;
use std::ffi::{CString, c_char};
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::thread;
use std::time::Duration;

use super::{
    CFRelease, CFTypeRef, CGAssociateMouseAndMouseCursorPosition, CGEventCreate,
    CGEventGetIntegerValueField, CGEventGetLocation, CGEventRef, CGEventTapProxy, CGPoint,
    CGSetLocalEventsSuppressionInterval, CGWarpMouseCursorPosition, EV_LEFT_DRAGGED,
    EV_MOUSE_MOVED, EV_OTHER_DRAGGED, EV_RIGHT_DRAGGED, EV_TAP_DISABLED_TIMEOUT,
    EV_TAP_DISABLED_USER, FIELD_DELTA_X, FIELD_DELTA_Y, HID_EVENT_TAP, READY, STOP, TAP_DEFAULT,
    TAP_LISTEN_ONLY, TapCallback, c_void, mask, now_ns, null_mut, pipeline_thread, reenable_taps,
    run_taps, stat_line,
};

#[repr(C)]
#[derive(Clone, Copy, Debug)]
struct CGSize {
    width: f64,
    height: f64,
}

#[repr(C)]
#[derive(Clone, Copy, Debug)]
struct CGRect {
    origin: CGPoint,
    size: CGSize,
}

#[link(name = "ApplicationServices", kind = "framework")]
unsafe extern "C" {
    fn CGMainDisplayID() -> u32;
    fn CGDisplayBounds(display: u32) -> CGRect;
    fn CGDisplayHideCursor(display: u32) -> i32;
    fn CGDisplayShowCursor(display: u32) -> i32;
    fn CGCursorIsVisible() -> u32;
    // Private, undocumented (see spec 07): lets a background app control cursor visibility.
    fn _CGSDefaultConnection() -> i32;
    fn CGSSetConnectionProperty(cid: i32, target: i32, key: CFTypeRef, value: CFTypeRef) -> i32;
}

#[link(name = "CoreFoundation", kind = "framework")]
unsafe extern "C" {
    static kCFBooleanTrue: CFTypeRef;
    fn CFStringCreateWithCString(alloc: CFTypeRef, s: *const c_char, encoding: u32) -> CFTypeRef;
}

const UTF8: u32 = 0x0800_0100;
const EDGE_MARGIN: f64 = 40.0;

const MOTION: [u32; 4] = [
    EV_MOUSE_MOVED,
    EV_LEFT_DRAGGED,
    EV_RIGHT_DRAGGED,
    EV_OTHER_DRAGGED,
];
// Clicks (1-4, 25, 26), scroll (22), and the gesture family (18-20, 29-34, 37).
const OTHER: [u32; 16] = [1, 2, 3, 4, 25, 26, 22, 18, 19, 20, 29, 30, 31, 32, 34, 37];

#[derive(Clone, Copy)]
struct Sample {
    t: u64,
    dx: i64,
    dy: i64,
    x: f64,
    y: f64,
    after_warp: bool,
}

static SWALLOW: AtomicBool = AtomicBool::new(false);
static EDGEWARP: AtomicBool = AtomicBool::new(false);
static AFTER_WARP: AtomicBool = AtomicBool::new(false);
static WARPS: AtomicU64 = AtomicU64::new(0);
static BOUNDS: Mutex<Option<CGRect>> = Mutex::new(None);
static SAMPLES: Mutex<Vec<Sample>> = Mutex::new(Vec::new());
static OTHER_SEEN: Mutex<BTreeMap<u32, u64>> = Mutex::new(BTreeMap::new());

fn near_edge(p: CGPoint, b: CGRect, margin: f64) -> bool {
    p.x <= b.origin.x + margin
        || p.y <= b.origin.y + margin
        || p.x >= b.origin.x + b.size.width - 1.0 - margin
        || p.y >= b.origin.y + b.size.height - 1.0 - margin
}

extern "C" fn park_cb(_: CGEventTapProxy, kind: u32, ev: CGEventRef, _: *mut c_void) -> CGEventRef {
    if kind == EV_TAP_DISABLED_TIMEOUT || kind == EV_TAP_DISABLED_USER {
        reenable_taps();
        return ev;
    }
    let swallow = SWALLOW.load(Ordering::Relaxed);
    if !MOTION.contains(&kind) {
        *OTHER_SEEN.lock().unwrap().entry(kind).or_default() += 1;
        return if swallow { null_mut() } else { ev };
    }
    let b = BOUNDS.lock().unwrap().unwrap();
    // SAFETY: `ev` is valid for the duration of the callback.
    let (p, dx, dy) = unsafe {
        (
            CGEventGetLocation(ev),
            CGEventGetIntegerValueField(ev, FIELD_DELTA_X),
            CGEventGetIntegerValueField(ev, FIELD_DELTA_Y),
        )
    };
    let after_warp = AFTER_WARP.swap(false, Ordering::Relaxed);
    SAMPLES.lock().unwrap().push(Sample {
        t: now_ns(),
        dx,
        dy,
        x: p.x,
        y: p.y,
        after_warp,
    });
    if EDGEWARP.load(Ordering::Relaxed) && near_edge(p, b, EDGE_MARGIN) {
        let centre = CGPoint {
            x: b.origin.x + b.size.width / 2.0,
            y: b.origin.y + b.size.height / 2.0,
        };
        // SAFETY: no preconditions.
        unsafe { CGWarpMouseCursorPosition(centre) };
        WARPS.fetch_add(1, Ordering::Relaxed);
        AFTER_WARP.store(true, Ordering::Relaxed);
    }
    if swallow { null_mut() } else { ev }
}

fn set_background_cursor_property() -> i32 {
    let key = CString::new("SetsCursorInBackground").unwrap();
    // SAFETY: valid C string; CF objects are released after use (kCFBooleanTrue is static).
    unsafe {
        let cf_key = CFStringCreateWithCString(std::ptr::null(), key.as_ptr(), UTF8);
        let cid = _CGSDefaultConnection();
        let rc = CGSSetConnectionProperty(cid, cid, cf_key, kCFBooleanTrue);
        CFRelease(cf_key);
        rc
    }
}

pub(crate) fn park(secs: u64, flags: &[String]) -> Result<String, String> {
    let has = |f: &str| flags.iter().any(|a| a == f);
    let (bg, hide, decouple) = (has("bg"), has("hide"), has("decouple"));
    SWALLOW.store(has("swallow"), Ordering::SeqCst);
    EDGEWARP.store(has("edgewarp"), Ordering::SeqCst);

    // SAFETY: no preconditions.
    let (display, bounds) = unsafe {
        let d = CGMainDisplayID();
        (d, CGDisplayBounds(d))
    };
    *BOUNDS.lock().unwrap() = Some(bounds);
    // SAFETY: no preconditions.
    unsafe { CGSetLocalEventsSuppressionInterval(0.0) };

    let options = if has("swallow") {
        TAP_DEFAULT
    } else {
        TAP_LISTEN_ONLY
    };
    let mut kinds = MOTION.to_vec();
    kinds.extend_from_slice(&OTHER);
    let events = mask(&kinds);
    let taps = thread::spawn(move || {
        pipeline_thread();
        run_taps(&[(HID_EVENT_TAP, options, events, park_cb as TapCallback)])
    });
    while !READY.load(Ordering::SeqCst) {
        if taps.is_finished() {
            return Err(taps.join().unwrap().err().unwrap_or_default());
        }
        thread::sleep(Duration::from_millis(10));
    }

    let bg_rc = if bg {
        set_background_cursor_property()
    } else {
        i32::MIN
    };
    // SAFETY: no preconditions. Re-associate after each visibility change (spec 07).
    let (hide_rc, assoc_rc, visible_after) = unsafe {
        let h = if hide {
            CGDisplayHideCursor(display)
        } else {
            i32::MIN
        };
        let a = CGAssociateMouseAndMouseCursorPosition(!decouple);
        (h, a, CGCursorIsVisible())
    };
    let logical_start = query_cursor();

    thread::sleep(Duration::from_secs(secs));

    let logical_end = query_cursor();
    // SAFETY: no preconditions; restore everything.
    unsafe {
        if hide {
            CGDisplayShowCursor(display);
        }
        CGAssociateMouseAndMouseCursorPosition(true);
    }
    STOP.store(true, Ordering::SeqCst);
    let _ = taps.join();

    Ok(summarise(
        flags,
        bounds,
        (bg_rc, hide_rc, assoc_rc, visible_after),
        logical_start,
        logical_end,
    ))
}

fn query_cursor() -> CGPoint {
    // SAFETY: valid null source; the +1 event is released.
    unsafe {
        let e = CGEventCreate(null_mut());
        let p = CGEventGetLocation(e);
        CFRelease(e);
        p
    }
}

fn summarise(
    flags: &[String],
    b: CGRect,
    (bg_rc, hide_rc, assoc_rc, visible): (i32, i32, i32, u32),
    logical_start: CGPoint,
    logical_end: CGPoint,
) -> String {
    let v = SAMPLES.lock().unwrap().clone();
    let rc = |r: i32| {
        if r == i32::MIN {
            "-".to_string()
        } else {
            r.to_string()
        }
    };
    let mut out = vec![
        format!("flags={}", flags.join(",")),
        format!(
            "rc: bg_property={} hide={} associate={} cursor_visible_after={visible}",
            rc(bg_rc),
            rc(hide_rc),
            assoc_rc
        ),
        format!(
            "display=({:.0},{:.0} {:.0}x{:.0}) logical_cursor start=({:.0},{:.0}) end=({:.0},{:.0})",
            b.origin.x,
            b.origin.y,
            b.size.width,
            b.size.height,
            logical_start.x,
            logical_start.y,
            logical_end.x,
            logical_end.y
        ),
    ];
    if v.len() < 2 {
        out.push(format!("motion_events={} (no motion recorded)", v.len()));
        out.push(format!(
            "other_events_by_type={:?}",
            OTHER_SEEN.lock().unwrap()
        ));
        return out.join("\n");
    }

    let (minx, maxx) = v
        .iter()
        .fold((f64::MAX, f64::MIN), |(a, z), s| (a.min(s.x), z.max(s.x)));
    let (miny, maxy) = v
        .iter()
        .fold((f64::MAX, f64::MIN), |(a, z), s| (a.min(s.y), z.max(s.y)));
    let (right, bottom) = (
        b.origin.x + b.size.width - 1.0,
        b.origin.y + b.size.height - 1.0,
    );

    // Edge clipping: when the drawn cursor sits on an edge, does the outward component of the
    // delta keep arriving (unclipped) or drop to zero (clipped)?
    let (mut at_edge, mut outward_nonzero, mut outward_zero) = (0, 0, 0);
    for s in v.iter().filter(|s| !s.after_warp) {
        let mut outward = None;
        if s.x >= right - 0.5 {
            outward = Some(s.dx);
        } else if s.x <= b.origin.x + 0.5 {
            outward = Some(-s.dx);
        }
        if s.y >= bottom - 0.5 {
            outward = Some(outward.unwrap_or(0).max(s.dy));
        } else if s.y <= b.origin.y + 0.5 {
            outward = Some(outward.unwrap_or(0).max(-s.dy));
        }
        if let Some(o) = outward {
            at_edge += 1;
            if o > 0 {
                outward_nonzero += 1
            } else {
                outward_zero += 1
            }
        }
    }

    let clean: Vec<&Sample> = v.iter().filter(|s| !s.after_warp).collect();
    let warped: Vec<&Sample> = v.iter().filter(|s| s.after_warp).collect();
    let max_abs = |xs: &[&Sample]| {
        xs.iter()
            .map(|s| s.dx.abs().max(s.dy.abs()))
            .max()
            .unwrap_or(0)
    };

    // Per-second activity: events and sum of |dx|+|dy|.
    let t0 = v[0].t;
    let mut buckets: BTreeMap<u64, (u64, i64)> = BTreeMap::new();
    for s in &v {
        let e = buckets.entry((s.t - t0) / 1_000_000_000).or_default();
        e.0 += 1;
        e.1 += s.dx.abs() + s.dy.abs();
    }
    let per_sec: Vec<String> = buckets.values().map(|(n, d)| format!("{n}/{d}")).collect();

    let dur = (v.last().unwrap().t - t0) as f64 / 1e9;
    out.extend([
        format!("motion_events={} rate={:.0}Hz", v.len(), v.len() as f64 / dur),
        format!(
            "drawn_cursor x=[{minx:.0}..{maxx:.0}] y=[{miny:.0}..{maxy:.0}] (display max x={right:.0} y={bottom:.0})"
        ),
        format!(
            "edge: samples_on_edge={at_edge} outward_delta_nonzero={outward_nonzero} outward_delta_zero={outward_zero}"
        ),
        format!(
            "edgewarp: warps={} clean_max_abs_delta={} post_warp_events={} post_warp_max_abs_delta={}",
            WARPS.load(Ordering::Relaxed),
            max_abs(&clean),
            warped.len(),
            max_abs(&warped)
        ),
        format!("per_second events/sum|d|: {}", per_sec.join(" ")),
        format!("other_events_by_type={:?}", OTHER_SEEN.lock().unwrap()),
    ]);
    let intervals: Vec<u64> = v.windows(2).map(|w| w[1].t - w[0].t).collect();
    out.push(stat_line("inter-event interval", intervals));
    out.join("\n")
}
