// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The PortalSwitch Authors

//! Injection latency and Raw Input behaviour under a swallowing low-level mouse hook.

use std::sync::atomic::{AtomicBool, AtomicI64, AtomicU32, AtomicU64, Ordering};
use std::sync::{Mutex, OnceLock};
use std::thread;

use windows::Win32::Foundation::{HWND, LPARAM, LRESULT, POINT, WPARAM};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::System::Threading::GetCurrentThreadId;
use windows::Win32::UI::Input::KeyboardAndMouse::{
    INPUT, INPUT_0, INPUT_MOUSE, MOUSEEVENTF_MOVE, MOUSEINPUT, SendInput,
};
use windows::Win32::UI::Input::{
    GetRawInputData, HRAWINPUT, RAWINPUT, RAWINPUTDEVICE, RAWINPUTHEADER, RID_INPUT,
    RIDEV_INPUTSINK, RIM_TYPEMOUSE, RegisterRawInputDevices,
};
use windows::Win32::UI::WindowsAndMessaging::{
    CallNextHookEx, CreateWindowExW, DefWindowProcW, DispatchMessageW, GetCursorPos, GetMessageW,
    HHOOK, HWND_MESSAGE, MSG, MSLLHOOKSTRUCT, PostThreadMessageW, RegisterClassW,
    SetWindowsHookExW, TranslateMessage, UnhookWindowsHookEx, WH_MOUSE_LL, WINDOW_EX_STYLE,
    WINDOW_STYLE, WM_INPUT, WM_MOUSEMOVE, WM_QUIT, WNDCLASSW,
};
use windows::core::PCWSTR;

use crate::util::{Result, now_ns, stat_line, wait_until, wide};

const MAGIC: usize = 0x5053_0000;
const LLMHF_INJECTED: u32 = 0x1;

// --- hook state -------------------------------------------------------------------------------

static HOOK_T: OnceLock<Vec<AtomicU64>> = OnceLock::new();
static SWALLOW: AtomicBool = AtomicBool::new(false);
static HOOK_SEEN_INJECTED: AtomicU64 = AtomicU64::new(0);
static HOOK_SEEN_OTHER: AtomicU64 = AtomicU64::new(0);
static HOOK_THREAD: AtomicU32 = AtomicU32::new(0);
static HOOK_READY: AtomicBool = AtomicBool::new(false);

unsafe extern "system" fn mouse_hook(code: i32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    if code >= 0 && wparam.0 as u32 == WM_MOUSEMOVE {
        let t = now_ns();
        // SAFETY: for WH_MOUSE_LL with code >= 0, lparam points to an MSLLHOOKSTRUCT.
        let info = unsafe { &*(lparam.0 as *const MSLLHOOKSTRUCT) };
        if info.flags & LLMHF_INJECTED != 0 {
            HOOK_SEEN_INJECTED.fetch_add(1, Ordering::Relaxed);
        } else {
            HOOK_SEEN_OTHER.fetch_add(1, Ordering::Relaxed);
        }
        if info.dwExtraInfo & !0xFFFF == MAGIC
            && let Some(v) = HOOK_T.get()
        {
            let seq = info.dwExtraInfo & 0xFFFF;
            if let Some(slot) = v.get(seq) {
                slot.store(t, Ordering::Relaxed);
            }
        }
        if SWALLOW.load(Ordering::Relaxed) {
            return LRESULT(1);
        }
    }
    // SAFETY: forwarding the unmodified hook arguments.
    unsafe { CallNextHookEx(None, code, wparam, lparam) }
}

/// Runs a message loop on the current thread until WM_QUIT. Required for LL hooks and Raw Input.
fn message_loop() {
    let mut msg = MSG::default();
    // SAFETY: standard message loop with a valid MSG buffer.
    unsafe {
        while GetMessageW(&mut msg, None, 0, 0).as_bool() {
            let _ = TranslateMessage(&msg);
            DispatchMessageW(&msg);
        }
    }
}

fn install_hook() -> Result<HHOOK> {
    // SAFETY: module handle of this executable; hook procedure lives for the process.
    let hmod = unsafe { GetModuleHandleW(PCWSTR::null())? };
    // SAFETY: valid hook procedure; thread id 0 = global LL hook.
    Ok(unsafe { SetWindowsHookExW(WH_MOUSE_LL, Some(mouse_hook), Some(hmod.into()), 0)? })
}

fn stop_thread(id: u32) {
    // SAFETY: posting WM_QUIT to a thread with a message queue.
    unsafe {
        let _ = PostThreadMessageW(id, WM_QUIT, WPARAM(0), LPARAM(0));
    }
}

fn send_move(dx: i32, dy: i32, extra: usize) -> u32 {
    let input = INPUT {
        r#type: INPUT_MOUSE,
        Anonymous: INPUT_0 {
            mi: MOUSEINPUT {
                dx,
                dy,
                mouseData: 0,
                dwFlags: MOUSEEVENTF_MOVE,
                time: 0,
                dwExtraInfo: extra,
            },
        },
    };
    // SAFETY: one well-formed INPUT.
    unsafe { SendInput(&[input], size_of::<INPUT>() as i32) }
}

