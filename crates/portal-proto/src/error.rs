// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The PortalSwitch Authors

use crate::Channel;

/// Reasons a frame is rejected. Every variant maps to a
/// `Goodbye` reason through [`Error::goodbye_reason`].
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum Error {
    /// The `length` header exceeds the maximum for the channel.
    #[error("frame length {len} exceeds the {max} byte limit of the {channel:?} channel")]
    FrameTooLarge {
        /// Declared length.
        len: u32,
        /// Channel limit.
        max: u32,
        /// Channel the frame was read from.
        channel: Channel,
    },
    /// The `length` header leaves no room for the 2-byte type.
    #[error("frame length {0} is smaller than the 2-byte type field")]
    LengthBelowMinimum(u32),
    /// Unknown message type with the critical bit set.
    #[error("unknown critical message type {0:#06x}")]
    UnknownCritical(u16),
    /// Payload shorter than the minimum for its message type.
    #[error("payload of message {0:#06x} is shorter than its minimum layout")]
    ShortPayload(u16),
    /// A `bool` field holding something other than 0 or 1.
    #[error("invalid bool value {0}")]
    InvalidBool(u8),
    /// A `str` field that is not valid UTF-8.
    #[error("string is not valid UTF-8")]
    InvalidUtf8,
    /// A field exceeds its documented maximum (string, blob, list or payload size).
    #[error("{field} exceeds its limit of {max}")]
    LimitExceeded {
        /// Field name.
        field: &'static str,
        /// Documented maximum.
        max: usize,
    },
}

impl Error {
    /// `Goodbye` reason code (`protocol/v1.md` §7) to send before closing the link.
    #[must_use]
    pub const fn goodbye_reason(&self) -> u16 {
        match self {
            Self::FrameTooLarge { .. } => crate::goodbye::FRAME_TOO_LARGE,
            Self::LimitExceeded { .. } => crate::goodbye::LIMIT_EXCEEDED,
            _ => crate::goodbye::PROTOCOL_VIOLATION,
        }
    }
}
