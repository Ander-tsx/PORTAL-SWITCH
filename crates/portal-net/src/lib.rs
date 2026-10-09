// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The PortalSwitch Authors

//! PortalSwitch networking: machine identity, pinned trust, TLS 1.3 with mutual
//! authentication, pairing codes and the link layer carrying protocol frames.

pub mod bridge;
pub mod discovery;
mod error;
pub mod frame;
mod fsutil;
pub mod identity;
pub mod link;
pub mod pairing;
pub mod queue;
pub mod session;
pub mod spki;
pub mod tcp;
pub mod tls;
pub mod trust;

pub use error::Error;
pub use identity::Identity;
pub use trust::{TrustState, TrustStore, TrustedPeer};
