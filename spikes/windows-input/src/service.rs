// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The PortalSwitch Authors

//! Service model: a LocalSystem service launches a SYSTEM-token agent into the active console
//! session. The agent follows the input desktop (Default <-> Winlogon/secure desktop) and proves
//! it can inject there by jiggling the cursor once per second.

use std::sync::OnceLock;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use windows::Win32::Foundation::{CloseHandle, HANDLE, WAIT_OBJECT_0};
use windows::Win32::Security::{
    DuplicateTokenEx, SecurityIdentification, SetTokenInformation, TOKEN_ACCESS_MASK,
    TOKEN_ADJUST_DEFAULT, TOKEN_ADJUST_SESSIONID, TOKEN_ASSIGN_PRIMARY, TOKEN_DUPLICATE,
    TOKEN_QUERY, TokenPrimary, TokenSessionId,
};
use windows::Win32::System::RemoteDesktop::WTSGetActiveConsoleSessionId;
use windows::Win32::System::Services::{
    CloseServiceHandle, ControlService, CreateServiceW, DeleteService, OpenSCManagerW,
    OpenServiceW, RegisterServiceCtrlHandlerExW, SC_MANAGER_ALL_ACCESS,
    SERVICE_ACCEPT_SESSIONCHANGE, SERVICE_ACCEPT_STOP, SERVICE_ALL_ACCESS,
    SERVICE_CONTROL_INTERROGATE, SERVICE_CONTROL_SESSIONCHANGE, SERVICE_CONTROL_STOP,
    SERVICE_DEMAND_START, SERVICE_ERROR_NORMAL, SERVICE_RUNNING, SERVICE_START_PENDING,
    SERVICE_STATUS, SERVICE_STATUS_HANDLE, SERVICE_STOPPED, SERVICE_TABLE_ENTRYW,
    SERVICE_WIN32_OWN_PROCESS, SetServiceStatus, StartServiceCtrlDispatcherW, StartServiceW,
};
use windows::Win32::System::StationsAndDesktops::{
    CloseDesktop, DESKTOP_ACCESS_FLAGS, DESKTOP_CONTROL_FLAGS, HDESK, OpenInputDesktop,
    SetThreadDesktop,
};
use windows::Win32::System::Threading::{
    CREATE_NO_WINDOW, CreateProcessAsUserW, GetCurrentProcess, OpenProcessToken,
    PROCESS_INFORMATION, STARTUPINFOW, TerminateProcess, WaitForSingleObject,
};
use windows::Win32::UI::Input::KeyboardAndMouse::{
    INPUT, INPUT_0, INPUT_MOUSE, MOUSEEVENTF_MOVE, MOUSEINPUT, SendInput,
};
use windows::Win32::UI::WindowsAndMessaging::GetCursorPos;
use windows::core::{PCWSTR, PWSTR};

use crate::desktop::{MAXIMUM_ALLOWED, desktop_name};
use crate::util::{Result, log_to, wide};

const NAME: &str = "PortalSwitchSpike";
const DISPLAY: &str = "PortalSwitch input spike";
const SERVICE_LOG: &str = "service.log";
const AGENT_LOG: &str = "agent.log";

// --- management -------------------------------------------------------------------------------