fn cursor() -> POINT {
    let mut p = POINT::default();
    // SAFETY: valid out pointer.
    unsafe {
        let _ = GetCursorPos(&mut p);
    }
    p
}

/// SendInput a marked relative move at `hz` for `secs`; the LL hook timestamps it.
pub(crate) fn inject(hz: u64, secs: u64, swallow: bool) -> Result<String> {
    let n = usize::try_from((hz * secs).min(0xFFFF))?;
    HOOK_T
        .set((0..n).map(|_| AtomicU64::new(0)).collect())
        .map_err(|_| "already initialised")?;
    SWALLOW.store(swallow, Ordering::SeqCst);

    let hook_thread = thread::spawn(|| -> Result<()> {
        // SAFETY: no preconditions.
        HOOK_THREAD.store(unsafe { GetCurrentThreadId() }, Ordering::SeqCst);
        let hook = install_hook()?;
        HOOK_READY.store(true, Ordering::SeqCst);
        message_loop();
        // SAFETY: hook installed above.
        unsafe { UnhookWindowsHookEx(hook)? };
        Ok(())
    });
    while !HOOK_READY.load(Ordering::SeqCst) {
        if hook_thread.is_finished() {
            return Err(format!("hook thread failed: {:?}", hook_thread.join()).into());
        }
        thread::sleep(std::time::Duration::from_millis(10));
    }
    thread::sleep(std::time::Duration::from_millis(200));

    let start_pos = cursor();
    let mut max_drift = 0i32;
    let mut t0 = vec![0u64; n];
    let mut call = Vec::with_capacity(n);
    let mut failed = 0u32;
    let period = 1_000_000_000 / hz;
    let start = now_ns();
    for (seq, slot) in t0.iter_mut().enumerate() {
        wait_until(start + seq as u64 * period);
        // Alternate +1/-1 px so the net motion is zero but swallowing is observable.
        let dx = if seq % 2 == 0 { 1 } else { -1 };
        let before = now_ns();
        *slot = before;
        if send_move(dx, 0, MAGIC | seq) != 1 {
            failed += 1;
        }
        call.push(now_ns() - before);
        if seq % 50 == 0 {
            let p = cursor();
            max_drift = max_drift
                .max((p.x - start_pos.x).abs())
                .max((p.y - start_pos.y).abs());
        }
    }
    thread::sleep(std::time::Duration::from_millis(300));
    stop_thread(HOOK_THREAD.load(Ordering::SeqCst));
    let _ = hook_thread.join();

    let seen = HOOK_T.get().ok_or("hook state")?;
    let mut lat = Vec::with_capacity(n);
    let mut lost = 0;
    for (i, &sent) in t0.iter().enumerate() {
        let t = seen[i].load(Ordering::Relaxed);
        if t == 0 {
            lost += 1;
        } else {
            lat.push(t.saturating_sub(sent));
        }
    }
    Ok([
        format!("mode=inject hz={hz} secs={secs} swallow={swallow} events={n} sendinput_failed={failed} not_seen_by_hook={lost}"),
        format!(
            "hook_seen injected={} other={} cursor_max_drift_px={max_drift}",
            HOOK_SEEN_INJECTED.load(Ordering::Relaxed),
            HOOK_SEEN_OTHER.load(Ordering::Relaxed)
        ),
        crate::desktop::summary(),
        stat_line("SendInput -> LL hook", lat),
        stat_line("SendInput call duration", call),
    ]
    .join("\n"))
}

// --- Raw Input --------------------------------------------------------------------------------

struct RawSample {
    t: u64,
    dx: i32,
    dy: i32,
    absolute: bool,
    injected: bool,
}

static RAW: Mutex<Vec<RawSample>> = Mutex::new(Vec::new());
static RAW_READY: AtomicBool = AtomicBool::new(false);
static RAW_THREAD: AtomicU32 = AtomicU32::new(0);
static INJECTED_SUM_X: AtomicI64 = AtomicI64::new(0);

unsafe extern "system" fn raw_wndproc(
    hwnd: HWND,
    msg: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    if msg == WM_INPUT {
        let t = now_ns();
        let mut raw = RAWINPUT::default();
        let mut size = size_of::<RAWINPUT>() as u32;
        // SAFETY: buffer is a RAWINPUT of `size` bytes.
        let got = unsafe {
            GetRawInputData(
                HRAWINPUT(lparam.0 as _),
                RID_INPUT,
                Some((&raw mut raw).cast()),
                &mut size,
                size_of::<RAWINPUTHEADER>() as u32,
            )
        };
        if got != u32::MAX && raw.header.dwType == RIM_TYPEMOUSE.0 {
            // SAFETY: dwType says the union holds mouse data.
            let m = unsafe { raw.data.mouse };
            RAW.lock().unwrap().push(RawSample {
                t,
                dx: m.lLastX,
                dy: m.lLastY,
                absolute: m.usFlags.0 & 0x1 != 0,
                injected: raw.header.hDevice.is_invalid(),
            });
        }
    }
    // SAFETY: default processing for everything (WM_INPUT requires it for cleanup).
    unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) }
}

