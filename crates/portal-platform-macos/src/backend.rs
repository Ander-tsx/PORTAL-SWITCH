// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The PortalSwitch Authors

//! The macOS backend: event tap capture, `CGEvent` injection, displays and permissions.
//!
//! **Status:** compiled and unit-tested for its pure parts; the OS-facing code follows the
//! behaviour verified in the Phase 0 spikes but still needs the on-device test plan.

#![allow(unsafe_code)]

use std::collections::HashSet;
use std::ptr::null_mut;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Mutex, MutexGuard};
use std::thread;
use std::time::{Duration, Instant};

use portal_core::input::{InputEvent, KeyAction, KeyEvent};
use portal_core::layout::Point;
use portal_core::peer::Inject;
use portal_platform::keymap::{hid_to_macos_keycode, keyboard_usage_id};
use portal_platform::{
    CaptureHandler, InputCapture, InputInjector, Monitor, PlatformError, Screens,
};

use crate::convert::{self, RawKey, RawScroll};
use crate::ffi::{self as sys, CGEventRef, CGEventTapProxy, CGPoint};

fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(std::sync::PoisonError::into_inner)
}

// ---------------------------------------------------------------------------------------------
// Permissions
// ---------------------------------------------------------------------------------------------

/// State of the permissions PortalSwitch needs on macOS.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Permissions {
    /// Accessibility: required to post input events.
    pub accessibility: bool,
    /// Input Monitoring: required to capture input.
    pub input_monitoring: bool,
    /// Posting synthetic events.
    pub post_events: bool,
}

impl Permissions {
    /// Everything needed to capture and inject.
    #[must_use]
    pub const fn all_granted(&self) -> bool {
        self.accessibility && self.input_monitoring && self.post_events
    }
}

/// Current permission state, without prompting.
#[must_use]
pub fn permissions() -> Permissions {
    // SAFETY: argument-free queries.
    unsafe {
        Permissions {
            accessibility: sys::AXIsProcessTrusted(),
            input_monitoring: sys::CGPreflightListenEventAccess(),
            post_events: sys::CGPreflightPostEventAccess(),
        }
    }
}

/// Ask macOS to show the permission prompts for whatever is missing.
pub fn request_permissions() {
    // SAFETY: argument-free requests; macOS shows its own dialogs.
    unsafe {
        sys::CGRequestListenEventAccess();
        sys::CGRequestPostEventAccess();
    }
}

// ---------------------------------------------------------------------------------------------
// Capture
// ---------------------------------------------------------------------------------------------

static HANDLER: Mutex<Option<CaptureHandler>> = Mutex::new(None);
static TAP: Mutex<usize> = Mutex::new(0);
static STOP: AtomicBool = AtomicBool::new(false);
static READY: AtomicBool = AtomicBool::new(false);

const CAPTURE_KINDS: [u32; 15] = [
    sys::EV_LEFT_DOWN,
    sys::EV_LEFT_UP,
    sys::EV_RIGHT_DOWN,
    sys::EV_RIGHT_UP,
    sys::EV_MOUSE_MOVED,
    sys::EV_LEFT_DRAGGED,
    sys::EV_RIGHT_DRAGGED,
    sys::EV_KEY_DOWN,
    sys::EV_KEY_UP,
    sys::EV_FLAGS_CHANGED,
    sys::EV_SCROLL,
    sys::EV_OTHER_DOWN,
    sys::EV_OTHER_UP,
    sys::EV_OTHER_DRAGGED,
    // Gesture-family events are swallowed while a peer is active so the host does not react.
    18,
];

fn kind_mask(kinds: &[u32]) -> u64 {
    kinds.iter().fold(0, |m, k| m | (1u64 << k))
}

