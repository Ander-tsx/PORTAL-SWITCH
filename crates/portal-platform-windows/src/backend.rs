// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The PortalSwitch Authors

//! The Windows backend.
//!
//! **Status:** type-checked in CI for Windows and unit-tested for its pure parts; the Win32
//! behaviour follows the Phase 0 spikes and still needs the on-device test plan.
//!
//! Capture combines two low-level hooks (to swallow events) with Raw Input (for true device
//! deltas, which keep flowing while the hooks swallow). Injection uses `SendInput`; relative
//! motion is injected as an absolute position so Windows pointer acceleration cannot distort it.

#![allow(unsafe_code)]
#![allow(
    clippy::cast_possible_truncation,
    clippy::cast_possible_wrap,
    clippy::cast_sign_loss
)]

use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::{Mutex, MutexGuard};
use std::thread;
use std::time::{Duration, Instant};

use portal_core::input::{InputEvent, KeyEvent};
use portal_core::layout::Point;
use portal_core::peer::Inject;
use portal_platform::{
    CaptureHandler, InputCapture, InputInjector, Monitor, PlatformError, Screens,
};
use windows::Win32::Foundation::{HWND, LPARAM, LRESULT, POINT, RECT, WPARAM};
use windows::Win32::Graphics::Gdi::{
    EnumDisplayMonitors, GetMonitorInfoW, HDC, HMONITOR, MONITORINFO,
};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::System::Threading::GetCurrentThreadId;
use windows::Win32::UI::HiDpi::{
    DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2, GetDpiForMonitor, MDT_EFFECTIVE_DPI,
    SetProcessDpiAwarenessContext,
};
use windows::Win32::UI::Input::KeyboardAndMouse::{
    GetKeyState, GetKeyboardLayout, GetKeyboardState, INPUT, INPUT_0, INPUT_KEYBOARD, INPUT_MOUSE,
    KEYBD_EVENT_FLAGS, KEYBDINPUT, KEYEVENTF_EXTENDEDKEY, KEYEVENTF_KEYUP, KEYEVENTF_SCANCODE,
    KEYEVENTF_UNICODE, MOUSE_EVENT_FLAGS, MOUSEEVENTF_ABSOLUTE, MOUSEEVENTF_HWHEEL,
    MOUSEEVENTF_LEFTDOWN, MOUSEEVENTF_LEFTUP, MOUSEEVENTF_MIDDLEDOWN, MOUSEEVENTF_MIDDLEUP,
    MOUSEEVENTF_MOVE, MOUSEEVENTF_RIGHTDOWN, MOUSEEVENTF_RIGHTUP, MOUSEEVENTF_VIRTUALDESK,
    MOUSEEVENTF_WHEEL, MOUSEEVENTF_XDOWN, MOUSEEVENTF_XUP, MOUSEINPUT, SendInput, ToUnicodeEx,
    VIRTUAL_KEY, VK_CAPITAL, VK_NUMLOCK, VK_SCROLL,
};
use windows::Win32::UI::Input::{
    GetRawInputData, HRAWINPUT, RAWINPUT, RAWINPUTDEVICE, RAWINPUTHEADER, RID_INPUT,
    RIDEV_INPUTSINK, RIM_TYPEMOUSE, RegisterRawInputDevices,
};
use windows::Win32::UI::WindowsAndMessaging::{
    CallNextHookEx, ClipCursor, CreateWindowExW, DefWindowProcW, DispatchMessageW, GetCursorPos,
    GetMessageW, GetSystemMetrics, HHOOK, HWND_MESSAGE, KBDLLHOOKSTRUCT, MSG, MSLLHOOKSTRUCT,
    PostThreadMessageW, RegisterClassW, SM_CXVIRTUALSCREEN, SM_CYVIRTUALSCREEN, SM_XVIRTUALSCREEN,
    SM_YVIRTUALSCREEN, SetCursorPos, SetWindowsHookExW, TranslateMessage, UnhookWindowsHookEx,
    WH_KEYBOARD_LL, WH_MOUSE_LL, WINDOW_EX_STYLE, WINDOW_STYLE, WM_INPUT, WM_KEYUP, WM_MOUSEMOVE,
    WM_QUIT, WM_SYSKEYUP, WNDCLASSW,
};
use windows::core::PCWSTR;

