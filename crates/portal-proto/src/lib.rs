// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The PortalSwitch Authors

//! PortalSwitch wire protocol, version 1 (`protocol/v1.md`).
//!
//! This crate is *sans-IO*: it turns [`Message`] values into frames and byte buffers back into
//! messages, and knows nothing about sockets, TLS or runtimes. Transports feed it bytes.
//!
//! ```
//! use portal_proto::{Channel, Decoded, Message, Ping, decode, encode_to_vec};
//!
//! let bytes = encode_to_vec(&Message::Ping(Ping { nonce: 7, sender_time_us: 42 }))?;
//! let (decoded, used) = decode(Channel::Control, &bytes)?.expect("complete frame");
//! assert_eq!(used, bytes.len());
//! assert!(matches!(decoded, Decoded::Message(Message::Ping(p)) if p.nonce == 7));
//! # Ok::<(), portal_proto::Error>(())
//! ```

mod codec;
mod error;
mod message;
mod wire;

pub use codec::{Decoded, decode, encode, encode_to_vec};
pub use error::Error;
pub use message::*;

/// Protocol major version implemented by this crate.
pub const VERSION_MAJOR: u8 = 1;
/// Protocol minor version implemented by this crate.
pub const VERSION_MINOR: u8 = 0;

/// Logical channel a frame travels on. It only selects the maximum frame size; which messages
/// belong on which channel is enforced by the session layer.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Channel {
    /// Latency-critical session, input and announcement messages.
    Control,
    /// Clipboard payloads and file transfers.
    Bulk,
}

impl Channel {
    /// Maximum `length` field of a frame on this channel.
    #[must_use]
    pub const fn max_frame_len(self) -> u32 {
        match self {
            Self::Control => 65_536,
            Self::Bulk => 262_208,
        }
    }
}
