// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The PortalSwitch Authors

//! An established link between two machines: control and bulk channels, liveness, orderly
//! shutdown (protocol §3, §5, §11).

use std::sync::atomic::{AtomicU32, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use portal_proto::{Channel, Decoded, Goodbye, Message, Ping, Pong, goodbye};
use tokio::io::{AsyncRead, AsyncWrite};
use tokio::sync::{Notify, mpsc, watch};
use tokio::time::Instant;

use crate::frame::{FrameReader, FrameWriter, LinkError};
use crate::queue::ControlQueue;
use crate::session::{Established, RemoteInfo};

/// Boxed byte streams, so a link does not care whether it runs over TLS, TCP or a test pipe.
pub type BoxRead = Box<dyn AsyncRead + Send + Unpin>;
/// See [`BoxRead`].
pub type BoxWrite = Box<dyn AsyncWrite + Send + Unpin>;

/// Timing parameters.
#[derive(Debug, Clone, Copy)]
pub struct LinkConfig {
    /// How often `Ping` is sent (and liveness checked).
    pub ping_interval: Duration,
    /// Silent intervals before the link is declared dead.
    pub missed_intervals: u32,
    /// How long the control queue may stay full before the link is declared stalled.
    pub stall_after: Duration,
}

impl Default for LinkConfig {
    fn default() -> Self {
        Self {
            ping_interval: Duration::from_secs(1),
            missed_intervals: 3,
            stall_after: Duration::from_secs(1),
        }
    }
}

/// Why a link ended.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CloseReason {
    /// We closed it.
    Local,
    /// The peer sent `Goodbye`.
    Peer { reason: u16, detail: String },
    /// Nothing heard for too long.
    Timeout,
    /// The control queue could not drain.
    Stalled,
    /// The peer broke the protocol.
    Protocol(String),
    /// The transport failed.
    Io(String),
}

/// What a link delivers to its owner.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LinkEvent {
    Control(Message),
    Bulk(Message),
    /// Always the last event.
    Closed(CloseReason),
}

/// Sending failed because the link is closed or stalled.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
#[error("the link is closed")]
pub struct LinkClosed;

struct Shared {
    queue: Mutex<ControlQueue>,
    wake: Notify,
    closing: watch::Sender<Option<CloseReason>>,
    started: Instant,
    last_rx_ms: AtomicU64,
    full_since_ms: AtomicU64,
    ping_nonce: AtomicU32,
}

const NOT_FULL: u64 = u64::MAX;

impl Shared {
    fn elapsed_ms(&self) -> u64 {
        u64::try_from(self.started.elapsed().as_millis()).unwrap_or(u64::MAX)
    }

    fn heard(&self) {
        self.last_rx_ms.store(self.elapsed_ms(), Ordering::Relaxed);
    }

    fn close(&self, reason: CloseReason) {
        self.closing.send_if_modified(|current| {
            if current.is_some() {
                return false;
            }
            *current = Some(reason);
            true
        });
    }

    fn is_closed(&self) -> bool {
        self.closing.borrow().is_some()
    }

    fn enqueue(&self, message: Message, stall_after: Duration) -> Result<(), LinkClosed> {
        if self.is_closed() {
            return Err(LinkClosed);
        }
        let pushed = self.queue.lock().map_err(|_| LinkClosed)?.push(message);
        if pushed.is_ok() {
            self.wake.notify_one();
            return Ok(());
        }
        // Full: remember since when, and give up on the link if it never drains.
        let now = self.elapsed_ms();
        let since = self.full_since_ms.load(Ordering::Relaxed);
        if since == NOT_FULL {
            self.full_since_ms.store(now, Ordering::Relaxed);
        } else if now.saturating_sub(since)
            >= u64::try_from(stall_after.as_millis()).unwrap_or(u64::MAX)
        {
            self.close(CloseReason::Stalled);
        }
        Err(LinkClosed)
    }
}

/// Cloneable handle for sending on the bulk channel.
#[derive(Clone)]
pub struct BulkSender {
    writer: Arc<tokio::sync::Mutex<FrameWriter<BoxWrite>>>,
    shared: Arc<Shared>,
}

impl std::fmt::Debug for BulkSender {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("BulkSender").finish_non_exhaustive()
    }
}