use crate::convert::{self, KeyTracker, RawKey};

/// Stamped into `dwExtraInfo` of everything we inject so the hooks let it through.
const MARK: usize = 0x5053_0002;

fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(std::sync::PoisonError::into_inner)
}

fn os(e: impl std::fmt::Display) -> PlatformError {
    PlatformError::Os(e.to_string())
}

/// Make coordinates physical pixels on every monitor. Call once, before any window exists.
pub fn enable_dpi_awareness() {
    // SAFETY: no preconditions; failure (already set) is harmless.
    unsafe {
        let _ = SetProcessDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2);
    }
}

// ---------------------------------------------------------------------------------------------
// Capture
// ---------------------------------------------------------------------------------------------

static HANDLER: Mutex<Option<CaptureHandler>> = Mutex::new(None);
static KEYS: Mutex<Option<KeyTracker>> = Mutex::new(None);
static THREAD_ID: AtomicU32 = AtomicU32::new(0);
static READY: AtomicBool = AtomicBool::new(false);
/// Verdict of the most recent Raw Input motion event, applied by the mouse hook to the cursor
/// movement that goes with it.
static SWALLOW_MOVES: AtomicBool = AtomicBool::new(false);

fn deliver(event: InputEvent) -> bool {
    lock(&HANDLER)
        .as_mut()
        .is_some_and(|handler| handler(event))
}

/// The character a key would type with the current keyboard state, if one printable one.
fn typed_character(vk: u32, scancode: u32) -> Option<char> {
    let mut state = [0u8; 256];
    // SAFETY: the buffer is 256 bytes, as the API requires; querying state has no side effects.
    unsafe {
        let _ = GetKeyState(0); // refresh the thread's view of the keyboard state
        GetKeyboardState(&mut state).ok()?;
        let layout = GetKeyboardLayout(0);
        let mut buf = [0u16; 8];
        // Flag 0x4: do not change the keyboard state (dead keys stay pending for the app).
        let n = ToUnicodeEx(vk, scancode, &state, &mut buf, 0x4, Some(layout));
        if n == 1 {
            char::from_u32(u32::from(buf[0])).filter(|c| !c.is_control())
        } else {
            None
        }
    }
}

unsafe extern "system" fn keyboard_hook(code: i32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    if code >= 0 {
        // SAFETY: for WH_KEYBOARD_LL with code >= 0, lparam points to a KBDLLHOOKSTRUCT.
        let info = unsafe { &*(lparam.0 as *const KBDLLHOOKSTRUCT) };
        if info.dwExtraInfo != MARK {
            let up = matches!(wparam.0 as u32, WM_KEYUP | WM_SYSKEYUP);
            let character = if up {
                None
            } else {
                typed_character(info.vkCode, info.scanCode)
            };
            let raw = RawKey {
                vk: info.vkCode,
                scancode: info.scanCode,
                extended: info.flags.0 & 0x1 != 0,
                up,
                character,
            };
            let event = lock(&KEYS).get_or_insert_with(KeyTracker::new).key(raw);
            if let Some(event) = event
                && deliver(InputEvent::Key(event))
            {
                return LRESULT(1);
            }
        }
    }
    // SAFETY: forwarding the unmodified hook arguments.
    unsafe { CallNextHookEx(None, code, wparam, lparam) }
}

