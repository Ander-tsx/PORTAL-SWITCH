// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The PortalSwitch Authors

//! Adapter from the local API to the running daemon.

use portal_ipc::proto::{ErrorBody, Event, PeerInfo, Request, StatusInfo, code};
use portal_ipc::server::{BoxFuture, Service};
use serde_json::{Value, json};
use tokio::sync::broadcast;

use crate::config::{Config, format_machine_id, parse_machine_id};
use crate::daemon::{DaemonEvent, DaemonHandle};

/// Serves [`Request`]s against a [`DaemonHandle`].
#[derive(Debug)]
pub struct DaemonService {
    handle: DaemonHandle,
    events: broadcast::Sender<Event>,
}

impl DaemonService {
    /// Bridge daemon events to API events and start answering requests.
    #[must_use]
    pub fn new(handle: DaemonHandle) -> Self {
        let (events, _) = broadcast::channel(256);
        let (forward, mut source) = (events.clone(), handle.subscribe());
        tokio::spawn(async move {
            loop {
                match source.recv().await {
                    Ok(event) => {
                        let _ = forward.send(convert(event));
                    }
                    Err(broadcast::error::RecvError::Lagged(_)) => {}
                    Err(broadcast::error::RecvError::Closed) => return,
                }
            }
        });
        Self { handle, events }
    }

    fn status(&self) -> StatusInfo {
        let s = self.handle.status();
        StatusInfo {
            machine: format_machine_id(s.machine),
            is_host: s.is_host,
            active: format_machine_id(s.active),
            locked: s.locked,
            peers: s
                .peers
                .into_iter()
                .map(|(id, name)| PeerInfo {
                    machine: format_machine_id(id),
                    name,
                })
                .collect(),
        }
    }
}

fn convert(event: DaemonEvent) -> Event {
    match event {
        DaemonEvent::PairingRequested {
            id,
            remote_name,
            code,
        } => Event::PairingRequested {
            id,
            remote_name,
            code,
        },
        DaemonEvent::PeerConnected { machine, name } => Event::PeerConnected {
            machine: format_machine_id(machine),
            name,
        },
        DaemonEvent::PeerDisconnected { machine } => Event::PeerDisconnected {
            machine: format_machine_id(machine),
        },
        DaemonEvent::ActiveChanged { machine } => Event::ActiveChanged {
            machine: format_machine_id(machine),
        },
        DaemonEvent::DialFailed { address, reason } => Event::DialFailed { address, reason },
    }
}

fn bad(message: impl Into<String>) -> ErrorBody {
    ErrorBody::new(code::BAD_REQUEST, message)
}

fn to_value<T: serde::Serialize>(value: &T) -> Result<Value, ErrorBody> {
    serde_json::to_value(value).map_err(|e| ErrorBody::new(code::FAILED, e.to_string()))
}

impl Service for DaemonService {
    fn call(&self, request: Request) -> BoxFuture<'_, Result<Value, ErrorBody>> {
        Box::pin(async move {
            match request {
                Request::Status => to_value(&self.status()),
                Request::Paired => to_value(
                    &self
                        .handle
                        .paired()
                        .into_iter()
                        .map(|(id, name)| PeerInfo {
                            machine: format_machine_id(id),
                            name,
                        })
                        .collect::<Vec<_>>(),
                ),
                Request::Connect { address } => {
                    let addr = address
                        .parse()
                        .map_err(|_| bad("address must look like 192.168.1.5:24850"))?;
                    self.handle
                        .connect(addr)
                        .await
                        .map(|()| Value::Null)
                        .map_err(|e| ErrorBody::new(code::FAILED, e.to_string()))
                }
                Request::Jump { machine } => {
                    let id = parse_machine_id(&machine).map_err(|e| bad(e.to_string()))?;
                    if !self.handle.status().peers.iter().any(|(p, _)| *p == id)
                        && id != self.handle.machine_id()
                    {
                        return Err(ErrorBody::new(
                            code::NOT_FOUND,
                            "that machine is not connected",
                        ));
                    }
                    if !self.handle.status().is_host {
                        return Err(ErrorBody::new(
                            code::NOT_HOST,
                            "only the host can switch screens",
                        ));
                    }
                    self.handle.jump(id);
                    Ok(Value::Null)
                }
                Request::SetLocked { locked } => {
                    self.handle.set_locked(locked);
                    Ok(Value::Null)
                }
                Request::AnswerPairing { id, accept } => {
                    self.handle.answer_pairing(id, accept);
                    Ok(Value::Null)
                }
                Request::Revoke { machine } => {
                    let id = parse_machine_id(&machine).map_err(|e| bad(e.to_string()))?;
                    let removed = self
                        .handle
                        .revoke(id)
                        .map_err(|e| ErrorBody::new(code::FAILED, e.to_string()))?;
                    Ok(json!({ "removed": removed }))
                }
                Request::GetConfig => to_value(&self.handle.config()),
                Request::SetConfig { config } => {
                    let config: Config =
                        serde_json::from_value(config).map_err(|e| bad(e.to_string()))?;
                    let restart = self
                        .handle
                        .set_config(config)
                        .map_err(|e| bad(e.to_string()))?;
                    Ok(json!({ "restart_required": restart }))
                }
                Request::Subscribe => Ok(Value::Null), // handled by the server
                Request::Shutdown => {
                    self.handle.shutdown();
                    Ok(Value::Null)
                }
            }
        })
    }

    fn events(&self) -> broadcast::Receiver<Event> {
        self.events.subscribe()
    }

    fn daemon_version(&self) -> String {
        portal_core::VERSION.to_owned()
    }
}
