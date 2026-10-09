// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The PortalSwitch Authors

//! Pure conversion between Windows input data and PortalSwitch input events.
//!
//! No OS calls live here, so the logic is unit-tested on every platform. The FFI module feeds
//! it the raw fields of hook and message structures.

use std::collections::BTreeSet;

use portal_core::input::{InputEvent, KeyAction, KeyEvent};
use portal_platform::keymap::{
    hid_to_windows_scancode, keyboard_usage, keyboard_usage_id, modifier_bit, named_key,
    windows_scancode_to_hid,
};

/// Window messages carried by the low-level mouse hook.
pub mod msg {
    pub const LBUTTONDOWN: u32 = 0x0201;
    pub const LBUTTONUP: u32 = 0x0202;
    pub const RBUTTONDOWN: u32 = 0x0204;
    pub const RBUTTONUP: u32 = 0x0205;
    pub const MBUTTONDOWN: u32 = 0x0207;
    pub const MBUTTONUP: u32 = 0x0208;
    pub const MOUSEWHEEL: u32 = 0x020A;
    pub const XBUTTONDOWN: u32 = 0x020B;
    pub const XBUTTONUP: u32 = 0x020C;
    pub const MOUSEHWHEEL: u32 = 0x020E;
}

/// Virtual-key codes with special handling.
pub mod vk {
    pub const PAUSE: u32 = 0x13;
    pub const NUMLOCK: u32 = 0x90;
}

const HID_PAUSE: u8 = 0x48;
const HID_NUMLOCK: u8 = 0x53;

/// Which button a mouse message presses or releases, as `(protocol button, down)`.
#[must_use]
pub const fn button(message: u32, mouse_data: u32) -> Option<(u8, bool)> {
    Some(match message {
        msg::LBUTTONDOWN => (1, true),
        msg::LBUTTONUP => (1, false),
        msg::RBUTTONDOWN => (2, true),
        msg::RBUTTONUP => (2, false),
        msg::MBUTTONDOWN => (3, true),
        msg::MBUTTONUP => (3, false),
        msg::XBUTTONDOWN | msg::XBUTTONUP => {
            // The high word says which extra button: 1 = back, 2 = forward.
            let which = (mouse_data >> 16) as u16;
            let number = match which {
                1 => 4,
                2 => 5,
                n => 3 + n as u8,
            };
            (number, message == msg::XBUTTONDOWN)
        }
        _ => return None,
    })
}

/// Wheel message → wheel event. Windows already uses 120 units per notch.
#[must_use]
pub const fn wheel(message: u32, mouse_data: u32) -> Option<InputEvent> {
    // The high word is a signed delta.
    let delta = (mouse_data >> 16) as u16 as i16;
    match message {
        msg::MOUSEWHEEL => Some(InputEvent::Wheel {
            dx: 0,
            dy: delta,
            flags: 0,
        }),
        msg::MOUSEHWHEEL => Some(InputEvent::Wheel {
            dx: delta,
            dy: 0,
            flags: 0,
        }),
        _ => None,
    }
}

/// Raw keyboard data from a low-level hook.
#[derive(Debug, Clone, Copy)]
pub struct RawKey {
    pub vk: u32,
    pub scancode: u32,
    pub extended: bool,
    pub up: bool,
    /// Character the key would type right now, if exactly one printable one.
    pub character: Option<char>,
}

/// Remembers which keys are down so repeats and modifier state can be derived.
#[derive(Debug, Default)]
pub struct KeyTracker {
    down: BTreeSet<u32>,
    modifiers: u32,
}

impl KeyTracker {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Modifier mask (protocol §8.1) of the keys currently down.
    #[must_use]
    pub const fn modifiers(&self) -> u32 {
        self.modifiers
    }

    /// Convert a hook key event; `None` when the key has no HID equivalent.
    pub fn key(&mut self, raw: RawKey) -> Option<KeyEvent> {
        let usage = match raw.vk {
            vk::PAUSE => HID_PAUSE,
            vk::NUMLOCK => HID_NUMLOCK,
            _ => windows_scancode_to_hid(raw.scancode as u16, raw.extended)?,
        };
        let key_id =
            raw.scancode | (u32::from(raw.extended) << 8) | (u32::from(raw.vk == vk::PAUSE) << 9);
        let action = if raw.up {
            self.down.remove(&key_id);
            KeyAction::Up
        } else if self.down.insert(key_id) {
            KeyAction::Down
        } else {
            KeyAction::Repeat
        };
        let bit = modifier_bit(usage);
        match action {
            KeyAction::Down if bit != 0 => self.modifiers |= bit,
            KeyAction::Up if bit != 0 => self.modifiers &= !bit,
            _ => {}
        }
        Some(KeyEvent {
            action,
            repeat_count: u16::from(action == KeyAction::Repeat),
            key_id,
            hid_usage: keyboard_usage(usage),
            codepoint: raw.character.map_or(0, u32::from),
            named_key: named_key(usage),
            modifiers: self.modifiers,
            flags: 0,
        })
    }
}

/// How to inject a key: its scan code and whether it carries the extended prefix.
#[must_use]
pub fn scancode_for(key: &KeyEvent) -> Option<(u16, bool)> {
    let usage = keyboard_usage_id(key.hid_usage)?;
    hid_to_windows_scancode(usage)
}

/// Whether the key press is a shortcut that must be injected as a physical key rather than as
/// typed text: Control or Meta is involved (Alt alone is allowed to compose characters).
#[must_use]
pub const fn is_shortcut(modifiers: u32) -> bool {
    modifiers & 0b1100_1100 != 0
}

