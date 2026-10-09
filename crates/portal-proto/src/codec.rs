// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The PortalSwitch Authors

//! Frame encoding and decoding (`protocol/v1.md` §4, §7).

use crate::message::{
    BulkAttach, ClipboardGrab, ConfigPush, DragStart, Enter, Goodbye, Hello, Key, Leave, LeaveAck,
    MAX_CHUNK_DATA, MAX_CONFIG_DOCUMENT, MAX_HELLO_PAYLOAD, MAX_MONITORS, Message, Monitor,
    MouseAbsolute, MouseButton, MouseMotion, PairConfirm, Ping, Pong, ReleaseAll, ScreenInfo,
    ScreenInfoAck, SessionStart, TransferBegin, TransferCancel, TransferChunk, TransferEnd,
    TransferItem, TransferItemEnd, TransferResume, Wheel, ty,
};
use crate::wire::{Reader, Writer};
use crate::{Channel, Error};

/// Result of decoding one complete frame.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Decoded {
    /// A known message.
    Message(Message),
    /// Unknown type without the critical bit: the frame was consumed and must be ignored.
    Ignored {
        /// The unknown type identifier.
        type_id: u16,
    },
}

/// Try to decode one frame from the start of `buf`.
///
/// Returns `Ok(None)` when more bytes are needed, otherwise the decoded frame and the number of
/// bytes it occupied. The `length` header is validated as soon as its four bytes are available,
/// before any payload is buffered.
///
/// # Errors
/// Any [`Error`]: the link must be closed with [`Error::goodbye_reason`].
pub fn decode(channel: Channel, buf: &[u8]) -> Result<Option<(Decoded, usize)>, Error> {
    let Some(header) = buf.first_chunk::<4>() else {
        return Ok(None);
    };
    let len = u32::from_le_bytes(*header);
    if len < 2 {
        return Err(Error::LengthBelowMinimum(len));
    }
    let max = channel.max_frame_len();
    if len > max {
        return Err(Error::FrameTooLarge { len, max, channel });
    }
    let total = 4 + len as usize;
    if buf.len() < total {
        return Ok(None);
    }
    let type_id = u16::from_le_bytes([buf[4], buf[5]]);
    let decoded = decode_payload(type_id, &buf[6..total])?;
    Ok(Some((decoded, total)))
}

