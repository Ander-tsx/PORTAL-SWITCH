// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The PortalSwitch Authors

//! The bounded control send queue (protocol §11).
//!
//! Latency-critical frames are never dropped or reordered. When the transport is slow, runs of
//! `MouseMotion` for the same entry are merged so a stall costs bandwidth, not accuracy.

use std::collections::VecDeque;

use portal_proto::{Message, MouseMotion};

/// Maximum queued control messages before the link counts as stalled.
pub const CAPACITY: usize = 4096;

/// The queue is full: the transport has not drained for too long.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Full;

/// FIFO of outgoing control messages with motion coalescing.
#[derive(Debug, Default)]
pub struct ControlQueue {
    items: VecDeque<Message>,
}

impl ControlQueue {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    #[must_use]
    pub fn len(&self) -> usize {
        self.items.len()
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.items.is_empty()
    }

    /// Queue a message, merging it into the tail when both are motion for the same entry.
    ///
    /// # Errors
    /// [`Full`] when the queue is at [`CAPACITY`] and the message cannot be merged.
    pub fn push(&mut self, message: Message) -> Result<(), Full> {
        if let (Some(Message::MouseMotion(tail)), Message::MouseMotion(new)) =
            (self.items.back_mut(), &message)
            && let Some(merged) = merge(tail, new)
        {
            *tail = merged;
            return Ok(());
        }
        if self.items.len() >= CAPACITY {
            return Err(Full);
        }
        self.items.push_back(message);
        Ok(())
    }

    /// Take the oldest message.
    pub fn pop(&mut self) -> Option<Message> {
        self.items.pop_front()
    }
}

fn merge(tail: &MouseMotion, new: &MouseMotion) -> Option<MouseMotion> {
    if tail.enter_seq != new.enter_seq {
        return None;
    }
    Some(MouseMotion {
        enter_seq: tail.enter_seq,
        t_us: tail.t_us,
        dx: tail.dx.checked_add(new.dx)?,
        dy: tail.dy.checked_add(new.dy)?,
    })
}

#[cfg(test)]
mod tests {
    use portal_proto::{Key, Ping};

    use super::*;

    fn motion(enter_seq: u32, dx: i16, dy: i16) -> Message {
        Message::MouseMotion(MouseMotion {
            enter_seq,
            t_us: 1,
            dx,
            dy,
        })
    }

    fn key(enter_seq: u32) -> Message {
        Message::Key(Key {
            enter_seq,
            t_us: 0,
            action: 1,
            repeat_count: 0,
            key_id: 1,
            hid_usage: 0,
            codepoint: 0,
            named_key: 0,
            modifiers: 0,
            flags: 0,
        })
    }

    #[test]
    fn consecutive_motion_is_merged() {
        let mut q = ControlQueue::new();
        q.push(motion(1, 3, -2)).unwrap();
        q.push(motion(1, 4, 5)).unwrap();
        assert_eq!(q.len(), 1);
        assert_eq!(q.pop(), Some(motion(1, 7, 3)));
    }

    #[test]
    fn motion_is_not_merged_across_keys_entries_or_overflow() {
        let mut q = ControlQueue::new();
        q.push(motion(1, 1, 1)).unwrap();
        q.push(key(1)).unwrap();
        q.push(motion(1, 1, 1)).unwrap(); // after a key: order must be kept
        q.push(motion(2, 1, 1)).unwrap(); // other entry
        q.push(motion(2, i16::MAX, 0)).unwrap();
        q.push(motion(2, 1, 0)).unwrap(); // would overflow i16
        assert_eq!(q.len(), 6);
    }

    #[test]
    fn order_of_important_messages_is_preserved() {
        let mut q = ControlQueue::new();
        let msgs = [
            key(1),
            Message::Ping(Ping {
                nonce: 1,
                sender_time_us: 0,
            }),
            key(1),
        ];
        for m in &msgs {
            q.push(m.clone()).unwrap();
        }
        for m in msgs {
            assert_eq!(q.pop(), Some(m));
        }
    }

    #[test]
    fn full_queue_refuses_but_still_merges_motion() {
        let mut q = ControlQueue::new();
        for _ in 0..CAPACITY - 1 {
            q.push(key(1)).unwrap();
        }
        q.push(motion(1, 1, 1)).unwrap();
        assert_eq!(q.push(key(1)), Err(Full));
        q.push(motion(1, 1, 1)).unwrap(); // merges into the tail
        assert_eq!(q.len(), CAPACITY);
    }
}
