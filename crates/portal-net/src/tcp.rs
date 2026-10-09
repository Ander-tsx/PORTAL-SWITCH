// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The PortalSwitch Authors

//! Transport mapping A: two TCP + TLS connections per link (protocol §10).
//!
//! The *control* connection carries the handshake and all latency-critical traffic; the *bulk*
//! connection is opened by the initiator after `SessionStart` and attached with the single-use
//! token. Either side may initiate; the acceptor demultiplexes both kinds on one listener by
//! ALPN.

use std::collections::{HashMap, HashSet};
use std::future::Future;
use std::net::SocketAddr;
use std::pin::Pin;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use portal_core::layout::MachineId;
use portal_proto::{BulkAttach, Channel, Goodbye, Message, goodbye};
use rustls::pki_types::ServerName;
use tokio::io::{AsyncRead, AsyncWrite, split};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::{mpsc, oneshot};
use tokio::time::timeout;
use tokio_rustls::{TlsAcceptor, TlsConnector};

use crate::Error;
use crate::frame::{FrameReader, FrameWriter, LinkError};
use crate::link::{BoxRead, BoxWrite, Link, LinkConfig, Streams};
use crate::pairing::{PairingLimiter, code_from_exported, exporter_context, exporter_label};
use crate::session::{
    Established, HandshakeError, LocalInfo, PairingPrompt, Role, TransportFacts, handshake,
};
use crate::tls::{
    ALPN_BULK, ALPN_CONTROL, SERVER_NAME, client_config, peer_spki_sha256, server_config,
};
use crate::trust::TrustStore;

/// How long the bulk connection may take to attach after `SessionStart`.
pub const BULK_ATTACH_TIMEOUT: Duration = Duration::from_secs(5);

type Confirm =
    Arc<dyn Fn(PairingPrompt) -> Pin<Box<dyn Future<Output = bool> + Send>> + Send + Sync>;
type BulkStreams = (BoxRead, Vec<u8>, BoxWrite);

