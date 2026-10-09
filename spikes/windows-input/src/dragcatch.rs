// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The PortalSwitch Authors

//! Drag source detection on Windows: a nearly invisible topmost strip on the right screen edge is
//! registered as an OLE drop target, so any standard drag that reaches the edge is "caught" and
//! its file list (CF_HDROP) logged. Mirrors the macOS `catch` mode of the drag spike.

use std::time::Instant;

use windows::Win32::Foundation::{HWND, LPARAM, LRESULT, POINTL, WPARAM};
use windows::Win32::Graphics::Gdi::HBRUSH;
use windows::Win32::System::Com::{DVASPECT_CONTENT, FORMATETC, IDataObject, TYMED_HGLOBAL};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::System::Ole::{
    CF_HDROP, DROPEFFECT, DROPEFFECT_COPY, IDropTarget, IDropTarget_Impl, OleInitialize,
    OleUninitialize, RegisterDragDrop, ReleaseStgMedium, RevokeDragDrop,
};
use windows::Win32::System::SystemServices::MODIFIERKEYS_FLAGS;
use windows::Win32::UI::Shell::{DragQueryFileW, HDROP};
use windows::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, DefWindowProcW, DispatchMessageW, GetMessageW, GetSystemMetrics, KillTimer,
    LWA_ALPHA, MSG, PostQuitMessage, RegisterClassW, SM_CXSCREEN, SM_CYSCREEN, SW_SHOWNOACTIVATE,
    SetLayeredWindowAttributes, SetTimer, ShowWindow, TranslateMessage, WM_TIMER, WNDCLASSW,
    WS_EX_LAYERED, WS_EX_NOACTIVATE, WS_EX_TOOLWINDOW, WS_EX_TOPMOST, WS_POPUP,
};
use windows::core::{PCWSTR, Ref, implement};

use crate::util::{Result, wide};

const STRIP: i32 = 6;

fn files(obj: &IDataObject) -> Vec<String> {
    let fmt = FORMATETC {
        cfFormat: CF_HDROP.0,
        ptd: std::ptr::null_mut(),
        dwAspect: DVASPECT_CONTENT.0,
        lindex: -1,
        tymed: TYMED_HGLOBAL.0 as u32,
    };
    // SAFETY: standard CF_HDROP retrieval; the medium is released below.
    unsafe {
        let Ok(mut medium) = obj.GetData(&fmt) else {
            return vec!["<no CF_HDROP: not a file drag, or virtual files>".into()];
        };
        let hdrop = HDROP(medium.u.hGlobal.0);
        let count = DragQueryFileW(hdrop, u32::MAX, None);
        let mut out = Vec::new();
        for i in 0..count {
            let len = DragQueryFileW(hdrop, i, None) as usize;
            let mut buf = vec![0u16; len + 1];
            DragQueryFileW(hdrop, i, Some(&mut buf));
            let path = String::from_utf16_lossy(&buf[..len]);
            let info = match std::fs::metadata(&path) {
                Ok(m) if m.is_dir() => "dir".to_string(),
                Ok(m) => format!("file {} bytes", m.len()),
                Err(e) => format!("unreadable: {e}"),
            };
            out.push(format!("{path} [{info}]"));
        }
        ReleaseStgMedium(&mut medium);
        out
    }
}

#[implement(IDropTarget)]
struct EdgeTarget {
    start: Instant,
}

impl EdgeTarget {
    fn log(&self, msg: &str) {
        println!(
            "[{:9.1} ms] {msg}",
            self.start.elapsed().as_secs_f64() * 1000.0
        );
    }
}

