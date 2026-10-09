// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The PortalSwitch Authors

//! Input events as seen by the routing core, independent of OS and wire encoding.

/// What happened to a key.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum KeyAction {
    Up,
    Down,
    Repeat,
}

/// A physical key event captured on the host (mirrors protocol `Key`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct KeyEvent {
    pub action: KeyAction,
    pub repeat_count: u16,
    /// Host identifier of the physical key; pairs a press with its release.
    pub key_id: u32,
    /// `(usage_page << 16) | usage`; 0 when unknown.
    pub hid_usage: u32,
    pub codepoint: u32,
    pub named_key: u16,
    /// Modifier mask (protocol §8.1) at the time of the event.
    pub modifiers: u32,
    pub flags: u8,
}

impl KeyEvent {
    /// The same key with another action.
    #[must_use]
    pub const fn with_action(mut self, action: KeyAction) -> Self {
        self.action = action;
        self
    }

    /// Shift, Control, Alt, Meta, `AltGr` or Fn: keys whose held state is carried across a switch.
    #[must_use]
    pub const fn is_modifier(&self) -> bool {
        // Named keys 50..=55 (§8.3) or HID keyboard page 0xE0..=0xE7.
        (self.named_key >= 50 && self.named_key <= 55)
            || (self.hid_usage >> 16 == 0x07
                && (self.hid_usage & 0xFFFF) >= 0xE0
                && (self.hid_usage & 0xFFFF) <= 0xE7)
    }
}

/// An input event captured on the host.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InputEvent {
    /// Pointer motion: raw `dx`/`dy` from the device and the host cursor's own (OS-clamped)
    /// position after the event.
    Motion {
        dx: i32,
        dy: i32,
        host_pos: crate::layout::Point,
    },
    Button {
        button: u8,
        down: bool,
    },
    /// Scroll in 1/120 notch units.
    Wheel {
        dx: i16,
        dy: i16,
        flags: u8,
    },
    Key(KeyEvent),
}
