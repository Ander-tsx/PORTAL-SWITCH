// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The PortalSwitch Authors

//! Session establishment on the control channel (protocol §5 and §9).
//!
//! `Hello` exchange → version and role checks → trust decision → (pairing) → `SessionStart`.
//! The functions are generic over the byte streams, so the same code runs over TLS/TCP and over
//! in-memory pipes in tests.

use std::future::Future;
use std::time::Duration;

use portal_core::layout::MachineId;
use portal_proto::{
    Goodbye, Hello, Message, PairConfirm, SessionStart, VERSION_MAJOR, VERSION_MINOR,
    goodbye as reason,
};
use ring::rand::{SecureRandom, SystemRandom};
use tokio::io::{AsyncRead, AsyncWrite};
use tokio::time::timeout;

use crate::frame::{FrameReader, FrameWriter, LinkError};
use crate::identity::Identity;
use crate::trust::{TrustState, TrustStore};

/// `Hello` must arrive within this long of the TLS handshake (protocol §5).
pub const HELLO_TIMEOUT: Duration = Duration::from_secs(5);
/// How long a user gets to compare codes and answer.
pub const PAIRING_TIMEOUT: Duration = Duration::from_secs(120);

/// Which end of the connection this is (a transport detail, not a product role).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Role {
    Initiator,
    Acceptor,
}

/// What this machine announces about itself.
#[derive(Debug, Clone)]
pub struct LocalInfo {
    pub identity: Identity,
    pub machine_name: String,
    pub app_version: String,
    pub capabilities: u64,
    /// This machine owns the physical keyboard and mouse for the session.
    pub is_host: bool,
}

/// What the other side announced, plus what the transport proved about it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RemoteInfo {
    pub machine_id: MachineId,
    pub machine_name: String,
    pub app_version: String,
    /// Capabilities both sides support.
    pub capabilities: u64,
    pub is_host: bool,
    /// Negotiated protocol minor version.
    pub minor: u8,
    /// Pin of the key the peer authenticated with.
    pub spki_sha256: [u8; 32],
}

/// Facts established by the transport before any protocol message is exchanged.
#[derive(Debug, Clone)]
pub struct TransportFacts {
    pub peer_spki_sha256: [u8; 32],
    /// Six-digit code both sides derive from the TLS session (see [`crate::pairing`]).
    pub pairing_code: String,
}

/// Shown to the user when an unknown machine connects.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PairingPrompt {
    pub remote_name: String,
    pub code: String,
}

/// A fully negotiated session (control channel only; bulk attaches separately).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Established {
    pub remote: RemoteInfo,
    pub session_id: [u8; 16],
    pub bulk_token: [u8; 16],
    /// The peer was unknown and the user confirmed the code: the caller must pin it.
    pub newly_paired: bool,
}

