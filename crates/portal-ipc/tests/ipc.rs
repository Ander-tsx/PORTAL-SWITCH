// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The PortalSwitch Authors

//! Client and server over the real local transport with a stub service.

#![allow(clippy::unwrap_used)]

use std::sync::Arc;

use portal_ipc::proto::{ErrorBody, Request, code};
use portal_ipc::server::{BoxFuture, Service, serve};
use portal_ipc::transport::{Endpoint, Listener};
use portal_ipc::{Client, ClientError, Event, token};
use serde_json::{Value, json};
use tokio::sync::{broadcast, watch};

struct Stub {
    events: broadcast::Sender<Event>,
}

impl Service for Stub {
    fn call(&self, request: Request) -> BoxFuture<'_, Result<Value, ErrorBody>> {
        Box::pin(async move {
            match request {
                Request::Status => Ok(json!({
                    "machine": "aa", "is_host": true, "active": "aa", "locked": false,
                    "peers": [{"machine": "bb", "name": "Laptop"}]
                })),
                Request::Jump { machine } if machine == "bb" => Ok(Value::Null),
                Request::Jump { .. } => Err(ErrorBody::new(code::NOT_FOUND, "no such machine")),
                _ => Err(ErrorBody::new(code::FAILED, "unsupported in stub")),
            }
        })
    }

    fn events(&self) -> broadcast::Receiver<Event> {
        self.events.subscribe()
    }

    fn daemon_version(&self) -> String {
        "test".into()
    }
}

struct Fixture {
    endpoint: Endpoint,
    token: String,
    events: broadcast::Sender<Event>,
    _stop: watch::Sender<bool>,
    dir: std::path::PathBuf,
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

async fn fixture(name: &str) -> Fixture {
    let dir = std::env::temp_dir().join(format!("psipc-{}-{name}", std::process::id()));
    let endpoint = Endpoint::in_dir(&dir);
    let token = token::load_or_create(&dir.join("ipc.token")).unwrap();
    let listener = Listener::bind(&endpoint).await.unwrap();
    let (events, _) = broadcast::channel(16);
    let (stop_tx, stop_rx) = watch::channel(false);
    tokio::spawn(serve(
        listener,
        Arc::new(Stub {
            events: events.clone(),
        }),
        token.clone(),
        stop_rx,
    ));
    Fixture {
        endpoint,
        token,
        events,
        _stop: stop_tx,
        dir,
    }
}

#[tokio::test]
async fn typed_calls_and_remote_errors() {
    let f = fixture("calls").await;
    let client = Client::connect(&f.endpoint, &f.token).await.unwrap();
    assert_eq!(client.hello.daemon_version, "test");
    let status = client.status().await.unwrap();
    assert!(status.is_host);
    assert_eq!(status.peers[0].name, "Laptop");

    client
        .call(Request::Jump {
            machine: "bb".into(),
        })
        .await
        .unwrap();
    let err = client
        .call(Request::Jump {
            machine: "zz".into(),
        })
        .await
        .unwrap_err();
    assert!(
        matches!(err, ClientError::Remote(ref e) if e.code == code::NOT_FOUND),
        "{err:?}"
    );
}

#[tokio::test]
async fn concurrent_requests_get_their_own_replies() {
    let f = fixture("concurrent").await;
    let client = Arc::new(Client::connect(&f.endpoint, &f.token).await.unwrap());
    let mut tasks = Vec::new();
    for i in 0..50 {
        let client = client.clone();
        tasks.push(tokio::spawn(async move {
            let machine = if i % 2 == 0 { "bb" } else { "zz" };
            client
                .call(Request::Jump {
                    machine: machine.into(),
                })
                .await
                .is_ok()
                == (i % 2 == 0)
        }));
    }
    for task in tasks {
        assert!(task.await.unwrap());
    }
}

#[tokio::test]
async fn events_arrive_after_subscribe() {
    let f = fixture("events").await;
    let client = Client::connect(&f.endpoint, &f.token).await.unwrap();
    let mut events = client.subscribe().await.unwrap();
    f.events
        .send(Event::ActiveChanged {
            machine: "bb".into(),
        })
        .unwrap();
    let got = tokio::time::timeout(std::time::Duration::from_secs(2), events.recv())
        .await
        .unwrap();
    assert_eq!(
        got,
        Some(Event::ActiveChanged {
            machine: "bb".into()
        })
    );
}

#[tokio::test]
async fn a_wrong_token_is_refused() {
    let f = fixture("auth").await;
    let err = Client::connect(&f.endpoint, "not-the-token")
        .await
        .unwrap_err();
    assert!(
        matches!(err, ClientError::Remote(ref e) if e.code == code::UNAUTHORIZED),
        "{err:?}"
    );
}

#[tokio::test]
async fn garbage_lines_get_an_error_reply_and_the_connection_survives() {
    use tokio::io::{AsyncBufReadExt as _, AsyncWriteExt as _, BufReader};
    let f = fixture("garbage").await;
    let io = portal_ipc::transport::connect(&f.endpoint).await.unwrap();
    let (r, mut w) = tokio::io::split(io);
    let mut r = BufReader::new(r);
    w.write_all(format!("{{\"version\":1,\"token\":\"{}\"}}\n", f.token).as_bytes())
        .await
        .unwrap();
    let mut line = String::new();
    r.read_line(&mut line).await.unwrap(); // server hello
    w.write_all(b"this is not json\n").await.unwrap();
    w.write_all(b"{\"id\":5,\"method\":\"explode\"}\n")
        .await
        .unwrap();
    w.write_all(b"{\"id\":6,\"method\":\"status\"}\n")
        .await
        .unwrap();
    let mut replies = Vec::new();
    for _ in 0..3 {
        line.clear();
        r.read_line(&mut line).await.unwrap();
        replies.push(serde_json::from_str::<Value>(&line).unwrap());
    }
    assert_eq!(replies[0]["ok"], false);
    assert_eq!(replies[1]["id"], 5);
    assert_eq!(replies[1]["error"]["code"], "bad_request");
    assert_eq!(replies[2]["ok"], true);
}

#[cfg(unix)]
#[tokio::test]
async fn a_second_daemon_cannot_steal_a_live_socket_but_a_stale_one_is_replaced() {
    let f = fixture("stale").await;
    assert!(
        Listener::bind(&f.endpoint).await.is_err(),
        "live socket must not be stolen"
    );

    let dir = std::env::temp_dir().join(format!("psipc-{}-stale2", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let endpoint = Endpoint::in_dir(&dir);
    drop(std::os::unix::net::UnixListener::bind(&endpoint.path).unwrap()); // leaves a dead file
    assert!(endpoint.path.exists());
    Listener::bind(&endpoint).await.unwrap();
    std::fs::remove_dir_all(dir).unwrap();
}
