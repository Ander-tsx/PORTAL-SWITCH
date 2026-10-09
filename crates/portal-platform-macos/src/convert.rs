// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The PortalSwitch Authors

//! Pure conversion between macOS event data and PortalSwitch input events.
//!
//! No OS calls live here, so the logic is unit-tested on every platform. The FFI modules read
//! the raw fields out of `CGEvent`s and pass them in.

use portal_core::input::{InputEvent, KeyAction, KeyEvent};
use portal_core::layout::Point;
use portal_platform::keymap::{keyboard_usage, macos_keycode_to_hid, modifier_bit, named_key};

/// `CGEventFlags` bits (generic and device-dependent).
pub mod flag {
    pub const CAPS_LOCK: u64 = 0x1_0000;
    pub const SHIFT: u64 = 0x2_0000;
    pub const CONTROL: u64 = 0x4_0000;
    pub const ALTERNATE: u64 = 0x8_0000;
    pub const COMMAND: u64 = 0x10_0000;
    pub const FN: u64 = 0x80_0000;
    // Device-dependent (NX_DEVICE*) bits identify the physical side.
    pub const L_CONTROL: u64 = 0x0001;
    pub const L_SHIFT: u64 = 0x0002;
    pub const R_SHIFT: u64 = 0x0004;
    pub const L_COMMAND: u64 = 0x0008;
    pub const R_COMMAND: u64 = 0x0010;
    pub const L_ALTERNATE: u64 = 0x0020;
    pub const R_ALTERNATE: u64 = 0x0040;
    pub const R_CONTROL: u64 = 0x2000;
}

/// Protocol modifier mask (§8.1) for a `CGEventFlags` value.
#[must_use]
pub fn modifier_mask(flags: u64) -> u32 {
    let side = |generic: u64, left: u64, right: u64, bit_l: u32, bit_r: u32| -> u32 {
        if flags & generic == 0 {
            return 0;
        }
        let mut out = 0;
        if flags & left != 0 {
            out |= 1 << bit_l;
        }
        if flags & right != 0 {
            out |= 1 << bit_r;
        }
        // Only the generic bit is known (e.g. synthetic events): call it the left key.
        if out == 0 { 1 << bit_l } else { out }
    };
    let mut mask = side(flag::SHIFT, flag::L_SHIFT, flag::R_SHIFT, 0, 1)
        | side(flag::CONTROL, flag::L_CONTROL, flag::R_CONTROL, 2, 3)
        | side(flag::ALTERNATE, flag::L_ALTERNATE, flag::R_ALTERNATE, 4, 5)
        | side(flag::COMMAND, flag::L_COMMAND, flag::R_COMMAND, 6, 7);
    if flags & flag::FN != 0 {
        mask |= 1 << 9;
    }
    mask
}

/// `CGEventFlags` for a protocol modifier mask: the inverse of [`modifier_mask`].
#[must_use]
pub fn modifier_flags(mask: u32) -> u64 {
    let pair = |l: u32, r: u32, generic: u64, left: u64, right: u64| -> u64 {
        let (has_l, has_r) = (mask & (1 << l) != 0, mask & (1 << r) != 0);
        if !(has_l || has_r) {
            return 0;
        }
        generic | if has_l { left } else { 0 } | if has_r { right } else { 0 }
    };
    let mut flags = pair(0, 1, flag::SHIFT, flag::L_SHIFT, flag::R_SHIFT)
        | pair(2, 3, flag::CONTROL, flag::L_CONTROL, flag::R_CONTROL)
        | pair(4, 5, flag::ALTERNATE, flag::L_ALTERNATE, flag::R_ALTERNATE)
        | pair(6, 7, flag::COMMAND, flag::L_COMMAND, flag::R_COMMAND);
    if mask & (1 << 9) != 0 {
        flags |= flag::FN;
    }
    flags
}

/// Whether a modifier mask contains Control or Command, i.e. the key press is a shortcut and
/// must be injected as a physical key rather than as typed text.
#[must_use]
pub const fn is_shortcut(mask: u32) -> bool {
    mask & 0b1100_1100 != 0
}

