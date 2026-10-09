// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The PortalSwitch Authors

//! The local API driving a real daemon: status, pairing prompts, dialing, jumping, config.

#![allow(clippy::unwrap_used)]

use std::time::Duration;

use portal_core::input::{InputEvent, KeyAction, KeyEvent};
use portal_ipc::proto::{Request, code};
use portal_ipc::{Client, ClientError, Endpoint, Event, token};
use portal_net::link::LinkConfig;
use portal_net::{Identity, TrustStore};
use portal_platform::fake::{FakeCapture, FakeCaptureHandle, FakeInjector, FakeScreens};
use portald::config::{Config, SwitchSettings, format_machine_id};
use portald::daemon::{DaemonHandle, DaemonOptions, IpcOptions, PairingPolicy, Platform, start};
use serde_json::json;

struct Machine {
    handle: DaemonHandle,
    client: Client,
    capture: FakeCaptureHandle,
    dir: Dir,
}

struct Dir(std::path::PathBuf);
impl Drop for Dir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

async fn machine(name: &str, is_host: bool, pairing: PairingPolicy) -> Machine {
    static NEXT: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
    let n = NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let dir = Dir(std::env::temp_dir().join(format!("psd-{}-{n}", std::process::id())));
    let token = token::load_or_create(&dir.0.join("ipc.token")).unwrap();
    let endpoint = Endpoint::in_dir(&dir.0);
    let (capture, capture_handle) = FakeCapture::new();
    let handle = start(
        DaemonOptions {
            config: Config {
                machine_name: name.into(),
                is_host,
                ..Config::default()
            },
            config_path: Some(dir.0.join("config.json")),
            identity: Identity::generate().unwrap(),
            trust: TrustStore::in_memory(),
            link: LinkConfig::default(),
            pairing,
            bind: "127.0.0.1:0".parse().unwrap(),
            discovery: false,
            ipc: Some(IpcOptions {
                endpoint: endpoint.clone(),
                token: token.clone(),
            }),
        },
        Platform {
            capture: Box::new(capture),
            injector: Box::new(FakeInjector::new()),
            screens: Box::new(FakeScreens::single(1920, 1080)),
        },
    )
    .await
    .unwrap();
    let client = Client::connect(&endpoint, &token).await.unwrap();
    Machine {
        handle,
        client,
        capture: capture_handle,
        dir,
    }
}