pub(crate) fn manage(cmd: &str) -> Result<String> {
    let name = wide(NAME);
    // SAFETY: SCM handles are closed before returning.
    unsafe {
        let scm = OpenSCManagerW(PCWSTR::null(), PCWSTR::null(), SC_MANAGER_ALL_ACCESS)
            .map_err(|e| format!("open SCM (run as administrator): {e}"))?;
        let result = match cmd {
            "install" => {
                let exe = std::env::current_exe()?;
                let bin = wide(&format!("\"{}\" service-main", exe.display()));
                let display = wide(DISPLAY);
                let svc = CreateServiceW(
                    scm,
                    PCWSTR(name.as_ptr()),
                    PCWSTR(display.as_ptr()),
                    SERVICE_ALL_ACCESS,
                    SERVICE_WIN32_OWN_PROCESS,
                    SERVICE_DEMAND_START,
                    SERVICE_ERROR_NORMAL,
                    PCWSTR(bin.as_ptr()),
                    PCWSTR::null(),
                    None,
                    PCWSTR::null(),
                    PCWSTR::null(), // LocalSystem
                    PCWSTR::null(),
                )?;
                let _ = CloseServiceHandle(svc);
                Ok(format!("installed {NAME} -> {}", exe.display()))
            }
            "uninstall" | "start" | "stop" => {
                let svc = OpenServiceW(scm, PCWSTR(name.as_ptr()), SERVICE_ALL_ACCESS)?;
                let r = match cmd {
                    "uninstall" => DeleteService(svc).map(|()| format!("uninstalled {NAME}")),
                    "start" => StartServiceW(svc, None).map(|()| format!("started {NAME}")),
                    _ => {
                        let mut st = SERVICE_STATUS::default();
                        ControlService(svc, SERVICE_CONTROL_STOP, &mut st)
                            .map(|()| format!("stopping {NAME}"))
                    }
                };
                let _ = CloseServiceHandle(svc);
                r.map_err(Into::into)
            }
            other => Err(format!("unknown service command '{other}'").into()),
        };
        let _ = CloseServiceHandle(scm);
        result
    }
}

// --- service process --------------------------------------------------------------------------

static STATUS_HANDLE: OnceLock<usize> = OnceLock::new();
static STOP: AtomicBool = AtomicBool::new(false);
static SESSION_CHANGED: AtomicBool = AtomicBool::new(false);

fn set_status(state: windows::Win32::System::Services::SERVICE_STATUS_CURRENT_STATE) {
    let Some(&h) = STATUS_HANDLE.get() else {
        return;
    };
    let st = SERVICE_STATUS {
        dwServiceType: SERVICE_WIN32_OWN_PROCESS,
        dwCurrentState: state,
        dwControlsAccepted: if state == SERVICE_RUNNING {
            SERVICE_ACCEPT_STOP | SERVICE_ACCEPT_SESSIONCHANGE
        } else {
            0
        },
        dwWin32ExitCode: 0,
        dwServiceSpecificExitCode: 0,
        dwCheckPoint: 0,
        dwWaitHint: 2000,
    };
    // SAFETY: handle obtained from RegisterServiceCtrlHandlerExW.
    unsafe {
        let _ = SetServiceStatus(SERVICE_STATUS_HANDLE(h as _), &st);
    }
}

unsafe extern "system" fn ctrl_handler(
    control: u32,
    event: u32,
    _: *mut core::ffi::c_void,
    _: *mut core::ffi::c_void,
) -> u32 {
    match control {
        c if c == SERVICE_CONTROL_STOP => {
            log_to(SERVICE_LOG, "stop requested");
            STOP.store(true, Ordering::SeqCst);
        }
        c if c == SERVICE_CONTROL_SESSIONCHANGE => {
            log_to(SERVICE_LOG, &format!("session change event {event}"));
            SESSION_CHANGED.store(true, Ordering::SeqCst);
        }
        c if c == SERVICE_CONTROL_INTERROGATE => {}
        _ => {}
    }
    0
}

unsafe extern "system" fn service_main(_argc: u32, _argv: *mut PWSTR) {
    let name = wide(NAME);
    // SAFETY: static handler; name outlives the call.
    match unsafe { RegisterServiceCtrlHandlerExW(PCWSTR(name.as_ptr()), Some(ctrl_handler), None) }
    {
        Ok(h) => {
            let _ = STATUS_HANDLE.set(h.0 as usize);
        }
        Err(e) => {
            log_to(
                SERVICE_LOG,
                &format!("RegisterServiceCtrlHandlerExW failed: {e}"),
            );
            return;
        }
    }
    set_status(SERVICE_START_PENDING);
    set_status(SERVICE_RUNNING);
    log_to(
        SERVICE_LOG,
        &format!("running; {}", crate::desktop::summary()),
    );
    supervise();
    set_status(SERVICE_STOPPED);
    log_to(SERVICE_LOG, "stopped");
}