/// Why connecting or accepting failed.
#[derive(Debug, thiserror::Error)]
pub enum ConnectError {
    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),
    #[error("TLS error: {0}")]
    Tls(#[from] rustls::Error),
    #[error(transparent)]
    Setup(#[from] Error),
    #[error(transparent)]
    Handshake(#[from] HandshakeError),
    #[error("link error: {0}")]
    Link(#[from] LinkError),
    #[error("this machine is already connected")]
    Duplicate,
    #[error("too many failed pairing attempts from this address")]
    LockedOut,
    #[error("the bulk connection did not attach")]
    BulkTimeout,
    #[error("the peer did not present a usable certificate")]
    NoCertificate,
}

struct Inner {
    local: LocalInfo,
    trust: Mutex<TrustStore>,
    limiter: Mutex<PairingLimiter>,
    connected: Mutex<HashSet<MachineId>>,
    pending_bulk: Mutex<HashMap<[u8; 16], PendingBulk>>,
    confirm: Confirm,
    config: LinkConfig,
    acceptor: TlsAcceptor,
    control_connector: TlsConnector,
    bulk_connector: TlsConnector,
}

struct PendingBulk {
    expected_spki: [u8; 32],
    deliver: oneshot::Sender<BulkStreams>,
}

/// Removes a machine from the connected set when its link is dropped.
struct ConnectedGuard {
    inner: Arc<Inner>,
    machine: MachineId,
}

impl Drop for ConnectedGuard {
    fn drop(&mut self) {
        if let Ok(mut set) = self.inner.connected.lock() {
            set.remove(&self.machine);
        }
    }
}

/// A machine's network presence: accepts links and opens links to others.
#[derive(Clone)]
pub struct Endpoint {
    inner: Arc<Inner>,
}

impl std::fmt::Debug for Endpoint {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Endpoint")
            .field("machine", &self.inner.local.identity.machine_id())
            .finish_non_exhaustive()
    }
}

impl Endpoint {
    /// Create an endpoint.
    ///
    /// `confirm` shows a pairing code to the user and returns their answer; it is only called
    /// for machines that are not pinned yet.
    ///
    /// # Errors
    /// The identity could not be turned into TLS configurations.
    pub fn new<F, Fut>(
        local: LocalInfo,
        trust: TrustStore,
        config: LinkConfig,
        confirm: F,
    ) -> Result<Self, Error>
    where
        F: Fn(PairingPrompt) -> Fut + Send + Sync + 'static,
        Fut: Future<Output = bool> + Send + 'static,
    {
        let confirm: Confirm = Arc::new(move |prompt| Box::pin(confirm(prompt)));
        let acceptor =
            TlsAcceptor::from(server_config(&local.identity, &[ALPN_CONTROL, ALPN_BULK])?);
        let control_connector = TlsConnector::from(client_config(&local.identity, ALPN_CONTROL)?);
        let bulk_connector = TlsConnector::from(client_config(&local.identity, ALPN_BULK)?);
        Ok(Self {
            inner: Arc::new(Inner {
                local,
                trust: Mutex::new(trust),
                limiter: Mutex::new(PairingLimiter::new()),
                connected: Mutex::new(HashSet::new()),
                pending_bulk: Mutex::new(HashMap::new()),
                confirm,
                config,
                acceptor,
                control_connector,
                bulk_connector,
            }),
        })
    }

    /// This machine's identity.
    #[must_use]
    pub fn identity(&self) -> &crate::Identity {
        &self.inner.local.identity
    }

    /// Run `f` with the trust store (e.g. to list or revoke peers).
    pub fn with_trust<T>(&self, f: impl FnOnce(&mut TrustStore) -> T) -> T {
        let mut guard = self
            .inner
            .trust
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        f(&mut guard)
    }

    /// Accept connections on `listener`, delivering established links to the returned receiver.
    /// The accept loop ends when the receiver is dropped.
    #[must_use]
    pub fn serve(&self, listener: TcpListener) -> mpsc::Receiver<Link> {
        let (tx, rx) = mpsc::channel(8);
        let endpoint = self.clone();
        tokio::spawn(async move {
            loop {
                let Ok((tcp, addr)) = listener.accept().await else {
                    continue;
                };
                if tx.is_closed() {
                    return;
                }
                let (endpoint, tx) = (endpoint.clone(), tx.clone());
                tokio::spawn(async move {
                    // Failures here are per-connection and already reported to the peer.
                    if let Ok(Some(link)) = endpoint.accept_one(tcp, addr).await {
                        let _ = tx.send(link).await;
                    }
                });
            }
        });
        rx
    }

    /// Connect to another machine and run the handshake.
    ///
    /// # Errors
    /// See [`ConnectError`].
    pub async fn connect(&self, addr: SocketAddr) -> Result<Link, ConnectError> {
        let inner = &self.inner;
        let tcp = TcpStream::connect(addr).await?;
        tcp.set_nodelay(true)?;
        let name = ServerName::try_from(SERVER_NAME).map_err(|_| ConnectError::NoCertificate)?;
        let tls = inner.control_connector.connect(name.clone(), tcp).await?;

        let conn = tls.get_ref().1;
        let peer_spki =
            peer_spki_sha256(conn.peer_certificates()).ok_or(ConnectError::NoCertificate)?;
        let ctx = exporter_context(&inner.local.identity.spki_sha256(), &peer_spki);
        let exported: [u8; 8] =
            conn.export_keying_material([0u8; 8], exporter_label(), Some(&ctx))?;
        let facts = TransportFacts {
            peer_spki_sha256: peer_spki,
            pairing_code: code_from_exported(exported),
        };

        let (r, w) = split(tls);
        let (mut reader, mut writer) = (FrameReader::new(r, Channel::Control), FrameWriter::new(w));
        let established = self
            .run_handshake(Role::Initiator, &mut reader, &mut writer, &facts, None)
            .await?;
        let guard = self.claim(&established, &mut writer).await?;

        // Open the bulk connection and attach it.
        let tcp = TcpStream::connect(addr).await?;
        tcp.set_nodelay(true)?;
        let tls = inner.bulk_connector.connect(name, tcp).await?;
        let bulk_spki = peer_spki_sha256(tls.get_ref().1.peer_certificates())
            .ok_or(ConnectError::NoCertificate)?;
        if bulk_spki != peer_spki {
            return Err(ConnectError::NoCertificate);
        }
        let (bulk_r, bulk_w) = split(tls);
        let mut bulk_writer = FrameWriter::new(bulk_w);
        bulk_writer
            .write(&Message::BulkAttach(BulkAttach {
                bulk_token: established.bulk_token,
            }))
            .await?;

        let (control_r, buffered) = reader.into_parts();
        let mut link = Link::start(
            established,
            Streams {
                read: Box::new(control_r),
                buffered,
                write: Box::new(writer.into_inner()),
            },
            Streams::new(Box::new(bulk_r), Box::new(bulk_writer.into_inner())),
            inner.config,
        );
        link.attach_guard(Box::new(guard));
        Ok(link)
    }

    /// Handle one incoming TCP connection. `Ok(None)` means it was a bulk connection that has
    /// been handed to the control session waiting for it.
    async fn accept_one(
        &self,
        tcp: TcpStream,
        addr: SocketAddr,
    ) -> Result<Option<Link>, ConnectError> {
        let inner = &self.inner;
        tcp.set_nodelay(true)?;
        let allowed = inner
            .limiter
            .lock()
            .is_ok_and(|mut l| l.allowed(addr.ip(), now_us()));
        if !allowed {
            return Err(ConnectError::LockedOut);
        }
        let tls = inner.acceptor.accept(tcp).await?;
        let conn = tls.get_ref().1;
        let peer_spki =
            peer_spki_sha256(conn.peer_certificates()).ok_or(ConnectError::NoCertificate)?;
        match conn.alpn_protocol() {
            Some(p) if p == ALPN_BULK => {
                self.attach_bulk(tls, peer_spki).await?;
                return Ok(None);
            }
            Some(p) if p == ALPN_CONTROL => {}
            _ => return Err(ConnectError::NoCertificate),
        }
        let ctx = exporter_context(&inner.local.identity.spki_sha256(), &peer_spki);
        let exported: [u8; 8] =
            conn.export_keying_material([0u8; 8], exporter_label(), Some(&ctx))?;
        let facts = TransportFacts {
            peer_spki_sha256: peer_spki,
            pairing_code: code_from_exported(exported),
        };

        let (r, w) = split(tls);
        let (mut reader, mut writer) = (FrameReader::new(r, Channel::Control), FrameWriter::new(w));
        let established = self
            .run_handshake(Role::Acceptor, &mut reader, &mut writer, &facts, Some(addr))
            .await?;
        let guard = self.claim(&established, &mut writer).await?;

        // Wait for the initiator's bulk connection.
        let (deliver, wait) = oneshot::channel();
        if let Ok(mut pending) = inner.pending_bulk.lock() {
            pending.insert(
                established.bulk_token,
                PendingBulk {
                    expected_spki: peer_spki,
                    deliver,
                },
            );
        }
        let bulk = timeout(BULK_ATTACH_TIMEOUT, wait).await;
        if let Ok(mut pending) = inner.pending_bulk.lock() {
            pending.remove(&established.bulk_token);
        }
        let (bulk_r, bulk_buffered, bulk_w) = bulk
            .map_err(|_| ConnectError::BulkTimeout)?
            .map_err(|_| ConnectError::BulkTimeout)?;

        let (control_r, buffered) = reader.into_parts();
        let mut link = Link::start(
            established,
            Streams {
                read: Box::new(control_r),
                buffered,
                write: Box::new(writer.into_inner()),
            },
            Streams {
                read: bulk_r,
                buffered: bulk_buffered,
                write: bulk_w,
            },
            inner.config,
        );
        link.attach_guard(Box::new(guard));
        Ok(Some(link))
    }

    /// A bulk connection arrived: read `BulkAttach` and hand the streams to the waiting session.
    async fn attach_bulk<S>(&self, tls: S, peer_spki: [u8; 32]) -> Result<(), ConnectError>
    where
        S: AsyncRead + AsyncWrite + Send + Unpin + 'static,
    {
        let (r, w) = split(tls);
        let mut reader = FrameReader::new(r, Channel::Bulk);
        let message = timeout(BULK_ATTACH_TIMEOUT, reader.read_message())
            .await
            .map_err(|_| ConnectError::BulkTimeout)??;
        let Message::BulkAttach(attach) = message else {
            return Err(ConnectError::BulkTimeout);
        };
        let pending = self
            .inner
            .pending_bulk
            .lock()
            .ok()
            .and_then(|mut p| p.remove(&attach.bulk_token))
            .ok_or(ConnectError::BulkTimeout)?;
        if pending.expected_spki != peer_spki {
            return Err(ConnectError::NoCertificate);
        }
        let (r, buffered) = reader.into_parts();
        pending
            .deliver
            .send((Box::new(r), buffered, Box::new(w)))
            .map_err(|_| ConnectError::BulkTimeout)
    }

    async fn run_handshake<R, W>(
        &self,
        role: Role,
        reader: &mut FrameReader<R>,
        writer: &mut FrameWriter<W>,
        facts: &TransportFacts,
        remote_addr: Option<SocketAddr>,
    ) -> Result<Established, ConnectError>
    where
        R: AsyncRead + Unpin,
        W: AsyncWrite + Unpin,
    {
        let inner = &self.inner;
        let trust_snapshot = {
            let guard = inner
                .trust
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            clone_store(&guard)
        };
        let confirm = inner.confirm.clone();
        let result = handshake(
            role,
            reader,
            writer,
            &inner.local,
            facts,
            &trust_snapshot,
            move |p| confirm(p),
        )
        .await;
        match result {
            Ok(established) => {
                if established.newly_paired {
                    let mut trust = inner
                        .trust
                        .lock()
                        .unwrap_or_else(std::sync::PoisonError::into_inner);
                    trust.pin(
                        established.remote.machine_id,
                        &established.remote.machine_name,
                        &established.remote.spki_sha256,
                    )?;
                    if let (Some(addr), Ok(mut limiter)) = (remote_addr, inner.limiter.lock()) {
                        limiter.record_success(addr.ip());
                    }
                }
                Ok(established)
            }
            Err(e) => {
                if matches!(
                    e,
                    HandshakeError::PairingRejected | HandshakeError::KeyMismatch
                ) && let (Some(addr), Ok(mut limiter)) = (remote_addr, inner.limiter.lock())
                {
                    limiter.record_failure(addr.ip(), now_us());
                }
                Err(e.into())
            }
        }
    }

    /// Mark the machine as connected, or refuse a second link from it.
    async fn claim<W: AsyncWrite + Unpin>(
        &self,
        established: &Established,
        writer: &mut FrameWriter<W>,
    ) -> Result<ConnectedGuard, ConnectError> {
        let fresh = self
            .inner
            .connected
            .lock()
            .is_ok_and(|mut set| set.insert(established.remote.machine_id));
        if !fresh {
            let _ = writer
                .write(&Message::Goodbye(Goodbye {
                    reason: goodbye::DUPLICATE_MACHINE,
                    detail: String::new(),
                }))
                .await;
            return Err(ConnectError::Duplicate);
        }
        Ok(ConnectedGuard {
            inner: self.inner.clone(),
            machine: established.remote.machine_id,
        })
    }
}

fn now_us() -> u64 {
    use std::time::{SystemTime, UNIX_EPOCH};
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| u64::try_from(d.as_micros()).unwrap_or(u64::MAX))
}

fn clone_store(store: &TrustStore) -> TrustStore {
    store.snapshot()
}
