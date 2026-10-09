// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The PortalSwitch Authors

//! Wire types of the local daemon API (version 1).
//!
//! Framing is one JSON document per line. After the hello exchange the client sends
//! `{"id":N,"method":"…","params":{…}}` and receives `{"id":N,"ok":true,"result":…}` or
//! `{"id":N,"ok":false,"error":{"code":"…","message":"…"}}`. After `subscribe` the daemon also
//! pushes `{"event":"…","data":{…}}` lines. Machine IDs are 32-digit hex strings.

use serde::{Deserialize, Serialize};
use serde_json::Value;

/// Version of this API. Bumped only for incompatible changes; additions keep the number.
pub const PROTOCOL_VERSION: u32 = 1;

/// First line from the client.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ClientHello {
    pub version: u32,
    /// Contents of the per-user token file; proves the client can read the user's files.
    pub token: String,
}

/// First line from the daemon (or an error reply if the token or version is wrong).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ServerHello {
    pub version: u32,
    pub daemon_version: String,
}

/// A request to the daemon.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "method", content = "params", rename_all = "snake_case")]
pub enum Request {
    Status,
    Paired,
    Connect {
        address: String,
    },
    Jump {
        machine: String,
    },
    SetLocked {
        locked: bool,
    },
    AnswerPairing {
        id: u64,
        accept: bool,
    },
    Revoke {
        machine: String,
    },
    GetConfig,
    /// Replace the configuration document (same shape as `get_config` returns).
    SetConfig {
        config: Value,
    },
    /// Start receiving [`Event`] lines on this connection.
    Subscribe,
    Shutdown,
}

/// A request line: an ID chosen by the client plus the request.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RequestFrame {
    pub id: u64,
    #[serde(flatten)]
    pub request: Request,
}

/// Error codes returned in [`ErrorBody::code`].
pub mod code {
    pub const BAD_REQUEST: &str = "bad_request";
    pub const UNAUTHORIZED: &str = "unauthorized";
    pub const VERSION: &str = "unsupported_version";
    pub const NOT_FOUND: &str = "not_found";
    pub const FAILED: &str = "failed";
    pub const NOT_HOST: &str = "not_host";
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, thiserror::Error)]
#[error("{code}: {message}")]
pub struct ErrorBody {
    pub code: String,
    pub message: String,
}

impl ErrorBody {
    #[must_use]
    pub fn new(code: &str, message: impl Into<String>) -> Self {
        Self {
            code: code.to_owned(),
            message: message.into(),
        }
    }
}

/// A reply line.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Reply {
    pub id: u64,
    pub ok: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub result: Option<Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<ErrorBody>,
}

impl Reply {
    #[must_use]
    pub const fn ok(id: u64, result: Value) -> Self {
        Self {
            id,
            ok: true,
            result: Some(result),
            error: None,
        }
    }

    #[must_use]
    pub const fn err(id: u64, error: ErrorBody) -> Self {
        Self {
            id,
            ok: false,
            result: None,
            error: Some(error),
        }
    }
}

/// A machine as the UI sees it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PeerInfo {
    pub machine: String,
    pub name: String,
}

/// Result of `status`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct StatusInfo {
    pub machine: String,
    pub is_host: bool,
    pub active: String,
    pub locked: bool,
    pub peers: Vec<PeerInfo>,
}

/// Pushed after `subscribe`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "event", content = "data", rename_all = "snake_case")]
pub enum Event {
    PairingRequested {
        id: u64,
        remote_name: String,
        code: String,
    },
    PeerConnected {
        machine: String,
        name: String,
    },
    PeerDisconnected {
        machine: String,
    },
    ActiveChanged {
        machine: String,
    },
    DialFailed {
        address: String,
        reason: String,
    },
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn requests_have_a_stable_json_shape() {
        let frame = RequestFrame {
            id: 7,
            request: Request::Jump {
                machine: "ab".into(),
            },
        };
        assert_eq!(
            serde_json::to_string(&frame).unwrap(),
            r#"{"id":7,"method":"jump","params":{"machine":"ab"}}"#
        );
        let back: RequestFrame = serde_json::from_str(r#"{"id":1,"method":"status"}"#).unwrap();
        assert_eq!(
            back,
            RequestFrame {
                id: 1,
                request: Request::Status
            }
        );
        let set: RequestFrame =
            serde_json::from_str(r#"{"id":2,"method":"set_locked","params":{"locked":true}}"#)
                .unwrap();
        assert_eq!(set.request, Request::SetLocked { locked: true });
    }

    #[test]
    fn unknown_methods_do_not_parse() {
        assert!(serde_json::from_str::<RequestFrame>(r#"{"id":1,"method":"nope"}"#).is_err());
    }

    #[test]
    fn replies_and_events_serialize_compactly() {
        let ok = Reply::ok(3, serde_json::json!({"x": 1}));
        assert_eq!(
            serde_json::to_string(&ok).unwrap(),
            r#"{"id":3,"ok":true,"result":{"x":1}}"#
        );
        let err = Reply::err(4, ErrorBody::new(code::NOT_FOUND, "no such machine"));
        assert_eq!(
            serde_json::to_string(&err).unwrap(),
            r#"{"id":4,"ok":false,"error":{"code":"not_found","message":"no such machine"}}"#
        );
        let event = Event::ActiveChanged {
            machine: "ff".into(),
        };
        assert_eq!(
            serde_json::to_string(&event).unwrap(),
            r#"{"event":"active_changed","data":{"machine":"ff"}}"#
        );
    }
}
