#!/usr/bin/env python3
"""Generate test vectors for the PortalSwitch wire protocol v1 (protocol/v1.md).

This encoder is written directly from the specification and is intentionally independent from
the Rust codec, so the two cross-check each other. Run from the repository root:

    python3 protocol/tools/gen_vectors.py > protocol/vectors/v1.json
"""

import hashlib
import json
import re
import struct
import sys
from pathlib import Path

SPEC = Path(__file__).resolve().parents[1] / "v1.md"

CONTROL_MAX = 65_536
BULK_MAX = 262_208


def u8(v): return struct.pack("<B", v)
def u16(v): return struct.pack("<H", v)
def u32(v): return struct.pack("<I", v)
def u64(v): return struct.pack("<Q", v)
def i16(v): return struct.pack("<h", v)
def i32(v): return struct.pack("<i", v)
def boolean(v): return u8(1 if v else 0)


def string(s, limit):
    b = s.encode("utf-8")
    assert len(b) <= limit, (s, limit)
    return u16(len(b)) + b


def blob(b):
    return u32(len(b)) + b


def frame(msg_type, payload):
    return u32(2 + len(payload)) + u16(msg_type) + payload


def input_prefix(enter_seq, t_us):
    return u32(enter_seq) + u32(t_us)


# --- Message encoders (field order = spec order) ---------------------------------------------

def hello(major, minor, flags, caps, machine_id, name, version):
    return (u8(major) + u8(minor) + u16(flags) + u64(caps) + machine_id
            + string(name, 64) + string(version, 32))

def session_start(session_id, bulk_token): return session_id + bulk_token
def goodbye(reason, detail=""): return u16(reason) + string(detail, 256)
def ping(nonce, t): return u32(nonce) + u64(t)
def pong(nonce, t): return u32(nonce) + u64(t)
def bulk_attach(token): return token
def pair_start(): return b""
def pair_confirm(accepted): return boolean(accepted)
def enter(seq, x, y, toggles, mods): return u32(seq) + i32(x) + i32(y) + u8(toggles) + u32(mods)
def leave(seq): return u32(seq)
def leave_ack(seq, ok): return u32(seq) + boolean(ok)
def mouse_motion(seq, t, dx, dy): return input_prefix(seq, t) + i16(dx) + i16(dy)
def mouse_absolute(seq, t, x, y): return input_prefix(seq, t) + i32(x) + i32(y)
def mouse_button(seq, t, button, down): return input_prefix(seq, t) + u8(button) + boolean(down)
def wheel(seq, t, dx, dy, flags): return input_prefix(seq, t) + i16(dx) + i16(dy) + u8(flags)

def key(seq, t, action, repeat, key_id, hid, codepoint, named, mods, flags):
    return (input_prefix(seq, t) + u8(action) + u16(repeat) + u32(key_id) + u32(hid)
            + u32(codepoint) + u16(named) + u32(mods) + u8(flags))

def release_all(seq): return u32(seq)
def clipboard_grab(clip_seq, enter_seq, formats): return u32(clip_seq) + u32(enter_seq) + u32(formats)

def drag_start(tid, count, total, first_name):
    return u32(tid) + u32(count) + u64(total) + string(first_name, 255)

def transfer_begin(tid, kind, count, total, ref_seq):
    return u32(tid) + u8(kind) + u32(count) + u64(total) + u32(ref_seq)

def transfer_item(tid, idx, item_kind, fmt, size, path):
    return u32(tid) + u32(idx) + u8(item_kind) + u16(fmt) + u64(size) + string(path, 1024)

def transfer_chunk(tid, idx, offset, data):
    assert len(data) <= 262_144
    return u32(tid) + u32(idx) + u64(offset) + blob(data)

def transfer_item_end(tid, idx, digest): return u32(tid) + u32(idx) + digest
def transfer_end(tid): return u32(tid)
def transfer_cancel(tid, reason): return u32(tid) + u16(reason)
def transfer_resume(tid, idx, offset): return u32(tid) + u32(idx) + u64(offset)