async fn eventually<F: std::future::Future<Output = bool>>(mut check: impl FnMut() -> F) {
    for _ in 0..200 {
        if check().await {
            return;
        }
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
    panic!("condition not reached");
}

#[tokio::test]
async fn status_pairing_dial_jump_and_events_through_the_api() {
    let host = machine("Studio", true, PairingPolicy::Ask).await;
    let peer = machine("Laptop", false, PairingPolicy::Ask).await;
    let mut host_events = host.client.subscribe().await.unwrap();
    let mut peer_events = peer.client.subscribe().await.unwrap();

    let status = host.client.status().await.unwrap();
    assert!(status.is_host);
    assert_eq!(status.machine, format_machine_id(host.handle.machine_id()));
    assert_eq!(status.active, status.machine);

    // Dial from the host; both UIs get the pairing prompt and the user accepts on each.
    let dialing = tokio::spawn({
        let client = host.handle.clone();
        let addr = peer.handle.local_addr();
        async move { client.connect(addr).await }
    });
    let host_prompt = wait(&mut host_events, |e| {
        matches!(e, Event::PairingRequested { .. })
    })
    .await;
    let peer_prompt = wait(&mut peer_events, |e| {
        matches!(e, Event::PairingRequested { .. })
    })
    .await;
    let host_code = accept(&host.client, host_prompt).await;
    let peer_code = accept(&peer.client, peer_prompt).await;
    assert_eq!(
        host_code, peer_code,
        "both users compare the same six digits"
    );
    dialing.await.unwrap().unwrap();
    wait(
        &mut host_events,
        |e| matches!(e, Event::PeerConnected { name, .. } if name == "Laptop"),
    )
    .await;

    assert_eq!(host.client.paired().await.unwrap().len(), 1);
    assert_eq!(host.client.status().await.unwrap().peers.len(), 1);

    // Jump there and back with the API (the "switch to machine X" hotkey).
    let laptop = format_machine_id(peer.handle.machine_id());
    host.client
        .call(Request::Jump {
            machine: laptop.clone(),
        })
        .await
        .unwrap();
    wait(
        &mut host_events,
        |e| matches!(e, Event::ActiveChanged { machine } if *machine == laptop),
    )
    .await;
    assert!(host.capture.send(InputEvent::Key(KeyEvent {
        action: KeyAction::Down,
        repeat_count: 0,
        key_id: 1,
        hid_usage: 0,
        codepoint: 97,
        named_key: 0,
        modifiers: 0,
        flags: 0,
    })));

    // Peers cannot switch screens.
    let err = peer
        .client
        .call(Request::Jump { machine: laptop })
        .await
        .unwrap_err();
    assert!(
        matches!(err, ClientError::Remote(ref e) if e.code == code::NOT_HOST || e.code == code::NOT_FOUND),
        "{err:?}"
    );
}

async fn accept(client: &Client, event: Event) -> String {
    let Event::PairingRequested { id, code, .. } = event else {
        panic!("expected a prompt")
    };
    client
        .call(Request::AnswerPairing { id, accept: true })
        .await
        .unwrap();
    code
}

async fn wait(rx: &mut tokio::sync::mpsc::Receiver<Event>, want: impl Fn(&Event) -> bool) -> Event {
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            let event = rx.recv().await.expect("event stream ended");
            if want(&event) {
                return event;
            }
        }
    })
    .await
    .expect("expected event never arrived")
}

#[tokio::test]
async fn configuration_can_be_read_updated_and_is_persisted() {
    let host = machine("Studio", true, PairingPolicy::Auto(true)).await;
    let current = host.client.call(Request::GetConfig).await.unwrap();
    assert_eq!(current["machine_name"], "Studio");
    // The daemon filled in our own screen.
    assert_eq!(current["layout"].as_array().unwrap().len(), 1);

    let mut changed: Config = serde_json::from_value(current).unwrap();
    changed.switch = SwitchSettings {
        delay_ms: 250,
        ..SwitchSettings::default()
    };
    changed.machine_name = "Studio Renamed".into();
    let result = host
        .client
        .call(Request::SetConfig {
            config: serde_json::to_value(&changed).unwrap(),
        })
        .await
        .unwrap();
    assert_eq!(result, json!({ "restart_required": false }));
    assert_eq!(host.handle.config().switch.delay_ms, 250);

    // Switching the role needs a restart; the daemon says so.
    changed.is_host = false;
    let result = host
        .client
        .call(Request::SetConfig {
            config: serde_json::to_value(&changed).unwrap(),
        })
        .await
        .unwrap();
    assert_eq!(result, json!({ "restart_required": true }));

    let on_disk = Config::load(&host.dir.0.join("config.json")).unwrap();
    assert_eq!(on_disk.machine_name, "Studio Renamed");

    // An invalid document is rejected with a clear error.
    let err = host
        .client
        .call(Request::SetConfig {
            config: json!({"layout": [{"machine_id": "nope", "x":0,"y":0,"width":1,"height":1}]}),
        })
        .await
        .unwrap_err();
    assert!(
        matches!(err, ClientError::Remote(ref e) if e.code == code::BAD_REQUEST),
        "{err:?}"
    );
}

#[tokio::test]
async fn shutdown_over_the_api_stops_the_daemon() {
    let host = machine("Studio", true, PairingPolicy::Auto(true)).await;
    host.client.call(Request::Shutdown).await.unwrap();
    // Capture is released; new events have nowhere to go.
    eventually(|| async {
        std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            host.capture.send(InputEvent::Wheel {
                dx: 0,
                dy: 1,
                flags: 0,
            })
        }))
        .is_err()
    })
    .await;
}