/// Translate one `CGEvent` into zero or more input events.
///
/// # Safety
/// `ev` must be a valid event for the duration of the call.
unsafe fn translate(kind: u32, ev: CGEventRef) -> Vec<InputEvent> {
    // SAFETY: caller guarantees `ev` is valid.
    unsafe {
        let field = |f: u32| sys::CGEventGetIntegerValueField(ev, f);
        match kind {
            sys::EV_MOUSE_MOVED
            | sys::EV_LEFT_DRAGGED
            | sys::EV_RIGHT_DRAGGED
            | sys::EV_OTHER_DRAGGED => {
                let p = sys::CGEventGetLocation(ev);
                vec![convert::motion(
                    field(sys::FIELD_DELTA_X),
                    field(sys::FIELD_DELTA_Y),
                    p.x,
                    p.y,
                )]
            }
            sys::EV_LEFT_DOWN => vec![InputEvent::Button {
                button: 1,
                down: true,
            }],
            sys::EV_LEFT_UP => vec![InputEvent::Button {
                button: 1,
                down: false,
            }],
            sys::EV_RIGHT_DOWN => vec![InputEvent::Button {
                button: 2,
                down: true,
            }],
            sys::EV_RIGHT_UP => vec![InputEvent::Button {
                button: 2,
                down: false,
            }],
            sys::EV_OTHER_DOWN | sys::EV_OTHER_UP => vec![InputEvent::Button {
                button: convert::button(field(sys::FIELD_BUTTON_NUMBER)),
                down: kind == sys::EV_OTHER_DOWN,
            }],
            sys::EV_SCROLL => vec![convert::wheel(RawScroll {
                line_dy: field(sys::FIELD_SCROLL_DELTA_Y),
                line_dx: field(sys::FIELD_SCROLL_DELTA_X),
                point_dy: field(sys::FIELD_SCROLL_POINT_Y),
                point_dx: field(sys::FIELD_SCROLL_POINT_X),
                continuous: field(sys::FIELD_SCROLL_CONTINUOUS) != 0,
            })],
            sys::EV_KEY_DOWN | sys::EV_KEY_UP => {
                let mut chars = [0u16; 4];
                let mut len = 0usize;
                sys::CGEventKeyboardGetUnicodeString(
                    ev,
                    chars.len(),
                    &raw mut len,
                    chars.as_mut_ptr(),
                );
                let character = char::decode_utf16(chars[..len.min(chars.len())].iter().copied())
                    .next()
                    .and_then(Result::ok)
                    .filter(|c| !c.is_control());
                let keycode = u16::try_from(field(sys::FIELD_KEYCODE)).unwrap_or(0);
                let raw = RawKey {
                    keycode,
                    flags: sys::CGEventGetFlags(ev),
                    autorepeat: field(sys::FIELD_KEY_AUTOREPEAT) != 0,
                    character,
                };
                vec![InputEvent::Key(convert::key_event(
                    kind == sys::EV_KEY_DOWN,
                    raw,
                ))]
            }
            sys::EV_FLAGS_CHANGED => {
                let keycode = u16::try_from(field(sys::FIELD_KEYCODE)).unwrap_or(0);
                convert::flags_changed(keycode, sys::CGEventGetFlags(ev))
                    .into_iter()
                    .map(InputEvent::Key)
                    .collect()
            }
            // Gesture events: nothing to forward, but they may still be swallowed.
            _ => Vec::new(),
        }
    }
}

extern "C" fn tap_callback(
    _proxy: CGEventTapProxy,
    kind: u32,
    ev: CGEventRef,
    _info: *mut std::ffi::c_void,
) -> CGEventRef {
    if kind == sys::EV_TAP_DISABLED_TIMEOUT || kind == sys::EV_TAP_DISABLED_USER {
        // The OS turns a slow tap off; turn it back on.
        let tap = *lock(&TAP);
        if tap != 0 {
            // SAFETY: `tap` is the port created by this module and never released while running.
            unsafe { sys::CGEventTapEnable(tap as sys::CFMachPortRef, true) };
        }
        return ev;
    }
    // SAFETY: `ev` is valid for the duration of the callback.
    unsafe {
        if sys::CGEventGetIntegerValueField(ev, sys::FIELD_USER_DATA) == sys::MARK_INJECTED {
            return ev; // our own injection: never capture it again
        }
        let events = translate(kind, ev);
        let mut swallow =
            kind == 18 && lock(&HANDLER).is_some() && SWALLOW_GESTURES.load(Ordering::Relaxed);
        if let Some(handler) = lock(&HANDLER).as_mut() {
            for event in events {
                swallow |= handler(event);
            }
        }
        if swallow { null_mut() } else { ev }
    }
}