unsafe extern "system" fn mouse_hook(code: i32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    if code >= 0 {
        // SAFETY: for WH_MOUSE_LL with code >= 0, lparam points to an MSLLHOOKSTRUCT.
        let info = unsafe { &*(lparam.0 as *const MSLLHOOKSTRUCT) };
        if info.dwExtraInfo != MARK {
            let message = wparam.0 as u32;
            let swallow = if message == WM_MOUSEMOVE {
                SWALLOW_MOVES.load(Ordering::Relaxed)
            } else if let Some((button, down)) = convert::button(message, info.mouseData) {
                deliver(InputEvent::Button { button, down })
            } else if let Some(wheel) = convert::wheel(message, info.mouseData) {
                deliver(wheel)
            } else {
                false
            };
            if swallow {
                return LRESULT(1);
            }
        }
    }
    // SAFETY: forwarding the unmodified hook arguments.
    unsafe { CallNextHookEx(None, code, wparam, lparam) }
}

unsafe extern "system" fn raw_wndproc(
    hwnd: HWND,
    msg: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    if msg == WM_INPUT {
        let mut raw = RAWINPUT::default();
        let mut size = size_of::<RAWINPUT>() as u32;
        // SAFETY: the buffer is a RAWINPUT of `size` bytes.
        let got = unsafe {
            GetRawInputData(
                HRAWINPUT(lparam.0 as _),
                RID_INPUT,
                Some((&raw mut raw).cast()),
                &raw mut size,
                size_of::<RAWINPUTHEADER>() as u32,
            )
        };
        // Events we injected ourselves have no device handle; absolute devices carry positions.
        if got != u32::MAX
            && raw.header.dwType == RIM_TYPEMOUSE.0
            && !raw.header.hDevice.is_invalid()
        {
            // SAFETY: dwType says the union holds mouse data.
            let mouse = unsafe { raw.data.mouse };
            if mouse.usFlags.0 & 0x1 == 0 && (mouse.lLastX != 0 || mouse.lLastY != 0) {
                let mut at = POINT::default();
                // SAFETY: valid out pointer.
                unsafe {
                    let _ = GetCursorPos(&mut at);
                }
                let swallow = deliver(InputEvent::Motion {
                    dx: mouse.lLastX,
                    dy: mouse.lLastY,
                    host_pos: Point { x: at.x, y: at.y },
                });
                SWALLOW_MOVES.store(swallow, Ordering::Relaxed);
            }
        }
    }
    // SAFETY: default processing for everything (WM_INPUT requires it for cleanup).
    unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) }
}

fn wide(text: &str) -> Vec<u16> {
    text.encode_utf16().chain(std::iter::once(0)).collect()
}

fn capture_thread() -> Result<(), String> {
    let class = wide("PortalSwitchRawInput");
    // SAFETY: Win32 calls with valid arguments; everything created here lives until the loop ends.
    unsafe {
        THREAD_ID.store(GetCurrentThreadId(), Ordering::SeqCst);
        let hinst = GetModuleHandleW(PCWSTR::null()).map_err(|e| e.to_string())?;
        let wc = WNDCLASSW {
            lpfnWndProc: Some(raw_wndproc),
            hInstance: hinst.into(),
            lpszClassName: PCWSTR(class.as_ptr()),
            ..Default::default()
        };
        RegisterClassW(&wc);
        let hwnd = CreateWindowExW(
            WINDOW_EX_STYLE(0),
            PCWSTR(class.as_ptr()),
            PCWSTR(class.as_ptr()),
            WINDOW_STYLE(0),
            0,
            0,
            0,
            0,
            Some(HWND_MESSAGE),
            None,
            Some(hinst.into()),
            None,
        )
        .map_err(|e| e.to_string())?;
        let device = RAWINPUTDEVICE {
            usUsagePage: 0x01,
            usUsage: 0x02,
            dwFlags: RIDEV_INPUTSINK,
            hwndTarget: hwnd,
        };
        RegisterRawInputDevices(&[device], size_of::<RAWINPUTDEVICE>() as u32)
            .map_err(|e| e.to_string())?;
        let keyboard: HHOOK =
            SetWindowsHookExW(WH_KEYBOARD_LL, Some(keyboard_hook), Some(hinst.into()), 0)
                .map_err(|e| e.to_string())?;
        let mouse: HHOOK = SetWindowsHookExW(WH_MOUSE_LL, Some(mouse_hook), Some(hinst.into()), 0)
            .map_err(|e| e.to_string())?;
        READY.store(true, Ordering::SeqCst);

        let mut msg = MSG::default();
        while GetMessageW(&mut msg, None, 0, 0).as_bool() {
            let _ = TranslateMessage(&msg);
            DispatchMessageW(&msg);
        }
        let _ = UnhookWindowsHookEx(keyboard);
        let _ = UnhookWindowsHookEx(mouse);
    }
    Ok(())
}

