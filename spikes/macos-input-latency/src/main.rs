// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The PortalSwitch Authors

//! Spike: input capture and injection latency on macOS.
//!
//! Modes (pass after `open -n -W <App> --args`):
//!   check                 report permission state; request missing permissions
//!   synthetic <hz> <secs> synthetic pipeline: post -> capture tap -> TCP loopback -> inject -> observe
//!   real <secs>           capture real mouse motion with the cursor decoupled from the mouse
//!
//! Results are appended to ~/Library/Logs/portalswitch-spike/.

#![allow(non_upper_case_globals, clippy::missing_safety_doc)]

use std::ffi::c_void;

mod park;
use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::ptr::null_mut;
use std::sync::atomic::{AtomicBool, AtomicU8, AtomicU64, Ordering};
use std::sync::mpsc::{Sender, channel};
use std::sync::{Mutex, OnceLock};
use std::thread;
use std::time::Duration;

// ---------------------------------------------------------------------------------------------
// FFI
// ---------------------------------------------------------------------------------------------

type CFTypeRef = *const c_void;
type CFStringRef = *const c_void;
type CFMachPortRef = *mut c_void;
type CFRunLoopRef = *mut c_void;
type CFRunLoopSourceRef = *mut c_void;
type CGEventRef = *mut c_void;
type CGEventSourceRef = *mut c_void;
type CGEventTapProxy = *mut c_void;
type TapCallback = extern "C" fn(CGEventTapProxy, u32, CGEventRef, *mut c_void) -> CGEventRef;

#[repr(C)]
#[derive(Clone, Copy, Debug)]
struct CGPoint {
    x: f64,
    y: f64,
}

#[link(name = "ApplicationServices", kind = "framework")]
unsafe extern "C" {
    fn CGEventTapCreate(
        tap: u32,
        place: u32,
        options: u32,
        events_of_interest: u64,
        callback: TapCallback,
        user_info: *mut c_void,
    ) -> CFMachPortRef;
    fn CGEventTapEnable(tap: CFMachPortRef, enable: bool);
    fn CGEventGetTimestamp(event: CGEventRef) -> u64;
    fn CGEventGetIntegerValueField(event: CGEventRef, field: u32) -> i64;
    fn CGEventSetIntegerValueField(event: CGEventRef, field: u32, value: i64);
    fn CGEventCreate(source: CGEventSourceRef) -> CGEventRef;
    fn CGEventGetLocation(event: CGEventRef) -> CGPoint;
    fn CGEventCreateMouseEvent(
        source: CGEventSourceRef,
        kind: u32,
        location: CGPoint,
        button: u32,
    ) -> CGEventRef;
    fn CGEventPost(tap: u32, event: CGEventRef);
    fn CGEventSourceCreate(state: i32) -> CGEventSourceRef;
    fn CGAssociateMouseAndMouseCursorPosition(connected: bool) -> i32;
    fn CGWarpMouseCursorPosition(point: CGPoint) -> i32;
    fn CGSetLocalEventsSuppressionInterval(seconds: f64) -> i32;
    fn AXIsProcessTrusted() -> bool;
    fn CGPreflightListenEventAccess() -> bool;
    fn CGPreflightPostEventAccess() -> bool;
    fn CGRequestListenEventAccess() -> bool;
    fn CGRequestPostEventAccess() -> bool;
}

#[link(name = "CoreFoundation", kind = "framework")]
unsafe extern "C" {
    static kCFRunLoopCommonModes: CFStringRef;
    static kCFRunLoopDefaultMode: CFStringRef;
    fn CFMachPortCreateRunLoopSource(
        alloc: CFTypeRef,
        port: CFMachPortRef,
        order: isize,
    ) -> CFRunLoopSourceRef;
    fn CFRunLoopGetCurrent() -> CFRunLoopRef;
    fn CFRunLoopAddSource(rl: CFRunLoopRef, source: CFRunLoopSourceRef, mode: CFStringRef);
    fn CFRunLoopRunInMode(mode: CFStringRef, seconds: f64, return_after_source: bool) -> i32;
    fn CFRelease(cf: CFTypeRef);
}

