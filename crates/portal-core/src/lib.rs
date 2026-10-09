// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The PortalSwitch Authors

//! Platform-independent core of PortalSwitch.
//!
//! This crate will hold the types and state machines shared by the daemon and the UI
//! (topology, routing, protocol messages). It must never depend on OS-specific code.

pub mod gate;
pub mod input;
pub mod layout;
pub mod peer;

/// Version of this build, as declared in the workspace manifest.
pub const VERSION: &str = env!("CARGO_PKG_VERSION");

#[cfg(test)]
mod tests {
    use super::VERSION;

    #[test]
    fn version_is_semver_like() {
        assert_eq!(VERSION.split('.').count(), 3);
    }
}
