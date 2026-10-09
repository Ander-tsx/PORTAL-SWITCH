// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The PortalSwitch Authors

//! The PortalSwitch daemon: networking, routing and platform glue.

/// Configuration model (re-exported from `portal-config`).
pub mod config {
    pub use portal_config::*;
}
pub mod daemon;
pub mod ipc;
