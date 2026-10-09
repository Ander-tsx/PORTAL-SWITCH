// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The PortalSwitch Authors

//! Raw CoreGraphics / CoreFoundation declarations.

#![allow(
    unsafe_code,
    unreachable_pub,
    non_upper_case_globals,
    clippy::missing_safety_doc
)]

use std::ffi::c_void;

pub type CFTypeRef = *const c_void;
pub type CFStringRef = *const c_void;
pub type CFMachPortRef = *mut c_void;
pub type CFRunLoopRef = *mut c_void;
pub type CFRunLoopSourceRef = *mut c_void;
pub type CGEventRef = *mut c_void;
pub type CGEventSourceRef = *mut c_void;
pub type CGEventTapProxy = *mut c_void;
pub type TapCallback = extern "C" fn(CGEventTapProxy, u32, CGEventRef, *mut c_void) -> CGEventRef;

#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct CGPoint {
    pub x: f64,
    pub y: f64,
}

#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct CGSize {
    pub width: f64,
    pub height: f64,
}

#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct CGRect {
    pub origin: CGPoint,
    pub size: CGSize,
}

#[link(name = "ApplicationServices", kind = "framework")]
unsafe extern "C" {
    pub fn CGEventTapCreate(
        tap: u32,
        place: u32,
        options: u32,
        events_of_interest: u64,
        callback: TapCallback,
        user_info: *mut c_void,
    ) -> CFMachPortRef;
    pub fn CGEventTapEnable(tap: CFMachPortRef, enable: bool);
    pub fn CGEventGetIntegerValueField(event: CGEventRef, field: u32) -> i64;
    pub fn CGEventSetIntegerValueField(event: CGEventRef, field: u32, value: i64);
    pub fn CGEventGetFlags(event: CGEventRef) -> u64;
    pub fn CGEventSetFlags(event: CGEventRef, flags: u64);
    pub fn CGEventCreate(source: CGEventSourceRef) -> CGEventRef;
    pub fn CGEventGetLocation(event: CGEventRef) -> CGPoint;
    pub fn CGEventCreateMouseEvent(
        source: CGEventSourceRef,
        kind: u32,
        location: CGPoint,
        button: u32,
    ) -> CGEventRef;
    pub fn CGEventCreateKeyboardEvent(
        source: CGEventSourceRef,
        keycode: u16,
        down: bool,
    ) -> CGEventRef;
    pub fn CGEventKeyboardSetUnicodeString(event: CGEventRef, len: usize, chars: *const u16);
    pub fn CGEventKeyboardGetUnicodeString(
        event: CGEventRef,
        max_len: usize,
        actual_len: *mut usize,
        chars: *mut u16,
    );
    pub fn CGEventCreateScrollWheelEvent2(
        source: CGEventSourceRef,
        units: u32,
        wheel_count: u32,
        wheel1: i32,
        wheel2: i32,
        wheel3: i32,
    ) -> CGEventRef;
    pub fn CGEventPost(tap: u32, event: CGEventRef);
    pub fn CGEventSourceCreate(state: i32) -> CGEventSourceRef;
    pub fn CGAssociateMouseAndMouseCursorPosition(connected: bool) -> i32;
    pub fn CGWarpMouseCursorPosition(point: CGPoint) -> i32;
    pub fn CGSetLocalEventsSuppressionInterval(seconds: f64) -> i32;
    pub fn CGMainDisplayID() -> u32;
    pub fn CGDisplayBounds(display: u32) -> CGRect;
    pub fn CGDisplayPixelsWide(display: u32) -> usize;
    pub fn CGDisplayHideCursor(display: u32) -> i32;
    pub fn CGDisplayShowCursor(display: u32) -> i32;
    pub fn CGGetActiveDisplayList(max: u32, displays: *mut u32, count: *mut u32) -> i32;
    pub fn AXIsProcessTrusted() -> bool;
    pub fn CGPreflightListenEventAccess() -> bool;
    pub fn CGPreflightPostEventAccess() -> bool;
    pub fn CGRequestListenEventAccess() -> bool;
    pub fn CGRequestPostEventAccess() -> bool;
    // Private, undocumented: lets a background app control cursor visibility (see ADR-0004).
    pub fn _CGSDefaultConnection() -> i32;
    pub fn CGSSetConnectionProperty(cid: i32, target: i32, key: CFTypeRef, value: CFTypeRef)
    -> i32;
}

