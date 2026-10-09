// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The PortalSwitch Authors

//! Receiver-side bookkeeping that guarantees no key or button stays stuck down.
//!
//! A peer feeds every input message from the host through [`PeerInput`] and performs the
//! returned [`Inject`] actions. The state machine tracks every synthetic press and releases
//! all of them when the host leaves, asks for it, goes silent or the link drops.

use std::collections::BTreeMap;

use crate::input::{KeyAction, KeyEvent};
use crate::layout::Point;

/// Something the platform layer must inject on this machine.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Inject {
    MoveTo(Point),
    MoveBy {
        dx: i32,
        dy: i32,
    },
    Button {
        button: u8,
        down: bool,
    },
    Wheel {
        dx: i16,
        dy: i16,
        flags: u8,
    },
    /// Press (or repeat) the key described by the event.
    KeyDown(KeyEvent),
    /// Release a key that is currently synthetically down.
    KeyUp(KeyEvent),
    /// Align lock-key toggle state (bit 0 Caps, 1 Num, 2 Scroll).
    SetToggles(u8),
}

/// Tracks what this machine has synthetically pressed on behalf of the host.
#[derive(Debug, Default)]
pub struct PeerInput {
    /// `enter_seq` of the host's current entry, or `None` while the pointer is elsewhere.
    current: Option<u32>,
    /// Highest `enter_seq` ever accepted; entries must be strictly increasing.
    last_seq: Option<u32>,
    keys: BTreeMap<u32, KeyEvent>,
    buttons: std::collections::BTreeSet<u8>,
}

impl PeerInput {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Whether the host's pointer is currently here.
    #[must_use]
    pub const fn is_active(&self) -> bool {
        self.current.is_some()
    }

    /// Number of synthetic keys currently held.
    #[must_use]
    pub fn held_keys(&self) -> usize {
        self.keys.len()
    }

    /// Number of synthetic buttons currently held.
    #[must_use]
    pub fn held_buttons(&self) -> usize {
        self.buttons.len()
    }

    /// `Enter`: returns `false` (and does nothing) when `seq` is not newer than the last one.
    pub fn enter(&mut self, seq: u32, pos: Point, toggles: u8, out: &mut Vec<Inject>) -> bool {
        if self.last_seq.is_some_and(|last| seq <= last) {
            return false;
        }
        // Entering again without a Leave: drop whatever the previous entry left held.
        self.release_all_into(out);
        self.last_seq = Some(seq);
        self.current = Some(seq);
        out.push(Inject::SetToggles(toggles));
        out.push(Inject::MoveTo(pos));
        true
    }

    /// `Leave`: releases everything. Returns whether the leave matched the current entry
    /// (the `LeaveAck.ok` value).
    pub fn leave(&mut self, seq: u32, out: &mut Vec<Inject>) -> bool {
        if self.current != Some(seq) {
            return false;
        }
        self.release_all_into(out);
        self.current = None;
        true
    }

    /// `ReleaseAll` for the current entry.
    pub fn release_all(&mut self, seq: u32, out: &mut Vec<Inject>) {
        if self.current == Some(seq) {
            self.release_all_into(out);
        }
    }

    /// Link closed, host silent for too long, desktop switch, sleep, …
    pub fn disconnected(&mut self, out: &mut Vec<Inject>) {
        self.release_all_into(out);
        self.current = None;
    }

    /// Absolute pointer move. Discarded when `seq` is stale.
    pub fn mouse_absolute(&mut self, seq: u32, pos: Point, out: &mut Vec<Inject>) {
        if self.current == Some(seq) {
            out.push(Inject::MoveTo(pos));
        }
    }

    /// Relative pointer motion. Discarded when `seq` is stale.
    pub fn mouse_motion(&mut self, seq: u32, dx: i32, dy: i32, out: &mut Vec<Inject>) {
        if self.current == Some(seq) {
            out.push(Inject::MoveBy { dx, dy });
        }
    }

    /// Mouse button. A release for a button we did not press is ignored.
    pub fn mouse_button(&mut self, seq: u32, button: u8, down: bool, out: &mut Vec<Inject>) {
        if self.current != Some(seq) {
            return;
        }
        if down {
            if self.buttons.insert(button) {
                out.push(Inject::Button { button, down: true });
            }
        } else if self.buttons.remove(&button) {
            out.push(Inject::Button {
                button,
                down: false,
            });
        }
    }

    /// Scroll. Discarded when `seq` is stale.
    pub fn wheel(&mut self, seq: u32, dx: i16, dy: i16, flags: u8, out: &mut Vec<Inject>) {
        if self.current == Some(seq) {
            out.push(Inject::Wheel { dx, dy, flags });
        }
    }