/// Lock-key toggle state in the `Enter.toggles` layout (bit 0 Caps Lock).
#[must_use]
pub const fn toggles(flags: u64) -> u8 {
    if flags & flag::CAPS_LOCK != 0 { 1 } else { 0 }
}

/// The `CGEventFlags` bit a modifier key sets while held (device-dependent when available).
const fn modifier_flag(keycode: u16) -> Option<(u64, u64)> {
    // (device-dependent bit, generic bit)
    match keycode {
        0x38 => Some((flag::L_SHIFT, flag::SHIFT)),
        0x3C => Some((flag::R_SHIFT, flag::SHIFT)),
        0x3B => Some((flag::L_CONTROL, flag::CONTROL)),
        0x3E => Some((flag::R_CONTROL, flag::CONTROL)),
        0x3A => Some((flag::L_ALTERNATE, flag::ALTERNATE)),
        0x3D => Some((flag::R_ALTERNATE, flag::ALTERNATE)),
        0x37 => Some((flag::L_COMMAND, flag::COMMAND)),
        0x36 => Some((flag::R_COMMAND, flag::COMMAND)),
        _ => None,
    }
}

/// Raw keyboard event data read from a `CGEvent`.
#[derive(Debug, Clone, Copy)]
pub struct RawKey {
    pub keycode: u16,
    pub flags: u64,
    pub autorepeat: bool,
    /// First Unicode scalar the event would type, if exactly one printable character.
    pub character: Option<char>,
}

fn base_key(keycode: u16, flags: u64, action: KeyAction, character: Option<char>) -> KeyEvent {
    let usage = macos_keycode_to_hid(keycode);
    let hid_usage = usage.map_or(0, keyboard_usage);
    KeyEvent {
        action,
        repeat_count: u16::from(action == KeyAction::Repeat),
        key_id: u32::from(keycode),
        hid_usage,
        codepoint: character.map_or(0, u32::from),
        named_key: usage.map_or(0, named_key),
        modifiers: modifier_mask(flags),
        flags: 0,
    }
}

/// `keyDown` / `keyUp` events.
#[must_use]
pub fn key_event(down: bool, raw: RawKey) -> KeyEvent {
    let action = match (down, raw.autorepeat) {
        (true, true) => KeyAction::Repeat,
        (true, false) => KeyAction::Down,
        (false, _) => KeyAction::Up,
    };
    base_key(raw.keycode, raw.flags, action, raw.character)
}

/// `flagsChanged` events: modifier keys going down or up (and Caps Lock toggling).
///
/// Returns the key events to report: one for a modifier key, a press followed by a release for
/// Caps Lock (macOS only reports its toggle), none for unknown keys.
#[must_use]
pub fn flags_changed(keycode: u16, flags: u64) -> Vec<KeyEvent> {
    if keycode == 0x39 {
        return vec![
            base_key(keycode, flags, KeyAction::Down, None),
            base_key(keycode, flags, KeyAction::Up, None),
        ];
    }
    let Some((device, generic)) = modifier_flag(keycode) else {
        return Vec::new();
    };
    // Prefer the side-specific bit; fall back to the generic one.
    let down = if flags
        & (flag::L_SHIFT
            | flag::R_SHIFT
            | flag::L_CONTROL
            | flag::R_CONTROL
            | flag::L_ALTERNATE
            | flag::R_ALTERNATE
            | flag::L_COMMAND
            | flag::R_COMMAND)
        != 0
    {
        flags & device != 0
    } else {
        flags & generic != 0
    };
    let action = if down { KeyAction::Down } else { KeyAction::Up };
    vec![base_key(keycode, flags, action, None)]
}

/// Mouse buttons: CoreGraphics numbers them from 0 (left, right, middle, back, forward).
#[must_use]
pub const fn button(cg_button: i64) -> u8 {
    // Clamp into the protocol's 1..=32 range.
    let n = cg_button + 1;
    if n < 1 {
        1
    } else if n > 32 {
        32
    } else {
        n as u8
    }
}