#[repr(C)]
struct MachTimebaseInfo {
    numer: u32,
    denom: u32,
}

unsafe extern "C" {
    fn clock_gettime_nsec_np(clock: u32) -> u64;
    fn mach_absolute_time() -> u64;
    fn mach_timebase_info(info: *mut MachTimebaseInfo) -> i32;
    fn pthread_set_qos_class_self_np(qos_class: u32, relative_priority: i32) -> i32;
}

const CLOCK_UPTIME_RAW: u32 = 8;

const HID_EVENT_TAP: u32 = 0;
const SESSION_EVENT_TAP: u32 = 1;
const HEAD_INSERT: u32 = 0;
const TAP_DEFAULT: u32 = 0;
const TAP_LISTEN_ONLY: u32 = 1;

const EV_LEFT_DRAGGED: u32 = 6;
const EV_RIGHT_DRAGGED: u32 = 7;
const EV_MOUSE_MOVED: u32 = 5;
const EV_OTHER_DRAGGED: u32 = 27;
const EV_TAP_DISABLED_TIMEOUT: u32 = 0xFFFF_FFFE;
const EV_TAP_DISABLED_USER: u32 = 0xFFFF_FFFF;

const FIELD_DELTA_X: u32 = 4;
const FIELD_DELTA_Y: u32 = 5;
const FIELD_USER_DATA: u32 = 42;

const SOURCE_HID_SYSTEM: i32 = 1;

const MARK_SRC: i64 = 0x5053_0001;
const MARK_INJ: i64 = 0x5053_0002;

fn now_ns() -> u64 {
    // SAFETY: no preconditions.
    unsafe { clock_gettime_nsec_np(CLOCK_UPTIME_RAW) }
}

fn mask(kinds: &[u32]) -> u64 {
    kinds.iter().fold(0, |m, k| m | (1u64 << k))
}

// ---------------------------------------------------------------------------------------------
// Shared state
// ---------------------------------------------------------------------------------------------

struct Samples {
    t0: Vec<AtomicU64>,
    t1: Vec<AtomicU64>,
    t2: Vec<AtomicU64>,
    t2b: Vec<AtomicU64>,
    t3: Vec<AtomicU64>,
}

impl Samples {
    fn new(n: usize) -> Self {
        let v = || (0..n).map(|_| AtomicU64::new(0)).collect();
        Self {
            t0: v(),
            t1: v(),
            t2: v(),
            t2b: v(),
            t3: v(),
        }
    }
}

#[derive(Clone, Copy)]
struct RealSample {
    now_ns: u64,
    now_mach: u64,
    event_ts: u64,
    dx: i64,
    dy: i64,
    x: f64,
    y: f64,
}

static SAMPLES: OnceLock<Samples> = OnceLock::new();
static CAPTURE_TX: OnceLock<Sender<u32>> = OnceLock::new();
static REAL: Mutex<Vec<RealSample>> = Mutex::new(Vec::new());
static TAPS: Mutex<Vec<usize>> = Mutex::new(Vec::new());
static TAP_DISABLED: AtomicU64 = AtomicU64::new(0);
static FOREIGN_EVENTS: AtomicU64 = AtomicU64::new(0);
static STOP: AtomicBool = AtomicBool::new(false);
static READY: AtomicBool = AtomicBool::new(false);
static QOS: AtomicBool = AtomicBool::new(false);

const QOS_CLASS_USER_INTERACTIVE: u32 = 0x21;

/// Called at the start of every pipeline thread; raises QoS when the `qos` flag is given.
fn pipeline_thread() {
    if QOS.load(Ordering::SeqCst) {
        // SAFETY: affects only the calling thread.
        unsafe { pthread_set_qos_class_self_np(QOS_CLASS_USER_INTERACTIVE, 0) };
    }
}

