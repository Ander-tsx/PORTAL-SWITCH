// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The PortalSwitch Authors

//! Message types (`protocol/v1.md` §6–§8).

#![allow(missing_docs)]

use crate::Channel;

/// Message type identifiers, including the critical bit (`0x8000`).
pub mod ty {
    /// Critical bit of a message type.
    pub const CRITICAL: u16 = 0x8000;

    pub const HELLO: u16 = 0x8001;
    pub const SESSION_START: u16 = 0x8002;
    pub const GOODBYE: u16 = 0x8003;
    pub const PING: u16 = 0x0004;
    pub const PONG: u16 = 0x0005;
    pub const BULK_ATTACH: u16 = 0x8006;
    pub const PAIR_START: u16 = 0x8010;
    pub const PAIR_CONFIRM: u16 = 0x8011;
    pub const ENTER: u16 = 0x8101;
    pub const LEAVE: u16 = 0x8102;
    pub const LEAVE_ACK: u16 = 0x8103;
    pub const MOUSE_MOTION: u16 = 0x8104;
    pub const MOUSE_ABSOLUTE: u16 = 0x8105;
    pub const MOUSE_BUTTON: u16 = 0x8106;
    pub const WHEEL: u16 = 0x8107;
    pub const KEY: u16 = 0x8108;
    pub const RELEASE_ALL: u16 = 0x8109;
    pub const CLIPBOARD_GRAB: u16 = 0x0201;
    pub const DRAG_START: u16 = 0x0301;
    pub const TRANSFER_BEGIN: u16 = 0x8302;
    pub const TRANSFER_ITEM: u16 = 0x8303;
    pub const TRANSFER_CHUNK: u16 = 0x8304;
    pub const TRANSFER_ITEM_END: u16 = 0x8305;
    pub const TRANSFER_END: u16 = 0x8306;
    pub const TRANSFER_CANCEL: u16 = 0x8307;
    pub const TRANSFER_RESUME: u16 = 0x8308;
    pub const SCREEN_INFO: u16 = 0x8401;
    pub const SCREEN_INFO_ACK: u16 = 0x0402;
    pub const CONFIG_PUSH: u16 = 0x0403;
}

/// `Goodbye` reason codes.
pub mod goodbye {
    pub const NORMAL: u16 = 1;
    pub const INCOMPATIBLE_VERSION: u16 = 2;
    pub const DUPLICATE_MACHINE: u16 = 3;
    pub const NOT_PAIRED: u16 = 4;
    pub const PAIRING_REJECTED: u16 = 5;
    pub const PROTOCOL_VIOLATION: u16 = 6;
    pub const FRAME_TOO_LARGE: u16 = 7;
    pub const TIMEOUT: u16 = 8;
    pub const SHUTTING_DOWN: u16 = 9;
    pub const LIMIT_EXCEEDED: u16 = 10;
    pub const INTERNAL_ERROR: u16 = 11;
}

/// Capability bits (`Hello::capabilities`).
pub mod cap {
    pub const CLIPBOARD_TEXT: u64 = 1 << 0;
    pub const CLIPBOARD_HTML: u64 = 1 << 1;
    pub const CLIPBOARD_PNG: u64 = 1 << 2;
    pub const CLIPBOARD_RTF: u64 = 1 << 3;
    pub const FILE_TRANSFER: u64 = 1 << 4;
    pub const DRAG_AND_DROP: u64 = 1 << 5;
    pub const RELATIVE_MOUSE: u64 = 1 << 6;
    pub const EAGER_CLIPBOARD: u64 = 1 << 7;
}

/// Modifier mask bits (`Key::modifiers`, `Enter::modifiers`).
pub mod modifier {
    pub const SHIFT_LEFT: u32 = 1 << 0;
    pub const SHIFT_RIGHT: u32 = 1 << 1;
    pub const CONTROL_LEFT: u32 = 1 << 2;
    pub const CONTROL_RIGHT: u32 = 1 << 3;
    pub const ALT_LEFT: u32 = 1 << 4;
    pub const ALT_RIGHT: u32 = 1 << 5;
    pub const META_LEFT: u32 = 1 << 6;
    pub const META_RIGHT: u32 = 1 << 7;
    pub const ALT_GR: u32 = 1 << 8;
    pub const FN: u32 = 1 << 9;
    pub const LEVEL5: u32 = 1 << 10;
}

/// Maximum monitors in one [`ScreenInfo`].
pub const MAX_MONITORS: usize = 16;
/// Maximum data bytes in one [`TransferChunk`].
pub const MAX_CHUNK_DATA: usize = 262_144;
/// Maximum size of a [`ConfigPush`] document.
pub const MAX_CONFIG_DOCUMENT: usize = 60 * 1024;
/// Maximum payload size of a [`Hello`].
pub const MAX_HELLO_PAYLOAD: usize = 4 * 1024;