/// Whether the key itself is a modifier.
#[must_use]
pub fn is_modifier_key(key: &KeyEvent) -> bool {
    keyboard_usage_id(key.hid_usage).is_some_and(|u| modifier_bit(u) != 0)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn raw(scancode: u32, extended: bool, up: bool) -> RawKey {
        RawKey {
            vk: 0,
            scancode,
            extended,
            up,
            character: None,
        }
    }

    #[test]
    fn mouse_messages_map_to_buttons() {
        assert_eq!(button(msg::LBUTTONDOWN, 0), Some((1, true)));
        assert_eq!(button(msg::RBUTTONUP, 0), Some((2, false)));
        assert_eq!(button(msg::MBUTTONDOWN, 0), Some((3, true)));
        assert_eq!(button(msg::XBUTTONDOWN, 1 << 16), Some((4, true)));
        assert_eq!(button(msg::XBUTTONUP, 2 << 16), Some((5, false)));
        assert_eq!(button(0x0200, 0), None); // plain mouse move
    }

    #[test]
    fn wheel_deltas_are_signed_and_directional() {
        let up = wheel(msg::MOUSEWHEEL, 120u32 << 16);
        assert_eq!(
            up,
            Some(InputEvent::Wheel {
                dx: 0,
                dy: 120,
                flags: 0
            })
        );
        let down = wheel(msg::MOUSEWHEEL, (-240i16 as u16 as u32) << 16);
        assert_eq!(
            down,
            Some(InputEvent::Wheel {
                dx: 0,
                dy: -240,
                flags: 0
            })
        );
        let right = wheel(msg::MOUSEHWHEEL, 120u32 << 16);
        assert_eq!(
            right,
            Some(InputEvent::Wheel {
                dx: 120,
                dy: 0,
                flags: 0
            })
        );
        assert_eq!(wheel(msg::LBUTTONDOWN, 0), None);
    }

    #[test]
    fn down_repeat_up_are_told_apart() {
        let mut keys = KeyTracker::new();
        let a = |up| raw(0x1E, false, up);
        assert_eq!(keys.key(a(false)).unwrap().action, KeyAction::Down);
        let repeat = keys.key(a(false)).unwrap();
        assert_eq!((repeat.action, repeat.repeat_count), (KeyAction::Repeat, 1));
        assert_eq!(keys.key(a(true)).unwrap().action, KeyAction::Up);
        assert_eq!(keys.key(a(false)).unwrap().action, KeyAction::Down);
    }

    #[test]
    fn modifier_state_follows_modifier_keys() {
        let mut keys = KeyTracker::new();
        keys.key(raw(0x2A, false, false)); // left Shift down
        keys.key(raw(0x1D, true, false)); // right Control down
        assert_eq!(keys.modifiers(), 1 | (1 << 3));
        let a = keys.key(raw(0x1E, false, false)).unwrap();
        assert_eq!(
            a.modifiers,
            1 | (1 << 3),
            "key events carry the modifiers held at the time"
        );
        keys.key(raw(0x2A, false, true));
        assert_eq!(keys.modifiers(), 1 << 3);
        keys.key(raw(0x1D, true, true));
        assert_eq!(keys.modifiers(), 0);
    }

    #[test]
    fn extended_keys_do_not_collide_with_their_numpad_twins() {
        let mut keys = KeyTracker::new();
        let enter = keys.key(raw(0x1C, false, false)).unwrap();
        let keypad_enter = keys.key(raw(0x1C, true, false)).unwrap();
        assert_ne!(enter.key_id, keypad_enter.key_id);
        assert_eq!(
            (enter.hid_usage, keypad_enter.hid_usage),
            (0x0007_0028, 0x0007_0058)
        );
        assert_eq!((enter.named_key, keypad_enter.named_key), (2, 2));
    }

    #[test]
    fn pause_and_numlock_use_the_virtual_key() {
        let mut keys = KeyTracker::new();
        let pause = keys.key(RawKey {
            vk: vk::PAUSE,
            scancode: 0x45,
            extended: false,
            up: false,
            character: None,
        });
        assert_eq!(pause.unwrap().hid_usage, 0x0007_0048);
        let num = keys.key(RawKey {
            vk: vk::NUMLOCK,
            scancode: 0x45,
            extended: true,
            up: false,
            character: None,
        });
        assert_eq!(num.unwrap().hid_usage, 0x0007_0053);
    }

    #[test]
    fn unknown_scancodes_are_dropped_and_injection_round_trips() {
        let mut keys = KeyTracker::new();
        assert!(keys.key(raw(0x7F, false, false)).is_none());
        for scancode in [0x1E, 0x39, 0x01, 0x3B] {
            let event = keys.key(raw(scancode, false, false)).unwrap();
            assert_eq!(scancode_for(&event), Some((scancode as u16, false)));
        }
        let arrow = keys.key(raw(0x4B, true, false)).unwrap();
        assert_eq!(scancode_for(&arrow), Some((0x4B, true)));
        assert!(is_modifier_key(&keys.key(raw(0x2A, false, false)).unwrap()));
    }

    #[test]
    fn shortcut_detection_matches_control_and_meta() {
        assert!(
            is_shortcut(1 << 2)
                && is_shortcut(1 << 3)
                && is_shortcut(1 << 6)
                && is_shortcut(1 << 7)
        );
        assert!(!is_shortcut(1) && !is_shortcut(1 << 4));
    }
}