/// Records Raw Input mouse deltas, optionally while a LL hook swallows motion for the rest of
/// the system. With `selftest`, injects ±5 px moves at 500 Hz itself.
pub(crate) fn rawinput(secs: u64, swallow: bool, selftest: bool) -> Result<String> {
    SWALLOW.store(swallow, Ordering::SeqCst);
    let worker = thread::spawn(move || -> Result<()> {
        // SAFETY: no preconditions.
        RAW_THREAD.store(unsafe { GetCurrentThreadId() }, Ordering::SeqCst);
        let class = wide("PortalSwitchSpikeRaw");
        // SAFETY: module handle of this executable.
        let hinst = unsafe { GetModuleHandleW(PCWSTR::null())? };
        let wc = WNDCLASSW {
            lpfnWndProc: Some(raw_wndproc),
            hInstance: hinst.into(),
            lpszClassName: PCWSTR(class.as_ptr()),
            ..Default::default()
        };
        // SAFETY: valid class description; strings outlive the call.
        unsafe {
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
            )?;
            let dev = RAWINPUTDEVICE {
                usUsagePage: 0x01,
                usUsage: 0x02,
                dwFlags: RIDEV_INPUTSINK,
                hwndTarget: hwnd,
            };
            RegisterRawInputDevices(&[dev], size_of::<RAWINPUTDEVICE>() as u32)?;
        }
        let hook = if swallow { Some(install_hook()?) } else { None };
        RAW_READY.store(true, Ordering::SeqCst);
        message_loop();
        if let Some(h) = hook {
            // SAFETY: hook installed above.
            unsafe { UnhookWindowsHookEx(h)? };
        }
        Ok(())
    });
    while !RAW_READY.load(Ordering::SeqCst) {
        if worker.is_finished() {
            return Err(format!("raw input thread failed: {:?}", worker.join()).into());
        }
        thread::sleep(std::time::Duration::from_millis(10));
    }

    let start_pos = cursor();
    let mut max_drift = 0i32;
    let end = now_ns() + secs * 1_000_000_000;
    let mut k: u64 = 0;
    let mut injected_sent = 0u64;
    let start = now_ns();
    while now_ns() < end {
        if selftest {
            wait_until(start + k * 2_000_000);
            let dx = if k.is_multiple_of(2) { 5 } else { -5 };
            if send_move(dx, 0, 0) == 1 {
                injected_sent += 1;
                INJECTED_SUM_X.fetch_add(i64::from(dx.abs()), Ordering::Relaxed);
            }
            k += 1;
        } else {
            thread::sleep(std::time::Duration::from_millis(5));
        }
        if k.is_multiple_of(25) || !selftest {
            let p = cursor();
            max_drift = max_drift
                .max((p.x - start_pos.x).abs())
                .max((p.y - start_pos.y).abs());
        }
    }
    thread::sleep(std::time::Duration::from_millis(200));
    stop_thread(RAW_THREAD.load(Ordering::SeqCst));
    let _ = worker.join();

    let v = RAW.lock().unwrap();
    let real: Vec<&RawSample> = v.iter().filter(|s| !s.injected).collect();
    let inj: Vec<&RawSample> = v.iter().filter(|s| s.injected).collect();
    let abs_count = v.iter().filter(|s| s.absolute).count();
    let sum = |xs: &[&RawSample]| {
        xs.iter()
            .map(|s| i64::from(s.dx.abs()) + i64::from(s.dy.abs()))
            .sum::<i64>()
    };
    let intervals: Vec<u64> = real.windows(2).map(|w| w[1].t - w[0].t).collect();
    let dur = real
        .last()
        .zip(real.first())
        .map_or(0.0, |(l, f)| (l.t - f.t) as f64 / 1e9);
    Ok([
        format!("mode=rawinput secs={secs} swallow={swallow} selftest={selftest}"),
        format!(
            "raw_events total={} real={} injected={} absolute={abs_count}",
            v.len(),
            real.len(),
            inj.len()
        ),
        format!(
            "real: rate={:.0}Hz sum|d|={}   injected: sent={injected_sent} sum|dx|_sent={} sum|d|_raw={}",
            if dur > 0.0 { real.len() as f64 / dur } else { 0.0 },
            sum(&real),
            INJECTED_SUM_X.load(Ordering::Relaxed),
            sum(&inj)
        ),
        format!(
            "ll_hook_seen injected={} other={} cursor_max_drift_px={max_drift}",
            HOOK_SEEN_INJECTED.load(Ordering::Relaxed),
            HOOK_SEEN_OTHER.load(Ordering::Relaxed)
        ),
        crate::desktop::summary(),
        stat_line("real raw input interval", intervals),
    ]
    .join("\n"))
}