impl BulkSender {
    /// Send one bulk message, waiting for transport flow control.
    ///
    /// # Errors
    /// The link is closed or the write failed (which closes the link).
    pub async fn send(&self, message: &Message) -> Result<(), LinkClosed> {
        if self.shared.is_closed() {
            return Err(LinkClosed);
        }
        let mut writer = self.writer.lock().await;
        writer.write(message).await.map_err(|e| {
            self.shared.close(CloseReason::Io(e.to_string()));
            LinkClosed
        })
    }
}

/// Cloneable handle for sending on the control channel.
#[derive(Clone)]
pub struct ControlSender {
    shared: Arc<Shared>,
    stall_after: Duration,
}

impl std::fmt::Debug for ControlSender {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ControlSender").finish_non_exhaustive()
    }
}

impl ControlSender {
    /// Queue a control message; never blocks.
    ///
    /// # Errors
    /// The link is closed, or its queue is full.
    pub fn send(&self, message: Message) -> Result<(), LinkClosed> {
        self.shared.enqueue(message, self.stall_after)
    }

    /// Begin an orderly shutdown (`Goodbye(NORMAL)`).
    pub fn close(&self) {
        self.shared.close(CloseReason::Local);
    }
}

/// A live link.
#[derive(Debug)]
pub struct Link {
    pub remote: RemoteInfo,
    pub session_id: [u8; 16],
    control: ControlSender,
    bulk: BulkSender,
    events: mpsc::Receiver<LinkEvent>,
}

impl Link {
    /// Take over the two channels of a negotiated session and start the link tasks.
    #[must_use]
    pub fn start(
        established: Established,
        control: (BoxRead, BoxWrite),
        bulk: (BoxRead, BoxWrite),
        config: LinkConfig,
    ) -> Self {
        let (closing, closing_rx) = watch::channel(None);
        let shared = Arc::new(Shared {
            queue: Mutex::new(ControlQueue::new()),
            wake: Notify::new(),
            closing,
            started: Instant::now(),
            last_rx_ms: AtomicU64::new(0),
            full_since_ms: AtomicU64::new(NOT_FULL),
            ping_nonce: AtomicU32::new(0),
        });
        let (events_tx, events) = mpsc::channel(1024);

        let remote_is_peer = !established.remote.is_host;
        tokio::spawn(control_writer(
            control.1,
            shared.clone(),
            closing_rx.clone(),
        ));
        tokio::spawn(reader(
            control.0,
            Channel::Control,
            shared.clone(),
            events_tx.clone(),
            closing_rx.clone(),
            remote_is_peer,
        ));
        tokio::spawn(reader(
            bulk.0,
            Channel::Bulk,
            shared.clone(),
            events_tx.clone(),
            closing_rx.clone(),
            remote_is_peer,
        ));
        tokio::spawn(ticker(shared.clone(), config, closing_rx.clone()));
        tokio::spawn(announce_close(events_tx, closing_rx));

        Self {
            remote: established.remote,
            session_id: established.session_id,
            control: ControlSender {
                shared: shared.clone(),
                stall_after: config.stall_after,
            },
            bulk: BulkSender {
                writer: Arc::new(tokio::sync::Mutex::new(FrameWriter::new(bulk.1))),
                shared,
            },
            events,
        }
    }

    /// Handle for sending control messages.
    #[must_use]
    pub fn control(&self) -> ControlSender {
        self.control.clone()
    }

    /// Handle for sending bulk messages.
    #[must_use]
    pub fn bulk(&self) -> BulkSender {
        self.bulk.clone()
    }

    /// Next event; `None` after [`LinkEvent::Closed`] has been delivered.
    pub async fn recv(&mut self) -> Option<LinkEvent> {
        self.events.recv().await
    }
}

fn goodbye_for(reason: &CloseReason) -> Option<Goodbye> {
    let (code, detail) = match reason {
        CloseReason::Local => (goodbye::NORMAL, String::new()),
        CloseReason::Timeout | CloseReason::Stalled => (goodbye::TIMEOUT, String::new()),
        CloseReason::Protocol(why) => {
            (goodbye::PROTOCOL_VIOLATION, why.chars().take(200).collect())
        }
        // The peer already hung up or the transport is broken: nobody to tell.
        CloseReason::Peer { .. } | CloseReason::Io(_) => return None,
    };
    Some(Goodbye {
        reason: code,
        detail,
    })
}

