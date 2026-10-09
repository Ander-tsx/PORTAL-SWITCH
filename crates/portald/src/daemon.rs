// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The PortalSwitch Authors

//! The daemon proper: wires configuration, the TLS endpoint, the routing state machine and the
//! platform backend together.
//!
//! Threading model: the capture backend calls into the router synchronously (it needs the
//! swallow verdict immediately). Every router [`Command`] is pushed, in order and while the
//! router lock is held, onto one queue that a dispatcher task executes: protocol messages go to
//! links, local effects go to the platform backend.

use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::Duration;

use portal_core::gate::Micros;
use portal_core::input::KeyAction;
use portal_core::layout::{Layout, MachineId};
use portal_core::peer::{Inject, PeerInput};
use portal_core::router::{Command, Event, Router};
use portal_net::bridge::{apply_host_message, command_message};
use portal_net::link::{BulkSender, ControlSender, Link, LinkConfig, LinkEvent};
use portal_net::session::{LocalInfo, PairingPrompt};
use portal_net::tcp::{ConnectError, Endpoint};
use portal_net::{Identity, TrustStore};
use portal_platform::{InputCapture, InputInjector, Screens, bounding_box};
use portal_proto::{Message, Monitor, ScreenInfo, ScreenInfoAck, cap};
use tokio::net::TcpListener;
use tokio::sync::{Notify, broadcast, mpsc, oneshot, watch};

use crate::config::{Config, ConfigError, ScreenConfig, format_machine_id, parse_machine_id};

fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(std::sync::PoisonError::into_inner)
}

/// What the daemon needs from the operating system.
pub struct Platform {
    pub capture: Box<dyn InputCapture>,
    pub injector: Box<dyn InputInjector>,
    pub screens: Box<dyn Screens>,
}

impl std::fmt::Debug for Platform {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Platform").finish_non_exhaustive()
    }
}

/// How pairing prompts are answered.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PairingPolicy {
    /// Emit [`DaemonEvent::PairingRequested`] and wait for [`DaemonHandle::answer_pairing`].
    Ask,
    /// Answer automatically (tests, unattended setups).
    Auto(bool),
}

/// Start-up parameters.
#[derive(Debug)]
pub struct DaemonOptions {
    pub config: Config,
    /// Where to persist configuration changes made by the daemon; `None` keeps them in memory.
    pub config_path: Option<std::path::PathBuf>,
    pub identity: Identity,
    pub trust: TrustStore,
    pub link: LinkConfig,
    pub pairing: PairingPolicy,
    /// Address to listen on; port 0 picks a free one.
    pub bind: SocketAddr,
    /// Serve the local UI/CLI API; `None` disables it.
    pub ipc: Option<IpcOptions>,
}

/// Where and how the local API is served.
#[derive(Debug, Clone)]
pub struct IpcOptions {
    pub endpoint: portal_ipc::Endpoint,
    /// Token clients must present; see [`portal_ipc::token`].
    pub token: String,
}

/// Notifications for the UI / IPC layer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DaemonEvent {
    PairingRequested {
        id: u64,
        remote_name: String,
        code: String,
    },
    PeerConnected {
        machine: MachineId,
        name: String,
    },
    PeerDisconnected {
        machine: MachineId,
    },
    ActiveChanged {
        machine: MachineId,
    },
    /// Dialing a configured peer failed for good (e.g. pairing was declined).
    DialFailed {
        address: String,
        reason: String,
    },
}

/// Snapshot of the daemon's state.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Status {
    pub machine: MachineId,
    pub is_host: bool,
    pub active: MachineId,
    pub locked: bool,
    pub peers: Vec<(MachineId, String)>,
}