def monitor(mid, x, y, w, h, scale, wmm, hmm):
    return u32(mid) + i32(x) + i32(y) + u32(w) + u32(h) + u16(scale) + u16(wmm) + u16(hmm)

def screen_info(seq, cx, cy, monitors):
    assert 1 <= len(monitors) <= 16
    return u32(seq) + i32(cx) + i32(cy) + u16(len(monitors)) + b"".join(monitors)

def screen_info_ack(seq): return u32(seq)
def config_push(version, document): return u64(version) + blob(document.encode("utf-8"))


TYPES = {
    "Hello": 0x8001, "SessionStart": 0x8002, "Goodbye": 0x8003, "Ping": 0x0004, "Pong": 0x0005,
    "BulkAttach": 0x8006, "PairStart": 0x8010, "PairConfirm": 0x8011,
    "Enter": 0x8101, "Leave": 0x8102, "LeaveAck": 0x8103, "MouseMotion": 0x8104,
    "MouseAbsolute": 0x8105, "MouseButton": 0x8106, "Wheel": 0x8107, "Key": 0x8108,
    "ReleaseAll": 0x8109, "ClipboardGrab": 0x0201, "DragStart": 0x0301,
    "TransferBegin": 0x8302, "TransferItem": 0x8303, "TransferChunk": 0x8304,
    "TransferItemEnd": 0x8305, "TransferEnd": 0x8306, "TransferCancel": 0x8307,
    "TransferResume": 0x8308, "ScreenInfo": 0x8401, "ScreenInfoAck": 0x0402, "ConfigPush": 0x0403,
}

# Smallest valid payload of each message (empty strings/lists/blobs where allowed),
# used to check the "min" values written in the spec.
MINIMAL = {
    "Hello": hello(1, 0, 0, 0, bytes(16), "", ""),
    "SessionStart": session_start(bytes(16), bytes(16)),
    "Goodbye": u16(1),  # detail is optional
    "Ping": ping(0, 0), "Pong": pong(0, 0), "BulkAttach": bulk_attach(bytes(16)),
    "PairStart": pair_start(), "PairConfirm": pair_confirm(False),
    "Enter": enter(0, 0, 0, 0, 0), "Leave": leave(0), "LeaveAck": leave_ack(0, False),
    "MouseMotion": mouse_motion(0, 0, 0, 0), "MouseAbsolute": mouse_absolute(0, 0, 0, 0),
    "MouseButton": mouse_button(0, 0, 0, False), "Wheel": wheel(0, 0, 0, 0, 0),
    "Key": key(0, 0, 0, 0, 0, 0, 0, 0, 0, 0), "ReleaseAll": release_all(0),
    "ClipboardGrab": clipboard_grab(0, 0, 0), "DragStart": drag_start(0, 0, 0, ""),
    "TransferBegin": transfer_begin(0, 0, 0, 0, 0),
    "TransferItem": transfer_item(0, 0, 0, 0, 0, ""),
    "TransferChunk": transfer_chunk(0, 0, 0, b""),
    "TransferItemEnd": transfer_item_end(0, 0, bytes(32)), "TransferEnd": transfer_end(0),
    "TransferCancel": transfer_cancel(0, 0), "TransferResume": transfer_resume(0, 0, 0),
    "ScreenInfo": u32(0) + i32(0) + i32(0) + u16(0),
    "ScreenInfoAck": screen_info_ack(0), "ConfigPush": config_push(0, ""),
}


