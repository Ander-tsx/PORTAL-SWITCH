// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The PortalSwitch Authors

//! Glue between the routing core and the wire protocol.
//!
//! The host turns router [`Command`]s into protocol messages; a peer feeds incoming input
//! messages into its [`PeerInput`] guard and answers `Leave` with `LeaveAck`.

use portal_core::input::{KeyAction, KeyEvent};
use portal_core::layout::{MachineId, Point};
use portal_core::peer::{Inject, PeerInput};
use portal_core::router::Command;
use portal_proto::{
    Enter, Key, Leave, Message, MouseButton, MouseMotion, ReleaseAll, Wheel, key_action,
};

/// The message (and its destination) for a router command, if it has one.
///
/// `t_us` is the host's monotonic clock, used for diagnostics only.
#[must_use]
#[allow(clippy::too_many_lines)] // one arm per command, mirroring the protocol table
pub fn command_message(command: &Command, t_us: u32) -> Option<(MachineId, Message)> {
    Some(match command {
        Command::Enter {
            to,
            enter_seq,
            pos,
            toggles,
            modifiers,
        } => (
            *to,
            Message::Enter(Enter {
                enter_seq: *enter_seq,
                x: pos.x,
                y: pos.y,
                toggles: *toggles,
                modifiers: *modifiers,
            }),
        ),
        Command::Leave { to, enter_seq } => (
            *to,
            Message::Leave(Leave {
                enter_seq: *enter_seq,
            }),
        ),
        Command::ReleaseAll { to, enter_seq } => (
            *to,
            Message::ReleaseAll(ReleaseAll {
                enter_seq: *enter_seq,
            }),
        ),
        Command::Motion {
            to,
            enter_seq,
            dx,
            dy,
        } => (
            *to,
            Message::MouseMotion(MouseMotion {
                enter_seq: *enter_seq,
                t_us,
                dx: saturate(*dx),
                dy: saturate(*dy),
            }),
        ),
        Command::Button {
            to,
            enter_seq,
            button,
            down,
        } => (
            *to,
            Message::MouseButton(MouseButton {
                enter_seq: *enter_seq,
                t_us,
                button: *button,
                down: *down,
            }),
        ),
        Command::Wheel {
            to,
            enter_seq,
            dx,
            dy,
            flags,
        } => (
            *to,
            Message::Wheel(Wheel {
                enter_seq: *enter_seq,
                t_us,
                dx: *dx,
                dy: *dy,
                flags: *flags,
            }),
        ),
        Command::Key {
            to,
            enter_seq,
            event,
        } => (
            *to,
            Message::Key(Key {
                enter_seq: *enter_seq,
                t_us,
                action: match event.action {
                    KeyAction::Up => key_action::UP,
                    KeyAction::Down => key_action::DOWN,
                    KeyAction::Repeat => key_action::REPEAT,
                },
                repeat_count: event.repeat_count,
                key_id: event.key_id,
                hid_usage: event.hid_usage,
                codepoint: event.codepoint,
                named_key: event.named_key,
                modifiers: event.modifiers,
                flags: event.flags,
            }),
        ),
        // Local-only commands.
        Command::Capture { .. }
        | Command::WarpHost(_)
        | Command::HostPress { .. }
        | Command::HostRelease { .. }
        | Command::ActiveChanged { .. } => return None,
    })
}

fn saturate(v: i32) -> i16 {
    i16::try_from(v).unwrap_or(if v < 0 { i16::MIN } else { i16::MAX })
}

/// A protocol `Key` as a core key event.
#[must_use]
pub fn key_event(key: &Key) -> KeyEvent {
    KeyEvent {
        action: match key.action {
            key_action::UP => KeyAction::Up,
            key_action::REPEAT => KeyAction::Repeat,
            _ => KeyAction::Down,
        },
        repeat_count: key.repeat_count,
        key_id: key.key_id,
        hid_usage: key.hid_usage,
        codepoint: key.codepoint,
        named_key: key.named_key,
        modifiers: key.modifiers,
        flags: key.flags,
    }
}

/// Apply a message from the host to this peer's input guard.
///
/// Returns the message to send back, if any (`LeaveAck`). Messages that are not input are
/// ignored here.
pub fn apply_host_message(
    input: &mut PeerInput,
    message: &Message,
    out: &mut Vec<Inject>,
) -> Option<Message> {
    match message {
        Message::Enter(m) => {
            input.enter(m.enter_seq, Point { x: m.x, y: m.y }, m.toggles, out);
            None
        }
        Message::Leave(m) => {
            let ok = input.leave(m.enter_seq, out);
            Some(Message::LeaveAck(portal_proto::LeaveAck {
                enter_seq: m.enter_seq,
                ok,
            }))
        }
        Message::ReleaseAll(m) => {
            input.release_all(m.enter_seq, out);
            None
        }
        Message::MouseMotion(m) => {
            input.mouse_motion(m.enter_seq, i32::from(m.dx), i32::from(m.dy), out);
            None
        }
        Message::MouseAbsolute(m) => {
            input.mouse_absolute(m.enter_seq, Point { x: m.x, y: m.y }, out);
            None
        }
        Message::MouseButton(m) => {
            input.mouse_button(m.enter_seq, m.button, m.down, out);
            None
        }
        Message::Wheel(m) => {
            input.wheel(m.enter_seq, m.dx, m.dy, m.flags, out);
            None
        }
        Message::Key(m) => {
            input.key(m.enter_seq, key_event(m), out);
            None
        }
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn motion_deltas_saturate_to_the_wire_range() {
        assert_eq!(saturate(100_000), i16::MAX);
        assert_eq!(saturate(-100_000), i16::MIN);
        assert_eq!(saturate(-5), -5);
    }

    #[test]
    fn local_commands_have_no_message() {
        assert!(command_message(&Command::Capture { swallow: true }, 0).is_none());
        assert!(command_message(&Command::WarpHost(Point::default()), 0).is_none());
    }

    #[test]
    fn leave_is_answered_with_an_ack_that_matches_the_entry() {
        let mut input = PeerInput::new();
        let mut out = Vec::new();
        apply_host_message(
            &mut input,
            &Message::Enter(Enter {
                enter_seq: 3,
                x: 1,
                y: 2,
                toggles: 0,
                modifiers: 0,
            }),
            &mut out,
        );
        let ack = apply_host_message(
            &mut input,
            &Message::Leave(Leave { enter_seq: 3 }),
            &mut out,
        );
        assert_eq!(
            ack,
            Some(Message::LeaveAck(portal_proto::LeaveAck {
                enter_seq: 3,
                ok: true
            }))
        );
    }
}
