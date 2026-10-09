// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The PortalSwitch Authors

//! Two complete daemons in one process, joined by real TLS on localhost, each on a fake
//! platform. This is the headless end-to-end check: captured input on the host appears as
//! injected input on the peer, and nothing stays pressed afterwards.

#![allow(clippy::unwrap_used)]

use std::future::Future;
use std::time::Duration;

use portal_core::input::{InputEvent, KeyAction, KeyEvent};
use portal_core::layout::Point;
use portal_core::peer::Inject;
use portal_net::link::LinkConfig;
use portal_net::{Identity, TrustStore};
use portal_platform::fake::{FakeCapture, FakeCaptureHandle, FakeInjector, FakeScreens};
use portald::config::Config;
use portald::daemon::{DaemonEvent, DaemonHandle, DaemonOptions, PairingPolicy, Platform, start};
use tokio::sync::broadcast;

struct Machine {
    handle: DaemonHandle,
    capture: FakeCaptureHandle,
    injector: FakeInjector,
    events: broadcast::Receiver<DaemonEvent>,
}

async fn machine(name: &str, is_host: bool, width: u32, height: u32) -> Machine {
    let (capture, capture_handle) = FakeCapture::new();
    let injector = FakeInjector::new();
    let config = Config {
        machine_name: name.to_owned(),
        is_host,
        ..Config::default()
    };
    let handle = start(
        DaemonOptions {
            config,
            config_path: None,
            identity: Identity::generate().unwrap(),
            trust: TrustStore::in_memory(),
            link: LinkConfig::default(),
            pairing: PairingPolicy::Auto(true),
            bind: "127.0.0.1:0".parse().unwrap(),
            ipc: None,
            discovery: false,
        },
        Platform {
            capture: Box::new(capture),
            injector: Box::new(injector.clone()),
            screens: Box::new(FakeScreens::single(width, height)),
        },
    )
    .await
    .unwrap();
    let events = handle.subscribe();
    Machine {
        handle,
        capture: capture_handle,
        injector,
        events,
    }
}

async fn eventually<F: Future<Output = bool>>(what: &str, mut check: impl FnMut() -> F) {
    for _ in 0..300 {
        if check().await {
            return;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    panic!("timed out waiting for: {what}");
}

async fn wait_event(
    rx: &mut broadcast::Receiver<DaemonEvent>,
    want: impl Fn(&DaemonEvent) -> bool,
) {
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            if want(&rx.recv().await.unwrap()) {
                return;
            }
        }
    })
    .await
    .expect("expected daemon event never arrived");
}

fn key(key_id: u32, action: KeyAction) -> InputEvent {
    InputEvent::Key(KeyEvent {
        action,
        repeat_count: 0,
        key_id,
        hid_usage: 0x0007_0004,
        codepoint: 97,
        named_key: 0,
        modifiers: 0,
        flags: 0,
    })
}

fn motion(dx: i32) -> InputEvent {
    InputEvent::Motion {
        dx,
        dy: 0,
        host_pos: Point { x: 1919, y: 540 },
    }
}

fn pressed(log: &[Inject], id: u32) -> bool {
    log.iter()
        .any(|i| matches!(i, Inject::KeyDown(k) if k.key_id == id))
}

fn released(log: &[Inject], id: u32) -> bool {
    log.iter()
        .any(|i| matches!(i, Inject::KeyUp(k) if k.key_id == id))
}

async fn connected_pair() -> (Machine, Machine) {
    let mut host = machine("Studio", true, 1920, 1080).await;
    let peer = machine("Laptop", false, 1440, 900).await;
    host.handle.connect(peer.handle.local_addr()).await.unwrap();
    let laptop_id = peer.handle.machine_id();
    wait_event(
        &mut host.events,
        |e| matches!(e, DaemonEvent::PeerConnected { machine, .. } if *machine == laptop_id),
    )
    .await;
    (host, peer)
}

#[tokio::test]
async fn typing_and_pointer_cross_to_the_peer_and_nothing_sticks() {
    let (host, peer) = connected_pair().await;
    let (studio_id, laptop_id) = (host.handle.machine_id(), peer.handle.machine_id());
    assert_eq!(host.handle.status().active, studio_id);

    // Push the pointer through the right edge: the event is swallowed, the laptop becomes active.
    assert!(host.capture.send(motion(1000)));
    eventually("laptop becomes active", || async {
        host.handle.status().active == laptop_id
    })
    .await;
    eventually("host cursor is parked", || async {
        host.capture.peer_active()
    })
    .await;

    // Typing and moving reach the laptop.
    assert!(host.capture.send(key(30, KeyAction::Down)));
    assert!(host.capture.send(InputEvent::Motion {
        dx: 12,
        dy: -3,
        host_pos: Point::default()
    }));
    eventually("laptop sees the key and the motion", || async {
        let log = peer.injector.log();
        pressed(&log, 30)
            && log
                .iter()
                .any(|i| matches!(i, Inject::MoveBy { dx: 12, dy: -3 }))
    })
    .await;

    // Come back to the host with the key still held: the laptop lets go on its own.
    assert!(host.capture.send(motion(-3000)));
    eventually("host is active again", || async {
        host.handle.status().active == studio_id
    })
    .await;
    eventually("laptop released the key", || async {
        released(&peer.injector.log(), 30)
    })
    .await;
    eventually("host cursor is visible again", || async {
        !host.capture.peer_active()
    })
    .await;
    // The host OS never saw the key go down, so its release stays hidden from applications.
    assert!(host.capture.send(key(30, KeyAction::Up)));
}

#[tokio::test]
async fn losing_the_peer_returns_the_pointer_and_releases_input() {
    let (host, peer) = connected_pair().await;
    let studio_id = host.handle.machine_id();
    let laptop_id = peer.handle.machine_id();

    assert!(host.capture.send(motion(1000)));
    eventually("laptop active", || async {
        host.handle.status().active == laptop_id
    })
    .await;
    assert!(host.capture.send(key(31, KeyAction::Down)));
    assert!(host.capture.send(InputEvent::Button {
        button: 1,
        down: true
    }));
    eventually("laptop holds key and button", || async {
        let log = peer.injector.log();
        pressed(&log, 31)
            && log.iter().any(|i| {
                matches!(
                    i,
                    Inject::Button {
                        button: 1,
                        down: true
                    }
                )
            })
    })
    .await;

    // The laptop's daemon goes away abruptly.
    peer.handle.shutdown();
    eventually("host takes the pointer back", || async {
        host.handle.status().active == studio_id
    })
    .await;
    eventually("laptop released everything", || async {
        let log = peer.injector.log();
        released(&log, 31)
            && log.iter().any(|i| {
                matches!(
                    i,
                    Inject::Button {
                        button: 1,
                        down: false
                    }
                )
            })
    })
    .await;
    assert_eq!(host.handle.status().peers, vec![]);
}

#[tokio::test]
async fn the_peer_can_be_the_one_who_dials() {
    let mut host = machine("Studio", true, 1920, 1080).await;
    let peer = machine("Laptop", false, 1440, 900).await;
    peer.handle.connect(host.handle.local_addr()).await.unwrap();
    let laptop_id = peer.handle.machine_id();
    wait_event(
        &mut host.events,
        |e| matches!(e, DaemonEvent::PeerConnected { machine, .. } if *machine == laptop_id),
    )
    .await;
    assert!(host.capture.send(motion(1000)));
    eventually("laptop active", || async {
        host.handle.status().active == laptop_id
    })
    .await;
}
