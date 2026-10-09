// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The PortalSwitch Authors

//! PortalSwitch networking: machine identity, pinned trust, TLS 1.3 with mutual
//! authentication, pairing codes and the link layer carrying protocol frames.

mod error;
mod fsutil;
pub mod identity;
pub mod pairing;
pub mod spki;
pub mod tls;
pub mod trust;

pub use error::Error;
pub use identity::Identity;
pub use trust::{TrustState, TrustStore, TrustedPeer};
