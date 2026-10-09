// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The PortalSwitch Authors

//! In-process fake backend: scripted capture, recording injector, fixed screens.

use std::sync::{Arc, Mutex};

use portal_core::input::InputEvent;
use portal_core::layout::Point;
use portal_core::peer::Inject;

use crate::{CaptureHandler, InputCapture, InputInjector, Monitor, PlatformError, Screens};

fn lock<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(std::sync::PoisonError::into_inner)
}

#[derive(Default)]
struct CaptureState {
    handler: Option<CaptureHandler>,
    peer_active: bool,
    warps: Vec<Point>,
}

/// Fake capture backend; the test drives it through a [`FakeCaptureHandle`].
#[derive(Clone, Default)]
pub struct FakeCapture {
    state: Arc<Mutex<CaptureState>>,
}

/// Test-side control of a [`FakeCapture`].
#[derive(Clone)]
pub struct FakeCaptureHandle {
    state: Arc<Mutex<CaptureState>>,
}

impl std::fmt::Debug for FakeCapture {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("FakeCapture").finish_non_exhaustive()
    }
}

impl std::fmt::Debug for FakeCaptureHandle {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("FakeCaptureHandle").finish_non_exhaustive()
    }
}

impl FakeCapture {
    #[must_use]
    pub fn new() -> (Self, FakeCaptureHandle) {
        let capture = Self::default();
        let handle = FakeCaptureHandle {
            state: capture.state.clone(),
        };
        (capture, handle)
    }
}

impl FakeCaptureHandle {
    /// Deliver an input event as the OS would; returns whether it was swallowed.
    ///
    /// # Panics
    /// If capture has not been started.
    #[must_use]
    pub fn send(&self, event: InputEvent) -> bool {
        // Take the handler out so it can lock other state (it may call back into the capture).
        let mut handler = lock(&self.state)
            .handler
            .take()
            .expect("capture not started");
        let swallowed = handler(event);
        lock(&self.state).handler = Some(handler);
        swallowed
    }

    /// Whether the host cursor is currently hidden/parked for a peer.
    #[must_use]
    pub fn peer_active(&self) -> bool {
        lock(&self.state).peer_active
    }

    /// Cursor warps requested so far.
    #[must_use]
    pub fn warps(&self) -> Vec<Point> {
        lock(&self.state).warps.clone()
    }
}

impl InputCapture for FakeCapture {
    fn start(&mut self, handler: CaptureHandler) -> Result<(), PlatformError> {
        lock(&self.state).handler = Some(handler);
        Ok(())
    }

    fn set_peer_active(&mut self, active: bool) -> Result<(), PlatformError> {
        lock(&self.state).peer_active = active;
        Ok(())
    }

    fn warp_cursor(&mut self, to: Point) -> Result<(), PlatformError> {
        lock(&self.state).warps.push(to);
        Ok(())
    }

    fn stop(&mut self) {
        lock(&self.state).handler = None;
    }
}

/// Fake injector that records every action.
#[derive(Clone, Default)]
pub struct FakeInjector {
    log: Arc<Mutex<Vec<Inject>>>,
}

impl std::fmt::Debug for FakeInjector {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("FakeInjector").finish_non_exhaustive()
    }
}

impl FakeInjector {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Everything injected so far.
    #[must_use]
    pub fn log(&self) -> Vec<Inject> {
        lock(&self.log).clone()
    }
}

impl InputInjector for FakeInjector {
    fn inject(&mut self, action: Inject) -> Result<(), PlatformError> {
        lock(&self.log).push(action);
        Ok(())
    }
}

/// Fake screens with a fixed arrangement.
#[derive(Debug, Clone)]
pub struct FakeScreens {
    pub monitors: Vec<Monitor>,
    pub cursor: Point,
}

impl FakeScreens {
    /// A single monitor at the origin.
    #[must_use]
    pub fn single(width: u32, height: u32) -> Self {
        Self {
            monitors: vec![Monitor {
                id: 1,
                x: 0,
                y: 0,
                width,
                height,
                scale_permille: 1000,
            }],
            cursor: Point {
                x: i32::try_from(width / 2).unwrap_or(0),
                y: i32::try_from(height / 2).unwrap_or(0),
            },
        }
    }
}

impl Screens for FakeScreens {
    fn monitors(&self) -> Vec<Monitor> {
        self.monitors.clone()
    }

    fn cursor(&self) -> Point {
        self.cursor
    }
}
