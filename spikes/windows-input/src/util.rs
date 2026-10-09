// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The PortalSwitch Authors

//! Shared helpers: clock, stats, wide strings, logging.

use std::io::Write as _;
use std::sync::OnceLock;

use windows::Win32::System::Performance::{QueryPerformanceCounter, QueryPerformanceFrequency};
use windows::Win32::UI::Input::KeyboardAndMouse::{
    INPUT, INPUT_0, INPUT_KEYBOARD, KEYBDINPUT, KEYEVENTF_KEYUP, KEYEVENTF_UNICODE, SendInput,
    VIRTUAL_KEY,
};

pub(crate) type Result<T> = std::result::Result<T, Box<dyn std::error::Error + Send + Sync>>;

static FREQ: OnceLock<i64> = OnceLock::new();

/// Monotonic nanoseconds from the performance counter.
pub(crate) fn now_ns() -> u64 {
    let freq = *FREQ.get_or_init(|| {
        let mut f = 0i64;
        // SAFETY: valid out pointer.
        unsafe { QueryPerformanceFrequency(&mut f).ok() };
        f.max(1)
    });
    let mut c = 0i64;
    // SAFETY: valid out pointer.
    unsafe { QueryPerformanceCounter(&mut c).ok() };
    (i128::from(c) * 1_000_000_000 / i128::from(freq)) as u64
}

/// Busy-waits (yielding) until `deadline` in `now_ns()` time. Sleep() has ~15.6 ms granularity.
pub(crate) fn wait_until(deadline: u64) {
    while now_ns() < deadline {
        std::thread::yield_now();
    }
}

pub(crate) fn stat_line(name: &str, mut v: Vec<u64>) -> String {
    v.sort_unstable();
    let pct = |p: f64| {
        if v.is_empty() {
            0.0
        } else {
            v[((v.len() - 1) as f64 * p).round() as usize] as f64 / 1000.0
        }
    };
    format!(
        "{name:<34} n={:<6} p50={:>8.1}us p95={:>8.1}us p99={:>8.1}us max={:>9.1}us",
        v.len(),
        pct(0.50),
        pct(0.95),
        pct(0.99),
        v.last().map_or(0.0, |&x| x as f64 / 1000.0),
    )
}

pub(crate) fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}

pub(crate) fn log_dir() -> std::path::PathBuf {
    let base = std::env::var("ProgramData").unwrap_or_else(|_| r"C:\ProgramData".into());
    std::path::Path::new(&base).join("PortalSwitchSpike")
}

/// Appends a timestamped line to `<log_dir>/<file>`; errors are ignored (best effort).
pub(crate) fn log_to(file: &str, msg: &str) {
    let dir = log_dir();
    let _ = std::fs::create_dir_all(&dir);
    let secs = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0.0, |d| d.as_secs_f64());
    if let Ok(mut f) = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(dir.join(file))
    {
        let _ = writeln!(f, "{secs:.3} {msg}");
    }
}

/// Types `count` letters "x" with Unicode key events; returns the number of events inserted.
/// SendInput does not report UIPI blocking, so the effect must be observed on screen.
pub(crate) fn type_x(count: usize) -> u32 {
    let key = |up: bool| INPUT {
        r#type: INPUT_KEYBOARD,
        Anonymous: INPUT_0 {
            ki: KEYBDINPUT {
                wVk: VIRTUAL_KEY(0),
                wScan: u16::from(b'x'),
                dwFlags: if up {
                    KEYEVENTF_UNICODE | KEYEVENTF_KEYUP
                } else {
                    KEYEVENTF_UNICODE
                },
                time: 0,
                dwExtraInfo: 0,
            },
        },
    };
    let inputs: Vec<INPUT> = (0..count).flat_map(|_| [key(false), key(true)]).collect();
    // SAFETY: well-formed INPUT array.
    unsafe { SendInput(&inputs, size_of::<INPUT>() as i32) }
}

pub(crate) const TYPE_TRIGGER: &str = "type.now";