fn reenable_taps() {
    TAP_DISABLED.fetch_add(1, Ordering::Relaxed);
    for &t in TAPS.lock().unwrap().iter() {
        // SAFETY: pointers are taps created by this process and never released.
        unsafe { CGEventTapEnable(t as CFMachPortRef, true) };
    }
}

fn seq_of(ud: i64, mark: i64) -> Option<usize> {
    ((ud >> 32) == mark).then_some((ud & 0xFFFF_FFFF) as usize)
}

// Capture tap (HID level, active): swallows SRC events and forwards their sequence number.
extern "C" fn capture_cb(
    _: CGEventTapProxy,
    kind: u32,
    ev: CGEventRef,
    _: *mut c_void,
) -> CGEventRef {
    let now = now_ns();
    if kind == EV_TAP_DISABLED_TIMEOUT || kind == EV_TAP_DISABLED_USER {
        reenable_taps();
        return ev;
    }
    // SAFETY: `ev` is a valid event for the duration of the callback.
    let ud = unsafe { CGEventGetIntegerValueField(ev, FIELD_USER_DATA) };
    if let Some(seq) = seq_of(ud, MARK_SRC) {
        let s = SAMPLES.get().unwrap();
        if seq < s.t1.len() {
            s.t1[seq].store(now, Ordering::Relaxed);
            let _ = CAPTURE_TX.get().unwrap().send(seq as u32);
        }
        return null_mut();
    }
    if seq_of(ud, MARK_INJ).is_none() {
        FOREIGN_EVENTS.fetch_add(1, Ordering::Relaxed);
    }
    ev
}

// Observer tap (session level, listen-only): timestamps INJ events as an app would see them.
extern "C" fn observe_cb(
    _: CGEventTapProxy,
    kind: u32,
    ev: CGEventRef,
    _: *mut c_void,
) -> CGEventRef {
    let now = now_ns();
    if kind == EV_TAP_DISABLED_TIMEOUT || kind == EV_TAP_DISABLED_USER {
        reenable_taps();
        return ev;
    }
    // SAFETY: `ev` is a valid event for the duration of the callback.
    let ud = unsafe { CGEventGetIntegerValueField(ev, FIELD_USER_DATA) };
    if let Some(seq) = seq_of(ud, MARK_INJ) {
        let s = SAMPLES.get().unwrap();
        if seq < s.t3.len() {
            s.t3[seq].store(now, Ordering::Relaxed);
        }
    }
    ev
}

// Real-input tap (HID level, listen-only): records native deltas of physical mouse motion.
extern "C" fn real_cb(_: CGEventTapProxy, kind: u32, ev: CGEventRef, _: *mut c_void) -> CGEventRef {
    let now = now_ns();
    // SAFETY: no preconditions.
    let now_mach = unsafe { mach_absolute_time() };
    if kind == EV_TAP_DISABLED_TIMEOUT || kind == EV_TAP_DISABLED_USER {
        reenable_taps();
        return ev;
    }
    // SAFETY: `ev` is a valid event for the duration of the callback; the cursor query
    // event is released immediately.
    let sample = unsafe {
        let q = CGEventCreate(null_mut());
        let p = CGEventGetLocation(q);
        CFRelease(q);
        RealSample {
            now_ns: now,
            now_mach,
            event_ts: CGEventGetTimestamp(ev),
            dx: CGEventGetIntegerValueField(ev, FIELD_DELTA_X),
            dy: CGEventGetIntegerValueField(ev, FIELD_DELTA_Y),
            x: p.x,
            y: p.y,
        }
    };
    REAL.lock().unwrap().push(sample);
    match STRATEGY.load(Ordering::SeqCst) {
        STRATEGY_SWALLOW => null_mut(),
        STRATEGY_WARP => {
            let o = *PARK.lock().unwrap();
            // SAFETY: no preconditions.
            unsafe { CGWarpMouseCursorPosition(o) };
            ev
        }
        _ => ev,
    }
}

const STRATEGY_DECOUPLE: u8 = 0;
const STRATEGY_SWALLOW: u8 = 1;
const STRATEGY_WARP: u8 = 2;
static STRATEGY: AtomicU8 = AtomicU8::new(STRATEGY_DECOUPLE);
static PARK: Mutex<CGPoint> = Mutex::new(CGPoint { x: 0.0, y: 0.0 });