impl IDropTarget_Impl for EdgeTarget_Impl {
    fn DragEnter(
        &self,
        data: Ref<IDataObject>,
        keys: MODIFIERKEYS_FLAGS,
        pt: &POINTL,
        effect: *mut DROPEFFECT,
    ) -> windows::core::Result<()> {
        let items = data.as_ref().map(files).unwrap_or_default();
        self.log(&format!(
            "drag entered edge at ({}, {}); keys=0x{:x}; {} item(s)",
            pt.x,
            pt.y,
            keys.0,
            items.len()
        ));
        for i in items {
            self.log(&format!("  {i}"));
        }
        // SAFETY: OLE passes a valid out pointer.
        unsafe { *effect = DROPEFFECT_COPY };
        Ok(())
    }

    fn DragOver(
        &self,
        _keys: MODIFIERKEYS_FLAGS,
        _pt: &POINTL,
        effect: *mut DROPEFFECT,
    ) -> windows::core::Result<()> {
        // SAFETY: OLE passes a valid out pointer.
        unsafe { *effect = DROPEFFECT_COPY };
        Ok(())
    }

    fn DragLeave(&self) -> windows::core::Result<()> {
        self.log("drag left the edge");
        Ok(())
    }

    fn Drop(
        &self,
        _data: Ref<IDataObject>,
        _keys: MODIFIERKEYS_FLAGS,
        _pt: &POINTL,
        effect: *mut DROPEFFECT,
    ) -> windows::core::Result<()> {
        self.log("drop on the edge accepted (a KVM would deliver to the other machine here)");
        // SAFETY: OLE passes a valid out pointer.
        unsafe { *effect = DROPEFFECT_COPY };
        Ok(())
    }
}

unsafe extern "system" fn wndproc(hwnd: HWND, msg: u32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    if msg == WM_TIMER {
        // SAFETY: timer created on this window.
        unsafe {
            let _ = KillTimer(Some(hwnd), 1);
            PostQuitMessage(0);
        }
        return LRESULT(0);
    }
    // SAFETY: default processing.
    unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) }
}

pub(crate) fn run(secs: u64) -> Result<String> {
    let class = wide("PortalSwitchSpikeEdge");
    // SAFETY: OLE initialised on this (STA) thread; window, target and OLE torn down at the end.
    unsafe {
        OleInitialize(None)?;
        let hinst = GetModuleHandleW(PCWSTR::null())?;
        let wc = WNDCLASSW {
            lpfnWndProc: Some(wndproc),
            hInstance: hinst.into(),
            lpszClassName: PCWSTR(class.as_ptr()),
            hbrBackground: HBRUSH::default(),
            ..Default::default()
        };
        RegisterClassW(&wc);
        let (w, h) = (GetSystemMetrics(SM_CXSCREEN), GetSystemMetrics(SM_CYSCREEN));
        let hwnd = CreateWindowExW(
            WS_EX_TOPMOST | WS_EX_TOOLWINDOW | WS_EX_LAYERED | WS_EX_NOACTIVATE,
            PCWSTR(class.as_ptr()),
            PCWSTR(class.as_ptr()),
            WS_POPUP,
            w - STRIP,
            0,
            STRIP,
            h,
            None,
            None,
            Some(hinst.into()),
            None,
        )?;
        // Alpha 1/255: invisible to the eye but still hit-tested (alpha 0 would not be).
        SetLayeredWindowAttributes(hwnd, Default::default(), 1, LWA_ALPHA)?;
        let target: IDropTarget = EdgeTarget {
            start: Instant::now(),
        }
        .into();
        RegisterDragDrop(hwnd, &target)?;
        let _ = ShowWindow(hwnd, SW_SHOWNOACTIVATE);
        SetTimer(Some(hwnd), 1, u32::try_from(secs * 1000)?, None);
        println!(
            "edge strip ready at x={}..{}; drag files to the right edge for {secs} s",
            w - STRIP,
            w
        );

        let mut msg = MSG::default();
        while GetMessageW(&mut msg, None, 0, 0).as_bool() {
            let _ = TranslateMessage(&msg);
            DispatchMessageW(&msg);
        }
        let _ = RevokeDragDrop(hwnd);
        OleUninitialize();
    }
    Ok("dragcatch: done".into())
}