/// While a peer is active, trackpad gesture events are swallowed too.
static SWALLOW_GESTURES: AtomicBool = AtomicBool::new(false);

/// Captures input with a session-wide active event tap.
#[derive(Debug, Default)]
pub struct MacCapture {
    thread: Option<thread::JoinHandle<()>>,
    cursor_hidden: bool,
}

impl MacCapture {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }
}

fn run_tap() -> Result<(), String> {
    // SAFETY: CoreFoundation/CoreGraphics calls with valid arguments; the tap and run-loop source
    // live until the thread ends.
    unsafe {
        let port = sys::CGEventTapCreate(
            sys::HID_EVENT_TAP,
            sys::HEAD_INSERT,
            sys::TAP_DEFAULT,
            kind_mask(&CAPTURE_KINDS),
            tap_callback,
            null_mut(),
        );
        if port.is_null() {
            return Err("CGEventTapCreate failed".to_owned());
        }
        let source = sys::CFMachPortCreateRunLoopSource(std::ptr::null(), port, 0);
        sys::CFRunLoopAddSource(
            sys::CFRunLoopGetCurrent(),
            source,
            sys::kCFRunLoopCommonModes,
        );
        sys::CGEventTapEnable(port, true);
        *lock(&TAP) = port as usize;
        READY.store(true, Ordering::SeqCst);
        while !STOP.load(Ordering::SeqCst) {
            sys::CFRunLoopRunInMode(sys::kCFRunLoopDefaultMode, 0.1, false);
        }
        sys::CGEventTapEnable(port, false);
        *lock(&TAP) = 0;
        sys::CFRelease(source);
        sys::CFRelease(port);
    }
    Ok(())
}

impl InputCapture for MacCapture {
    fn start(&mut self, handler: CaptureHandler) -> Result<(), PlatformError> {
        if self.thread.is_some() {
            return Ok(());
        }
        let perms = permissions();
        if !perms.input_monitoring {
            return Err(PlatformError::Permission("Input Monitoring"));
        }
        if !perms.accessibility {
            return Err(PlatformError::Permission("Accessibility"));
        }
        *lock(&HANDLER) = Some(handler);
        STOP.store(false, Ordering::SeqCst);
        READY.store(false, Ordering::SeqCst);
        // SAFETY: no preconditions. Without this, warps briefly suppress local events.
        unsafe { sys::CGSetLocalEventsSuppressionInterval(0.0) };
        let (tx, rx) = std::sync::mpsc::channel();
        self.thread = Some(
            thread::Builder::new()
                .name("portal-capture".into())
                .spawn(move || {
                    let _ = tx.send(run_tap());
                })
                .map_err(|e| PlatformError::Os(e.to_string()))?,
        );
        // Wait for the tap to come up, or for its failure.
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            if READY.load(Ordering::SeqCst) {
                return Ok(());
            }
            if let Ok(Err(why)) = rx.try_recv() {
                self.thread.take();
                return Err(PlatformError::Os(why));
            }
            if Instant::now() > deadline {
                return Err(PlatformError::Os("event tap did not start".into()));
            }
            thread::sleep(Duration::from_millis(5));
        }
    }

    fn set_peer_active(&mut self, active: bool) -> Result<(), PlatformError> {
        SWALLOW_GESTURES.store(active, Ordering::Relaxed);
        if active == self.cursor_hidden {
            return Ok(());
        }
        // SAFETY: documented CoreGraphics calls; the private connection property lets a background
        // (agent) app hide the cursor. Re-associating after each visibility change is required.
        unsafe {
            let display = sys::CGMainDisplayID();
            if active {
                let key = std::ffi::CString::new("SetsCursorInBackground")
                    .map_err(|e| PlatformError::Os(e.to_string()))?;
                let cf_key =
                    sys::CFStringCreateWithCString(std::ptr::null(), key.as_ptr(), sys::UTF8);
                let cid = sys::_CGSDefaultConnection();
                sys::CGSSetConnectionProperty(cid, cid, cf_key, sys::kCFBooleanTrue);
                sys::CFRelease(cf_key);
                sys::CGDisplayHideCursor(display);
            } else {
                sys::CGDisplayShowCursor(display);
            }
            sys::CGAssociateMouseAndMouseCursorPosition(true);
        }
        self.cursor_hidden = active;
        Ok(())
    }

    fn warp_cursor(&mut self, to: Point) -> Result<(), PlatformError> {
        // SAFETY: no preconditions.
        let rc = unsafe {
            sys::CGWarpMouseCursorPosition(CGPoint {
                x: f64::from(to.x),
                y: f64::from(to.y),
            })
        };
        if rc == 0 {
            Ok(())
        } else {
            Err(PlatformError::Os(format!(
                "CGWarpMouseCursorPosition: {rc}"
            )))
        }
    }

    fn stop(&mut self) {
        let _ = self.set_peer_active(false);
        STOP.store(true, Ordering::SeqCst);
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
        *lock(&HANDLER) = None;
    }
}

