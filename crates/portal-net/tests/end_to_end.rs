// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The PortalSwitch Authors

//! Two endpoints over real TCP + TLS on localhost: pair, move the pointer across the edge,
//! type, come back, and verify the peer is left with nothing pressed.

#![allow(clippy::unwrap_used, clippy::too_many_lines)]

use std::future::Future;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use portal_core::gate::SwitchConfig;
use portal_core::input::{InputEvent, KeyAction, KeyEvent};
use portal_core::layout::{Layout, Point, Rect, Screen};
use portal_core::peer::{Inject, PeerInput};
use portal_core::router::{Event, Router};
use portal_net::bridge::{apply_host_message, command_message};
use portal_net::link::{Link, LinkConfig, LinkEvent};
use portal_net::session::{LocalInfo, PairingPrompt};
use portal_net::tcp::{ConnectError, Endpoint};
use portal_net::{Identity, TrustStore};
use portal_proto::Message;
use tokio::net::TcpListener;
use tokio::sync::mpsc;

struct Node {
    endpoint: Endpoint,
    prompts: Arc<Mutex<Vec<PairingPrompt>>>,
    answer: Arc<AtomicBool>,
}

fn node(name: &str, is_host: bool, trust: TrustStore) -> Node {
    let prompts = Arc::new(Mutex::new(Vec::new()));
    let answer = Arc::new(AtomicBool::new(true));
    let local = LocalInfo {
        identity: Identity::generate().unwrap(),
        machine_name: name.to_owned(),
        app_version: "0.1.0".to_owned(),
        capabilities: 0xff,
        is_host,
    };
    let (p, a) = (prompts.clone(), answer.clone());
    let endpoint = Endpoint::new(local, trust, LinkConfig::default(), move |prompt| {
        p.lock().unwrap().push(prompt);
        let answer = a.load(Ordering::SeqCst);
        async move { answer }
    })
    .unwrap();
    Node {
        endpoint,
        prompts,
        answer,
    }
}

async fn eventually<F: Future<Output = bool>>(mut check: impl FnMut() -> F) {
    for _ in 0..200 {
        if check().await {
            return;
        }
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
    panic!("condition not reached in time");
}

fn key(key_id: u32, action: KeyAction) -> KeyEvent {
    KeyEvent {
        action,
        repeat_count: 0,
        key_id,
        hid_usage: 0x0007_0004,
        codepoint: 97,
        named_key: 0,
        modifiers: 0,
        flags: 0,
    }
}

/// Connect `laptop` (initiator) to `studio` (acceptor); returns both links.
async fn link_up(studio: &Node, laptop: &Node) -> (Link, Link) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let mut incoming = studio.endpoint.serve(listener);
    let (laptop_link, studio_link) = tokio::join!(laptop.endpoint.connect(addr), incoming.recv());
    (
        studio_link.expect("studio accepted a link"),
        laptop_link.unwrap(),
    )
}

#[tokio::test]
async fn pair_route_input_and_leave_without_stuck_keys() {
    let studio = node("Studio", true, TrustStore::in_memory());
    let laptop = node("Laptop", false, TrustStore::in_memory());
    let (mut studio_link, mut laptop_link) = link_up(&studio, &laptop).await;

    // Both users saw the same code, once.
    let studio_prompt = studio.prompts.lock().unwrap().clone();
    let laptop_prompt = laptop.prompts.lock().unwrap().clone();
    assert_eq!((studio_prompt.len(), laptop_prompt.len()), (1, 1));
    assert_eq!(studio_prompt[0].code, laptop_prompt[0].code);
    assert_eq!(studio_prompt[0].remote_name, "Laptop");
    assert_eq!(laptop_prompt[0].remote_name, "Studio");

    // The laptop applies whatever the host sends to its stuck-input guard.
    let injected = Arc::new(Mutex::new(Vec::<Inject>::new()));
    {
        let injected = injected.clone();
        let reply = laptop_link.control();
        tokio::spawn(async move {
            let mut input = PeerInput::new();
            while let Some(event) = laptop_link.recv().await {
                if let LinkEvent::Control(message) = event {
                    let mut out = Vec::new();
                    if let Some(answer) = apply_host_message(&mut input, &message, &mut out) {
                        reply.send(answer).unwrap();
                    }
                    injected.lock().unwrap().extend(out);
                }
            }
        });
    }
    // The studio forwards acknowledgements to the router.
    let (ack_tx, mut acks) = mpsc::unbounded_channel();
    let to_peer = studio_link.control();
    tokio::spawn(async move {
        while let Some(event) = studio_link.recv().await {
            if let LinkEvent::Control(Message::LeaveAck(ack)) = event {
                let _ = ack_tx.send(ack);
            }
        }
    });

    let studio_id = studio.endpoint.identity().machine_id();
    let laptop_id = laptop.endpoint.identity().machine_id();
    let layout = Layout::new(vec![
        Screen {
            id: studio_id,
            rect: Rect::new(0, 0, 1920, 1080),
            connected: true,
        },
        Screen {
            id: laptop_id,
            rect: Rect::new(1920, 0, 1440, 900),
            connected: false,
        },
    ])
    .unwrap();
    let mut router = Router::new(studio_id, layout, SwitchConfig::default());
    let mut now = 1_000u64;

    let feed = |router: &mut Router, now: u64, event: Event| -> bool {
        let mut commands = Vec::new();
        let swallow = router.handle(now, event, &mut commands);
        for command in &commands {
            if let Some((to, message)) = command_message(command, 0) {
                assert_eq!(to, laptop_id);
                to_peer.send(message).unwrap();
            }
        }
        swallow
    };

    feed(&mut router, now, Event::PeerConnected(laptop_id));
    assert!(feed(
        &mut router,
        now,
        Event::Input(InputEvent::Motion {
            dx: 1000,
            dy: 0,
            host_pos: Point { x: 1919, y: 540 }
        })
    ));
    assert_eq!(router.active(), laptop_id);

    now += 10_000;
    feed(
        &mut router,
        now,
        Event::Input(InputEvent::Key(key(30, KeyAction::Down))),
    );
    feed(
        &mut router,
        now,
        Event::Input(InputEvent::Motion {
            dx: 5,
            dy: 0,
            host_pos: Point::default(),
        }),
    );
    eventually(|| async {
        injected
            .lock()
            .unwrap()
            .iter()
            .any(|i| matches!(i, Inject::KeyDown(k) if k.key_id == 30))
    })
    .await;

    // Push back across the left edge of the laptop with the key still held.
    now += 10_000;
    feed(
        &mut router,
        now,
        Event::Input(InputEvent::Motion {
            dx: -500,
            dy: 0,
            host_pos: Point::default(),
        }),
    );
    assert!(router.is_switching(), "waiting for the laptop's LeaveAck");
    let ack = tokio::time::timeout(Duration::from_secs(5), acks.recv())
        .await
        .unwrap()
        .unwrap();
    assert!(ack.ok);
    now += 1_000;
    feed(
        &mut router,
        now,
        Event::LeaveAck {
            from: laptop_id,
            enter_seq: ack.enter_seq,
            ok: ack.ok,
        },
    );
    assert_eq!(router.active(), studio_id);

    // The laptop released the held key on its own, before the user lifted it.
    eventually(|| async {
        injected
            .lock()
            .unwrap()
            .iter()
            .any(|i| matches!(i, Inject::KeyUp(k) if k.key_id == 30))
    })
    .await;
    // Lifting the key now is swallowed: the host OS never saw it go down.
    assert!(feed(
        &mut router,
        now,
        Event::Input(InputEvent::Key(key(30, KeyAction::Up)))
    ));
}