/// Creates the taps on the current thread and runs its run loop until `STOP`.
fn run_taps(taps: &[(u32, u32, u64, TapCallback)]) -> Result<(), String> {
    // SAFETY: CoreFoundation/CoreGraphics calls with valid arguments; objects live for the
    // whole process (spike), so they are intentionally not released.
    unsafe {
        let rl = CFRunLoopGetCurrent();
        for &(location, options, events, cb) in taps {
            let port = CGEventTapCreate(location, HEAD_INSERT, options, events, cb, null_mut());
            if port.is_null() {
                return Err(format!(
                    "CGEventTapCreate failed (location {location}, options {options})"
                ));
            }
            let src = CFMachPortCreateRunLoopSource(std::ptr::null(), port, 0);
            CFRunLoopAddSource(rl, src, kCFRunLoopCommonModes);
            CGEventTapEnable(port, true);
            TAPS.lock().unwrap().push(port as usize);
        }
        READY.store(true, Ordering::SeqCst);
        while !STOP.load(Ordering::SeqCst) {
            CFRunLoopRunInMode(kCFRunLoopDefaultMode, 0.1, false);
        }
    }
    Ok(())
}

// ---------------------------------------------------------------------------------------------
// Stats / report
// ---------------------------------------------------------------------------------------------

fn pct(sorted: &[u64], p: f64) -> u64 {
    if sorted.is_empty() {
        return 0;
    }
    let i = ((sorted.len() - 1) as f64 * p).round() as usize;
    sorted[i]
}

fn stat_line(name: &str, mut v: Vec<u64>) -> String {
    v.sort_unstable();
    let us = |ns: u64| ns as f64 / 1000.0;
    format!(
        "{name:<34} n={:<6} p50={:>8.1}us p95={:>8.1}us p99={:>8.1}us max={:>9.1}us",
        v.len(),
        us(pct(&v, 0.50)),
        us(pct(&v, 0.95)),
        us(pct(&v, 0.99)),
        us(v.last().copied().unwrap_or(0)),
    )
}

fn report(mode: &str, body: &str) {
    let dir = format!(
        "{}/Library/Logs/portalswitch-spike",
        std::env::var("HOME").unwrap()
    );
    std::fs::create_dir_all(&dir).unwrap();
    let path = format!("{dir}/latency.log");
    let text = format!("===== mode={mode} uptime_ns={}\n{body}\n", now_ns());
    let mut f = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
        .unwrap();
    f.write_all(text.as_bytes()).unwrap();
    print!("{text}");
}

fn permissions() -> String {
    // SAFETY: no preconditions.
    unsafe {
        format!(
            "accessibility={} input_monitoring={} post_events={}",
            AXIsProcessTrusted(),
            CGPreflightListenEventAccess(),
            CGPreflightPostEventAccess()
        )
    }
}

fn ensure_permissions(mode: &str) -> bool {
    // SAFETY: no preconditions.
    let ok = unsafe { CGPreflightListenEventAccess() && CGPreflightPostEventAccess() };
    if !ok {
        // SAFETY: no preconditions; shows the system prompts / registers the app in Settings.
        unsafe {
            CGRequestPostEventAccess();
            CGRequestListenEventAccess();
        }
        report(
            mode,
            &format!("missing permissions, requested. {}", permissions()),
        );
    }
    ok
}

// ---------------------------------------------------------------------------------------------
// Modes
// ---------------------------------------------------------------------------------------------

const FRAME: usize = 16;