impl Drop for MacCapture {
    fn drop(&mut self) {
        self.stop();
    }
}

// ---------------------------------------------------------------------------------------------
// Injection
// ---------------------------------------------------------------------------------------------

/// A `CGEvent` that is released on drop.
struct Event(CGEventRef);

impl Event {
    fn new(raw: CGEventRef) -> Option<Self> {
        (!raw.is_null()).then_some(Self(raw))
    }

    fn mark(&self) {
        // SAFETY: valid event owned by `self`.
        unsafe {
            sys::CGEventSetIntegerValueField(self.0, sys::FIELD_USER_DATA, sys::MARK_INJECTED);
        };
    }

    fn post(&self) {
        self.mark();
        // SAFETY: valid event owned by `self`.
        unsafe { sys::CGEventPost(sys::HID_EVENT_TAP, self.0) };
    }
}

impl Drop for Event {
    fn drop(&mut self) {
        // SAFETY: the event was created by a `Create` function (+1 retain).
        unsafe { sys::CFRelease(self.0) };
    }
}

// SAFETY: events are only used from the injector's own thread of control.
unsafe impl Send for MacInjector {}

/// Injects input with `CGEventPost`.
#[derive(Debug)]
pub struct MacInjector {
    source: sys::CGEventSourceRef,
    buttons: HashSet<u8>,
    last_click: Option<(u8, Instant, CGPoint, i64)>,
}

impl MacInjector {
    /// # Errors
    /// The event source could not be created.
    pub fn new() -> Result<Self, PlatformError> {
        // SAFETY: no preconditions.
        let source = unsafe { sys::CGEventSourceCreate(sys::SOURCE_HID_SYSTEM) };
        if source.is_null() {
            return Err(PlatformError::Os("CGEventSourceCreate failed".into()));
        }
        Ok(Self {
            source,
            buttons: HashSet::new(),
            last_click: None,
        })
    }

    fn cursor() -> CGPoint {
        // SAFETY: a null-source event just reports the current state; released afterwards.
        unsafe {
            let e = sys::CGEventCreate(null_mut());
            if e.is_null() {
                return CGPoint::default();
            }
            let p = sys::CGEventGetLocation(e);
            sys::CFRelease(e);
            p
        }
    }

    fn move_event(&self, to: CGPoint, dx: i64, dy: i64) {
        let (kind, button) = if self.buttons.contains(&1) {
            (sys::EV_LEFT_DRAGGED, 0)
        } else if self.buttons.contains(&2) {
            (sys::EV_RIGHT_DRAGGED, 1)
        } else if let Some(other) = self.buttons.iter().find(|b| **b > 2) {
            (sys::EV_OTHER_DRAGGED, u32::from(*other) - 1)
        } else {
            (sys::EV_MOUSE_MOVED, 0)
        };
        // SAFETY: valid source; the event is released by `Event`.
        let event =
            unsafe { Event::new(sys::CGEventCreateMouseEvent(self.source, kind, to, button)) };
        if let Some(event) = event {
            // SAFETY: valid event.
            unsafe {
                sys::CGEventSetIntegerValueField(event.0, sys::FIELD_DELTA_X, dx);
                sys::CGEventSetIntegerValueField(event.0, sys::FIELD_DELTA_Y, dy);
                sys::CGWarpMouseCursorPosition(to);
            }
            event.post();
        }
    }