    /// Key event with the rules of spec 03: unknown up/repeat are ignored, a down for a key
    /// already down is a repeat.
    pub fn key(&mut self, seq: u32, event: KeyEvent, out: &mut Vec<Inject>) {
        if self.current != Some(seq) {
            return;
        }
        match event.action {
            KeyAction::Down => {
                let action = if self.keys.contains_key(&event.key_id) {
                    KeyAction::Repeat
                } else {
                    KeyAction::Down
                };
                self.keys
                    .insert(event.key_id, event.with_action(KeyAction::Down));
                out.push(Inject::KeyDown(event.with_action(action)));
            }
            KeyAction::Repeat => {
                if self.keys.contains_key(&event.key_id) {
                    out.push(Inject::KeyDown(event));
                }
            }
            KeyAction::Up => {
                if let Some(down) = self.keys.remove(&event.key_id) {
                    out.push(Inject::KeyUp(down.with_action(KeyAction::Up)));
                }
            }
        }
    }

    fn release_all_into(&mut self, out: &mut Vec<Inject>) {
        for (_, down) in std::mem::take(&mut self.keys) {
            out.push(Inject::KeyUp(down.with_action(KeyAction::Up)));
        }
        for button in std::mem::take(&mut self.buttons) {
            out.push(Inject::Button {
                button,
                down: false,
            });
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key(id: u32, action: KeyAction) -> KeyEvent {
        KeyEvent {
            action,
            repeat_count: 0,
            key_id: id,
            hid_usage: 0x0007_0004,
            codepoint: 97,
            named_key: 0,
            modifiers: 0,
            flags: 0,
        }
    }

    fn entered() -> (PeerInput, Vec<Inject>) {
        let mut p = PeerInput::new();
        let mut out = Vec::new();
        assert!(p.enter(1, Point::default(), 0, &mut out));
        out.clear();
        (p, out)
    }

    #[test]
    fn stale_sequence_numbers_are_discarded() {
        let (mut p, mut out) = entered();
        p.key(0, key(1, KeyAction::Down), &mut out);
        p.mouse_button(7, 1, true, &mut out);
        assert_eq!(out, vec![]);
        assert_eq!(p.held_keys() + p.held_buttons(), 0);
    }

    #[test]
    fn unknown_up_and_repeat_are_ignored_and_double_down_is_a_repeat() {
        let (mut p, mut out) = entered();
        p.key(1, key(5, KeyAction::Up), &mut out);
        p.key(1, key(5, KeyAction::Repeat), &mut out);
        assert_eq!(out, vec![]);
        p.key(1, key(5, KeyAction::Down), &mut out);
        p.key(1, key(5, KeyAction::Down), &mut out);
        assert_eq!(out[0], Inject::KeyDown(key(5, KeyAction::Down)));
        assert_eq!(out[1], Inject::KeyDown(key(5, KeyAction::Repeat)));
        assert_eq!(p.held_keys(), 1);
    }

    #[test]
    fn leave_releases_everything() {
        let (mut p, mut out) = entered();
        p.key(1, key(1, KeyAction::Down), &mut out);
        p.mouse_button(1, 1, true, &mut out);
        out.clear();
        assert!(p.leave(1, &mut out));
        assert_eq!(out.len(), 2);
        assert_eq!(
            (p.held_keys(), p.held_buttons(), p.is_active()),
            (0, 0, false)
        );
    }

    #[test]
    fn leave_with_wrong_sequence_is_refused() {
        let (mut p, mut out) = entered();
        assert!(!p.leave(9, &mut out));
        assert!(p.is_active());
    }

    #[test]
    fn re_entering_without_leave_releases_first() {
        let (mut p, mut out) = entered();
        p.key(1, key(1, KeyAction::Down), &mut out);
        out.clear();
        assert!(p.enter(2, Point::default(), 0, &mut out));
        assert!(out.contains(&Inject::KeyUp(key(1, KeyAction::Up))));
        assert_eq!(p.held_keys(), 0);
    }

    #[test]
    fn enter_sequence_must_increase() {
        let (mut p, mut out) = entered();
        assert!(!p.enter(1, Point::default(), 0, &mut out));
        assert!(!p.enter(0, Point::default(), 0, &mut out));
    }

    #[test]
    fn disconnect_releases_and_deactivates() {
        let (mut p, mut out) = entered();
        p.key(1, key(1, KeyAction::Down), &mut out);
        out.clear();
        p.disconnected(&mut out);
        assert_eq!(out, vec![Inject::KeyUp(key(1, KeyAction::Up))]);
        assert!(!p.is_active());
    }
}
