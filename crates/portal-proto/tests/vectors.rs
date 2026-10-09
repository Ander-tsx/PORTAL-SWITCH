// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The PortalSwitch Authors

//! Cross-checks the codec against `protocol/vectors/v1.json`, produced by an independent
//! Python encoder.

#![allow(clippy::unwrap_used, clippy::cast_possible_truncation)]

use portal_proto::{Channel, Decoded, Error, Message, decode, encode_to_vec, goodbye};

fn unhex(s: &str) -> Vec<u8> {
    (0..s.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(&s[i..i + 2], 16).unwrap())
        .collect()
}

fn vectors() -> Vec<serde_json::Value> {
    let path = concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../protocol/vectors/v1.json"
    );
    let doc: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(path).unwrap()).unwrap();
    doc["vectors"].as_array().unwrap().clone()
}

#[test]
fn valid_vectors_decode_and_reencode_identically() {
    let mut checked = 0;
    for v in vectors().iter().filter(|v| v["expect"] == "ok") {
        let name = v["name"].as_str().unwrap();
        let frame = unhex(v["frame"].as_str().unwrap());
        let channel = if frame.len() > 70_000 {
            Channel::Bulk
        } else {
            Channel::Control
        };
        let (decoded, used) = decode(channel, &frame)
            .unwrap_or_else(|e| panic!("{name}: {e}"))
            .unwrap();
        assert_eq!(used, frame.len(), "{name}: consumed length");
        let Decoded::Message(message) = decoded else {
            panic!("{name}: ignored")
        };
        let reencoded = encode_to_vec(&message).unwrap();
        if name == "appended_field" {
            // Unknown trailing bytes are dropped; the known prefix must survive a round trip.
            let again = decode(channel, &reencoded).unwrap().unwrap().0;
            assert_eq!(again, Decoded::Message(message), "{name}: round trip");
        } else {
            assert_eq!(reencoded, frame, "{name}: re-encode");
        }
        checked += 1;
    }
    assert!(checked >= 30, "only {checked} vectors checked");
}

#[test]
fn every_prefix_of_a_valid_frame_asks_for_more() {
    for v in vectors().iter().filter(|v| v["expect"] == "ok") {
        let frame = unhex(v["frame"].as_str().unwrap());
        for cut in 0..frame.len() {
            assert_eq!(
                decode(Channel::Bulk, &frame[..cut]),
                Ok(None),
                "{} cut {cut}",
                v["name"]
            );
        }
    }
}

#[test]
fn rejection_vectors() {
    for v in vectors().iter().filter(|v| v["expect"] != "ok") {
        let name = v["name"].as_str().unwrap();
        let frame = unhex(v["frame"].as_str().unwrap());
        let result = decode(Channel::Control, &frame);
        match v["expect"].as_str().unwrap() {
            "ignore" => {
                let (decoded, used) = result.unwrap().unwrap();
                assert_eq!(used, frame.len());
                assert!(matches!(decoded, Decoded::Ignored { .. }), "{name}");
            }
            "violation" => {
                let err = result.expect_err(name);
                assert_eq!(
                    err.goodbye_reason(),
                    goodbye::PROTOCOL_VIOLATION,
                    "{name}: {err}"
                );
            }
            "frame_too_large" => {
                let err = result.expect_err(name);
                assert!(matches!(err, Error::FrameTooLarge { .. }), "{name}");
                assert_eq!(err.goodbye_reason(), goodbye::FRAME_TOO_LARGE);
            }
            other => panic!("unknown expectation {other}"),
        }
    }
}

#[test]
fn trailing_bytes_after_known_layout_are_ignored() {
    // Ping (type 0x0004) with three extra bytes appended: a newer peer may add fields.
    let mut frame = Vec::new();
    frame.extend_from_slice(&(2u32 + 12 + 3).to_le_bytes());
    frame.extend_from_slice(&0x0004u16.to_le_bytes());
    frame.extend_from_slice(&7u32.to_le_bytes());
    frame.extend_from_slice(&42u64.to_le_bytes());
    frame.extend_from_slice(&[9, 9, 9]);
    let (decoded, used) = decode(Channel::Control, &frame).unwrap().unwrap();
    assert_eq!(used, frame.len());
    let Decoded::Message(Message::Ping(ping)) = decoded else {
        panic!("not a ping")
    };
    assert_eq!((ping.nonce, ping.sender_time_us), (7, 42));
}

/// Deterministic xorshift fuzzing: arbitrary and mutated input must never panic, and anything
/// that decodes must consume a sane number of bytes.
#[test]
fn random_and_mutated_input_never_panics() {
    let mut state = 0x9e37_79b9_7f4a_7c15_u64;
    let mut next = move || {
        state ^= state << 13;
        state ^= state >> 7;
        state ^= state << 17;
        state
    };
    let seeds: Vec<Vec<u8>> = vectors()
        .iter()
        .filter(|v| v["expect"] == "ok")
        .map(|v| unhex(v["frame"].as_str().unwrap()))
        .collect();

    for round in 0..200_000_u64 {
        let buf: Vec<u8> = if round % 2 == 0 {
            let len = (next() % 80) as usize;
            (0..len).map(|_| next() as u8).collect()
        } else {
            let mut b = seeds[(next() as usize) % seeds.len()].clone();
            for _ in 0..=(next() % 3) {
                let i = (next() as usize) % b.len();
                b[i] = next() as u8;
            }
            b
        };
        for channel in [Channel::Control, Channel::Bulk] {
            if let Ok(Some((_, used))) = decode(channel, &buf) {
                assert!(used <= buf.len());
            }
        }
    }
}