/// Why the daemon could not start.
#[derive(Debug, thiserror::Error)]
pub enum DaemonError {
    #[error(transparent)]
    Config(#[from] ConfigError),
    #[error(transparent)]
    Net(#[from] portal_net::Error),
    #[error(transparent)]
    Platform(#[from] portal_platform::PlatformError),
    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),
    #[error("this computer's display size is unknown")]
    NoDisplay,
}

struct PairingBroker {
    policy: PairingPolicy,
    events: broadcast::Sender<DaemonEvent>,
    pending: Mutex<HashMap<u64, oneshot::Sender<bool>>>,
    next: AtomicU64,
}

impl PairingBroker {
    async fn ask(&self, prompt: PairingPrompt) -> bool {
        match self.policy {
            PairingPolicy::Auto(answer) => answer,
            PairingPolicy::Ask => {
                let id = self.next.fetch_add(1, Ordering::Relaxed);
                let (tx, rx) = oneshot::channel();
                lock(&self.pending).insert(id, tx);
                let _ = self.events.send(DaemonEvent::PairingRequested {
                    id,
                    remote_name: prompt.remote_name,
                    code: prompt.code,
                });
                rx.await.unwrap_or(false)
            }
        }
    }

    fn answer(&self, id: u64, accept: bool) {
        if let Some(tx) = lock(&self.pending).remove(&id) {
            let _ = tx.send(accept);
        }
    }
}

struct PeerLink {
    name: String,
    control: ControlSender,
    #[allow(dead_code)] // used by the clipboard/file features that attach to this handle
    bulk: BulkSender,
    ready: bool,
}

struct Shared {
    me: MachineId,
    is_host: bool,
    started: tokio::time::Instant,
    router: Mutex<Router>,
    out: mpsc::UnboundedSender<Command>,
    tick: Notify,
    links: Mutex<HashMap<MachineId, PeerLink>>,
    config: Mutex<Config>,
    config_path: Option<std::path::PathBuf>,
    injector: Mutex<Box<dyn InputInjector>>,
    capture: Mutex<Box<dyn InputCapture>>,
    screens: Box<dyn Screens>,
    events: broadcast::Sender<DaemonEvent>,
    broker: Arc<PairingBroker>,
    shutdown: watch::Sender<bool>,
    stopped: AtomicBool,
}

impl Shared {
    fn now_us(&self) -> Micros {
        u64::try_from(self.started.elapsed().as_micros()).unwrap_or(u64::MAX)
    }

    /// Feed the router; its commands are queued in order while the lock is held.
    fn route(&self, event: Event) -> bool {
        let mut commands = Vec::new();
        let swallow = {
            let mut router = lock(&self.router);
            let swallow = router.handle(self.now_us(), event, &mut commands);
            for command in commands {
                let _ = self.out.send(command);
            }
            swallow
        };
        self.tick.notify_one();
        swallow
    }

    fn inject(&self, action: Inject) {
        let _ = lock(&self.injector).inject(action);
    }

    /// Our display arrangement as a `ScreenInfo` message.
    fn screen_info(&self) -> Message {
        let monitors = self
            .screens
            .monitors()
            .into_iter()
            .map(|m| Monitor {
                monitor_id: m.id,
                x: m.x,
                y: m.y,
                width: m.width,
                height: m.height,
                scale_permille: m.scale_permille,
                width_mm: 0,
                height_mm: 0,
            })
            .collect();
        let cursor = self.screens.cursor();
        Message::ScreenInfo(ScreenInfo {
            info_seq: 1,
            cursor_x: cursor.x,
            cursor_y: cursor.y,
            monitors,
        })
    }

    /// Rebuild the layout from the configuration and the set of ready links.
    fn build_layout(&self) -> Option<Layout> {
        let ready: Vec<MachineId> = lock(&self.links)
            .iter()
            .filter(|(_, l)| l.ready)
            .map(|(id, _)| *id)
            .collect();
        lock(&self.config)
            .layout(|id| id == self.me || ready.contains(&id))
            .ok()
    }

    fn persist_config(&self) {
        if let Some(path) = &self.config_path {
            let _ = lock(&self.config).save(path);
        }
    }

    /// Record a peer's screen size, placing it to the right of the others the first time.
    fn learn_screen(&self, machine: MachineId, width: u32, height: u32) {
        {
            let mut config = lock(&self.config);
            let id = format_machine_id(machine);
            if let Some(existing) = config.layout.iter_mut().find(|s| s.machine_id == id) {
                existing.width = width;
                existing.height = height;
            } else {
                let right = config
                    .layout
                    .iter()
                    .map(|s| i64::from(s.x) + i64::from(s.width))
                    .max()
                    .unwrap_or(0);
                config.layout.push(ScreenConfig {
                    machine_id: id,
                    x: i32::try_from(right).unwrap_or(0),
                    y: 0,
                    width,
                    height,
                });
            }
        }
        self.persist_config();
    }
}

/// Control surface of a running daemon.
#[derive(Clone)]
pub struct DaemonHandle {
    shared: Arc<Shared>,
    endpoint: Endpoint,
    addr: SocketAddr,
}

impl std::fmt::Debug for DaemonHandle {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("DaemonHandle")
            .field("addr", &self.addr)
            .finish_non_exhaustive()
    }
}

impl DaemonHandle {
    /// Address the daemon is listening on.
    #[must_use]
    pub const fn local_addr(&self) -> SocketAddr {
        self.addr
    }