// One flat match mirrors the message table of the spec; splitting it would hide that.
#[allow(clippy::too_many_lines)]
fn decode_payload(type_id: u16, payload: &[u8]) -> Result<Decoded, Error> {
    let mut r = Reader::new(payload, type_id);
    let message = match type_id {
        ty::HELLO => {
            if payload.len() > MAX_HELLO_PAYLOAD {
                return Err(Error::LimitExceeded {
                    field: "Hello payload",
                    max: MAX_HELLO_PAYLOAD,
                });
            }
            Message::Hello(Hello {
                major: r.u8()?,
                minor: r.u8()?,
                flags: r.u16()?,
                capabilities: r.u64()?,
                machine_id: r.bytes16()?,
                machine_name: r.str("machine_name", 64)?,
                app_version: r.str("app_version", 32)?,
            })
        }
        ty::SESSION_START => Message::SessionStart(SessionStart {
            session_id: r.bytes16()?,
            bulk_token: r.bytes16()?,
        }),
        ty::GOODBYE => {
            let reason = r.u16()?;
            let detail = if r.remaining() == 0 {
                String::new()
            } else {
                r.str("detail", 256)?
            };
            Message::Goodbye(Goodbye { reason, detail })
        }
        ty::PING => Message::Ping(Ping {
            nonce: r.u32()?,
            sender_time_us: r.u64()?,
        }),
        ty::PONG => Message::Pong(Pong {
            nonce: r.u32()?,
            echoed_time_us: r.u64()?,
        }),
        ty::BULK_ATTACH => Message::BulkAttach(BulkAttach {
            bulk_token: r.bytes16()?,
        }),
        ty::PAIR_START => Message::PairStart,
        ty::PAIR_CONFIRM => Message::PairConfirm(PairConfirm {
            accepted: r.bool()?,
        }),
        ty::ENTER => Message::Enter(Enter {
            enter_seq: r.u32()?,
            x: r.i32()?,
            y: r.i32()?,
            toggles: r.u8()?,
            modifiers: r.u32()?,
        }),
        ty::LEAVE => Message::Leave(Leave {
            enter_seq: r.u32()?,
        }),
        ty::LEAVE_ACK => Message::LeaveAck(LeaveAck {
            enter_seq: r.u32()?,
            ok: r.bool()?,
        }),
        ty::MOUSE_MOTION => Message::MouseMotion(MouseMotion {
            enter_seq: r.u32()?,
            t_us: r.u32()?,
            dx: r.i16()?,
            dy: r.i16()?,
        }),
        ty::MOUSE_ABSOLUTE => Message::MouseAbsolute(MouseAbsolute {
            enter_seq: r.u32()?,
            t_us: r.u32()?,
            x: r.i32()?,
            y: r.i32()?,
        }),
        ty::MOUSE_BUTTON => Message::MouseButton(MouseButton {
            enter_seq: r.u32()?,
            t_us: r.u32()?,
            button: r.u8()?,
            down: r.bool()?,
        }),
        ty::WHEEL => Message::Wheel(Wheel {
            enter_seq: r.u32()?,
            t_us: r.u32()?,
            dx: r.i16()?,
            dy: r.i16()?,
            flags: r.u8()?,
        }),
        ty::KEY => Message::Key(Key {
            enter_seq: r.u32()?,
            t_us: r.u32()?,
            action: r.u8()?,
            repeat_count: r.u16()?,
            key_id: r.u32()?,
            hid_usage: r.u32()?,
            codepoint: r.u32()?,
            named_key: r.u16()?,
            modifiers: r.u32()?,
            flags: r.u8()?,
        }),
        ty::RELEASE_ALL => Message::ReleaseAll(ReleaseAll {
            enter_seq: r.u32()?,
        }),
        ty::CLIPBOARD_GRAB => Message::ClipboardGrab(ClipboardGrab {
            clip_seq: r.u32()?,
            enter_seq: r.u32()?,
            formats: r.u32()?,
        }),
        ty::DRAG_START => Message::DragStart(DragStart {
            transfer_id: r.u32()?,
            item_count: r.u32()?,
            total_size: r.u64()?,
            first_name: r.str("first_name", 255)?,
        }),
        ty::TRANSFER_BEGIN => Message::TransferBegin(TransferBegin {
            transfer_id: r.u32()?,
            kind: r.u8()?,
            item_count: r.u32()?,
            total_size: r.u64()?,
            ref_seq: r.u32()?,
        }),
        ty::TRANSFER_ITEM => Message::TransferItem(TransferItem {
            transfer_id: r.u32()?,
            item_index: r.u32()?,
            item_kind: r.u8()?,
            format: r.u16()?,
            size: r.u64()?,
            path: r.str("path", 1024)?,
        }),
        ty::TRANSFER_CHUNK => Message::TransferChunk(TransferChunk {
            transfer_id: r.u32()?,
            item_index: r.u32()?,
            offset: r.u64()?,
            data: r.blob("chunk data", MAX_CHUNK_DATA)?,
        }),
        ty::TRANSFER_ITEM_END => Message::TransferItemEnd(TransferItemEnd {
            transfer_id: r.u32()?,
            item_index: r.u32()?,
            sha256: r.bytes32()?,
        }),
        ty::TRANSFER_END => Message::TransferEnd(TransferEnd {
            transfer_id: r.u32()?,
        }),
        ty::TRANSFER_CANCEL => Message::TransferCancel(TransferCancel {
            transfer_id: r.u32()?,
            reason: r.u16()?,
        }),
        ty::TRANSFER_RESUME => Message::TransferResume(TransferResume {
            transfer_id: r.u32()?,
            item_index: r.u32()?,
            offset: r.u64()?,
        }),
        ty::SCREEN_INFO => {
            let info_seq = r.u32()?;
            let cursor_x = r.i32()?;
            let cursor_y = r.i32()?;
            let count = usize::from(r.u16()?);
            if count > MAX_MONITORS {
                return Err(Error::LimitExceeded {
                    field: "monitors",
                    max: MAX_MONITORS,
                });
            }
            let mut monitors = Vec::with_capacity(count);
            for _ in 0..count {
                monitors.push(Monitor {
                    monitor_id: r.u32()?,
                    x: r.i32()?,
                    y: r.i32()?,
                    width: r.u32()?,
                    height: r.u32()?,
                    scale_permille: r.u16()?,
                    width_mm: r.u16()?,
                    height_mm: r.u16()?,
                });
            }
            Message::ScreenInfo(ScreenInfo {
                info_seq,
                cursor_x,
                cursor_y,
                monitors,
            })
        }
        ty::SCREEN_INFO_ACK => Message::ScreenInfoAck(ScreenInfoAck { info_seq: r.u32()? }),
        ty::CONFIG_PUSH => Message::ConfigPush(ConfigPush {
            config_version: r.u64()?,
            document: r.blob("config document", MAX_CONFIG_DOCUMENT)?,
        }),
        other if other & ty::CRITICAL != 0 => return Err(Error::UnknownCritical(other)),
        other => return Ok(Decoded::Ignored { type_id: other }),
    };
    Ok(Decoded::Message(message))
}

