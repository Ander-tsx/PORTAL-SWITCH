// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The PortalSwitch Authors

//! Windows backends for PortalSwitch: low-level hooks plus Raw Input for capture, `SendInput`
//! for injection and the monitor list for the layout.
//!
//! The pure conversion logic lives in [`convert`] and compiles everywhere; the Win32 code is
//! Windows only. This crate is empty on other platforms.

// Win32 fields are small bounded integers (scan codes, wheel deltas, pixel coordinates), so the
// numeric casts between them and the protocol types cannot lose information in practice.
#![allow(
    clippy::cast_possible_truncation,
    clippy::cast_possible_wrap,
    clippy::cast_sign_loss,
    clippy::cast_lossless,
    clippy::borrow_as_ptr
)]

pub mod convert;

#[cfg(windows)]
mod backend;

#[cfg(windows)]
pub use backend::{WinCapture, WinInjector, WinScreens, enable_dpi_awareness};