def check_spec_minimums():
    text = SPEC.read_text(encoding="utf-8")
    problems = []
    for name, payload in MINIMAL.items():
        m = re.search(rf"\*\*{name}\*\* `0x([0-9A-F]{{4}})` \((?:[a-z]+, )?min (\d+)\)", text)
        if not m:
            problems.append(f"{name}: no '(min N)' entry found in spec")
            continue
        spec_type, spec_min = int(m.group(1), 16), int(m.group(2))
        if spec_type != TYPES[name]:
            problems.append(f"{name}: type 0x{spec_type:04X} in spec, 0x{TYPES[name]:04X} here")
        if spec_min != len(payload):
            problems.append(f"{name}: spec min {spec_min}, layout gives {len(payload)}")
    monitor_size = len(monitor(0, 0, 0, 0, 0, 0, 0, 0))
    if f"Size {monitor_size} bytes." not in text:
        problems.append(f"Monitor: layout gives {monitor_size} bytes, spec disagrees")
    if problems:
        sys.exit("spec/layout mismatch:\n  " + "\n  ".join(problems))


MACHINE_A = bytes.fromhex("6f1c2a3b4c5d4e6f8091a2b3c4d5e6f7")
SESSION = bytes.fromhex("00112233445566778899aabbccddeeff")
TOKEN = bytes.fromhex("ffeeddccbbaa99887766554433221100")
CHUNK = "Hola, ñandú 🐦\n".encode("utf-8")

OK_VECTORS = [
    ("hello_host", "Hello", hello(1, 0, 0x0001, 0b1_0001_0111, MACHINE_A, "Estudio Mac", "0.1.0"),
     "host Hello; caps text+HTML+PNG+files+drag&drop"),
    ("session_start", "SessionStart", session_start(SESSION, TOKEN), ""),
    ("goodbye_incompatible", "Goodbye", goodbye(2, "1.0"), "reason INCOMPATIBLE_VERSION, detail = local version"),
    ("goodbye_no_detail", "Goodbye", u16(1), "detail field absent"),
    ("ping", "Ping", ping(7, 1_234_567_890_123), ""),
    ("pong", "Pong", pong(7, 1_234_567_890_123), ""),
    ("bulk_attach", "BulkAttach", bulk_attach(TOKEN), ""),
    ("pair_start", "PairStart", pair_start(), "empty payload"),
    ("pair_confirm_yes", "PairConfirm", pair_confirm(True), ""),
    ("enter", "Enter", enter(42, 1919, -300, 0b001, 0x0001), "Caps Lock on, Shift-left held"),
    ("leave", "Leave", leave(42), ""),
    ("leave_ack_ok", "LeaveAck", leave_ack(42, True), ""),
    ("mouse_motion_neg", "MouseMotion", mouse_motion(42, 4_000_000_000, -5, 12), "negative dx, t_us near wrap"),
    ("mouse_absolute", "MouseAbsolute", mouse_absolute(42, 10, -2560, 1440), "monitor left of origin"),
    ("mouse_button_left_down", "MouseButton", mouse_button(42, 11, 1, True), ""),
    ("wheel_trackpad", "Wheel", wheel(42, 12, 0, -60, 0x01), "half notch, continuous device"),
    ("key_n_tilde_down", "Key",
     key(42, 13, 1, 0, 0x0000_002D, 0x0007_0033, ord("ñ"), 0, 0, 0),
     "ES layout: physical ';' position (HID 0x33) producing U+00F1"),
    ("key_repeat", "Key", key(42, 14, 2, 3, 0x2D, 0x0007_0033, ord("ñ"), 0, 0, 0), "3 repeats"),
    ("key_cmd_c", "Key",
     key(42, 15, 1, 0, 0x08, 0x0007_0006, ord("c"), 0, 1 << 6, 0), "Cmd(Meta-left)+C"),
    ("key_volume_up", "Key", key(42, 16, 1, 0, 0x48, 0x000C_00E9, 0, 60, 0, 0), "consumer page Volume Up"),
    ("key_dead_acute", "Key", key(42, 17, 1, 0, 0x27, 0x0007_0034, 0x00B4, 0, 0, 0x01), "dead key flag"),
    ("release_all", "ReleaseAll", release_all(42), ""),
    ("clipboard_grab", "ClipboardGrab", clipboard_grab(9, 42, 0b0011), "text + HTML"),
    ("drag_start", "DragStart", drag_start(3, 2, 10_485_760, "Informe año.pdf"), "UTF-8 name"),
    ("transfer_begin_files", "TransferBegin", transfer_begin(3, 2, 2, 10_485_760, 0), ""),
    ("transfer_item_nested", "TransferItem", transfer_item(3, 1, 1, 0, 1024, "Fotos/Señal 1.png"), ""),
    ("transfer_item_clipboard_html", "TransferItem", transfer_item(4, 0, 3, 2, 48, ""), "clipboard HTML, no path"),
    ("transfer_chunk", "TransferChunk", transfer_chunk(3, 1, 65_536, CHUNK), ""),
    ("transfer_item_end", "TransferItemEnd", transfer_item_end(3, 1, hashlib.sha256(CHUNK).digest()), "sha256 of the chunk data above"),
    ("transfer_end", "TransferEnd", transfer_end(3), ""),
    ("transfer_cancel_bad_path", "TransferCancel", transfer_cancel(3, 3), ""),
    ("transfer_resume", "TransferResume", transfer_resume(3, 1, 65_536), ""),
    ("screen_info_two_monitors", "ScreenInfo",
     screen_info(1, 100, 200, [monitor(1, 0, 0, 2560, 1600, 2000, 302, 189),
                               monitor(2, 2560, -200, 1920, 1080, 1000, 527, 296)]), ""),
    ("screen_info_ack", "ScreenInfoAck", screen_info_ack(1), ""),
    ("config_push", "ConfigPush", config_push(5, '{"schema":1}'), ""),
]


