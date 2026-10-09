// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The PortalSwitch Authors

//! A native window that hosts the PortalSwitch UI and bridges it to the daemon's local API.
//!
//! The page posts `{id, method, params}` through `window.ipc`; the shell forwards the call to
//! `portald` over the local socket and answers through `window.__portalReply`. Daemon events are
//! pushed through `window.__portalEvent`.

use std::borrow::Cow;
use std::sync::Arc;

use include_dir::{Dir, include_dir};
use portal_config::Paths;
use portal_ipc::{Client, ClientError, Endpoint, Request, token};
use serde_json::{Value, json};
use tao::dpi::LogicalSize;
use tao::event::{Event, WindowEvent};
use tao::event_loop::{ControlFlow, EventLoopBuilder, EventLoopProxy};
use tao::window::WindowBuilder;
use tokio::sync::Mutex;
use wry::WebViewBuilder;
use wry::http::{Response, header::CONTENT_TYPE};

static DIST: Dir = include_dir!("$CARGO_MANIFEST_DIR/../dist");

enum UserEvent {
    Script(String),
}

struct Daemon {
    endpoint: Endpoint,
    token_path: std::path::PathBuf,
    client: Mutex<Option<Arc<Client>>>,
    proxy: EventLoopProxy<UserEvent>,
}

impl Daemon {
    async fn client(&self) -> Result<Arc<Client>, String> {
        let mut slot = self.client.lock().await;
        if let Some(client) = slot.as_ref() {
            return Ok(client.clone());
        }
        let token = token::read(&self.token_path).map_err(|_| "PortalSwitch is not running".to_owned())?;
        let client = Client::connect(&self.endpoint, &token)
            .await
            .map_err(|_| "PortalSwitch is not running".to_owned())?;
        let client = Arc::new(client);
        // Forward daemon events to the page for as long as this connection lives.
        if let Ok(mut events) = client.subscribe().await {
            let proxy = self.proxy.clone();
            tokio::spawn(async move {
                while let Some(event) = events.recv().await {
                    if let Ok(json) = serde_json::to_string(&event) {
                        let _ = proxy.send_event(UserEvent::Script(format!(
                            "window.__portalEvent && window.__portalEvent({json})"
                        )));
                    }
                }
            });
        }
        *slot = Some(client.clone());
        Ok(client)
    }

    async fn call(&self, method: &str, params: Value) -> Result<Value, String> {
        if method == "app_version" {
            return Ok(json!(env!("CARGO_PKG_VERSION")));
        }
        let mut request = json!({ "method": method });
        if !params.is_null() {
            request["params"] = params;
        }
        let request: Request = serde_json::from_value(request).map_err(|e| e.to_string())?;
        let client = self.client().await?;
        match client.call(request).await {
            Ok(value) => Ok(value),
            Err(ClientError::Remote(e)) => Err(e.message),
            Err(e) => {
                // The connection broke: forget it so the next call reconnects.
                *self.client.lock().await = None;
                Err(e.to_string())
            }
        }
    }
}

fn mime(path: &str) -> &'static str {
    match path.rsplit('.').next().unwrap_or("") {
        "html" => "text/html; charset=utf-8",
        "js" => "text/javascript",
        "css" => "text/css",
        "svg" => "image/svg+xml",
        "png" => "image/png",
        _ => "application/octet-stream",
    }
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let runtime = tokio::runtime::Builder::new_multi_thread().worker_threads(2).enable_all().build()?;
    let event_loop = EventLoopBuilder::<UserEvent>::with_user_event().build();
    let proxy = event_loop.create_proxy();

    let paths = Paths::default_for_user();
    let daemon = Arc::new(Daemon {
        endpoint: Endpoint::in_dir(&paths.dir),
        token_path: paths.dir.join("ipc.token"),
        client: Mutex::new(None),
        proxy: proxy.clone(),
    });

    let window = WindowBuilder::new()
        .with_title("PortalSwitch")
        .with_inner_size(LogicalSize::new(940.0, 760.0))
        .with_min_inner_size(LogicalSize::new(560.0, 520.0))
        .build(&event_loop)?;

    let handle = runtime.handle().clone();
    let webview = WebViewBuilder::new()
        .with_initialization_script("window.__PORTAL_SHELL__ = true;")
        .with_custom_protocol("portal".into(), |_id, request| {
            let path = request.uri().path().trim_start_matches('/');
            let path = if path.is_empty() { "index.html" } else { path };
            match DIST.get_file(path) {
                Some(file) => Response::builder()
                    .header(CONTENT_TYPE, mime(path))
                    .body(Cow::Borrowed(file.contents()))
                    .unwrap_or_default(),
                None => Response::builder().status(404).body(Cow::Borrowed(&b"not found"[..])).unwrap_or_default(),
            }
        })
        .with_ipc_handler(move |request| {
            let Ok(message) = serde_json::from_str::<Value>(request.body()) else { return };
            let id = message["id"].as_u64().unwrap_or(0);
            let method = message["method"].as_str().unwrap_or_default().to_owned();
            let params = message["params"].clone();
            let (daemon, proxy) = (daemon.clone(), proxy.clone());
            handle.spawn(async move {
                let script = match daemon.call(&method, params).await {
                    Ok(value) => format!("window.__portalReply({id}, true, {value})"),
                    Err(message) => format!("window.__portalReply({id}, false, {})", json!(message)),
                };
                let _ = proxy.send_event(UserEvent::Script(script));
            });
        })
        .with_url("portal://localhost/index.html")
        .build(&window)?;

    event_loop.run(move |event, _, control_flow| {
        *control_flow = ControlFlow::Wait;
        match event {
            Event::UserEvent(UserEvent::Script(script)) => {
                let _ = webview.evaluate_script(&script);
            }
            Event::WindowEvent { event: WindowEvent::CloseRequested, .. } => *control_flow = ControlFlow::Exit,
            _ => {}
        }
    });
}