async fn control_writer(
    out: BoxWrite,
    shared: Arc<Shared>,
    mut closing: watch::Receiver<Option<CloseReason>>,
) {
    let mut writer = FrameWriter::new(out);
    loop {
        // Drain whatever is queued.
        loop {
            let next = shared.queue.lock().ok().and_then(|mut q| q.pop());
            let Some(message) = next else { break };
            if let Err(e) = writer.write(&message).await {
                shared.close(CloseReason::Io(e.to_string()));
                break;
            }
            shared.full_since_ms.store(NOT_FULL, Ordering::Relaxed);
        }
        let reason = closing.borrow().clone();
        if let Some(reason) = reason {
            if let Some(bye) = goodbye_for(&reason) {
                let _ = writer.write(&Message::Goodbye(bye)).await;
            }
            let _ = writer.shutdown().await;
            return;
        }
        tokio::select! {
            () = shared.wake.notified() => {}
            _ = closing.changed() => {}
        }
    }
}

async fn reader(
    input: BoxRead,
    channel: Channel,
    shared: Arc<Shared>,
    events: mpsc::Sender<LinkEvent>,
    mut closing: watch::Receiver<Option<CloseReason>>,
    remote_is_peer: bool,
) {
    let mut reader = FrameReader::new(input, channel);
    loop {
        let frame = tokio::select! {
            frame = reader.read() => frame,
            _ = closing.changed() => return,
        };
        shared.heard();
        let message = match frame {
            Ok(Decoded::Message(m)) => m,
            Ok(Decoded::Ignored { .. }) => continue,
            Err(LinkError::Closed) => {
                shared.close(CloseReason::Io("connection closed".into()));
                return;
            }
            Err(LinkError::Proto(e)) => {
                shared.close(CloseReason::Protocol(e.to_string()));
                return;
            }
            Err(e) => {
                shared.close(CloseReason::Io(e.to_string()));
                return;
            }
        };
        match message {
            Message::Ping(Ping {
                nonce,
                sender_time_us,
            }) => {
                let _ = shared.enqueue(
                    Message::Pong(Pong {
                        nonce,
                        echoed_time_us: sender_time_us,
                    }),
                    Duration::from_secs(1),
                );
            }
            Message::Pong(_) => {}
            Message::Goodbye(g) => {
                shared.close(CloseReason::Peer {
                    reason: g.reason,
                    detail: g.detail,
                });
                return;
            }
            m if remote_is_peer && m.is_input() => {
                shared.close(CloseReason::Protocol("a peer sent an input message".into()));
                return;
            }
            m => {
                let event = match channel {
                    Channel::Control => LinkEvent::Control(m),
                    Channel::Bulk => LinkEvent::Bulk(m),
                };
                tokio::select! {
                    sent = events.send(event) => if sent.is_err() { return },
                    _ = closing.changed() => return,
                }
            }
        }
    }
}

async fn ticker(
    shared: Arc<Shared>,
    config: LinkConfig,
    mut closing: watch::Receiver<Option<CloseReason>>,
) {
    let mut interval = tokio::time::interval(config.ping_interval);
    interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    interval.tick().await; // the first tick completes immediately
    loop {
        tokio::select! {
            _ = interval.tick() => {}
            _ = closing.changed() => return,
        }
        let silent_ms = shared
            .elapsed_ms()
            .saturating_sub(shared.last_rx_ms.load(Ordering::Relaxed));
        let limit_ms = u64::try_from(config.ping_interval.as_millis()).unwrap_or(u64::MAX)
            * u64::from(config.missed_intervals);
        if silent_ms > limit_ms {
            shared.close(CloseReason::Timeout);
            return;
        }
        let nonce = shared.ping_nonce.fetch_add(1, Ordering::Relaxed);
        let now_us = u64::try_from(shared.started.elapsed().as_micros()).unwrap_or(u64::MAX);
        let _ = shared.enqueue(
            Message::Ping(Ping {
                nonce,
                sender_time_us: now_us,
            }),
            config.stall_after,
        );
    }
}

async fn announce_close(
    events: mpsc::Sender<LinkEvent>,
    mut closing: watch::Receiver<Option<CloseReason>>,
) {
    let reason = loop {
        if let Some(reason) = closing.borrow().clone() {
            break reason;
        }
        if closing.changed().await.is_err() {
            return;
        }
    };
    // Let the writer say goodbye before the owner reacts.
    let _ = events.send(LinkEvent::Closed(reason)).await;
}
