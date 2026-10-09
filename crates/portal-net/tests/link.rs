// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The PortalSwitch Authors

//! Link behaviour over in-memory pipes: delivery, liveness, shutdown and protocol rules.

#![allow(clippy::unwrap_used, clippy::cast_possible_truncation)]

use std::time::Duration;

use portal_core::layout::MachineId;
use portal_net::link::{BoxRead, BoxWrite, CloseReason, Link, LinkConfig, LinkEvent, Streams};
use portal_net::session::{Established, RemoteInfo};
use portal_proto::{Message, MouseMotion, PairConfirm, Ping, ReleaseAll, goodbye};
use tokio::io::{duplex, split};

fn established(remote_is_host: bool) -> Established {
    Established {
        remote: RemoteInfo {
            machine_id: MachineId([7; 16]),
            machine_name: "remote".into(),
            app_version: "0.1.0".into(),
            capabilities: 0,
            is_host: remote_is_host,
            minor: 0,
            spki_sha256: [0; 32],
        },
        session_id: [1; 16],
        bulk_token: [2; 16],
        newly_paired: false,
    }
}

fn pipe(buffer: usize) -> (Streams, Streams) {
    let (a, b) = duplex(buffer);
    let (ar, aw) = split(a);
    let (br, bw) = split(b);
    let ar: BoxRead = Box::new(ar);
    let aw: BoxWrite = Box::new(aw);
    let br: BoxRead = Box::new(br);
    let bw: BoxWrite = Box::new(bw);
    (Streams::new(ar, aw), Streams::new(br, bw))
}

/// A host link and a peer link wired together.
fn pair(config: LinkConfig, buffer: usize) -> (Link, Link) {
    let (host_control, peer_control) = pipe(buffer);
    let (host_bulk, peer_bulk) = pipe(buffer);
    let host = Link::start(established(false), host_control, host_bulk, config);
    let peer = Link::start(established(true), peer_control, peer_bulk, config);
    (host, peer)
}

async fn next_control(link: &mut Link) -> Message {
    loop {
        match link.recv().await.expect("link ended") {
            LinkEvent::Control(m) => return m,
            LinkEvent::Bulk(_) => {}
            LinkEvent::Closed(reason) => panic!("closed: {reason:?}"),
        }
    }
}

async fn closed(link: &mut Link) -> CloseReason {
    loop {
        if let LinkEvent::Closed(reason) = link.recv().await.expect("no Closed event") {
            return reason;
        }
    }
}

#[tokio::test]
async fn control_and_bulk_messages_are_delivered() {
    let (host, mut peer) = pair(LinkConfig::default(), 64 * 1024);
    host.control()
        .send(Message::ReleaseAll(ReleaseAll { enter_seq: 5 }))
        .unwrap();
    assert_eq!(
        next_control(&mut peer).await,
        Message::ReleaseAll(ReleaseAll { enter_seq: 5 })
    );

    host.bulk()
        .send(&Message::TransferEnd(portal_proto::TransferEnd {
            transfer_id: 9,
        }))
        .await
        .unwrap();
    assert_eq!(
        peer.recv().await.unwrap(),
        LinkEvent::Bulk(Message::TransferEnd(portal_proto::TransferEnd {
            transfer_id: 9
        }))
    );
}

#[tokio::test]
async fn orderly_close_is_seen_on_both_ends() {
    let (host, mut peer) = pair(LinkConfig::default(), 4096);
    let mut host = host;
    host.control().close();
    assert_eq!(closed(&mut host).await, CloseReason::Local);
    assert_eq!(
        closed(&mut peer).await,
        CloseReason::Peer {
            reason: goodbye::NORMAL,
            detail: String::new()
        }
    );
}

#[tokio::test]
async fn a_peer_sending_input_to_the_host_is_a_protocol_violation() {
    let (mut host, peer) = pair(LinkConfig::default(), 4096);
    peer.control()
        .send(Message::ReleaseAll(ReleaseAll { enter_seq: 1 }))
        .unwrap();
    assert!(matches!(closed(&mut host).await, CloseReason::Protocol(_)));
}

#[tokio::test(start_paused = true)]
async fn healthy_links_stay_up_through_pings() {
    let (host, mut peer) = pair(LinkConfig::default(), 4096);
    tokio::time::sleep(Duration::from_secs(30)).await;
    host.control()
        .send(Message::PairConfirm(PairConfirm { accepted: true }))
        .unwrap();
    assert_eq!(
        next_control(&mut peer).await,
        Message::PairConfirm(PairConfirm { accepted: true })
    );
}

#[tokio::test(start_paused = true)]
async fn a_silent_peer_is_declared_dead_after_three_intervals() {
    // The far end exists (so writes succeed) but never answers.
    let (control, _silent_control) = pipe(4096);
    let (bulk, _silent_bulk) = pipe(4096);
    let mut link = Link::start(established(false), control, bulk, LinkConfig::default());
    let started = tokio::time::Instant::now();
    assert_eq!(closed(&mut link).await, CloseReason::Timeout);
    let waited = started.elapsed();
    assert!(
        waited >= Duration::from_secs(3) && waited <= Duration::from_secs(5),
        "{waited:?}"
    );
}

#[tokio::test]
async fn motion_is_coalesced_while_the_transport_is_stalled_and_nothing_is_lost() {
    // A tiny pipe that nobody reads fills up at once; the sender keeps going without blocking.
    let (host, mut peer) = pair(
        LinkConfig {
            stall_after: Duration::from_secs(60),
            ..LinkConfig::default()
        },
        256,
    );
    let mut expected = (0i64, 0i64);
    for i in 0..20_000i64 {
        let (dx, dy) = ((i % 7) as i16 - 3, (i % 5) as i16 - 2);
        expected.0 += i64::from(dx);
        expected.1 += i64::from(dy);
        host.control()
            .send(Message::MouseMotion(MouseMotion {
                enter_seq: 1,
                t_us: 0,
                dx,
                dy,
            }))
            .expect("coalescing keeps the queue from filling");
    }
    // A key press after the motion must arrive after all of it.
    host.control()
        .send(Message::ReleaseAll(ReleaseAll { enter_seq: 1 }))
        .unwrap();
    let mut got = (0i64, 0i64);
    loop {
        match next_control(&mut peer).await {
            Message::MouseMotion(m) => {
                got.0 += i64::from(m.dx);
                got.1 += i64::from(m.dy);
            }
            Message::ReleaseAll(_) => break,
            Message::Ping(Ping { .. }) | Message::Pong(_) => {}
            other => panic!("unexpected {other:?}"),
        }
    }
    assert_eq!(got, expected, "total motion must be conserved");
}
