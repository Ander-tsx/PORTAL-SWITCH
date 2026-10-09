// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The PortalSwitch Authors

//! Physical key identification shared by all platforms.
//!
//! Keys travel on the wire as USB HID usages (page 0x07 for the keyboard). Each backend converts
//! between its native key codes and HID usages with the tables below. The tables are plain data
//! so they can be tested on every OS, not only the one they are used on.

/// HID usage page of keyboard keys.
pub const PAGE_KEYBOARD: u32 = 0x07;

/// `(usage_page << 16) | usage` for a keyboard-page usage.
#[must_use]
pub const fn keyboard_usage(usage: u8) -> u32 {
    (PAGE_KEYBOARD << 16) | usage as u32
}

/// The keyboard-page usage of a combined HID value, if it is on the keyboard page.
#[must_use]
pub const fn keyboard_usage_id(hid_usage: u32) -> Option<u8> {
    if hid_usage >> 16 == PAGE_KEYBOARD && hid_usage & 0xffff <= 0xff {
        Some((hid_usage & 0xff) as u8)
    } else {
        None
    }
}

/// macOS virtual key code (`kVK_*`, ANSI/ISO/JIS) → HID keyboard usage.
const MACOS: &[(u16, u8)] = &[
    (0x00, 0x04),
    (0x01, 0x16),
    (0x02, 0x07),
    (0x03, 0x09),
    (0x04, 0x0B),
    (0x05, 0x0A),
    (0x06, 0x1D),
    (0x07, 0x1B),
    (0x08, 0x06),
    (0x09, 0x19),
    (0x0A, 0x64),
    (0x0B, 0x05),
    (0x0C, 0x14),
    (0x0D, 0x1A),
    (0x0E, 0x08),
    (0x0F, 0x15),
    (0x10, 0x1C),
    (0x11, 0x17),
    (0x12, 0x1E),
    (0x13, 0x1F),
    (0x14, 0x20),
    (0x15, 0x21),
    (0x16, 0x23),
    (0x17, 0x22),
    (0x18, 0x2E),
    (0x19, 0x26),
    (0x1A, 0x24),
    (0x1B, 0x2D),
    (0x1C, 0x25),
    (0x1D, 0x27),
    (0x1E, 0x30),
    (0x1F, 0x12),
    (0x20, 0x18),
    (0x21, 0x2F),
    (0x22, 0x0C),
    (0x23, 0x13),
    (0x24, 0x28),
    (0x25, 0x0F),
    (0x26, 0x0D),
    (0x27, 0x34),
    (0x28, 0x0E),
    (0x29, 0x33),
    (0x2A, 0x31),
    (0x2B, 0x36),
    (0x2C, 0x38),
    (0x2D, 0x11),
    (0x2E, 0x10),
    (0x2F, 0x37),
    (0x30, 0x2B),
    (0x31, 0x2C),
    (0x32, 0x35),
    (0x33, 0x2A),
    (0x35, 0x29),
    (0x36, 0xE7),
    (0x37, 0xE3),
    (0x38, 0xE1),
    (0x39, 0x39),
    (0x3A, 0xE2),
    (0x3B, 0xE0),
    (0x3C, 0xE5),
    (0x3D, 0xE6),
    (0x3E, 0xE4),
    (0x40, 0x6C),
    (0x41, 0x63),
    (0x43, 0x55),
    (0x45, 0x57),
    (0x47, 0x53),
    (0x4B, 0x54),
    (0x4C, 0x58),
    (0x4E, 0x56),
    (0x4F, 0x6D),
    (0x50, 0x6E),
    (0x51, 0x67),
    (0x52, 0x62),
    (0x53, 0x59),
    (0x54, 0x5A),
    (0x55, 0x5B),
    (0x56, 0x5C),
    (0x57, 0x5D),
    (0x58, 0x5E),
    (0x59, 0x5F),
    (0x5A, 0x6F),
    (0x5B, 0x60),
    (0x5C, 0x61),
    (0x5D, 0x89),
    (0x5E, 0x87),
    (0x5F, 0x85),
    (0x60, 0x3E),
    (0x61, 0x3F),
    (0x62, 0x40),
    (0x63, 0x3C),
    (0x64, 0x41),
    (0x65, 0x42),
    (0x66, 0x91),
    (0x67, 0x44),
    (0x68, 0x90),
    (0x69, 0x68),
    (0x6A, 0x6B),
    (0x6B, 0x69),
    (0x6D, 0x43),
    (0x6E, 0x65),
    (0x6F, 0x45),
    (0x71, 0x6A),
    (0x72, 0x49),
    (0x73, 0x4A),
    (0x74, 0x4B),
    (0x75, 0x4C),
    (0x76, 0x3D),
    (0x77, 0x4D),
    (0x78, 0x3B),
    (0x79, 0x4E),
    (0x7A, 0x3A),
    (0x7B, 0x50),
    (0x7C, 0x4F),
    (0x7D, 0x51),
    (0x7E, 0x52),
];

