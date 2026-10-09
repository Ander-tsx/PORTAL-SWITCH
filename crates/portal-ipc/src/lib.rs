// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The PortalSwitch Authors

//! Local API between the PortalSwitch daemon and the UI or command-line tools.
//!
//! JSON lines over a Unix domain socket (macOS/Linux) or a named pipe (Windows), authenticated
//! with a per-user token file. See [`proto`] for the wire format.

pub mod client;
pub mod proto;
pub mod server;
pub mod token;
pub mod transport;

pub use client::{Client, ClientError};
pub use proto::{Event, Request, StatusInfo};
pub use transport::Endpoint;