/// Append the frame for `message` to `out`.
///
/// On error `out` is left unchanged.
///
/// # Errors
/// [`Error::LimitExceeded`] for oversized fields, [`Error::FrameTooLarge`] if the finished frame
/// would exceed the maximum of the message's channel.
pub fn encode(message: &Message, out: &mut Vec<u8>) -> Result<(), Error> {
    let start = out.len();
    let result = encode_inner(message, out, start);
    if result.is_err() {
        out.truncate(start);
    }
    result
}

// One flat match mirrors the message table of the spec; splitting it would hide that.
#[allow(clippy::too_many_lines)]
fn encode_inner(message: &Message, out: &mut Vec<u8>, start: usize) -> Result<(), Error> {
    let mut w = Writer::new(out);
    w.u32(0); // length, patched below
    w.u16(message.type_id());
    match message {
        Message::Hello(m) => {
            w.u8(m.major);
            w.u8(m.minor);
            w.u16(m.flags);
            w.u64(m.capabilities);
            w.raw(&m.machine_id);
            w.str("machine_name", 64, &m.machine_name)?;
            w.str("app_version", 32, &m.app_version)?;
        }
        Message::SessionStart(m) => {
            w.raw(&m.session_id);
            w.raw(&m.bulk_token);
        }
        Message::Goodbye(m) => {
            w.u16(m.reason);
            // An empty detail is sent as an absent field (shortest encoding).
            if !m.detail.is_empty() {
                w.str("detail", 256, &m.detail)?;
            }
        }
        Message::Ping(m) => {
            w.u32(m.nonce);
            w.u64(m.sender_time_us);
        }
        Message::Pong(m) => {
            w.u32(m.nonce);
            w.u64(m.echoed_time_us);
        }
        Message::BulkAttach(m) => w.raw(&m.bulk_token),
        Message::PairStart => {}
        Message::PairConfirm(m) => w.bool(m.accepted),
        Message::Enter(m) => {
            w.u32(m.enter_seq);
            w.i32(m.x);
            w.i32(m.y);
            w.u8(m.toggles);
            w.u32(m.modifiers);
        }
        Message::Leave(m) => w.u32(m.enter_seq),
        Message::LeaveAck(m) => {
            w.u32(m.enter_seq);
            w.bool(m.ok);
        }
        Message::MouseMotion(m) => {
            w.u32(m.enter_seq);
            w.u32(m.t_us);
            w.i16(m.dx);
            w.i16(m.dy);
        }
        Message::MouseAbsolute(m) => {
            w.u32(m.enter_seq);
            w.u32(m.t_us);
            w.i32(m.x);
            w.i32(m.y);
        }
        Message::MouseButton(m) => {
            w.u32(m.enter_seq);
            w.u32(m.t_us);
            w.u8(m.button);
            w.bool(m.down);
        }
        Message::Wheel(m) => {
            w.u32(m.enter_seq);
            w.u32(m.t_us);
            w.i16(m.dx);
            w.i16(m.dy);
            w.u8(m.flags);
        }
        Message::Key(m) => {
            w.u32(m.enter_seq);
            w.u32(m.t_us);
            w.u8(m.action);
            w.u16(m.repeat_count);
            w.u32(m.key_id);
            w.u32(m.hid_usage);
            w.u32(m.codepoint);
            w.u16(m.named_key);
            w.u32(m.modifiers);
            w.u8(m.flags);
        }
        Message::ReleaseAll(m) => w.u32(m.enter_seq),
        Message::ClipboardGrab(m) => {
            w.u32(m.clip_seq);
            w.u32(m.enter_seq);
            w.u32(m.formats);
        }
        Message::DragStart(m) => {
            w.u32(m.transfer_id);
            w.u32(m.item_count);
            w.u64(m.total_size);
            w.str("first_name", 255, &m.first_name)?;
        }
        Message::TransferBegin(m) => {
            w.u32(m.transfer_id);
            w.u8(m.kind);
            w.u32(m.item_count);
            w.u64(m.total_size);
            w.u32(m.ref_seq);
        }
        Message::TransferItem(m) => {
            w.u32(m.transfer_id);
            w.u32(m.item_index);
            w.u8(m.item_kind);
            w.u16(m.format);
            w.u64(m.size);
            w.str("path", 1024, &m.path)?;
        }
        Message::TransferChunk(m) => {
            w.u32(m.transfer_id);
            w.u32(m.item_index);
            w.u64(m.offset);
            w.blob("chunk data", MAX_CHUNK_DATA, &m.data)?;
        }
        Message::TransferItemEnd(m) => {
            w.u32(m.transfer_id);
            w.u32(m.item_index);
            w.raw(&m.sha256);
        }
        Message::TransferEnd(m) => w.u32(m.transfer_id),
        Message::TransferCancel(m) => {
            w.u32(m.transfer_id);
            w.u16(m.reason);
        }
        Message::TransferResume(m) => {
            w.u32(m.transfer_id);
            w.u32(m.item_index);
            w.u64(m.offset);
        }
        Message::ScreenInfo(m) => {
            if m.monitors.len() > MAX_MONITORS {
                return Err(Error::LimitExceeded {
                    field: "monitors",
                    max: MAX_MONITORS,
                });
            }
            w.u32(m.info_seq);
            w.i32(m.cursor_x);
            w.i32(m.cursor_y);
            // At most `MAX_MONITORS` (checked above).
            #[allow(clippy::cast_possible_truncation)]
            w.u16(m.monitors.len() as u16);
            for mon in &m.monitors {
                w.u32(mon.monitor_id);
                w.i32(mon.x);
                w.i32(mon.y);
                w.u32(mon.width);
                w.u32(mon.height);
                w.u16(mon.scale_permille);
                w.u16(mon.width_mm);
                w.u16(mon.height_mm);
            }
        }
        Message::ScreenInfoAck(m) => w.u32(m.info_seq),
        Message::ConfigPush(m) => {
            w.u64(m.config_version);
            w.blob("config document", MAX_CONFIG_DOCUMENT, &m.document)?;
        }
    }
    let len = out.len() - start - 4;
    let channel = message.channel();
    let max = channel.max_frame_len();
    // Frames are bounded far below `u32::MAX` by the field limits above.
    #[allow(clippy::cast_possible_truncation)]
    let len32 = len as u32;
    if len32 > max {
        return Err(Error::FrameTooLarge {
            len: len32,
            max,
            channel,
        });
    }
    out[start..start + 4].copy_from_slice(&len32.to_le_bytes());
    Ok(())
}

/// Encode `message` into a fresh buffer.
///
/// # Errors
/// See [`encode`].
pub fn encode_to_vec(message: &Message) -> Result<Vec<u8>, Error> {
    let mut out = Vec::new();
    encode(message, &mut out)?;
    Ok(out)
}