    fn button(&mut self, button: u8, down: bool) {
        let at = Self::cursor();
        let (kind, cg_button) = match (button, down) {
            (1, true) => (sys::EV_LEFT_DOWN, 0),
            (1, false) => (sys::EV_LEFT_UP, 0),
            (2, true) => (sys::EV_RIGHT_DOWN, 1),
            (2, false) => (sys::EV_RIGHT_UP, 1),
            (b, true) => (sys::EV_OTHER_DOWN, u32::from(b).saturating_sub(1)),
            (b, false) => (sys::EV_OTHER_UP, u32::from(b).saturating_sub(1)),
        };
        // Click counting: same button, close in time and space.
        let mut count = 1;
        if down {
            if let Some((b, t, p, c)) = self.last_click
                && b == button
                && t.elapsed() < Duration::from_millis(500)
                && (p.x - at.x).abs() < 4.0
                && (p.y - at.y).abs() < 4.0
            {
                count = c + 1;
            }
            self.last_click = Some((button, Instant::now(), at, count));
            self.buttons.insert(button);
        } else {
            count = self.last_click.map_or(1, |l| l.3);
            self.buttons.remove(&button);
        }
        // SAFETY: valid source; event released by `Event`.
        let event = unsafe {
            Event::new(sys::CGEventCreateMouseEvent(
                self.source,
                kind,
                at,
                cg_button,
            ))
        };
        if let Some(event) = event {
            // SAFETY: valid event.
            unsafe {
                sys::CGEventSetIntegerValueField(event.0, sys::FIELD_CLICK_STATE, count);
                if cg_button > 1 {
                    sys::CGEventSetIntegerValueField(
                        event.0,
                        sys::FIELD_BUTTON_NUMBER,
                        i64::from(cg_button),
                    );
                }
            }
            event.post();
        }
    }

    fn wheel(&self, dx: i16, dy: i16, flags: u8) {
        let continuous = flags & 1 != 0;
        // Continuous devices scroll in pixels (10 per notch); wheels scroll in whole lines.
        let amount = |v: i16| -> i32 {
            let v = i32::from(v);
            if continuous {
                v * 10 / 120
            } else {
                // Whole lines, at least one for any non-zero amount.
                ((v.abs() + 119) / 120).min(100) * v.signum()
            }
        };
        let (w1, w2) = (amount(dy), -amount(dx));
        let units = u32::from(!continuous); // CGScrollEventUnit: 0 pixel, 1 line
        // SAFETY: valid source; event released by `Event`.
        let event = unsafe {
            Event::new(sys::CGEventCreateScrollWheelEvent2(
                self.source,
                units,
                2,
                w1,
                w2,
                0,
            ))
        };
        if let Some(event) = event {
            event.post();
        }
    }

    fn key(&self, key: &KeyEvent, down: bool) {
        let usage = keyboard_usage_id(key.hid_usage);
        let keycode = usage.and_then(hid_to_macos_keycode);
        let shortcut = convert::is_shortcut(key.modifiers);
        let modifier_key = keycode.is_some_and(convert::is_modifier_key);
        // Plain typing is injected as text so the character is right whatever this layout is;
        // shortcuts and modifier keys are injected as the physical key.
        let as_text = key.codepoint != 0 && !shortcut && !modifier_key;
        let Some(code) = keycode.or(as_text.then_some(0)) else {
            return;
        };
        // SAFETY: valid source; event released by `Event`.
        let event = unsafe { Event::new(sys::CGEventCreateKeyboardEvent(self.source, code, down)) };
        let Some(event) = event else { return };
        // SAFETY: valid event.
        unsafe {
            if as_text {
                if let Some(c) = char::from_u32(key.codepoint) {
                    let mut buf = [0u16; 2];
                    let units = c.encode_utf16(&mut buf);
                    sys::CGEventKeyboardSetUnicodeString(event.0, units.len(), units.as_ptr());
                }
                sys::CGEventSetFlags(event.0, 0);
            } else {
                sys::CGEventSetFlags(event.0, convert::modifier_flags(key.modifiers));
            }
            if key.action == KeyAction::Repeat {
                sys::CGEventSetIntegerValueField(event.0, sys::FIELD_KEY_AUTOREPEAT, 1);
            }
        }
        event.post();
    }
}

impl Drop for MacInjector {
    fn drop(&mut self) {
        // SAFETY: the source was created in `new` and is released once.
        unsafe { sys::CFRelease(self.source) };
    }
}