def hexs(b): return b.hex()


def main():
    check_spec_minimums()
    vectors = []
    for name, msg, payload, note in OK_VECTORS:
        f = frame(TYPES[msg], payload)
        assert len(f) - 4 <= (BULK_MAX if 0x8302 <= TYPES[msg] <= 0x8308 else CONTROL_MAX)
        vectors.append({"name": name, "message": msg, "type": f"0x{TYPES[msg]:04X}",
                        "expect": "ok", "note": note, "frame": hexs(f)})

    edge = [
        ("unknown_noncritical", frame(0x7F01, b"\x01\x02\x03"), "ignore",
         "unknown type without critical bit: skip the frame"),
        ("unknown_critical", frame(0xFF01, b""), "violation",
         "unknown type with critical bit: Goodbye(PROTOCOL_VIOLATION)"),
        ("appended_field", frame(TYPES["MouseMotion"], mouse_motion(42, 1, 3, 4) + b"\xAA\xBB"), "ok",
         "known type with extra trailing bytes: parse prefix (dx=3, dy=4), ignore the rest"),
        ("short_payload", frame(TYPES["Key"], key(0, 0, 0, 0, 0, 0, 0, 0, 0, 0)[:-1]), "violation",
         "Key payload one byte shorter than minimum"),
        ("bad_bool", frame(TYPES["PairConfirm"], b"\x02"), "violation", "bool must be 0 or 1"),
        ("bad_utf8", frame(TYPES["Goodbye"], u16(1) + u16(2) + b"\xC3\x28"), "violation",
         "invalid UTF-8 in str"),
        ("control_too_large", u32(CONTROL_MAX + 1) + u16(0x0004), "frame_too_large",
         "length header above control maximum: reject before reading payload"),
        ("length_below_minimum", u32(1) + b"\x04", "violation", "length < 2 (no room for type)"),
    ]
    for name, f, expect, note in edge:
        vectors.append({"name": name, "expect": expect, "note": note, "frame": hexs(f)})

    json.dump({"protocol": "portalswitch", "version": "1.0",
               "generator": "protocol/tools/gen_vectors.py", "vectors": vectors},
              sys.stdout, indent=2, ensure_ascii=False)
    sys.stdout.write("\n")


if __name__ == "__main__":
    main()