fn synthetic(hz: u64, secs: u64) -> Result<String, String> {
    let n = (hz * secs) as usize;
    SAMPLES.set(Samples::new(n)).ok();
    let s = SAMPLES.get().unwrap();

    // Network: loopback TCP, Nagle off on both ends.
    let listener = TcpListener::bind("127.0.0.1:0").map_err(|e| e.to_string())?;
    let addr = listener.local_addr().map_err(|e| e.to_string())?;
    let mut tx_sock = TcpStream::connect(addr).map_err(|e| e.to_string())?;
    let (mut rx_sock, _) = listener.accept().map_err(|e| e.to_string())?;
    tx_sock.set_nodelay(true).map_err(|e| e.to_string())?;
    rx_sock.set_nodelay(true).map_err(|e| e.to_string())?;

    // SAFETY: valid null source; returns a +1 event we release.
    let origin = unsafe {
        let e = CGEventCreate(null_mut());
        let p = CGEventGetLocation(e);
        CFRelease(e);
        p
    };

    // Sender: capture tap -> channel -> wire frame (same size as a real motion frame).
    let (tx, rx) = channel::<u32>();
    CAPTURE_TX.set(tx).ok();
    thread::spawn(move || {
        pipeline_thread();
        let mut buf = [0u8; FRAME];
        while let Ok(seq) = rx.recv() {
            buf[..4].copy_from_slice(&(FRAME as u32).to_le_bytes());
            buf[4..8].copy_from_slice(&seq.to_le_bytes());
            if tx_sock.write_all(&buf).is_err() {
                break;
            }
        }
    });

    // Receiver: wire frame -> inject INJ event at HID level.
    thread::spawn(move || {
        pipeline_thread();
        let mut buf = [0u8; FRAME];
        // SAFETY: valid state id; the source lives for the whole process.
        let source = unsafe { CGEventSourceCreate(SOURCE_HID_SYSTEM) };
        let s = SAMPLES.get().unwrap();
        while rx_sock.read_exact(&mut buf).is_ok() {
            let t2 = now_ns();
            let seq = u32::from_le_bytes(buf[4..8].try_into().unwrap()) as usize;
            // SAFETY: valid source/location; event released after posting.
            unsafe {
                let e = CGEventCreateMouseEvent(source, EV_MOUSE_MOVED, origin, 0);
                CGEventSetIntegerValueField(e, FIELD_USER_DATA, (MARK_INJ << 32) | seq as i64);
                CGEventPost(HID_EVENT_TAP, e);
                CFRelease(e);
            }
            let t2b = now_ns();
            if seq < s.t2.len() {
                s.t2[seq].store(t2, Ordering::Relaxed);
                s.t2b[seq].store(t2b, Ordering::Relaxed);
            }
        }
    });

    let taps = thread::spawn(|| {
        pipeline_thread();
        run_taps(&[
            (
                HID_EVENT_TAP,
                TAP_DEFAULT,
                mask(&[EV_MOUSE_MOVED]),
                capture_cb as TapCallback,
            ),
            (
                SESSION_EVENT_TAP,
                TAP_LISTEN_ONLY,
                mask(&[EV_MOUSE_MOVED]),
                observe_cb as TapCallback,
            ),
        ])
    });
    while !READY.load(Ordering::SeqCst) {
        if taps.is_finished() {
            return Err(taps.join().unwrap().err().unwrap_or_default());
        }
        thread::sleep(Duration::from_millis(10));
    }
    thread::sleep(Duration::from_millis(300));

    // Generator: paced SRC events at HID level (stands in for physical mouse reports).
    // SAFETY: valid state id.
    let source = unsafe { CGEventSourceCreate(SOURCE_HID_SYSTEM) };
    let period = 1_000_000_000 / hz;
    let start = now_ns();
    for seq in 0..n {
        let due = start + seq as u64 * period;
        while now_ns() < due {
            thread::sleep(Duration::from_micros(100));
        }
        // SAFETY: valid source/location; event released after posting.
        unsafe {
            let e = CGEventCreateMouseEvent(source, EV_MOUSE_MOVED, origin, 0);
            CGEventSetIntegerValueField(e, FIELD_USER_DATA, (MARK_SRC << 32) | seq as i64);
            s.t0[seq].store(now_ns(), Ordering::Relaxed);
            CGEventPost(HID_EVENT_TAP, e);
            CFRelease(e);
        }
    }
    thread::sleep(Duration::from_millis(500));
    STOP.store(true, Ordering::SeqCst);
    let _ = taps.join();

    let get = |v: &Vec<AtomicU64>, i: usize| v[i].load(Ordering::Relaxed);
    let (mut post_cap, mut net, mut post_call, mut inj_obs, mut total) =
        (vec![], vec![], vec![], vec![], vec![]);
    let (mut lost_cap, mut lost_obs) = (0, 0);
    for i in 0..n {
        let (t0, t1, t2, t2b, t3) = (
            get(&s.t0, i),
            get(&s.t1, i),
            get(&s.t2, i),
            get(&s.t2b, i),
            get(&s.t3, i),
        );
        if t1 == 0 {
            lost_cap += 1;
            continue;
        }
        if t3 == 0 {
            lost_obs += 1;
            continue;
        }
        post_cap.push(t1 - t0);
        net.push(t2 - t1);
        post_call.push(t2b - t2);
        inj_obs.push(t3 - t2);
        total.push(t3 - t0);
    }
    Ok([
        format!("rate={hz}Hz secs={secs} events={n} qos={} lost_at_capture={lost_cap} lost_at_observer={lost_obs}", QOS.load(Ordering::SeqCst)),
        format!(
            "tap_disabled={} foreign_events_seen={}",
            TAP_DISABLED.load(Ordering::Relaxed),
            FOREIGN_EVENTS.load(Ordering::Relaxed)
        ),
        permissions(),
        stat_line("A post(HID) -> capture tap", post_cap),
        stat_line("B capture -> channel -> TCP -> recv", net),
        stat_line("C CGEventPost call duration", post_call),
        stat_line("D recv -> inject -> session tap", inj_obs),
        stat_line("E total A+B+D", total),
    ]
    .join("\n"))
}