/// Captures input with low-level hooks and Raw Input.
#[derive(Debug, Default)]
pub struct WinCapture {
    thread: Option<thread::JoinHandle<()>>,
    clipped: bool,
}

impl WinCapture {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }
}

impl InputCapture for WinCapture {
    fn start(&mut self, handler: CaptureHandler) -> Result<(), PlatformError> {
        if self.thread.is_some() {
            return Ok(());
        }
        *lock(&HANDLER) = Some(handler);
        *lock(&KEYS) = Some(KeyTracker::new());
        READY.store(false, Ordering::SeqCst);
        let (tx, rx) = std::sync::mpsc::channel();
        self.thread = Some(
            thread::Builder::new()
                .name("portal-capture".into())
                .spawn(move || {
                    let _ = tx.send(capture_thread());
                })
                .map_err(os)?,
        );
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
                return Err(PlatformError::Os("input hooks did not start".into()));
            }
            thread::sleep(Duration::from_millis(5));
        }
    }

    fn set_peer_active(&mut self, active: bool) -> Result<(), PlatformError> {
        if active == self.clipped {
            return Ok(());
        }
        // SAFETY: valid out pointer / rectangle for the duration of the calls.
        unsafe {
            if active {
                let mut at = POINT::default();
                GetCursorPos(&mut at).map_err(os)?;
                // Park the cursor: confine it to the single pixel it is on.
                let cage = RECT {
                    left: at.x,
                    top: at.y,
                    right: at.x + 1,
                    bottom: at.y + 1,
                };
                ClipCursor(Some(&raw const cage)).map_err(os)?;
            } else {
                ClipCursor(None).map_err(os)?;
            }
        }
        SWALLOW_MOVES.store(active, Ordering::Relaxed);
        self.clipped = active;
        Ok(())
    }

    fn warp_cursor(&mut self, to: Point) -> Result<(), PlatformError> {
        // SAFETY: no preconditions.
        unsafe { SetCursorPos(to.x, to.y) }.map_err(os)
    }

    fn stop(&mut self) {
        let _ = self.set_peer_active(false);
        let id = THREAD_ID.load(Ordering::SeqCst);
        if id != 0 {
            // SAFETY: posting WM_QUIT to the capture thread's message queue.
            unsafe {
                let _ = PostThreadMessageW(id, WM_QUIT, WPARAM(0), LPARAM(0));
            }
        }
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
        *lock(&HANDLER) = None;
    }
}

impl Drop for WinCapture {
    fn drop(&mut self) {
        self.stop();
    }
}

// ---------------------------------------------------------------------------------------------
// Injection
// ---------------------------------------------------------------------------------------------

/// Injects input with `SendInput`.
#[derive(Debug, Default)]
pub struct WinInjector;

impl WinInjector {
    #[must_use]
    pub const fn new() -> Self {
        Self
    }
}

fn send(inputs: &[INPUT]) {
    // SAFETY: well-formed INPUT structures and the correct element size.
    unsafe {
        SendInput(inputs, size_of::<INPUT>() as i32);
    }
}

fn mouse_input(dx: i32, dy: i32, data: i32, flags: MOUSE_EVENT_FLAGS) -> INPUT {
    INPUT {
        r#type: INPUT_MOUSE,
        Anonymous: INPUT_0 {
            mi: MOUSEINPUT {
                dx,
                dy,
                mouseData: data as u32,
                dwFlags: flags,
                time: 0,
                dwExtraInfo: MARK,
            },
        },
    }
}