/// Windows set-1 scan code → `(extended, HID usage)`.
const WINDOWS: &[(u16, bool, u8)] = &[
    (0x01, false, 0x29),
    (0x02, false, 0x1E),
    (0x03, false, 0x1F),
    (0x04, false, 0x20),
    (0x05, false, 0x21),
    (0x06, false, 0x22),
    (0x07, false, 0x23),
    (0x08, false, 0x24),
    (0x09, false, 0x25),
    (0x0A, false, 0x26),
    (0x0B, false, 0x27),
    (0x0C, false, 0x2D),
    (0x0D, false, 0x2E),
    (0x0E, false, 0x2A),
    (0x0F, false, 0x2B),
    (0x10, false, 0x14),
    (0x11, false, 0x1A),
    (0x12, false, 0x08),
    (0x13, false, 0x15),
    (0x14, false, 0x17),
    (0x15, false, 0x1C),
    (0x16, false, 0x18),
    (0x17, false, 0x0C),
    (0x18, false, 0x12),
    (0x19, false, 0x13),
    (0x1A, false, 0x2F),
    (0x1B, false, 0x30),
    (0x1C, false, 0x28),
    (0x1D, false, 0xE0),
    (0x1E, false, 0x04),
    (0x1F, false, 0x16),
    (0x20, false, 0x07),
    (0x21, false, 0x09),
    (0x22, false, 0x0A),
    (0x23, false, 0x0B),
    (0x24, false, 0x0D),
    (0x25, false, 0x0E),
    (0x26, false, 0x0F),
    (0x27, false, 0x33),
    (0x28, false, 0x34),
    (0x29, false, 0x35),
    (0x2A, false, 0xE1),
    (0x2B, false, 0x31),
    (0x2C, false, 0x1D),
    (0x2D, false, 0x1B),
    (0x2E, false, 0x06),
    (0x2F, false, 0x19),
    (0x30, false, 0x05),
    (0x31, false, 0x11),
    (0x32, false, 0x10),
    (0x33, false, 0x36),
    (0x34, false, 0x37),
    (0x35, false, 0x38),
    (0x36, false, 0xE5),
    (0x37, false, 0x55),
    (0x38, false, 0xE2),
    (0x39, false, 0x2C),
    (0x3A, false, 0x39),
    (0x3B, false, 0x3A),
    (0x3C, false, 0x3B),
    (0x3D, false, 0x3C),
    (0x3E, false, 0x3D),
    (0x3F, false, 0x3E),
    (0x40, false, 0x3F),
    (0x41, false, 0x40),
    (0x42, false, 0x41),
    (0x43, false, 0x42),
    (0x44, false, 0x43),
    (0x45, false, 0x53),
    (0x46, false, 0x47),
    (0x47, false, 0x5F),
    (0x48, false, 0x60),
    (0x49, false, 0x61),
    (0x4A, false, 0x56),
    (0x4B, false, 0x5C),
    (0x4C, false, 0x5D),
    (0x4D, false, 0x5E),
    (0x4E, false, 0x57),
    (0x4F, false, 0x59),
    (0x50, false, 0x5A),
    (0x51, false, 0x5B),
    (0x52, false, 0x62),
    (0x53, false, 0x63),
    (0x56, false, 0x64),
    (0x57, false, 0x44),
    (0x58, false, 0x45),
    (0x64, false, 0x68),
    (0x65, false, 0x69),
    (0x66, false, 0x6A),
    (0x67, false, 0x6B),
    (0x68, false, 0x6C),
    (0x69, false, 0x6D),
    (0x6A, false, 0x6E),
    (0x6B, false, 0x6F),
    (0x6C, false, 0x70),
    (0x6D, false, 0x71),
    (0x6E, false, 0x72),
    (0x70, false, 0x88),
    (0x71, false, 0x91),
    (0x72, false, 0x90),
    (0x73, false, 0x87),
    (0x76, false, 0x73),
    (0x79, false, 0x8A),
    (0x7B, false, 0x8B),
    (0x7D, false, 0x89),
    (0x1C, true, 0x58),
    (0x1D, true, 0xE4),
    (0x35, true, 0x54),
    (0x37, true, 0x46),
    (0x38, true, 0xE6),
    (0x47, true, 0x4A),
    (0x48, true, 0x52),
    (0x49, true, 0x4B),
    (0x4B, true, 0x50),
    (0x4D, true, 0x4F),
    (0x4F, true, 0x4D),
    (0x50, true, 0x51),
    (0x51, true, 0x4E),
    (0x52, true, 0x49),
    (0x53, true, 0x4C),
    (0x5B, true, 0xE3),
    (0x5C, true, 0xE7),
    (0x5D, true, 0x65),
];

