// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The PortalSwitch Authors

//! macOS backends for PortalSwitch: event tap capture, `CGEvent` injection and displays.
//!
//! The pure conversion logic lives in [`convert`] and compiles everywhere; the FFI modules are
//! macOS only. This crate is empty on other platforms.

// Coordinates and key codes are bounded by the OS (small integers, display sizes), so the
// numeric casts between CoreGraphics and protocol types cannot lose information in practice.
#![allow(
    clippy::cast_possible_truncation,
    clippy::cast_possible_wrap,
    clippy::cast_precision_loss,
    clippy::cast_sign_loss
)]

pub mod convert;

#[cfg(target_os = "macos")]
mod backend;
#[cfg(target_os = "macos")]
mod ffi;

#[cfg(target_os = "macos")]
pub use backend::{
    MacCapture, MacInjector, MacScreens, Permissions, permissions, request_permissions,
};