fn real(secs: u64, strategy: &str) -> Result<String, String> {
    let s = match strategy {
        "decouple" => STRATEGY_DECOUPLE,
        "swallow" => STRATEGY_SWALLOW,
        "warp" => STRATEGY_WARP,
        other => return Err(format!("unknown strategy {other}")),
    };
    STRATEGY.store(s, Ordering::SeqCst);
    // SAFETY: valid null source; returns a +1 event we release.
    unsafe {
        let e = CGEventCreate(null_mut());
        *PARK.lock().unwrap() = CGEventGetLocation(e);
        CFRelease(e);
        // Without this, warping freezes local input for ~250 ms after each warp.
        CGSetLocalEventsSuppressionInterval(0.0);
    }
    let options = if s == STRATEGY_SWALLOW {
        TAP_DEFAULT
    } else {
        TAP_LISTEN_ONLY
    };
    let taps = thread::spawn(move || {
        pipeline_thread();
        run_taps(&[(
            HID_EVENT_TAP,
            options,
            mask(&[
                EV_MOUSE_MOVED,
                EV_LEFT_DRAGGED,
                EV_RIGHT_DRAGGED,
                EV_OTHER_DRAGGED,
            ]),
            real_cb as TapCallback,
        )])
    });
    while !READY.load(Ordering::SeqCst) {
        if taps.is_finished() {
            return Err(taps.join().unwrap().err().unwrap_or_default());
        }
        thread::sleep(Duration::from_millis(10));
    }
    // Decouple the cursor from the mouse: motion still produces events with deltas,
    // the on-screen cursor stays put. Restored below and on process exit.
    // SAFETY: no preconditions.
    let rc = if s == STRATEGY_DECOUPLE {
        unsafe { CGAssociateMouseAndMouseCursorPosition(false) }
    } else {
        -1
    };
    thread::sleep(Duration::from_secs(secs));
    // SAFETY: no preconditions.
    unsafe { CGAssociateMouseAndMouseCursorPosition(true) };
    STOP.store(true, Ordering::SeqCst);
    let _ = taps.join();

    let v = REAL.lock().unwrap().clone();
    if v.len() < 2 {
        return Ok(format!(
            "decouple_rc={rc} events={} (move the mouse during the run)",
            v.len()
        ));
    }
    let mut tb = MachTimebaseInfo { numer: 0, denom: 0 };
    // SAFETY: valid out pointer.
    unsafe { mach_timebase_info(&mut tb) };
    let ticks_to_ns = |t: u64| t * u64::from(tb.numer) / u64::from(tb.denom);

    // CGEventGetTimestamp is documented as nanoseconds but has been observed to be mach ticks
    // on Apple silicon; report both interpretations and pick the plausible one.
    let as_ns: Vec<i128> = v
        .iter()
        .map(|s| s.now_ns as i128 - s.event_ts as i128)
        .collect();
    let as_ticks: Vec<i128> = v
        .iter()
        .map(|s| ticks_to_ns(s.now_mach) as i128 - ticks_to_ns(s.event_ts) as i128)
        .collect();
    let plausible = |d: &[i128]| {
        d.iter()
            .filter(|&&x| (0..1_000_000_000).contains(&x))
            .count()
    };
    let (unit, lat) = if plausible(&as_ticks) >= plausible(&as_ns) {
        ("mach_ticks", as_ticks)
    } else {
        ("nanoseconds", as_ns)
    };
    let lat: Vec<u64> = lat
        .into_iter()
        .filter(|&x| x >= 0)
        .map(|x| x as u64)
        .collect();

    let intervals: Vec<u64> = v.windows(2).map(|w| w[1].now_ns - w[0].now_ns).collect();
    let dur_s = (v.last().unwrap().now_ns - v[0].now_ns) as f64 / 1e9;
    let (sx, sy) = v
        .iter()
        .fold((0i64, 0i64), |(a, b), s| (a + s.dx, b + s.dy));
    let zero = v.iter().filter(|s| s.dx == 0 && s.dy == 0).count();
    let max_d = v
        .iter()
        .map(|s| s.dx.abs().max(s.dy.abs()))
        .max()
        .unwrap_or(0);
    let park = *PARK.lock().unwrap();
    let max_drift = v
        .iter()
        .map(|s| (s.x - park.x).abs().max((s.y - park.y).abs()))
        .fold(0.0f64, f64::max);
    let drifted = v
        .iter()
        .filter(|s| (s.x - park.x).abs() > 2.0 || (s.y - park.y).abs() > 2.0)
        .count();

    Ok([
        format!(
            "decouple_rc={rc} events={} duration={dur_s:.2}s rate={:.0}Hz tap_disabled={}",
            v.len(),
            v.len() as f64 / dur_s,
            TAP_DISABLED.load(Ordering::Relaxed)
        ),
        format!(
            "strategy={strategy} sum_dx={sx} sum_dy={sy} zero_delta_events={zero} max_abs_delta={max_d}"
        ),
        format!(
            "cursor_max_drift_px={max_drift:.0} samples_cursor_off_park_gt2px={drifted}/{}",
            v.len()
        ),
        format!("event_timestamp_unit={unit} timebase={}/{}", tb.numer, tb.denom),
        permissions(),
        stat_line("hardware timestamp -> HID tap", lat),
        stat_line("inter-event interval", intervals),
    ]
    .join("\n"))
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let mode = args.first().map_or("check", String::as_str);
    QOS.store(args.iter().any(|a| a == "qos"), Ordering::SeqCst);
    let num = |i: usize, d: u64| args.get(i).and_then(|s| s.parse().ok()).unwrap_or(d);

    if mode == "check" {
        ensure_permissions(mode);
        report(mode, &permissions());
        return;
    }
    if !ensure_permissions(mode) {
        return;
    }
    let result = match mode {
        "synthetic" => synthetic(num(1, 1000), num(2, 10)),
        "real" => real(num(1, 8), args.get(2).map_or("decouple", String::as_str)),
        "park" => park::park(num(1, 8), args.get(2..).unwrap_or_default()),
        other => Err(format!("unknown mode {other}")),
    };
    match result {
        Ok(body) => report(mode, &body),
        Err(e) => report(mode, &format!("error: {e}")),
    }
}