#[tokio::test]
async fn second_connection_needs_no_pairing_and_duplicates_are_refused() {
    let studio = node("Studio", true, TrustStore::in_memory());
    let laptop = node("Laptop", false, TrustStore::in_memory());
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let mut incoming = studio.endpoint.serve(listener);

    let (first, accepted) = tokio::join!(laptop.endpoint.connect(addr), incoming.recv());
    let first = first.unwrap();
    let accepted = accepted.unwrap();

    // A second simultaneous link from the same machine is refused.
    let duplicate = laptop.endpoint.connect(addr).await;
    assert!(
        matches!(duplicate, Err(ConnectError::Duplicate)),
        "{duplicate:?}"
    );

    // After the first ends, the machine can come back, already trusted.
    drop(first);
    drop(accepted);
    tokio::time::sleep(Duration::from_millis(100)).await;
    let (again, accepted_again) = tokio::join!(laptop.endpoint.connect(addr), incoming.recv());
    again.unwrap();
    accepted_again.unwrap();
    assert_eq!(
        studio.prompts.lock().unwrap().len(),
        1,
        "pairing happens only once"
    );
    assert_eq!(laptop.prompts.lock().unwrap().len(), 1);
    assert_eq!(studio.endpoint.with_trust(|t| t.peers().count()), 1);
}

#[tokio::test]
async fn declining_the_code_prevents_the_link_and_pins_nothing() {
    let studio = node("Studio", true, TrustStore::in_memory());
    let laptop = node("Laptop", false, TrustStore::in_memory());
    studio.answer.store(false, Ordering::SeqCst);
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let mut incoming = studio.endpoint.serve(listener);

    let result = laptop.endpoint.connect(addr).await;
    assert!(result.is_err(), "pairing was declined on the studio");
    assert_eq!(studio.endpoint.with_trust(|t| t.peers().count()), 0);
    assert_eq!(laptop.endpoint.with_trust(|t| t.peers().count()), 0);
    assert!(
        tokio::time::timeout(Duration::from_millis(300), incoming.recv())
            .await
            .is_err(),
        "no link may be produced"
    );
}

#[tokio::test]
async fn a_pinned_machine_presenting_another_key_is_rejected() {
    let laptop = node("Laptop", false, TrustStore::in_memory());
    // The studio remembers this machine ID under a different key.
    let mut trust = TrustStore::in_memory();
    let other = Identity::generate().unwrap();
    trust
        .pin(
            laptop.endpoint.identity().machine_id(),
            "Laptop",
            &other.spki_sha256(),
        )
        .unwrap();
    let studio = node("Studio", true, trust);

    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let mut incoming = studio.endpoint.serve(listener);
    let result = laptop.endpoint.connect(addr).await;
    assert!(result.is_err());
    assert_eq!(
        studio.prompts.lock().unwrap().len(),
        0,
        "no pairing prompt for an impostor"
    );
    assert!(
        tokio::time::timeout(Duration::from_millis(300), incoming.recv())
            .await
            .is_err()
    );
}