fn key_input(vk: u16, scan: u16, flags: KEYBD_EVENT_FLAGS) -> INPUT {
    INPUT {
        r#type: INPUT_KEYBOARD,
        Anonymous: INPUT_0 {
            ki: KEYBDINPUT {
                wVk: VIRTUAL_KEY(vk),
                wScan: scan,
                dwFlags: flags,
                time: 0,
                dwExtraInfo: MARK,
            },
        },
    }
}

/// The virtual desktop rectangle `(left, top, width, height)` in physical pixels.
fn virtual_desktop() -> (i32, i32, i32, i32) {
    // SAFETY: argument-free metric queries.
    unsafe {
        (
            GetSystemMetrics(SM_XVIRTUALSCREEN),
            GetSystemMetrics(SM_YVIRTUALSCREEN),
            GetSystemMetrics(SM_CXVIRTUALSCREEN).max(1),
            GetSystemMetrics(SM_CYVIRTUALSCREEN).max(1),
        )
    }
}

fn move_absolute(x: i32, y: i32) {
    let (left, top, width, height) = virtual_desktop();
    let x = x.clamp(left, left + width - 1);
    let y = y.clamp(top, top + height - 1);
    // Normalise to the 0..=65535 range across the virtual desktop.
    let norm = |v: i32, origin: i32, extent: i32| -> i32 {
        let span = i64::from((extent - 1).max(1));
        ((i64::from(v - origin) * 65535 + span / 2) / span) as i32
    };
    send(&[mouse_input(
        norm(x, left, width),
        norm(y, top, height),
        0,
        MOUSEEVENTF_MOVE | MOUSEEVENTF_ABSOLUTE | MOUSEEVENTF_VIRTUALDESK,
    )]);
}

fn cursor() -> POINT {
    let mut at = POINT::default();
    // SAFETY: valid out pointer.
    unsafe {
        let _ = GetCursorPos(&mut at);
    }
    at
}

fn inject_button(button: u8, down: bool) {
    let (flags, data) = match (button, down) {
        (1, true) => (MOUSEEVENTF_LEFTDOWN, 0),
        (1, false) => (MOUSEEVENTF_LEFTUP, 0),
        (2, true) => (MOUSEEVENTF_RIGHTDOWN, 0),
        (2, false) => (MOUSEEVENTF_RIGHTUP, 0),
        (3, true) => (MOUSEEVENTF_MIDDLEDOWN, 0),
        (3, false) => (MOUSEEVENTF_MIDDLEUP, 0),
        (4, d) => (
            if d {
                MOUSEEVENTF_XDOWN
            } else {
                MOUSEEVENTF_XUP
            },
            1,
        ),
        (5, d) => (
            if d {
                MOUSEEVENTF_XDOWN
            } else {
                MOUSEEVENTF_XUP
            },
            2,
        ),
        _ => return,
    };
    send(&[mouse_input(0, 0, data, flags)]);
}

fn inject_key(key: &KeyEvent, down: bool) {
    let text = key.codepoint != 0
        && !convert::is_shortcut(key.modifiers)
        && !convert::is_modifier_key(key);
    let up = if down {
        KEYBD_EVENT_FLAGS(0)
    } else {
        KEYEVENTF_KEYUP
    };
    if text && let Some(c) = char::from_u32(key.codepoint) {
        // Typed text goes in as Unicode so the right character appears on any layout.
        let mut units = [0u16; 2];
        let inputs: Vec<INPUT> = c
            .encode_utf16(&mut units)
            .iter()
            .map(|u| key_input(0, *u, KEYEVENTF_UNICODE | up))
            .collect();
        send(&inputs);
        return;
    }
    if let Some((scan, extended)) = convert::scancode_for(key) {
        let ext = if extended {
            KEYEVENTF_EXTENDEDKEY
        } else {
            KEYBD_EVENT_FLAGS(0)
        };
        send(&[key_input(0, scan, KEYEVENTF_SCANCODE | ext | up)]);
    }
}

