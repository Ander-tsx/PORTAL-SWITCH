// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The PortalSwitch Authors

//! Mutual TLS between two identities over an in-memory pipe, pairing codes and the trust store.

#![allow(clippy::unwrap_used)]

use portal_net::pairing::{code_from_exported, exporter_context, exporter_label};
use portal_net::tls::{ALPN_CONTROL, SERVER_NAME, client_config, peer_spki_sha256, server_config};
use portal_net::{Identity, TrustState, TrustStore};
use rustls::pki_types::ServerName;
use tokio::io::duplex;
use tokio_rustls::{TlsAcceptor, TlsConnector};

struct Session {
    peer_pin: [u8; 32],
    code: String,
}

/// Run one TLS handshake between `client` and `server` and derive each side's view.
async fn handshake(client: &Identity, server: &Identity) -> (Session, Session) {
    let (c_io, s_io) = duplex(64 * 1024);
    let acceptor = TlsAcceptor::from(server_config(server, ALPN_CONTROL).unwrap());
    let connector = TlsConnector::from(client_config(client, ALPN_CONTROL).unwrap());
    let name = ServerName::try_from(SERVER_NAME).unwrap();

    let (c, s) = tokio::join!(connector.connect(name, c_io), acceptor.accept(s_io));
    let (c, s) = (c.unwrap(), s.unwrap());

    let (_, c_conn) = c.get_ref();
    let (_, s_conn) = s.get_ref();
    let c_peer = peer_spki_sha256(c_conn.peer_certificates()).unwrap();
    let s_peer = peer_spki_sha256(s_conn.peer_certificates()).unwrap();
    let c_ctx = exporter_context(&client.spki_sha256(), &c_peer);
    let s_ctx = exporter_context(&server.spki_sha256(), &s_peer);
    let c_code: [u8; 8] = c_conn
        .export_keying_material([0u8; 8], exporter_label(), Some(&c_ctx))
        .unwrap();
    let s_code: [u8; 8] = s_conn
        .export_keying_material([0u8; 8], exporter_label(), Some(&s_ctx))
        .unwrap();
    (
        Session {
            peer_pin: c_peer,
            code: code_from_exported(c_code),
        },
        Session {
            peer_pin: s_peer,
            code: code_from_exported(s_code),
        },
    )
}

#[tokio::test]
async fn both_sides_learn_the_other_key_and_show_the_same_code() {
    let (a, b) = (Identity::generate().unwrap(), Identity::generate().unwrap());
    let (client_view, server_view) = handshake(&a, &b).await;
    assert_eq!(
        client_view.peer_pin,
        b.spki_sha256(),
        "client sees the server's pin"
    );
    assert_eq!(
        server_view.peer_pin,
        a.spki_sha256(),
        "server sees the client's pin"
    );
    assert_eq!(client_view.code, server_view.code);
    assert_eq!(client_view.code.len(), 6);
}

#[tokio::test]
async fn a_man_in_the_middle_produces_different_codes() {
    let (alice, bob, mallory) = (
        Identity::generate().unwrap(),
        Identity::generate().unwrap(),
        Identity::generate().unwrap(),
    );
    // Alice believes she talks to Bob; Mallory relays between two separate TLS sessions.
    let (alice_view, _mallory_server_side) = handshake(&alice, &mallory).await;
    let (_mallory_client_side, bob_view) = handshake(&mallory, &bob).await;
    assert_ne!(
        alice_view.code, bob_view.code,
        "the user would see two different codes"
    );
}

#[tokio::test]
async fn codes_differ_between_sessions() {
    let (a, b) = (Identity::generate().unwrap(), Identity::generate().unwrap());
    let first = handshake(&a, &b).await.0.code;
    let mut differing = false;
    for _ in 0..5 {
        differing |= handshake(&a, &b).await.0.code != first;
    }
    assert!(
        differing,
        "codes must be bound to the session, not only to the keys"
    );
}

#[test]
fn identity_survives_a_restart_and_keeps_its_pin() {
    let dir = std::env::temp_dir().join(format!("portalswitch-id-{}", std::process::id()));
    let path = dir.join("identity.json");
    let first = Identity::load_or_create(&path).unwrap();
    let second = Identity::load_or_create(&path).unwrap();
    assert_eq!(first.machine_id(), second.machine_id());
    assert_eq!(first.spki_sha256(), second.spki_sha256());
    assert_eq!(first.cert_der(), second.cert_der());

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        let mode = std::fs::metadata(&path).unwrap().permissions().mode();
        assert_eq!(
            mode & 0o077,
            0,
            "identity file must be private, mode {mode:o}"
        );
    }
    // A corrupt file is an error, never silently replaced by a new identity.
    std::fs::write(&path, "not json").unwrap();
    assert!(Identity::load_or_create(&path).is_err());
    std::fs::remove_dir_all(dir).unwrap();
}

#[test]
fn machine_ids_are_uuid_v4() {
    let id = Identity::generate().unwrap().machine_id().0;
    assert_eq!(id[6] >> 4, 4);
    assert_eq!(id[8] >> 6, 0b10);
}

#[test]
fn trust_store_pins_detects_key_changes_and_revokes() {
    let dir = std::env::temp_dir().join(format!("portalswitch-trust-{}", std::process::id()));
    let path = dir.join("trust.json");
    let peer = Identity::generate().unwrap();
    let impostor = Identity::generate().unwrap();

    let mut store = TrustStore::open(&path).unwrap();
    assert_eq!(
        store.check(peer.machine_id(), &peer.spki_sha256()),
        TrustState::Unknown
    );
    store
        .pin(peer.machine_id(), "Studio Mac", &peer.spki_sha256())
        .unwrap();
    assert_eq!(
        store.check(peer.machine_id(), &peer.spki_sha256()),
        TrustState::Trusted
    );
    assert_eq!(
        store.check(peer.machine_id(), &impostor.spki_sha256()),
        TrustState::KeyMismatch,
        "same machine ID with another key must be rejected"
    );
    assert!(store.is_key_pinned(&peer.spki_sha256()));

    // Persisted.
    let reopened = TrustStore::open(&path).unwrap();
    assert_eq!(
        reopened.check(peer.machine_id(), &peer.spki_sha256()),
        TrustState::Trusted
    );
    assert_eq!(reopened.peers().count(), 1);

    let mut store = reopened;
    assert!(store.revoke(peer.machine_id()).unwrap());
    assert!(!store.revoke(peer.machine_id()).unwrap());
    assert_eq!(
        store.check(peer.machine_id(), &peer.spki_sha256()),
        TrustState::Unknown
    );
    assert_eq!(TrustStore::open(&path).unwrap().peers().count(), 0);
    std::fs::remove_dir_all(dir).unwrap();
}