/// Why a handshake did not complete.
#[derive(Debug, thiserror::Error)]
pub enum HandshakeError {
    #[error(transparent)]
    Link(#[from] LinkError),
    #[error("the peer ended the session: reason {reason} ({detail})")]
    PeerClosed { reason: u16, detail: String },
    #[error("incompatible protocol version (peer {peer_major}.{peer_minor})")]
    Incompatible { peer_major: u8, peer_minor: u8 },
    #[error("the peer's key does not match the one pinned for that machine")]
    KeyMismatch,
    #[error("pairing was declined")]
    PairingRejected,
    #[error("protocol violation: {0}")]
    Violation(&'static str),
    #[error("no random numbers available")]
    Random,
}

fn random<const N: usize>() -> Result<[u8; N], HandshakeError> {
    let mut out = [0u8; N];
    SystemRandom::new()
        .fill(&mut out)
        .map_err(|_| HandshakeError::Random)?;
    Ok(out)
}

async fn recv<R: AsyncRead + Unpin>(
    reader: &mut FrameReader<R>,
    limit: Duration,
) -> Result<Message, HandshakeError> {
    match timeout(limit, reader.read_message()).await {
        Err(_) => Err(LinkError::Timeout.into()),
        Ok(Err(e)) => Err(e.into()),
        Ok(Ok(Message::Goodbye(g))) => Err(HandshakeError::PeerClosed {
            reason: g.reason,
            detail: g.detail,
        }),
        Ok(Ok(m)) => Ok(m),
    }
}

async fn refuse<W: AsyncWrite + Unpin>(
    writer: &mut FrameWriter<W>,
    code: u16,
    detail: &str,
) -> Result<(), HandshakeError> {
    // Best effort: the connection is being dropped either way.
    let _ = writer
        .write(&Message::Goodbye(Goodbye {
            reason: code,
            detail: detail.to_owned(),
        }))
        .await;
    Ok(())
}

/// Run the handshake.
///
/// `confirm` is called at most once, only when the peer is not yet pinned; it shows the code to
/// the user and resolves to their answer.
///
/// # Errors
/// See [`HandshakeError`]. When a problem is detected locally a `Goodbye` is sent first.
pub async fn handshake<R, W, F, Fut>(
    role: Role,
    reader: &mut FrameReader<R>,
    writer: &mut FrameWriter<W>,
    local: &LocalInfo,
    facts: &TransportFacts,
    trust: &TrustStore,
    confirm: F,
) -> Result<Established, HandshakeError>
where
    R: AsyncRead + Unpin,
    W: AsyncWrite + Unpin,
    F: FnOnce(PairingPrompt) -> Fut,
    Fut: Future<Output = bool>,
{
    let hello = Message::Hello(Hello {
        major: VERSION_MAJOR,
        minor: VERSION_MINOR,
        flags: if local.is_host {
            portal_proto::HELLO_FLAG_HOST
        } else {
            0
        },
        capabilities: local.capabilities,
        machine_id: local.identity.machine_id().0,
        machine_name: local.machine_name.clone(),
        app_version: local.app_version.clone(),
    });
    // Both sides send immediately; read and write concurrently so small pipes cannot deadlock.
    let (sent, received) = tokio::join!(writer.write(&hello), recv(reader, HELLO_TIMEOUT));
    sent?;
    let Message::Hello(peer) = received? else {
        refuse(writer, reason::PROTOCOL_VIOLATION, "expected Hello").await?;
        return Err(HandshakeError::Violation("first message was not Hello"));
    };

    if peer.major != VERSION_MAJOR {
        refuse(
            writer,
            reason::INCOMPATIBLE_VERSION,
            &format!("{VERSION_MAJOR}.{VERSION_MINOR}"),
        )
        .await?;
        return Err(HandshakeError::Incompatible {
            peer_major: peer.major,
            peer_minor: peer.minor,
        });
    }
    let peer_is_host = peer.flags & portal_proto::HELLO_FLAG_HOST != 0;
    if peer_is_host == local.is_host {
        refuse(
            writer,
            reason::PROTOCOL_VIOLATION,
            "exactly one side must be the host",
        )
        .await?;
        return Err(HandshakeError::Violation(
            "both or neither side claims the host role",
        ));
    }
    let remote = RemoteInfo {
        machine_id: MachineId(peer.machine_id),
        machine_name: peer.machine_name,
        app_version: peer.app_version,
        capabilities: peer.capabilities & local.capabilities,
        is_host: peer_is_host,
        // Today's minor is 0; keep the rule so the first minor bump negotiates correctly.
        #[allow(clippy::unnecessary_min_or_max)]
        minor: peer.minor.min(VERSION_MINOR),
        spki_sha256: facts.peer_spki_sha256,
    };

    let newly_paired = match trust.check(remote.machine_id, &facts.peer_spki_sha256) {
        TrustState::Trusted => false,
        TrustState::KeyMismatch => {
            refuse(writer, reason::NOT_PAIRED, "key changed").await?;
            return Err(HandshakeError::KeyMismatch);
        }
        TrustState::Unknown => {
            pair(role, reader, writer, &remote, facts, confirm).await?;
            true
        }
    };

    let (session_id, bulk_token) = match role {
        Role::Acceptor => {
            let start = SessionStart {
                session_id: random()?,
                bulk_token: random()?,
            };
            writer.write(&Message::SessionStart(start.clone())).await?;
            (start.session_id, start.bulk_token)
        }
        Role::Initiator => {
            let Message::SessionStart(s) = recv(reader, HELLO_TIMEOUT).await? else {
                refuse(writer, reason::PROTOCOL_VIOLATION, "expected SessionStart").await?;
                return Err(HandshakeError::Violation("expected SessionStart"));
            };
            (s.session_id, s.bulk_token)
        }
    };
    Ok(Established {
        remote,
        session_id,
        bulk_token,
        newly_paired,
    })
}

/// Pairing (protocol §9): the initiator asks, both show the code, both confirm.
async fn pair<R, W, F, Fut>(
    role: Role,
    reader: &mut FrameReader<R>,
    writer: &mut FrameWriter<W>,
    remote: &RemoteInfo,
    facts: &TransportFacts,
    confirm: F,
) -> Result<(), HandshakeError>
where
    R: AsyncRead + Unpin,
    W: AsyncWrite + Unpin,
    F: FnOnce(PairingPrompt) -> Fut,
    Fut: Future<Output = bool>,
{
    match role {
        Role::Initiator => writer.write(&Message::PairStart).await?,
        Role::Acceptor => {
            if recv(reader, PAIRING_TIMEOUT).await? != Message::PairStart {
                refuse(writer, reason::NOT_PAIRED, "pair first").await?;
                return Err(HandshakeError::Violation(
                    "unpaired peer sent a non-pairing message",
                ));
            }
        }
    }
    let prompt = PairingPrompt {
        remote_name: remote.machine_name.clone(),
        code: facts.pairing_code.clone(),
    };
    let accepted = timeout(PAIRING_TIMEOUT, confirm(prompt))
        .await
        .unwrap_or(false);
    // Send our answer while waiting for theirs: either side may answer first.
    let answer = Message::PairConfirm(PairConfirm { accepted });
    let (sent, theirs) = tokio::join!(writer.write(&answer), recv(reader, PAIRING_TIMEOUT));
    sent?;
    let Message::PairConfirm(theirs) = theirs? else {
        refuse(writer, reason::PROTOCOL_VIOLATION, "expected PairConfirm").await?;
        return Err(HandshakeError::Violation("expected PairConfirm"));
    };
    let theirs = theirs.accepted;
    if accepted && theirs {
        Ok(())
    } else {
        refuse(writer, reason::PAIRING_REJECTED, "").await?;
        Err(HandshakeError::PairingRejected)
    }
}

#[cfg(test)]
mod tests {
    use portal_proto::Channel;
    use tokio::io::duplex;

    use super::*;

    struct Peer {
        local: LocalInfo,
        trust: TrustStore,
    }

    fn peer(name: &str, is_host: bool) -> Peer {
        Peer {
            local: LocalInfo {
                identity: Identity::generate().unwrap(),
                machine_name: name.to_owned(),
                app_version: "0.1.0".to_owned(),
                capabilities: 0b1111,
                is_host,
            },
            trust: TrustStore::in_memory(),
        }
    }

    /// Run both ends of a handshake over a pipe with the given user answers.
    async fn run(
        a: &Peer,
        b: &Peer,
        a_answer: bool,
        b_answer: bool,
    ) -> (
        Result<Established, HandshakeError>,
        Result<Established, HandshakeError>,
    ) {
        let (a_io, b_io) = duplex(64 * 1024);
        let (a_r, a_w) = tokio::io::split(a_io);
        let (b_r, b_w) = tokio::io::split(b_io);
        let a_facts = TransportFacts {
            peer_spki_sha256: b.local.identity.spki_sha256(),
            pairing_code: "123456".into(),
        };
        let b_facts = TransportFacts {
            peer_spki_sha256: a.local.identity.spki_sha256(),
            pairing_code: "123456".into(),
        };
        let mut a_reader = FrameReader::new(a_r, Channel::Control);
        let mut a_writer = FrameWriter::new(a_w);
        let mut b_reader = FrameReader::new(b_r, Channel::Control);
        let mut b_writer = FrameWriter::new(b_w);
        tokio::join!(
            handshake(
                Role::Initiator,
                &mut a_reader,
                &mut a_writer,
                &a.local,
                &a_facts,
                &a.trust,
                |_| async move { a_answer }
            ),
            handshake(
                Role::Acceptor,
                &mut b_reader,
                &mut b_writer,
                &b.local,
                &b_facts,
                &b.trust,
                |_| async move { b_answer }
            ),
        )
    }

    fn pin(on: &mut Peer, other: &Peer) {
        let id = &other.local.identity;
        on.trust
            .pin(
                id.machine_id(),
                &other.local.machine_name,
                &id.spki_sha256(),
            )
            .unwrap();
    }

    #[tokio::test]
    async fn paired_peers_connect_without_prompting() {
        let (mut host, mut laptop) = (peer("Studio", true), peer("Laptop", false));
        pin(&mut host, &laptop);
        pin(&mut laptop, &host);
        let (h, l) = run(&host, &laptop, false, false).await; // answers would reject: must not be asked
        let (h, l) = (h.unwrap(), l.unwrap());
        assert!(!h.newly_paired && !l.newly_paired);
        assert_eq!(h.session_id, l.session_id);
        assert_eq!(h.bulk_token, l.bulk_token);
        assert_eq!(h.remote.machine_name, "Laptop");
        assert_eq!(l.remote.machine_name, "Studio");
        assert!(l.remote.is_host && !h.remote.is_host);
        assert_eq!(h.remote.capabilities, 0b1111);
    }

    #[tokio::test]
    async fn unknown_peers_pair_when_both_users_accept() {
        let (host, laptop) = (peer("Studio", true), peer("Laptop", false));
        let (h, l) = run(&host, &laptop, true, true).await;
        assert!(h.unwrap().newly_paired);
        assert!(l.unwrap().newly_paired);
    }

    #[tokio::test]
    async fn one_decline_fails_both_sides() {
        let (host, laptop) = (peer("Studio", true), peer("Laptop", false));
        let (h, l) = run(&host, &laptop, true, false).await;
        assert!(matches!(h, Err(HandshakeError::PairingRejected)), "{h:?}");
        assert!(matches!(l, Err(HandshakeError::PairingRejected)), "{l:?}");
    }

    #[tokio::test]
    async fn a_pinned_machine_with_a_new_key_is_refused() {
        let (mut host, laptop) = (peer("Studio", true), peer("Laptop", false));
        // The host pinned "Laptop" earlier, but under a different key.
        let other_key = Identity::generate().unwrap();
        host.trust
            .pin(
                laptop.local.identity.machine_id(),
                "Laptop",
                &other_key.spki_sha256(),
            )
            .unwrap();
        let (h, l) = run(&host, &laptop, true, true).await;
        assert!(matches!(h, Err(HandshakeError::KeyMismatch)), "{h:?}");
        assert!(
            matches!(
                l,
                Err(HandshakeError::PeerClosed { reason: 4, .. } | HandshakeError::Link(_))
            ),
            "{l:?}"
        );
    }

    #[tokio::test]
    async fn two_hosts_or_two_non_hosts_are_a_violation() {
        for flags in [(true, true), (false, false)] {
            let (a, b) = (peer("A", flags.0), peer("B", flags.1));
            let (x, y) = run(&a, &b, true, true).await;
            assert!(matches!(x, Err(HandshakeError::Violation(_))), "{x:?}");
            assert!(matches!(y, Err(HandshakeError::Violation(_))), "{y:?}");
        }
    }

    #[tokio::test]
    async fn major_version_mismatch_is_reported() {
        let (a_io, b_io) = duplex(4096);
        let (a_r, a_w) = tokio::io::split(a_io);
        let (b_r, b_w) = tokio::io::split(b_io);
        let host = peer("Studio", true);
        let facts = TransportFacts {
            peer_spki_sha256: [0; 32],
            pairing_code: String::new(),
        };
        // A scripted peer announcing protocol 2.0.
        let script = tokio::spawn(async move {
            let mut reader = FrameReader::new(b_r, Channel::Control);
            let mut writer = FrameWriter::new(b_w);
            writer
                .write(&Message::Hello(Hello {
                    major: 2,
                    minor: 0,
                    flags: 0,
                    capabilities: 0,
                    machine_id: [9; 16],
                    machine_name: "Future".into(),
                    app_version: "9.0.0".into(),
                }))
                .await
                .unwrap();
            let _ = reader.read_message().await; // our Hello
            reader.read_message().await.unwrap() // our Goodbye
        });
        let mut reader = FrameReader::new(a_r, Channel::Control);
        let mut writer = FrameWriter::new(a_w);
        let result = handshake(
            Role::Initiator,
            &mut reader,
            &mut writer,
            &host.local,
            &facts,
            &host.trust,
            |_| async { true },
        )
        .await;
        assert!(
            matches!(
                result,
                Err(HandshakeError::Incompatible { peer_major: 2, .. })
            ),
            "{result:?}"
        );
        let Message::Goodbye(g) = script.await.unwrap() else {
            panic!("expected Goodbye")
        };
        assert_eq!(g.reason, reason::INCOMPATIBLE_VERSION);
        assert_eq!(g.detail, "1.0");
    }

    #[tokio::test(start_paused = true)]
    async fn silent_peer_times_out_after_five_seconds() {
        let (a_io, _keep_open) = duplex(4096);
        let (a_r, a_w) = tokio::io::split(a_io);
        let host = peer("Studio", true);
        let facts = TransportFacts {
            peer_spki_sha256: [0; 32],
            pairing_code: String::new(),
        };
        let mut reader = FrameReader::new(a_r, Channel::Control);
        let mut writer = FrameWriter::new(a_w);
        let result = handshake(
            Role::Acceptor,
            &mut reader,
            &mut writer,
            &host.local,
            &facts,
            &host.trust,
            |_| async { true },
        )
        .await;
        assert!(
            matches!(result, Err(HandshakeError::Link(LinkError::Timeout))),
            "{result:?}"
        );
    }
}
