// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The PortalSwitch Authors

//! Platform abstraction: everything OS-specific the daemon needs, behind traits.
//!
//! Real backends live in `portal-platform-macos` and `portal-platform-windows`. The [`fake`]
//! module provides an in-process backend so the whole daemon can run headlessly in CI.

pub mod fake;

use portal_core::input::InputEvent;
use portal_core::layout::Point;
use portal_core::peer::Inject;

/// Something a platform backend could not do.
#[derive(Debug, thiserror::Error)]
pub enum PlatformError {
    /// A required OS permission (e.g. macOS Accessibility / Input Monitoring) is missing.
    #[error("permission missing: {0}")]
    Permission(&'static str),
    /// The OS refused or failed an operation.
    #[error("{0}")]
    Os(String),
}

/// Called for every captured event, on the capture thread. Returning `true` withholds the
/// event from host applications.
pub type CaptureHandler = Box<dyn FnMut(InputEvent) -> bool + Send>;

/// Captures keyboard and mouse input on the machine that owns the physical devices.
pub trait InputCapture: Send {
    /// Start delivering events to `handler`.
    ///
    /// # Errors
    /// Permissions missing or the OS refused to install the tap/hook.
    fn start(&mut self, handler: CaptureHandler) -> Result<(), PlatformError>;

    /// While `active`, the host cursor is hidden and parked (a peer is receiving input).
    ///
    /// # Errors
    /// The OS refused the change.
    fn set_peer_active(&mut self, active: bool) -> Result<(), PlatformError>;

    /// Move the host's real cursor.
    ///
    /// # Errors
    /// The OS refused the move.
    fn warp_cursor(&mut self, to: Point) -> Result<(), PlatformError>;

    /// Stop capturing and release the OS hooks.
    fn stop(&mut self);
}

/// Synthesizes input on this machine.
pub trait InputInjector: Send {
    /// Perform one injection. Injected events must be marked synthetic so they are not
    /// captured again.
    ///
    /// # Errors
    /// The OS refused the injection.
    fn inject(&mut self, action: Inject) -> Result<(), PlatformError>;
}

/// One physical display.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Monitor {
    pub id: u32,
    pub x: i32,
    pub y: i32,
    pub width: u32,
    pub height: u32,
    /// 1000 = 1×.
    pub scale_permille: u16,
}

/// The machine's display arrangement.
pub trait Screens: Send + Sync {
    /// Current monitors in global pixel coordinates.
    fn monitors(&self) -> Vec<Monitor>;
    /// Current cursor position in global coordinates.
    fn cursor(&self) -> Point;
}

/// Width and height of the bounding box of all monitors, and its origin.
#[must_use]
pub fn bounding_box(monitors: &[Monitor]) -> Option<(Point, u32, u32)> {
    let first = monitors.first()?;
    let (mut left, mut top) = (i64::from(first.x), i64::from(first.y));
    let (mut right, mut bottom) = (left + i64::from(first.width), top + i64::from(first.height));
    for m in monitors {
        left = left.min(i64::from(m.x));
        top = top.min(i64::from(m.y));
        right = right.max(i64::from(m.x) + i64::from(m.width));
        bottom = bottom.max(i64::from(m.y) + i64::from(m.height));
    }
    Some((
        Point {
            x: i32::try_from(left).ok()?,
            y: i32::try_from(top).ok()?,
        },
        u32::try_from(right - left).ok()?,
        u32::try_from(bottom - top).ok()?,
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    const fn mon(id: u32, x: i32, y: i32, width: u32, height: u32) -> Monitor {
        Monitor {
            id,
            x,
            y,
            width,
            height,
            scale_permille: 1000,
        }
    }

    #[test]
    fn bounding_box_spans_all_monitors() {
        let monitors = [mon(1, 0, 0, 1920, 1080), mon(2, -1280, 100, 1280, 720)];
        let (origin, width, height) = bounding_box(&monitors).unwrap();
        assert_eq!(
            (origin, width, height),
            (Point { x: -1280, y: 0 }, 3200, 1080)
        );
        assert!(bounding_box(&[]).is_none());
    }
}
