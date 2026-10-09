// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The PortalSwitch Authors

//! The daemon side of the local API.

use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;
use std::time::Duration;

use serde_json::Value;
use tokio::io::{
    AsyncBufReadExt as _, AsyncRead, AsyncReadExt as _, AsyncWriteExt as _, BufReader,
};
use tokio::sync::{broadcast, mpsc, watch};
use tokio::time::timeout;

use crate::proto::{
    ClientHello, ErrorBody, Event, PROTOCOL_VERSION, Reply, Request, RequestFrame, ServerHello,
    code,
};
use crate::token;
use crate::transport::{BoxIo, Listener};

/// Longest accepted line (a configuration document fits comfortably).
pub const MAX_LINE: u64 = 1024 * 1024;
const HELLO_TIMEOUT: Duration = Duration::from_secs(5);

/// A boxed future, so [`Service`] stays object safe.
pub type BoxFuture<'a, T> = Pin<Box<dyn Future<Output = T> + Send + 'a>>;

/// What the daemon implements.
pub trait Service: Send + Sync + 'static {
    /// Handle one request. `Subscribe` is handled by the server and never reaches this.
    fn call(&self, request: Request) -> BoxFuture<'_, Result<Value, ErrorBody>>;
    /// Stream of events for subscribers.
    fn events(&self) -> broadcast::Receiver<Event>;
    /// Reported in the hello.
    fn daemon_version(&self) -> String;
}

/// Accept clients until `stop` fires.
pub async fn serve(
    mut listener: Listener,
    service: Arc<dyn Service>,
    token: String,
    mut stop: watch::Receiver<bool>,
) {
    loop {
        tokio::select! {
            accepted = listener.accept() => {
                let Ok(io) = accepted else { continue };
                tokio::spawn(connection(io, service.clone(), token.clone()));
            }
            _ = stop.changed() => return,
        }
    }
}

/// Read one `\n`-terminated line of at most [`MAX_LINE`] bytes.
pub(crate) async fn read_line<R: AsyncRead + Unpin>(
    reader: &mut BufReader<R>,
    limit: u64,
) -> std::io::Result<Option<String>> {
    let mut buf = Vec::new();
    let n = reader.take(limit).read_until(b'\n', &mut buf).await?;
    if n == 0 {
        return Ok(None);
    }
    if buf.last() != Some(&b'\n') {
        if u64::try_from(n).unwrap_or(u64::MAX) >= limit {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                "line too long",
            ));
        }
        return Ok(None); // closed mid-line
    }
    String::from_utf8(buf)
        .map(|s| Some(s.trim_end().to_owned()))
        .map_err(|_| std::io::Error::new(std::io::ErrorKind::InvalidData, "not UTF-8"))
}

async fn connection(io: BoxIo, service: Arc<dyn Service>, expected_token: String) {
    let (read, mut write) = tokio::io::split(io);
    let mut reader = BufReader::new(read);

    // Handshake.
    let Ok(Ok(Some(line))) = timeout(HELLO_TIMEOUT, read_line(&mut reader, 4096)).await else {
        return;
    };
    let refuse =
        |error: ErrorBody| serde_json::to_string(&Reply::err(0, error)).unwrap_or_default() + "\n";
    let Ok(hello) = serde_json::from_str::<ClientHello>(&line) else {
        let _ = write
            .write_all(refuse(ErrorBody::new(code::BAD_REQUEST, "expected hello")).as_bytes())
            .await;
        return;
    };
    if !token::matches(&expected_token, &hello.token) {
        let _ = write
            .write_all(refuse(ErrorBody::new(code::UNAUTHORIZED, "bad token")).as_bytes())
            .await;
        return;
    }
    if hello.version != PROTOCOL_VERSION {
        let message = format!("server speaks version {PROTOCOL_VERSION}");
        let _ = write
            .write_all(refuse(ErrorBody::new(code::VERSION, message)).as_bytes())
            .await;
        return;
    }
    let server_hello = ServerHello {
        version: PROTOCOL_VERSION,
        daemon_version: service.daemon_version(),
    };
    let Ok(text) = serde_json::to_string(&server_hello) else {
        return;
    };
    if write.write_all((text + "\n").as_bytes()).await.is_err() {
        return;
    }

    // One writer task so replies and events never interleave mid-line.
    let (lines, mut outgoing) = mpsc::channel::<String>(256);
    let writer = tokio::spawn(async move {
        while let Some(line) = outgoing.recv().await {
            if write.write_all(line.as_bytes()).await.is_err()
                || write.write_all(b"\n").await.is_err()
            {
                return;
            }
        }
    });

    let mut subscribed = false;
    while let Ok(Some(line)) = read_line(&mut reader, MAX_LINE).await {
        if line.is_empty() {
            continue;
        }
        let frame: RequestFrame = match serde_json::from_str(&line) {
            Ok(f) => f,
            Err(e) => {
                let id = serde_json::from_str::<Value>(&line)
                    .ok()
                    .and_then(|v| v.get("id").and_then(Value::as_u64))
                    .unwrap_or(0);
                send(
                    &lines,
                    &Reply::err(id, ErrorBody::new(code::BAD_REQUEST, e.to_string())),
                )
                .await;
                continue;
            }
        };
        let reply = if frame.request == Request::Subscribe {
            if !subscribed {
                subscribed = true;
                tokio::spawn(forward_events(service.events(), lines.clone()));
            }
            Reply::ok(frame.id, Value::Null)
        } else {
            match service.call(frame.request).await {
                Ok(result) => Reply::ok(frame.id, result),
                Err(error) => Reply::err(frame.id, error),
            }
        };
        send(&lines, &reply).await;
    }
    drop(lines);
    writer.abort();
}

async fn send(lines: &mpsc::Sender<String>, reply: &Reply) {
    if let Ok(text) = serde_json::to_string(reply) {
        let _ = lines.send(text).await;
    }
}

async fn forward_events(mut events: broadcast::Receiver<Event>, lines: mpsc::Sender<String>) {
    loop {
        match events.recv().await {
            Ok(event) => {
                let Ok(text) = serde_json::to_string(&event) else {
                    continue;
                };
                if lines.send(text).await.is_err() {
                    return;
                }
            }
            Err(broadcast::error::RecvError::Lagged(_)) => {}
            Err(broadcast::error::RecvError::Closed) => return,
        }
    }
}