/// Scroll event data: whole-line deltas and, for trackpads, pixel deltas.
#[derive(Debug, Clone, Copy)]
pub struct RawScroll {
    pub line_dy: i64,
    pub line_dx: i64,
    pub point_dy: i64,
    pub point_dx: i64,
    pub continuous: bool,
}

/// Pixels that count as one wheel notch for continuous devices.
const PIXELS_PER_NOTCH: i64 = 10;

/// Scroll event → wheel units (1/120 notch).
#[must_use]
pub fn wheel(raw: RawScroll) -> InputEvent {
    let to_units = |lines: i64, points: i64| -> i64 {
        if raw.continuous {
            points * 120 / PIXELS_PER_NOTCH
        } else {
            lines * 120
        }
    };
    let clamp = |v: i64| i16::try_from(v).unwrap_or(if v < 0 { i16::MIN } else { i16::MAX });
    InputEvent::Wheel {
        // macOS reports "content moves right" as positive; the protocol is "scroll right".
        dx: clamp(-to_units(raw.line_dx, raw.point_dx)),
        dy: clamp(to_units(raw.line_dy, raw.point_dy)),
        flags: u8::from(raw.continuous),
    }
}

/// Pointer motion with the host cursor position after the event.
#[must_use]
pub fn motion(dx: i64, dy: i64, x: f64, y: f64) -> InputEvent {
    let clamp = |v: i64| i32::try_from(v).unwrap_or(if v < 0 { i32::MIN } else { i32::MAX });
    InputEvent::Motion {
        dx: clamp(dx),
        dy: clamp(dy),
        // CoreGraphics positions are fractional; the layout works in whole points.
        host_pos: Point {
            x: x.floor() as i32,
            y: y.floor() as i32,
        },
    }
}