    /// This machine's ID.
    #[must_use]
    pub fn machine_id(&self) -> MachineId {
        self.shared.me
    }

    /// Subscribe to daemon events.
    #[must_use]
    pub fn subscribe(&self) -> broadcast::Receiver<DaemonEvent> {
        self.shared.events.subscribe()
    }

    /// Answer a [`DaemonEvent::PairingRequested`].
    pub fn answer_pairing(&self, id: u64, accept: bool) {
        self.shared.broker.answer(id, accept);
    }

    /// Dial another machine once; the link then runs in the background.
    ///
    /// # Errors
    /// Connection, pairing or protocol failures.
    pub async fn connect(&self, addr: SocketAddr) -> Result<(), ConnectError> {
        let link = self.endpoint.connect(addr).await?;
        tokio::spawn(run_link(self.shared.clone(), link));
        Ok(())
    }

    /// Hotkey-style direct jump to a machine.
    pub fn jump(&self, machine: MachineId) {
        if self.shared.is_host {
            self.shared.route(Event::Jump(machine));
        }
    }

    /// Lock or unlock the cursor to the current screen.
    pub fn set_locked(&self, locked: bool) {
        if self.shared.is_host {
            self.shared.route(Event::SetLocked(locked));
        }
    }

    /// Current state.
    #[must_use]
    pub fn status(&self) -> Status {
        let (active, locked) = {
            let router = lock(&self.shared.router);
            (router.active(), router.is_locked())
        };
        let peers = lock(&self.shared.links)
            .iter()
            .filter(|(_, l)| l.ready)
            .map(|(id, l)| (*id, l.name.clone()))
            .collect();
        Status {
            machine: self.shared.me,
            is_host: self.shared.is_host,
            active,
            locked,
            peers,
        }
    }

    /// Paired machines.
    #[must_use]
    pub fn paired(&self) -> Vec<(MachineId, String)> {
        self.endpoint
            .with_trust(|t| t.peers().map(|(id, p)| (id, p.name.clone())).collect())
    }

    /// Forget a paired machine and drop its link.
    ///
    /// # Errors
    /// The trust store could not be saved.
    pub fn revoke(&self, machine: MachineId) -> Result<bool, portal_net::Error> {
        let removed = self.endpoint.with_trust(|t| t.revoke(machine))?;
        if let Some(link) = lock(&self.shared.links).get(&machine) {
            link.control.close();
        }
        Ok(removed)
    }

    /// A copy of the current configuration.
    #[must_use]
    pub fn config(&self) -> Config {
        lock(&self.shared.config).clone()
    }