pub(crate) fn run_dispatcher() -> Result<String> {
    let mut name = wide(NAME);
    let table = [
        SERVICE_TABLE_ENTRYW {
            lpServiceName: PWSTR(name.as_mut_ptr()),
            lpServiceProc: Some(service_main),
        },
        SERVICE_TABLE_ENTRYW::default(),
    ];
    // SAFETY: null-terminated table; blocks until the service stops.
    unsafe { StartServiceCtrlDispatcherW(table.as_ptr())? };
    Ok(String::new())
}

/// Keeps one agent alive in the active console session.
fn supervise() {
    let mut agent: Option<(HANDLE, u32)> = None;
    while !STOP.load(Ordering::SeqCst) {
        // SAFETY: no preconditions.
        let session = unsafe { WTSGetActiveConsoleSessionId() };
        let exited = agent.is_some_and(|(h, _)| {
            // SAFETY: valid process handle.
            unsafe { WaitForSingleObject(h, 0) == WAIT_OBJECT_0 }
        });
        let moved = agent.is_some_and(|(_, s)| s != session);
        if SESSION_CHANGED.swap(false, Ordering::SeqCst) || exited || moved || agent.is_none() {
            if let Some((h, s)) = agent.take() {
                log_to(
                    SERVICE_LOG,
                    &format!("replacing agent (session {s}, exited={exited})"),
                );
                // SAFETY: valid process handle, closed after terminating.
                unsafe {
                    let _ = TerminateProcess(h, 0);
                    let _ = CloseHandle(h);
                }
            }
            if session != u32::MAX {
                match launch_agent(session) {
                    Ok(h) => {
                        log_to(SERVICE_LOG, &format!("agent launched in session {session}"));
                        agent = Some((h, session));
                    }
                    Err(e) => log_to(SERVICE_LOG, &format!("agent launch failed: {e}")),
                }
            }
        }
        std::thread::sleep(Duration::from_millis(500));
    }
    if let Some((h, _)) = agent {
        // SAFETY: valid process handle.
        unsafe {
            let _ = TerminateProcess(h, 0);
            let _ = CloseHandle(h);
        }
    }
}

/// Starts `<exe> agent` in `session` with a copy of the service's SYSTEM token.
fn launch_agent(session: u32) -> Result<HANDLE> {
    let exe = std::env::current_exe()?;
    let mut cmd = wide(&format!("\"{}\" agent", exe.display()));
    let mut desktop = wide("winsta0\\default");
    // SAFETY: handles closed on every path; buffers outlive the calls.
    unsafe {
        let mut own = HANDLE::default();
        OpenProcessToken(
            GetCurrentProcess(),
            TOKEN_DUPLICATE
                | TOKEN_QUERY
                | TOKEN_ASSIGN_PRIMARY
                | TOKEN_ADJUST_SESSIONID
                | TOKEN_ADJUST_DEFAULT,
            &mut own,
        )?;
        let mut primary = HANDLE::default();
        let dup = DuplicateTokenEx(
            own,
            TOKEN_ACCESS_MASK(MAXIMUM_ALLOWED),
            None,
            SecurityIdentification,
            TokenPrimary,
            &mut primary,
        );
        let _ = CloseHandle(own);
        dup?;
        let set = SetTokenInformation(
            primary,
            TokenSessionId,
            (&raw const session).cast(),
            size_of::<u32>() as u32,
        );
        if let Err(e) = set {
            let _ = CloseHandle(primary);
            return Err(format!("SetTokenInformation(TokenSessionId): {e}").into());
        }
        let si = STARTUPINFOW {
            cb: size_of::<STARTUPINFOW>() as u32,
            lpDesktop: PWSTR(desktop.as_mut_ptr()),
            ..Default::default()
        };
        let mut pi = PROCESS_INFORMATION::default();
        let created = CreateProcessAsUserW(
            Some(primary),
            PCWSTR::null(),
            Some(PWSTR(cmd.as_mut_ptr())),
            None,
            None,
            false,
            CREATE_NO_WINDOW,
            None,
            PCWSTR::null(),
            &si,
            &mut pi,
        );
        let _ = CloseHandle(primary);
        created?;
        let _ = CloseHandle(pi.hThread);
        Ok(pi.hProcess)
    }
}