/// Whether the key is a modifier (used to decide Unicode vs. physical injection).
#[must_use]
pub fn is_modifier_key(keycode: u16) -> bool {
    macos_keycode_to_hid(keycode).is_some_and(|u| modifier_bit(u) != 0)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn raw(keycode: u16, flags: u64, autorepeat: bool, character: Option<char>) -> RawKey {
        RawKey {
            keycode,
            flags,
            autorepeat,
            character,
        }
    }

    #[test]
    fn modifier_masks_keep_left_and_right_apart() {
        assert_eq!(modifier_mask(0), 0);
        assert_eq!(modifier_mask(flag::SHIFT | flag::L_SHIFT), 1);
        assert_eq!(modifier_mask(flag::SHIFT | flag::R_SHIFT), 2);
        assert_eq!(
            modifier_mask(flag::COMMAND | flag::L_COMMAND | flag::R_COMMAND),
            0b1100_0000
        );
        assert_eq!(modifier_mask(flag::CONTROL | flag::R_CONTROL), 1 << 3);
        assert_eq!(modifier_mask(flag::ALTERNATE | flag::L_ALTERNATE), 1 << 4);
        // Only the generic bit set: reported as the left key.
        assert_eq!(modifier_mask(flag::COMMAND), 1 << 6);
        assert_eq!(modifier_mask(flag::FN), 1 << 9);
        // Caps Lock is a toggle, not a held modifier.
        assert_eq!(modifier_mask(flag::CAPS_LOCK), 0);
        assert_eq!(toggles(flag::CAPS_LOCK), 1);
    }

    #[test]
    fn modifier_flags_invert_the_mask() {
        for mask in [
            0u32,
            1,
            2,
            3,
            4,
            1 << 3,
            1 << 4,
            0b1100_0000,
            0b0101_0101,
            1 << 9,
        ] {
            assert_eq!(modifier_mask(modifier_flags(mask)), mask, "mask {mask:#b}");
        }
        assert!(is_shortcut(1 << 6) && is_shortcut(1 << 3));
        assert!(!is_shortcut(0b11) && !is_shortcut(1 << 4));
    }

    #[test]
    fn letter_keys_carry_hid_character_and_modifiers() {
        let event = key_event(
            true,
            raw(0x00, flag::COMMAND | flag::L_COMMAND, false, Some('a')),
        );
        assert_eq!(event.action, KeyAction::Down);
        assert_eq!(event.key_id, 0);
        assert_eq!(event.hid_usage, 0x0007_0004);
        assert_eq!(event.codepoint, 'a' as u32);
        assert_eq!(event.modifiers, 1 << 6);
        assert_eq!(event.named_key, 0);

        assert_eq!(
            key_event(true, raw(0x00, 0, true, Some('a'))).action,
            KeyAction::Repeat
        );
        assert_eq!(
            key_event(false, raw(0x00, 0, false, None)).action,
            KeyAction::Up
        );
    }

    #[test]
    fn named_keys_are_reported() {
        let escape = key_event(true, raw(0x35, 0, false, None));
        assert_eq!(escape.named_key, 1);
        let left = key_event(true, raw(0x7B, 0, false, None));
        assert_eq!((left.named_key, left.hid_usage), (11, 0x0007_0050));
    }

    #[test]
    fn modifier_key_presses_come_from_flag_changes() {
        // Left Shift goes down, then up.
        let down = flags_changed(0x38, flag::SHIFT | flag::L_SHIFT);
        assert_eq!(down.len(), 1);
        assert_eq!(
            (down[0].action, down[0].hid_usage),
            (KeyAction::Down, 0x0007_00E1)
        );
        let up = flags_changed(0x38, 0);
        assert_eq!(up[0].action, KeyAction::Up);
        // Right Shift released while Left Shift stays held.
        let still = flags_changed(0x3C, flag::SHIFT | flag::L_SHIFT);
        assert_eq!(still[0].action, KeyAction::Up);
        assert_eq!(still[0].hid_usage, 0x0007_00E5);
        // Generic-only flags (no side information).
        assert_eq!(
            flags_changed(0x37, flag::COMMAND)[0].action,
            KeyAction::Down
        );
        assert_eq!(flags_changed(0x37, 0)[0].action, KeyAction::Up);
        // Unknown keys report nothing.
        assert_eq!(flags_changed(0x7F, 0), vec![]);
    }

    #[test]
    fn caps_lock_becomes_a_press_and_release() {
        let events = flags_changed(0x39, flag::CAPS_LOCK);
        assert_eq!(
            events.iter().map(|e| e.action).collect::<Vec<_>>(),
            [KeyAction::Down, KeyAction::Up]
        );
        assert_eq!(events[0].named_key, 40);
    }

    #[test]
    fn buttons_are_renumbered_for_the_protocol() {
        assert_eq!(
            [button(0), button(1), button(2), button(3), button(4)],
            [1, 2, 3, 4, 5]
        );
        assert_eq!(button(-5), 1);
        assert_eq!(button(500), 32);
    }

    #[test]
    fn wheel_units_and_directions() {
        let InputEvent::Wheel { dx, dy, flags } = wheel(RawScroll {
            line_dy: 2,
            line_dx: 0,
            point_dy: 0,
            point_dx: 0,
            continuous: false,
        }) else {
            panic!()
        };
        assert_eq!((dx, dy, flags), (0, 240, 0));
        let InputEvent::Wheel { dx, dy, flags } = wheel(RawScroll {
            line_dy: 0,
            line_dx: 0,
            point_dy: -5,
            point_dx: 10,
            continuous: true,
        }) else {
            panic!()
        };
        assert_eq!((dx, dy, flags), (-120, -60, 1));
    }

    #[test]
    fn motion_floors_positions_and_clamps_deltas() {
        let InputEvent::Motion { dx, dy, host_pos } = motion(3, -4, 10.9, 20.2) else {
            panic!()
        };
        assert_eq!((dx, dy, host_pos), (3, -4, Point { x: 10, y: 20 }));
        let InputEvent::Motion { dx, .. } = motion(i64::MAX, 0, 0.0, 0.0) else {
            panic!()
        };
        assert_eq!(dx, i32::MAX);
    }
}