/// `Hello::flags` bit: this side is the host.
pub const HELLO_FLAG_HOST: u16 = 1;

/// First message on every link, sent by both sides.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Hello {
    pub major: u8,
    pub minor: u8,
    pub flags: u16,
    pub capabilities: u64,
    pub machine_id: [u8; 16],
    pub machine_name: String,
    pub app_version: String,
}

/// Acceptor → initiator; carries the single-use bulk attach token.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SessionStart {
    pub session_id: [u8; 16],
    pub bulk_token: [u8; 16],
}

/// Orderly or fatal link shutdown.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Goodbye {
    /// One of the [`goodbye`] constants.
    pub reason: u16,
    pub detail: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Ping {
    pub nonce: u32,
    pub sender_time_us: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Pong {
    pub nonce: u32,
    pub echoed_time_us: u64,
}

/// First frame on a bulk connection (dual-TCP mapping).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BulkAttach {
    pub bulk_token: [u8; 16],
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PairConfirm {
    pub accepted: bool,
}

/// The pointer enters the peer.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Enter {
    pub enter_seq: u32,
    pub x: i32,
    pub y: i32,
    /// Lock toggle bits: 0 Caps, 1 Num, 2 Scroll.
    pub toggles: u8,
    pub modifiers: u32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Leave {
    pub enter_seq: u32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LeaveAck {
    pub enter_seq: u32,
    pub ok: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MouseMotion {
    pub enter_seq: u32,
    pub t_us: u32,
    pub dx: i16,
    pub dy: i16,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MouseAbsolute {
    pub enter_seq: u32,
    pub t_us: u32,
    pub x: i32,
    pub y: i32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MouseButton {
    pub enter_seq: u32,
    pub t_us: u32,
    /// 1 left, 2 right, 3 middle, 4 back, 5 forward, 6–32 extra.
    pub button: u8,
    pub down: bool,
}

/// Scroll in 1/120 notch units.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Wheel {
    pub enter_seq: u32,
    pub t_us: u32,
    pub dx: i16,
    pub dy: i16,
    /// Bit 0: continuous (high-resolution) device.
    pub flags: u8,
}

/// Key action values for [`Key::action`].
pub mod key_action {
    pub const UP: u8 = 0;
    pub const DOWN: u8 = 1;
    pub const REPEAT: u8 = 2;
}

/// `Key::flags` bits.
pub mod key_flag {
    pub const DEAD_KEY: u8 = 1 << 0;
    pub const KEYPAD: u8 = 1 << 1;
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Key {
    pub enter_seq: u32,
    pub t_us: u32,
    /// One of [`key_action`].
    pub action: u8,
    pub repeat_count: u16,
    pub key_id: u32,
    /// `(usage_page << 16) | usage`.
    pub hid_usage: u32,
    pub codepoint: u32,
    pub named_key: u16,
    pub modifiers: u32,
    /// Bits of [`key_flag`].
    pub flags: u8,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ReleaseAll {
    pub enter_seq: u32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ClipboardGrab {
    pub clip_seq: u32,
    pub enter_seq: u32,
    /// Bit 0 text, 1 HTML, 2 PNG, 3 RTF.
    pub formats: u32,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DragStart {
    pub transfer_id: u32,
    pub item_count: u32,
    pub total_size: u64,
    pub first_name: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TransferBegin {
    pub transfer_id: u32,
    /// 1 clipboard, 2 files.
    pub kind: u8,
    pub item_count: u32,
    pub total_size: u64,
    pub ref_seq: u32,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TransferItem {
    pub transfer_id: u32,
    pub item_index: u32,
    /// 1 file, 2 directory, 3 clipboard format.
    pub item_kind: u8,
    pub format: u16,
    pub size: u64,
    pub path: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TransferChunk {
    pub transfer_id: u32,
    pub item_index: u32,
    pub offset: u64,
    pub data: Vec<u8>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TransferItemEnd {
    pub transfer_id: u32,
    pub item_index: u32,
    pub sha256: [u8; 32],
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TransferEnd {
    pub transfer_id: u32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TransferCancel {
    pub transfer_id: u32,
    /// 1 user, 2 declined, 3 bad path, 4 disk error, 5 integrity, 6 superseded, 7 too large.
    pub reason: u16,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TransferResume {
    pub transfer_id: u32,
    pub item_index: u32,
    pub offset: u64,
}

/// One monitor in global pixel coordinates.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Monitor {
    pub monitor_id: u32,
    pub x: i32,
    pub y: i32,
    pub width: u32,
    pub height: u32,
    /// 1000 = 1×.
    pub scale_permille: u16,
    /// 0 = unknown.
    pub width_mm: u16,
    /// 0 = unknown.
    pub height_mm: u16,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ScreenInfo {
    pub info_seq: u32,
    pub cursor_x: i32,
    pub cursor_y: i32,
    pub monitors: Vec<Monitor>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ScreenInfoAck {
    pub info_seq: u32,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConfigPush {
    pub config_version: u64,
    /// UTF-8 JSON, at most [`MAX_CONFIG_DOCUMENT`] bytes.
    pub document: Vec<u8>,
}

/// Every message of protocol v1.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Message {
    Hello(Hello),
    SessionStart(SessionStart),
    Goodbye(Goodbye),
    Ping(Ping),
    Pong(Pong),
    BulkAttach(BulkAttach),
    PairStart,
    PairConfirm(PairConfirm),
    Enter(Enter),
    Leave(Leave),
    LeaveAck(LeaveAck),
    MouseMotion(MouseMotion),
    MouseAbsolute(MouseAbsolute),
    MouseButton(MouseButton),
    Wheel(Wheel),
    Key(Key),
    ReleaseAll(ReleaseAll),
    ClipboardGrab(ClipboardGrab),
    DragStart(DragStart),
    TransferBegin(TransferBegin),
    TransferItem(TransferItem),
    TransferChunk(TransferChunk),
    TransferItemEnd(TransferItemEnd),
    TransferEnd(TransferEnd),
    TransferCancel(TransferCancel),
    TransferResume(TransferResume),
    ScreenInfo(ScreenInfo),
    ScreenInfoAck(ScreenInfoAck),
    ConfigPush(ConfigPush),
}

impl Message {
    /// Wire type identifier (with the critical bit where the registry sets it).
    #[must_use]
    pub const fn type_id(&self) -> u16 {
        match self {
            Self::Hello(_) => ty::HELLO,
            Self::SessionStart(_) => ty::SESSION_START,
            Self::Goodbye(_) => ty::GOODBYE,
            Self::Ping(_) => ty::PING,
            Self::Pong(_) => ty::PONG,
            Self::BulkAttach(_) => ty::BULK_ATTACH,
            Self::PairStart => ty::PAIR_START,
            Self::PairConfirm(_) => ty::PAIR_CONFIRM,
            Self::Enter(_) => ty::ENTER,
            Self::Leave(_) => ty::LEAVE,
            Self::LeaveAck(_) => ty::LEAVE_ACK,
            Self::MouseMotion(_) => ty::MOUSE_MOTION,
            Self::MouseAbsolute(_) => ty::MOUSE_ABSOLUTE,
            Self::MouseButton(_) => ty::MOUSE_BUTTON,
            Self::Wheel(_) => ty::WHEEL,
            Self::Key(_) => ty::KEY,
            Self::ReleaseAll(_) => ty::RELEASE_ALL,
            Self::ClipboardGrab(_) => ty::CLIPBOARD_GRAB,
            Self::DragStart(_) => ty::DRAG_START,
            Self::TransferBegin(_) => ty::TRANSFER_BEGIN,
            Self::TransferItem(_) => ty::TRANSFER_ITEM,
            Self::TransferChunk(_) => ty::TRANSFER_CHUNK,
            Self::TransferItemEnd(_) => ty::TRANSFER_ITEM_END,
            Self::TransferEnd(_) => ty::TRANSFER_END,
            Self::TransferCancel(_) => ty::TRANSFER_CANCEL,
            Self::TransferResume(_) => ty::TRANSFER_RESUME,
            Self::ScreenInfo(_) => ty::SCREEN_INFO,
            Self::ScreenInfoAck(_) => ty::SCREEN_INFO_ACK,
            Self::ConfigPush(_) => ty::CONFIG_PUSH,
        }
    }

    /// Channel this message is sent on.
    #[must_use]
    pub const fn channel(&self) -> Channel {
        match self {
            Self::BulkAttach(_)
            | Self::TransferBegin(_)
            | Self::TransferItem(_)
            | Self::TransferChunk(_)
            | Self::TransferItemEnd(_)
            | Self::TransferEnd(_)
            | Self::TransferCancel(_)
            | Self::TransferResume(_) => Channel::Bulk,
            _ => Channel::Control,
        }
    }

    /// Input messages flow host → peer only and carry an `enter_seq`.
    #[must_use]
    pub const fn is_input(&self) -> bool {
        matches!(
            self,
            Self::Enter(_)
                | Self::Leave(_)
                | Self::MouseMotion(_)
                | Self::MouseAbsolute(_)
                | Self::MouseButton(_)
                | Self::Wheel(_)
                | Self::Key(_)
                | Self::ReleaseAll(_)
        )
    }
}