#[link(name = "CoreFoundation", kind = "framework")]
unsafe extern "C" {
    pub static kCFRunLoopCommonModes: CFStringRef;
    pub static kCFRunLoopDefaultMode: CFStringRef;
    pub static kCFBooleanTrue: CFTypeRef;
    pub fn CFMachPortCreateRunLoopSource(
        alloc: CFTypeRef,
        port: CFMachPortRef,
        order: isize,
    ) -> CFRunLoopSourceRef;
    pub fn CFRunLoopGetCurrent() -> CFRunLoopRef;
    pub fn CFRunLoopAddSource(rl: CFRunLoopRef, source: CFRunLoopSourceRef, mode: CFStringRef);
    pub fn CFRunLoopRunInMode(mode: CFStringRef, seconds: f64, return_after_source: bool) -> i32;
    pub fn CFRelease(cf: CFTypeRef);
    pub fn CFStringCreateWithCString(
        alloc: CFTypeRef,
        s: *const std::ffi::c_char,
        encoding: u32,
    ) -> CFTypeRef;
}

pub const HID_EVENT_TAP: u32 = 0;
pub const HEAD_INSERT: u32 = 0;
pub const TAP_DEFAULT: u32 = 0;
pub const SOURCE_HID_SYSTEM: i32 = 1;
pub const UTF8: u32 = 0x0800_0100;

// CGEventType
pub const EV_LEFT_DOWN: u32 = 1;
pub const EV_LEFT_UP: u32 = 2;
pub const EV_RIGHT_DOWN: u32 = 3;
pub const EV_RIGHT_UP: u32 = 4;
pub const EV_MOUSE_MOVED: u32 = 5;
pub const EV_LEFT_DRAGGED: u32 = 6;
pub const EV_RIGHT_DRAGGED: u32 = 7;
pub const EV_KEY_DOWN: u32 = 10;
pub const EV_KEY_UP: u32 = 11;
pub const EV_FLAGS_CHANGED: u32 = 12;
pub const EV_SCROLL: u32 = 22;
pub const EV_OTHER_DOWN: u32 = 25;
pub const EV_OTHER_UP: u32 = 26;
pub const EV_OTHER_DRAGGED: u32 = 27;
pub const EV_TAP_DISABLED_TIMEOUT: u32 = 0xFFFF_FFFE;
pub const EV_TAP_DISABLED_USER: u32 = 0xFFFF_FFFF;

// CGEventField
pub const FIELD_CLICK_STATE: u32 = 1;
pub const FIELD_BUTTON_NUMBER: u32 = 3;
pub const FIELD_DELTA_X: u32 = 4;
pub const FIELD_DELTA_Y: u32 = 5;
pub const FIELD_KEY_AUTOREPEAT: u32 = 8;
pub const FIELD_KEYCODE: u32 = 9;
pub const FIELD_SCROLL_DELTA_Y: u32 = 11;
pub const FIELD_SCROLL_DELTA_X: u32 = 12;
pub const FIELD_USER_DATA: u32 = 42;
pub const FIELD_SCROLL_CONTINUOUS: u32 = 88;
pub const FIELD_SCROLL_POINT_Y: u32 = 96;
pub const FIELD_SCROLL_POINT_X: u32 = 97;

/// Stamped on every event we inject so the capture tap lets it through untouched.
pub const MARK_INJECTED: i64 = 0x5053_0002;