impl InputInjector for MacInjector {
    fn inject(&mut self, action: Inject) -> Result<(), PlatformError> {
        match action {
            Inject::MoveTo(p) => {
                self.move_event(
                    CGPoint {
                        x: f64::from(p.x),
                        y: f64::from(p.y),
                    },
                    0,
                    0,
                );
            }
            Inject::MoveBy { dx, dy } => {
                let now = Self::cursor();
                let target = clamp_to_displays(CGPoint {
                    x: now.x + f64::from(dx),
                    y: now.y + f64::from(dy),
                });
                self.move_event(target, i64::from(dx), i64::from(dy));
            }
            Inject::Button { button, down } => self.button(button, down),
            Inject::Wheel { dx, dy, flags } => self.wheel(dx, dy, flags),
            Inject::KeyDown(key) => self.key(&key, true),
            Inject::KeyUp(key) => self.key(&key, false),
            // Lock-key state cannot be set directly; Caps Lock follows forwarded key presses.
            Inject::SetToggles(_) => {}
        }
        Ok(())
    }
}

fn clamp_to_displays(p: CGPoint) -> CGPoint {
    let monitors = MacScreens.monitors();
    let Some((origin, width, height)) = portal_platform::bounding_box(&monitors) else {
        return p;
    };
    CGPoint {
        x: p.x.clamp(
            f64::from(origin.x),
            f64::from(origin.x) + f64::from(width) - 1.0,
        ),
        y: p.y.clamp(
            f64::from(origin.y),
            f64::from(origin.y) + f64::from(height) - 1.0,
        ),
    }
}

// ---------------------------------------------------------------------------------------------
// Screens
// ---------------------------------------------------------------------------------------------

/// Reads the display arrangement from CoreGraphics (positions in points).
#[derive(Debug, Default, Clone, Copy)]
pub struct MacScreens;

impl Screens for MacScreens {
    fn monitors(&self) -> Vec<Monitor> {
        let mut ids = [0u32; 16];
        let mut count = 0u32;
        // SAFETY: the buffer holds 16 entries, as declared.
        let rc = unsafe { sys::CGGetActiveDisplayList(16, ids.as_mut_ptr(), &raw mut count) };
        if rc != 0 {
            return Vec::new();
        }
        ids[..count as usize]
            .iter()
            .map(|&id| {
                // SAFETY: `id` came from the active display list.
                let (bounds, pixels) =
                    unsafe { (sys::CGDisplayBounds(id), sys::CGDisplayPixelsWide(id)) };
                let scale = if bounds.size.width > 0.0 {
                    pixels as f64 / bounds.size.width
                } else {
                    1.0
                };
                Monitor {
                    id,
                    x: bounds.origin.x as i32,
                    y: bounds.origin.y as i32,
                    width: bounds.size.width as u32,
                    height: bounds.size.height as u32,
                    scale_permille: (scale * 1000.0).round() as u16,
                }
            })
            .collect()
    }

    fn cursor(&self) -> Point {
        let p = MacInjector::cursor();
        Point {
            x: p.x as i32,
            y: p.y as i32,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_the_real_display_arrangement() {
        // Works on any logged-in Mac session, including CI runners with a virtual display.
        let monitors = MacScreens.monitors();
        if monitors.is_empty() {
            return; // headless machine without a display
        }
        assert!(
            monitors
                .iter()
                .all(|m| m.width > 0 && m.height > 0 && m.scale_permille >= 1000)
        );
        assert!(portal_platform::bounding_box(&monitors).is_some());
    }

    #[test]
    fn permission_queries_do_not_prompt_or_crash() {
        let p = permissions();
        assert_eq!(
            p.all_granted(),
            p.accessibility && p.input_monitoring && p.post_events
        );
    }

    #[test]
    fn the_kind_mask_covers_the_event_types() {
        let mask = kind_mask(&CAPTURE_KINDS);
        for kind in [
            sys::EV_KEY_DOWN,
            sys::EV_SCROLL,
            sys::EV_MOUSE_MOVED,
            sys::EV_FLAGS_CHANGED,
        ] {
            assert_ne!(mask & (1 << kind), 0);
        }
    }
}