/// HID usage of the Pause key, which Windows reports through a multi-byte sequence.
pub const HID_PAUSE: u8 = 0x48;

/// macOS virtual key code → HID keyboard usage.
#[must_use]
pub fn macos_keycode_to_hid(code: u16) -> Option<u8> {
    MACOS.iter().find(|(c, _)| *c == code).map(|(_, h)| *h)
}

/// HID keyboard usage → macOS virtual key code.
#[must_use]
pub fn hid_to_macos_keycode(usage: u8) -> Option<u16> {
    MACOS.iter().find(|(_, h)| *h == usage).map(|(c, _)| *c)
}

/// Windows scan code (set 1; `extended` = the `E0` prefix) → HID keyboard usage.
#[must_use]
pub fn windows_scancode_to_hid(scancode: u16, extended: bool) -> Option<u8> {
    WINDOWS
        .iter()
        .find(|(s, e, _)| *s == scancode && *e == extended)
        .map(|(_, _, h)| *h)
}

/// HID keyboard usage → Windows `(scan code, extended)`.
#[must_use]
pub fn hid_to_windows_scancode(usage: u8) -> Option<(u16, bool)> {
    WINDOWS
        .iter()
        .find(|(_, _, h)| *h == usage)
        .map(|(s, e, _)| (*s, *e))
}

/// Logical non-character key (protocol §8.3) for a HID usage; 0 for ordinary keys.
#[must_use]
pub const fn named_key(usage: u8) -> u16 {
    match usage {
        0x29 => 1,                                 // Escape
        0x28 | 0x58 => 2,                          // Enter, keypad Enter
        0x2B => 3,                                 // Tab
        0x2A => 4,                                 // Backspace
        0x4C => 5,                                 // Delete
        0x49 => 6,                                 // Insert
        0x4A => 7,                                 // Home
        0x4D => 8,                                 // End
        0x4B => 9,                                 // Page Up
        0x4E => 10,                                // Page Down
        0x50 => 11,                                // Left
        0x4F => 12,                                // Right
        0x52 => 13,                                // Up
        0x51 => 14,                                // Down
        0x3A..=0x45 => 15 + (usage - 0x3A) as u16, // F1–F12
        0x68..=0x73 => 27 + (usage - 0x68) as u16, // F13–F24
        0x39 => 40,                                // Caps Lock
        0x53 => 41,                                // Num Lock
        0x47 => 42,                                // Scroll Lock
        0x46 => 43,                                // Print Screen
        0x48 => 44,                                // Pause
        0x65 => 45,                                // Context menu
        0xE1 | 0xE5 => 50,                         // Shift
        0xE0 | 0xE4 => 51,                         // Control
        0xE2 | 0xE6 => 52,                         // Alt
        0xE3 | 0xE7 => 53,                         // Meta
        0x8A => 80,                                // Henkan
        0x8B => 81,                                // Muhenkan
        0x88 => 82,                                // Kana toggle
        0x90 => 83,                                // Hangul
        0x91 => 84,                                // Hanja
        _ => 0,
    }
}

