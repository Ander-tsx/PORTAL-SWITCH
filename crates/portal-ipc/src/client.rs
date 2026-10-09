// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The PortalSwitch Authors

//! A client for the local API, used by the UI and command-line tools.

use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use serde::de::DeserializeOwned;
use serde_json::Value;
use tokio::io::{AsyncWriteExt as _, BufReader};
use tokio::sync::{mpsc, oneshot};

use crate::proto::{
    ClientHello, ErrorBody, Event, PROTOCOL_VERSION, PeerInfo, Reply, Request, RequestFrame,
    ServerHello, StatusInfo,
};
use crate::server::{MAX_LINE, read_line};
use crate::transport::{Endpoint, connect};

/// Why a call failed.
#[derive(Debug, thiserror::Error)]
pub enum ClientError {
    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),
    #[error("the daemon sent something unexpected: {0}")]
    Protocol(String),
    #[error("the daemon refused: {0}")]
    Remote(#[from] ErrorBody),
    #[error("the connection to the daemon closed")]
    Closed,
}

type Pending = Arc<Mutex<HashMap<u64, oneshot::Sender<Reply>>>>;

/// A connection to the daemon.
#[derive(Debug)]
pub struct Client {
    lines: mpsc::Sender<String>,
    pending: Pending,
    next_id: AtomicU64,
    events: Mutex<Option<mpsc::Receiver<Event>>>,
    /// What the daemon announced.
    pub hello: ServerHello,
}

impl Client {
    /// Connect and authenticate.
    ///
    /// # Errors
    /// The daemon is not running, the token is wrong, or the versions are incompatible.
    pub async fn connect(endpoint: &Endpoint, token: &str) -> Result<Self, ClientError> {
        let io = connect(endpoint).await?;
        let (read, mut write) = tokio::io::split(io);
        let mut reader = BufReader::new(read);
        let hello = ClientHello {
            version: PROTOCOL_VERSION,
            token: token.to_owned(),
        };
        let text =
            serde_json::to_string(&hello).map_err(|e| ClientError::Protocol(e.to_string()))?;
        write.write_all((text + "\n").as_bytes()).await?;

        let line = read_line(&mut reader, 4096)
            .await?
            .ok_or(ClientError::Closed)?;
        let hello: ServerHello = match serde_json::from_str(&line) {
            Ok(h) => h,
            Err(_) => {
                return Err(match serde_json::from_str::<Reply>(&line) {
                    Ok(Reply { error: Some(e), .. }) => ClientError::Remote(e),
                    _ => ClientError::Protocol(line),
                });
            }
        };

        let pending: Pending = Arc::new(Mutex::new(HashMap::new()));
        let (event_tx, event_rx) = mpsc::channel(256);
        let (lines, mut outgoing) = mpsc::channel::<String>(64);
        tokio::spawn(async move {
            while let Some(line) = outgoing.recv().await {
                if write.write_all(line.as_bytes()).await.is_err()
                    || write.write_all(b"\n").await.is_err()
                {
                    return;
                }
            }
        });
        let routing = pending.clone();
        tokio::spawn(async move {
            while let Ok(Some(line)) = read_line(&mut reader, MAX_LINE).await {
                let Ok(value) = serde_json::from_str::<Value>(&line) else {
                    continue;
                };
                if value.get("event").is_some() {
                    if let Ok(event) = serde_json::from_value::<Event>(value) {
                        let _ = event_tx.try_send(event);
                    }
                } else if let Ok(reply) = serde_json::from_value::<Reply>(value) {
                    let waiter = routing.lock().ok().and_then(|mut p| p.remove(&reply.id));
                    if let Some(waiter) = waiter {
                        let _ = waiter.send(reply);
                    }
                }
            }
            // Connection gone: fail everything still waiting.
            if let Ok(mut p) = routing.lock() {
                p.clear();
            }
        });

        Ok(Self {
            lines,
            pending,
            next_id: AtomicU64::new(1),
            events: Mutex::new(Some(event_rx)),
            hello,
        })
    }

    /// Send a request and wait for its reply.
    ///
    /// # Errors
    /// See [`ClientError`].
    pub async fn call(&self, request: Request) -> Result<Value, ClientError> {
        let id = self.next_id.fetch_add(1, Ordering::Relaxed);
        let (tx, rx) = oneshot::channel();
        self.pending
            .lock()
            .map_err(|_| ClientError::Closed)?
            .insert(id, tx);
        let text = serde_json::to_string(&RequestFrame { id, request })
            .map_err(|e| ClientError::Protocol(e.to_string()))?;
        self.lines
            .send(text)
            .await
            .map_err(|_| ClientError::Closed)?;
        let reply = rx.await.map_err(|_| ClientError::Closed)?;
        if reply.ok {
            Ok(reply.result.unwrap_or(Value::Null))
        } else {
            Err(reply
                .error
                .unwrap_or_else(|| ErrorBody::new("failed", "unknown error"))
                .into())
        }
    }

    async fn typed<T: DeserializeOwned>(&self, request: Request) -> Result<T, ClientError> {
        serde_json::from_value(self.call(request).await?)
            .map_err(|e| ClientError::Protocol(e.to_string()))
    }

    /// `status`.
    ///
    /// # Errors
    /// See [`ClientError`].
    pub async fn status(&self) -> Result<StatusInfo, ClientError> {
        self.typed(Request::Status).await
    }

    /// `paired`.
    ///
    /// # Errors
    /// See [`ClientError`].
    pub async fn paired(&self) -> Result<Vec<PeerInfo>, ClientError> {
        self.typed(Request::Paired).await
    }

    /// Start receiving events; call once.
    ///
    /// # Errors
    /// See [`ClientError`]; also if events were already taken.
    pub async fn subscribe(&self) -> Result<mpsc::Receiver<Event>, ClientError> {
        let receiver = self
            .events
            .lock()
            .map_err(|_| ClientError::Closed)?
            .take()
            .ok_or_else(|| ClientError::Protocol("already subscribed".into()))?;
        self.call(Request::Subscribe).await?;
        Ok(receiver)
    }
}