/// Align a lock key's toggle state by tapping it once if it differs.
fn align_toggle(vk: VIRTUAL_KEY, want: bool, scan: u16, extended: bool) {
    // SAFETY: argument-only query.
    let on = unsafe { GetKeyState(i32::from(vk.0)) } & 1 != 0;
    if on != want {
        let ext = if extended {
            KEYEVENTF_EXTENDEDKEY
        } else {
            KEYBD_EVENT_FLAGS(0)
        };
        send(&[
            key_input(0, scan, KEYEVENTF_SCANCODE | ext),
            key_input(0, scan, KEYEVENTF_SCANCODE | ext | KEYEVENTF_KEYUP),
        ]);
    }
}

impl InputInjector for WinInjector {
    fn inject(&mut self, action: Inject) -> Result<(), PlatformError> {
        match action {
            Inject::MoveTo(p) => move_absolute(p.x, p.y),
            Inject::MoveBy { dx, dy } => {
                let at = cursor();
                move_absolute(at.x + dx, at.y + dy);
            }
            Inject::Button { button, down } => inject_button(button, down),
            Inject::Wheel { dx, dy, .. } => {
                if dy != 0 {
                    send(&[mouse_input(0, 0, i32::from(dy), MOUSEEVENTF_WHEEL)]);
                }
                if dx != 0 {
                    send(&[mouse_input(0, 0, i32::from(dx), MOUSEEVENTF_HWHEEL)]);
                }
            }
            Inject::KeyDown(key) => inject_key(&key, true),
            Inject::KeyUp(key) => inject_key(&key, false),
            Inject::SetToggles(bits) => {
                align_toggle(VK_CAPITAL, bits & 1 != 0, 0x3A, false);
                align_toggle(VK_NUMLOCK, bits & 2 != 0, 0x45, true);
                align_toggle(VK_SCROLL, bits & 4 != 0, 0x46, false);
            }
        }
        Ok(())
    }
}

// ---------------------------------------------------------------------------------------------
// Screens
// ---------------------------------------------------------------------------------------------

/// Reads the monitor arrangement (physical pixels; call [`enable_dpi_awareness`] first).
#[derive(Debug, Default, Clone, Copy)]
pub struct WinScreens;

unsafe extern "system" fn collect_monitor(
    monitor: HMONITOR,
    _dc: HDC,
    _rect: *mut RECT,
    data: LPARAM,
) -> windows::core::BOOL {
    // SAFETY: `data` is the `Vec<Monitor>` passed by `monitors`, alive for the whole enumeration.
    let out = unsafe { &mut *(data.0 as *mut Vec<Monitor>) };
    let mut info = MONITORINFO {
        cbSize: size_of::<MONITORINFO>() as u32,
        ..Default::default()
    };
    // SAFETY: valid monitor handle and correctly sized structure.
    if unsafe { GetMonitorInfoW(monitor, &mut info) }.as_bool() {
        let (mut dpi_x, mut dpi_y) = (96u32, 96u32);
        // SAFETY: valid monitor handle and out pointers.
        let _ = unsafe { GetDpiForMonitor(monitor, MDT_EFFECTIVE_DPI, &mut dpi_x, &mut dpi_y) };
        let r = info.rcMonitor;
        out.push(Monitor {
            id: u32::try_from(out.len()).unwrap_or(0) + 1,
            x: r.left,
            y: r.top,
            width: (r.right - r.left).max(0) as u32,
            height: (r.bottom - r.top).max(0) as u32,
            scale_permille: (dpi_x * 1000 / 96) as u16,
        });
    }
    true.into()
}

impl Screens for WinScreens {
    fn monitors(&self) -> Vec<Monitor> {
        let mut out: Vec<Monitor> = Vec::new();
        // SAFETY: the callback only touches `out`, which outlives the call.
        unsafe {
            let _ = EnumDisplayMonitors(
                None,
                None,
                Some(collect_monitor),
                LPARAM((&raw mut out) as isize),
            );
        }
        out
    }

    fn cursor(&self) -> Point {
        let at = cursor();
        Point { x: at.x, y: at.y }
    }
}