// --- agent ------------------------------------------------------------------------------------

fn send_move(dx: i32) -> u32 {
    let input = INPUT {
        r#type: INPUT_MOUSE,
        Anonymous: INPUT_0 {
            mi: MOUSEINPUT {
                dx,
                dy: 0,
                mouseData: 0,
                dwFlags: MOUSEEVENTF_MOVE,
                time: 0,
                dwExtraInfo: 0,
            },
        },
    };
    // SAFETY: one well-formed INPUT.
    unsafe { SendInput(&[input], size_of::<INPUT>() as i32) }
}

fn cursor_x() -> Option<i32> {
    let mut p = windows::Win32::Foundation::POINT::default();
    // SAFETY: valid out pointer.
    unsafe { GetCursorPos(&mut p).ok().map(|()| p.x) }
}

/// Follows the input desktop and jiggles the cursor once per second, logging the outcome.
pub(crate) fn agent() -> Result<String> {
    log_to(
        AGENT_LOG,
        &format!("agent start; {}", crate::desktop::summary()),
    );
    let mut current: Option<(HDESK, String)> = None;
    loop {
        // SAFETY: desktop handles are closed when replaced; this thread owns no windows/hooks,
        // so SetThreadDesktop is allowed.
        let switched = unsafe {
            match OpenInputDesktop(
                DESKTOP_CONTROL_FLAGS(0),
                false,
                DESKTOP_ACCESS_FLAGS(MAXIMUM_ALLOWED),
            ) {
                Ok(d) => {
                    let name = desktop_name(d);
                    if current.as_ref().is_some_and(|(_, n)| *n == name) {
                        let _ = CloseDesktop(d);
                        None
                    } else {
                        match SetThreadDesktop(d) {
                            Ok(()) => {
                                if let Some((old, _)) = current.replace((d, name.clone())) {
                                    let _ = CloseDesktop(old);
                                }
                                Some(Ok(name))
                            }
                            Err(e) => {
                                let _ = CloseDesktop(d);
                                Some(Err(format!("SetThreadDesktop({name}): {e}")))
                            }
                        }
                    }
                }
                Err(e) => Some(Err(format!("OpenInputDesktop: {e}"))),
            }
        };
        if let Some(s) = switched {
            log_to(AGENT_LOG, &format!("desktop switch -> {s:?}"));
        }

        let before = cursor_x();
        let sent = send_move(20);
        std::thread::sleep(Duration::from_millis(150));
        let after = cursor_x();
        let back = send_move(-20);
        let desk = current.as_ref().map_or("?", |(_, n)| n.as_str());
        log_to(
            AGENT_LOG,
            &format!(
                "desktop={desk} sendinput={sent}/{back} cursor_x {before:?}->{after:?} moved={}",
                before.zip(after).is_some_and(|(b, a)| a != b)
            ),
        );
        let trigger = crate::util::log_dir().join(crate::util::TYPE_TRIGGER);
        if let Ok(text) = std::fs::read_to_string(&trigger) {
            let _ = std::fs::remove_file(&trigger);
            let delay = text.trim().parse().unwrap_or(5);
            log_to(
                AGENT_LOG,
                &format!("typetest requested, typing in {delay} s"),
            );
            std::thread::sleep(Duration::from_secs(delay));
            let n = crate::util::type_x(5);
            log_to(
                AGENT_LOG,
                &format!("typetest: SendInput inserted {n}/10 on desktop={desk}"),
            );
        }
        std::thread::sleep(Duration::from_millis(850));
    }
}