/// Protocol modifier bit (§8.1) held down by a modifier key, 0 for other keys.
#[must_use]
pub const fn modifier_bit(usage: u8) -> u32 {
    match usage {
        0xE1 => 1 << 0, // left Shift
        0xE5 => 1 << 1,
        0xE0 => 1 << 2, // left Control
        0xE4 => 1 << 3,
        0xE2 => 1 << 4, // left Alt
        0xE6 => 1 << 5,
        0xE3 => 1 << 6, // left Meta
        0xE7 => 1 << 7,
        _ => 0,
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashSet;

    use super::*;

    #[test]
    fn tables_are_one_to_one() {
        let codes: HashSet<_> = MACOS.iter().map(|(c, _)| c).collect();
        let usages: HashSet<_> = MACOS.iter().map(|(_, h)| h).collect();
        assert_eq!((codes.len(), usages.len()), (MACOS.len(), MACOS.len()));
        let keys: HashSet<_> = WINDOWS.iter().map(|(s, e, _)| (s, e)).collect();
        let win_usages: HashSet<_> = WINDOWS.iter().map(|(_, _, h)| h).collect();
        assert_eq!(
            (keys.len(), win_usages.len()),
            (WINDOWS.len(), WINDOWS.len())
        );
    }

    #[test]
    fn conversions_round_trip() {
        for &(code, usage) in MACOS {
            assert_eq!(macos_keycode_to_hid(code), Some(usage));
            assert_eq!(hid_to_macos_keycode(usage), Some(code));
        }
        for &(scan, ext, usage) in WINDOWS {
            assert_eq!(windows_scancode_to_hid(scan, ext), Some(usage));
            assert_eq!(hid_to_windows_scancode(usage), Some((scan, ext)));
        }
    }

    #[test]
    fn well_known_keys_agree_across_platforms() {
        // (name, macOS keycode, Windows scancode+extended, HID)
        let known = [
            ("A", 0x00, (0x1E, false), 0x04),
            ("S", 0x01, (0x1F, false), 0x16),
            ("Q", 0x0C, (0x10, false), 0x14),
            ("1", 0x12, (0x02, false), 0x1E),
            ("Return", 0x24, (0x1C, false), 0x28),
            ("Tab", 0x30, (0x0F, false), 0x2B),
            ("Space", 0x31, (0x39, false), 0x2C),
            ("Backspace", 0x33, (0x0E, false), 0x2A),
            ("Escape", 0x35, (0x01, false), 0x29),
            ("Left Shift", 0x38, (0x2A, false), 0xE1),
            ("Left Control", 0x3B, (0x1D, false), 0xE0),
            ("Left Alt/Option", 0x3A, (0x38, false), 0xE2),
            ("Left Meta", 0x37, (0x5B, true), 0xE3),
            ("Right Meta", 0x36, (0x5C, true), 0xE7),
            ("Right Control", 0x3E, (0x1D, true), 0xE4),
            ("F1", 0x7A, (0x3B, false), 0x3A),
            ("F12", 0x6F, (0x58, false), 0x45),
            ("Left arrow", 0x7B, (0x4B, true), 0x50),
            ("Up arrow", 0x7E, (0x48, true), 0x52),
            ("Delete forward", 0x75, (0x53, true), 0x4C),
            ("Home", 0x73, (0x47, true), 0x4A),
            ("ISO extra key", 0x0A, (0x56, false), 0x64),
            ("Keypad 5", 0x57, (0x4C, false), 0x5D),
            ("Keypad Enter", 0x4C, (0x1C, true), 0x58),
        ];
        for (name, mac, (scan, ext), hid) in known {
            assert_eq!(macos_keycode_to_hid(mac), Some(hid), "{name} on macOS");
            assert_eq!(
                windows_scancode_to_hid(scan, ext),
                Some(hid),
                "{name} on Windows"
            );
        }
    }

    #[test]
    fn letters_cover_the_whole_alphabet_on_both_platforms() {
        // HID 0x04..=0x1D are A..Z.
        for usage in 0x04..=0x1Du8 {
            assert!(
                hid_to_macos_keycode(usage).is_some(),
                "macOS lacks HID {usage:#x}"
            );
            assert!(
                hid_to_windows_scancode(usage).is_some(),
                "Windows lacks HID {usage:#x}"
            );
        }
        for usage in 0x1E..=0x27u8 {
            assert!(
                hid_to_macos_keycode(usage).is_some() && hid_to_windows_scancode(usage).is_some()
            );
        }
    }

    #[test]
    fn named_keys_and_modifier_bits() {
        assert_eq!(named_key(0x29), 1);
        assert_eq!(named_key(0x3A), 15); // F1
        assert_eq!(named_key(0x45), 26); // F12
        assert_eq!(named_key(0x68), 27); // F13
        assert_eq!(named_key(0x73), 38); // F24
        assert_eq!(named_key(0x04), 0); // 'A' is a character key
        assert_eq!(modifier_bit(0xE1), 1);
        assert_eq!(modifier_bit(0xE7), 1 << 7);
        assert_eq!(modifier_bit(0x04), 0);
    }

    #[test]
    fn usage_packing() {
        assert_eq!(keyboard_usage(0x04), 0x0007_0004);
        assert_eq!(keyboard_usage_id(0x0007_0004), Some(0x04));
        assert_eq!(keyboard_usage_id(0x000C_00E9), None);
    }
}