    /// Replace the configuration, apply what can change live and persist it.
    ///
    /// Returns `true` when the new configuration needs a restart to take full effect (role or
    /// listen port changed).
    ///
    /// # Errors
    /// The new layout is invalid.
    pub fn set_config(&self, new: Config) -> Result<bool, ConfigError> {
        let me = self.shared.me;
        let mut new = new;
        // Our own screen always reflects the real display.
        let _ = ensure_own_screen(&mut new, me, &*self.shared.screens);
        let ready: Vec<MachineId> = lock(&self.shared.links)
            .iter()
            .filter(|(_, l)| l.ready)
            .map(|(id, _)| *id)
            .collect();
        let layout = new.layout(|id| id == me || ready.contains(&id))?;
        let restart = {
            let mut config = lock(&self.shared.config);
            let restart = config.is_host != new.is_host || config.listen_port != new.listen_port;
            *config = new.clone();
            restart
        };
        self.shared.persist_config();
        if self.shared.is_host {
            self.shared.route(Event::SetLayout(layout));
            self.shared.route(Event::SetConfig(new.switch.to_core()));
        }
        Ok(restart)
    }

    /// Stop the daemon: close links, stop capturing.
    pub fn shutdown(&self) {
        if self.shared.stopped.swap(true, Ordering::SeqCst) {
            return;
        }
        let _ = self.shared.shutdown.send(true);
        lock(&self.shared.capture).stop();
        self.shared.tick.notify_one();
    }
}

/// Make sure our own screen is in the layout with its real size.
fn ensure_own_screen(config: &mut Config, me: MachineId, screens: &dyn Screens) -> Option<()> {
    let (_, width, height) = bounding_box(&screens.monitors())?;
    let my_id = format_machine_id(me);
    match config.layout.iter_mut().find(|s| s.machine_id == my_id) {
        Some(s) => (s.width, s.height) = (width, height),
        None => config.layout.insert(
            0,
            ScreenConfig {
                machine_id: my_id,
                x: 0,
                y: 0,
                width,
                height,
            },
        ),
    }
    Some(())
}

/// Start the daemon.
///
/// # Errors
/// Invalid configuration, platform permission problems, or the listen socket failing.
pub async fn start(
    options: DaemonOptions,
    platform: Platform,
) -> Result<DaemonHandle, DaemonError> {
    let DaemonOptions {
        mut config,
        config_path,
        identity,
        trust,
        link,
        pairing,
        bind,
        ipc,
    } = options;
    let me = identity.machine_id();

    ensure_own_screen(&mut config, me, &*platform.screens).ok_or(DaemonError::NoDisplay)?;
    let layout = config.layout(|id| id == me)?;
    let router = Router::new(me, layout, config.switch.to_core());

    let (events, _) = broadcast::channel(256);
    let broker = Arc::new(PairingBroker {
        policy: pairing,
        events: events.clone(),
        pending: Mutex::new(HashMap::new()),
        next: AtomicU64::new(1),
    });
    let local = LocalInfo {
        identity,
        machine_name: config.machine_name.clone(),
        app_version: portal_core::VERSION.to_owned(),
        capabilities: cap::CLIPBOARD_TEXT | cap::FILE_TRANSFER,
        is_host: config.is_host,
    };
    let endpoint = {
        let broker = broker.clone();
        Endpoint::new(local, trust, link, move |prompt| {
            let broker = broker.clone();
            async move { broker.ask(prompt).await }
        })?
    };

    let (out, out_rx) = mpsc::unbounded_channel();
    let (shutdown, shutdown_rx) = watch::channel(false);
    let shared = Arc::new(Shared {
        me,
        is_host: config.is_host,
        started: tokio::time::Instant::now(),
        router: Mutex::new(router),
        out,
        tick: Notify::new(),
        links: Mutex::new(HashMap::new()),
        config: Mutex::new(config.clone()),
        config_path,
        injector: Mutex::new(platform.injector),
        capture: Mutex::new(platform.capture),
        screens: platform.screens,
        events,
        broker,
        shutdown,
        stopped: AtomicBool::new(false),
    });

    let listener = TcpListener::bind(bind).await?;
    let addr = listener.local_addr()?;
    let incoming = endpoint.serve(listener);

    tokio::spawn(dispatcher(shared.clone(), out_rx));
    tokio::spawn(accept_links(shared.clone(), incoming, shutdown_rx.clone()));
    if config.is_host {
        let handler_shared = shared.clone();
        lock(&shared.capture).start(Box::new(move |input| {
            handler_shared.route(Event::Input(input))
        }))?;
        tokio::spawn(ticker(shared.clone(), shutdown_rx.clone()));
    }
    let handle = DaemonHandle {
        shared: shared.clone(),
        endpoint: endpoint.clone(),
        addr,
    };
    if let Some(ipc) = ipc {
        let listener = portal_ipc::transport::Listener::bind(&ipc.endpoint).await?;
        let service = Arc::new(crate::ipc::DaemonService::new(handle.clone()));
        tokio::spawn(portal_ipc::server::serve(
            listener,
            service,
            ipc.token,
            shutdown_rx.clone(),
        ));
    }
    for peer in config.peers {
        if let Ok(address) = peer.address.parse::<SocketAddr>() {
            tokio::spawn(dial_loop(
                shared.clone(),
                endpoint.clone(),
                address,
                shutdown_rx.clone(),
            ));
        }
    }
    Ok(handle)
}

async fn accept_links(
    shared: Arc<Shared>,
    mut incoming: mpsc::Receiver<Link>,
    mut stop: watch::Receiver<bool>,
) {
    loop {
        tokio::select! {
            link = incoming.recv() => match link {
                Some(link) => { tokio::spawn(run_link(shared.clone(), link)); }
                None => return,
            },
            _ = stop.changed() => return,
        }
    }
}

/// Keep a configured peer connected: retry with backoff, give up only on fatal errors.
async fn dial_loop(
    shared: Arc<Shared>,
    endpoint: Endpoint,
    addr: SocketAddr,
    mut stop: watch::Receiver<bool>,
) {
    const BACKOFF: [u64; 4] = [1, 3, 5, 5];
    let mut attempt = 0usize;
    loop {
        if *stop.borrow() {
            return;
        }
        match endpoint.connect(addr).await {
            Ok(link) => {
                attempt = 0;
                run_link(shared.clone(), link).await;
            }
            Err(ConnectError::Handshake(e)) if is_fatal(&e) => {
                let _ = shared.events.send(DaemonEvent::DialFailed {
                    address: addr.to_string(),
                    reason: e.to_string(),
                });
                return;
            }
            Err(_) => {}
        }
        let wait = BACKOFF[attempt.min(BACKOFF.len() - 1)];
        attempt += 1;
        tokio::select! {
            () = tokio::time::sleep(Duration::from_secs(wait)) => {}
            _ = stop.changed() => return,
        }
    }
}

const fn is_fatal(error: &portal_net::session::HandshakeError) -> bool {
    use portal_net::session::HandshakeError as H;
    matches!(
        error,
        H::PairingRejected | H::KeyMismatch | H::Incompatible { .. }
    )
}

/// Execute router commands in order.
async fn dispatcher(shared: Arc<Shared>, mut commands: mpsc::UnboundedReceiver<Command>) {
    while let Some(command) = commands.recv().await {
        if let Some((to, message)) = command_message(
            &command,
            u32::try_from(shared.now_us() & 0xffff_ffff).unwrap_or(0),
        ) {
            if let Some(link) = lock(&shared.links).get(&to) {
                let _ = link.control.send(message);
            }
            continue;
        }
        match command {
            Command::Capture { swallow } => {
                let _ = lock(&shared.capture).set_peer_active(swallow);
            }
            Command::WarpHost(to) => {
                let _ = lock(&shared.capture).warp_cursor(to);
            }
            Command::HostPress { keys } => {
                for key in keys {
                    shared.inject(Inject::KeyDown(key.with_action(KeyAction::Down)));
                }
            }
            Command::HostRelease { keys, buttons } => {
                for key in keys {
                    shared.inject(Inject::KeyUp(key.with_action(KeyAction::Up)));
                }
                for button in buttons {
                    shared.inject(Inject::Button {
                        button,
                        down: false,
                    });
                }
            }
            Command::ActiveChanged { machine } => {
                let _ = shared.events.send(DaemonEvent::ActiveChanged { machine });
            }
            _ => {}
        }
    }
}

/// Wake the router when a delayed switch or a leave-ack timeout is due.
async fn ticker(shared: Arc<Shared>, mut stop: watch::Receiver<bool>) {
    loop {
        let deadline = lock(&shared.router).deadline();
        let due = deadline.map(|at| Duration::from_micros(at.saturating_sub(shared.now_us())));
        tokio::select! {
            () = async { tokio::time::sleep(due.unwrap_or_default()).await }, if due.is_some() => {
                shared.route(Event::Tick);
            }
            () = shared.tick.notified() => {}
            _ = stop.changed() => return,
        }
    }
}

/// Run one established link until it closes.
async fn run_link(shared: Arc<Shared>, mut link: Link) {
    let machine = link.remote.machine_id;
    let name = link.remote.machine_name.clone();
    // The host of a session sends input; only it routes.
    let we_are_host = shared.is_host;
    lock(&shared.links).insert(
        machine,
        PeerLink {
            name: name.clone(),
            control: link.control(),
            bulk: link.bulk(),
            ready: false,
        },
    );
    let _ = link.control().send(shared.screen_info());

    let mut input = PeerInput::new();
    let mut stop = shared.shutdown.subscribe();
    loop {
        let event = tokio::select! {
            event = link.recv() => event,
            _ = stop.changed() => { link.control().close(); continue; }
        };
        let Some(event) = event else { break };
        match event {
            LinkEvent::Control(message) => {
                handle_control(&shared, &link, &mut input, we_are_host, &message);
            }
            LinkEvent::Bulk(_) => {} // clipboard / file transfer land here in later phases
            LinkEvent::Closed(_) => break,
        }
    }

    lock(&shared.links).remove(&machine);
    if we_are_host {
        let layout = shared.build_layout();
        shared.route(Event::PeerDisconnected(machine));
        if let Some(layout) = layout {
            shared.route(Event::SetLayout(layout));
        }
    } else {
        let mut released = Vec::new();
        input.disconnected(&mut released);
        for action in released {
            shared.inject(action);
        }
    }
    let _ = shared
        .events
        .send(DaemonEvent::PeerDisconnected { machine });
}

fn handle_control(
    shared: &Arc<Shared>,
    link: &Link,
    input: &mut PeerInput,
    we_are_host: bool,
    message: &Message,
) {
    let machine = link.remote.machine_id;
    match message {
        Message::ScreenInfo(info) => {
            let _ = link.control().send(Message::ScreenInfoAck(ScreenInfoAck {
                info_seq: info.info_seq,
            }));
            let monitors: Vec<portal_platform::Monitor> = info
                .monitors
                .iter()
                .map(|m| portal_platform::Monitor {
                    id: m.monitor_id,
                    x: m.x,
                    y: m.y,
                    width: m.width,
                    height: m.height,
                    scale_permille: m.scale_permille,
                })
                .collect();
            let Some((_, width, height)) = bounding_box(&monitors) else {
                return;
            };
            if let Some(entry) = lock(&shared.links).get_mut(&machine) {
                entry.ready = true;
            }
            if we_are_host {
                shared.learn_screen(machine, width, height);
                if let Some(layout) = shared.build_layout() {
                    shared.route(Event::SetLayout(layout));
                    shared.route(Event::PeerConnected(machine));
                }
            }
            let _ = shared.events.send(DaemonEvent::PeerConnected {
                machine,
                name: link.remote.machine_name.clone(),
            });
        }
        Message::LeaveAck(ack) if we_are_host => {
            shared.route(Event::LeaveAck {
                from: machine,
                enter_seq: ack.enter_seq,
                ok: ack.ok,
            });
        }
        m if !we_are_host && m.is_input() => {
            let mut injects = Vec::new();
            if let Some(reply) = apply_host_message(input, m, &mut injects) {
                let _ = link.control().send(reply);
            }
            for action in injects {
                shared.inject(action);
            }
        }
        _ => {}
    }
}

/// Parse a machine ID from the CLI / IPC.
///
/// # Errors
/// Not a 32-digit hex string.
pub fn machine_id_from_str(text: &str) -> Result<MachineId, ConfigError> {
    parse_machine_id(text)
}
