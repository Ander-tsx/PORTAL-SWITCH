//! Session, elevation, integrity level and desktop information for the current process.

use windows::Win32::Foundation::{CloseHandle, HANDLE};
use windows::Win32::Security::{
    GetSidSubAuthority, GetSidSubAuthorityCount, GetTokenInformation, TOKEN_ELEVATION,
    TOKEN_MANDATORY_LABEL, TOKEN_QUERY, TokenElevation, TokenIntegrityLevel,
};
use windows::Win32::System::RemoteDesktop::{ProcessIdToSessionId, WTSGetActiveConsoleSessionId};
use windows::Win32::System::StationsAndDesktops::{
    CloseDesktop, DESKTOP_ACCESS_FLAGS, DESKTOP_CONTROL_FLAGS, GetThreadDesktop,
    GetUserObjectInformationW, HDESK, OpenInputDesktop, UOI_NAME,
};
use windows::Win32::System::Threading::{
    GetCurrentProcess, GetCurrentProcessId, GetCurrentThreadId, OpenProcessToken,
};

use crate::util::Result;

/// MAXIMUM_ALLOWED access right.
pub(crate) const MAXIMUM_ALLOWED: u32 = 0x0200_0000;

pub(crate) fn desktop_name(desk: HDESK) -> String {
    let mut buf = [0u16; 256];
    let mut needed = 0u32;
    // SAFETY: buffer and length match; HDESK is a user object handle.
    let ok = unsafe {
        GetUserObjectInformationW(
            HANDLE(desk.0),
            UOI_NAME,
            Some(buf.as_mut_ptr().cast()),
            (buf.len() * 2) as u32,
            Some(&mut needed),
        )
    };
    match ok {
        Ok(()) => {
            let end = buf.iter().position(|&c| c == 0).unwrap_or(buf.len());
            String::from_utf16_lossy(&buf[..end])
        }
        Err(e) => format!("<name error {e}>"),
    }
}

/// Name of the desktop currently receiving input (e.g. "Default", "Winlogon").
pub(crate) fn input_desktop_name() -> String {
    // SAFETY: handle closed below.
    match unsafe {
        OpenInputDesktop(
            DESKTOP_CONTROL_FLAGS(0),
            false,
            DESKTOP_ACCESS_FLAGS(MAXIMUM_ALLOWED),
        )
    } {
        Ok(d) => {
            let name = desktop_name(d);
            // SAFETY: handle opened above.
            unsafe {
                let _ = CloseDesktop(d);
            }
            name
        }
        Err(e) => format!("<no access: {e}>"),
    }
}

fn token_info<T: Default>(
    class: windows::Win32::Security::TOKEN_INFORMATION_CLASS,
) -> Option<Vec<u8>> {
    let mut token = HANDLE::default();
    // SAFETY: token handle closed below; buffer sized from the first call.
    unsafe {
        OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut token).ok()?;
        let mut len = 0u32;
        let _ = GetTokenInformation(token, class, None, 0, &mut len);
        let mut buf = vec![0u8; len.max(size_of::<T>() as u32) as usize];
        let r = GetTokenInformation(
            token,
            class,
            Some(buf.as_mut_ptr().cast()),
            buf.len() as u32,
            &mut len,
        );
        let _ = CloseHandle(token);
        r.ok()?;
        Some(buf)
    }
}

fn elevated() -> Option<bool> {
    let buf = token_info::<TOKEN_ELEVATION>(TokenElevation)?;
    // SAFETY: buffer holds a TOKEN_ELEVATION.
    Some(unsafe { (*buf.as_ptr().cast::<TOKEN_ELEVATION>()).TokenIsElevated } != 0)
}

fn integrity() -> String {
    let Some(buf) = token_info::<TOKEN_MANDATORY_LABEL>(TokenIntegrityLevel) else {
        return "?".into();
    };
    // SAFETY: buffer holds a TOKEN_MANDATORY_LABEL whose SID lives inside the buffer.
    let rid = unsafe {
        let label = &*buf.as_ptr().cast::<TOKEN_MANDATORY_LABEL>();
        let sid = label.Label.Sid;
        let count = *GetSidSubAuthorityCount(sid);
        *GetSidSubAuthority(sid, u32::from(count) - 1)
    };
    match rid {
        0x0000 => "untrusted".into(),
        0x1000 => "low".into(),
        0x2000 => "medium".into(),
        0x2100 => "medium-plus".into(),
        0x3000 => "high".into(),
        0x4000 => "system".into(),
        other => format!("0x{other:x}"),
    }
}

pub(crate) fn summary() -> String {
    let mut session = 0u32;
    // SAFETY: valid out pointer; thread desktop handle is not owned (no close).
    let (console, thread_desk) = unsafe {
        let _ = ProcessIdToSessionId(GetCurrentProcessId(), &mut session);
        (
            WTSGetActiveConsoleSessionId(),
            GetThreadDesktop(GetCurrentThreadId()).map_or_else(|e| format!("<{e}>"), desktop_name),
        )
    };
    format!(
        "session={session} console_session={console} elevated={} integrity={} input_desktop={} thread_desktop={thread_desk}",
        elevated().map_or("?".into(), |e| e.to_string()),
        integrity(),
        input_desktop_name(),
    )
}

pub(crate) fn report() -> Result<String> {
    Ok(summary())
}
