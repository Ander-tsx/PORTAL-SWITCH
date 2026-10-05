//! Spike: input latency while bulk data loads the link, for three transports.
//!
//!   channels server [--bind 0.0.0.0:47000]
//!   channels client --connect HOST:47000 --transport tcp1|tcp2|quic
//!                   [--rate 1000] [--secs 10] [--bulk none|saturate|reverse]
//!                   [--bulk-rate MBPS|adaptive] [--target-ms 4] [--cc cubic|bbr]
//!                   [--mss N] [--label TEXT]
//!
//! tcp1: one TCP+TLS connection carrying probes and bulk through one FIFO writer (baseline).
//! tcp2: control and bulk on separate TCP+TLS connections.
//! quic: one QUIC connection, control on a high-priority bidi stream, bulk on its own stream.
//!
//! The client sends small probe frames at `rate` Hz on the control channel; the server echoes
//! them. RTT is measured on the client clock only. Frames follow protocol v1 framing
//! (u32 length, u16 type, payload).
//!
//! Bulk rate: `0` = unlimited, `N` = fixed N Mbit/s, `adaptive` = delay-based controller driven
//! by the probe RTT (queueing delay above the recent minimum kept under `--target-ms`). For
//! `reverse` bulk the client (receiver) runs the controller and sends the rate to the server.

use std::collections::VecDeque;
use std::future::Future;
use std::net::SocketAddr;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant};

use rustls::client::danger::{HandshakeSignatureValid, ServerCertVerified, ServerCertVerifier};
use rustls::crypto::CryptoProvider;
use rustls::pki_types::{CertificateDer, PrivateKeyDer, PrivatePkcs8KeyDer, ServerName, UnixTime};
use rustls::{DigitallySignedStruct, SignatureScheme};
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::mpsc;
use tokio::task::JoinHandle;

type BoxError = Box<dyn std::error::Error + Send + Sync>;
type Result<T> = std::result::Result<T, BoxError>;

const PROBE: u16 = 0x0004;
const CHUNK: u16 = 0x8304;
const CHUNK_SIZE: usize = 256 * 1024;
const ALPN: &[u8] = b"portalswitch-spike";

const ROLE_SINGLE: u8 = 1;
const ROLE_CONTROL: u8 = 2;
const ROLE_BULK: u8 = 3;
const ROLE_BULK_REVERSE: u8 = 4;
const ROLE_SINGLE_REVERSE: u8 = 5;

/// Probes sent during the measured window carry this bit in their sequence number.
const MEASURED: u32 = 0x8000_0000;

static T0: OnceLock<Instant> = OnceLock::new();

fn log(msg: &str) {
    let t = T0.get_or_init(Instant::now).elapsed().as_secs_f64();
    eprintln!("[{t:9.3}s] {msg}");
}

/// Spawns a task and logs its error, if any, with context.
fn spawn_logged<F>(ctx: String, fut: F)
where
    F: Future<Output = Result<()>> + Send + 'static,
{
    tokio::spawn(async move {
        if let Err(e) = fut.await {
            log(&format!("{ctx}: {e}"));
        }
    });
}

// ---------------------------------------------------------------------------------------------
// TLS
// ---------------------------------------------------------------------------------------------

fn provider() -> Arc<CryptoProvider> {
    Arc::new(rustls::crypto::ring::default_provider())
}

fn server_tls() -> Result<rustls::ServerConfig> {
    let ck = rcgen::generate_simple_self_signed(vec!["portalswitch".into()])?;
    let cert = ck.cert.der().clone();
    let key = PrivateKeyDer::Pkcs8(PrivatePkcs8KeyDer::from(ck.signing_key.serialize_der()));
    let mut cfg = rustls::ServerConfig::builder_with_provider(provider())
        .with_protocol_versions(&[&rustls::version::TLS13])?
        .with_no_client_auth()
        .with_single_cert(vec![cert], key)?;
    cfg.alpn_protocols = vec![ALPN.to_vec()];
    Ok(cfg)
}

/// Spike only: accepts any server certificate (pinning is out of scope here).
#[derive(Debug)]
struct AcceptAny(Arc<CryptoProvider>);

impl ServerCertVerifier for AcceptAny {
    fn verify_server_cert(
        &self,
        _: &CertificateDer<'_>,
        _: &[CertificateDer<'_>],
        _: &ServerName<'_>,
        _: &[u8],
        _: UnixTime,
    ) -> std::result::Result<ServerCertVerified, rustls::Error> {
        Ok(ServerCertVerified::assertion())
    }
    fn verify_tls12_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> std::result::Result<HandshakeSignatureValid, rustls::Error> {
        rustls::crypto::verify_tls12_signature(
            message,
            cert,
            dss,
            &self.0.signature_verification_algorithms,
        )
    }
    fn verify_tls13_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> std::result::Result<HandshakeSignatureValid, rustls::Error> {
        rustls::crypto::verify_tls13_signature(
            message,
            cert,
            dss,
            &self.0.signature_verification_algorithms,
        )
    }
    fn supported_verify_schemes(&self) -> Vec<SignatureScheme> {
        self.0.signature_verification_algorithms.supported_schemes()
    }
}

fn client_tls() -> Result<rustls::ClientConfig> {
    let p = provider();
    let mut cfg = rustls::ClientConfig::builder_with_provider(p.clone())
        .with_protocol_versions(&[&rustls::version::TLS13])?
        .dangerous()
        .with_custom_certificate_verifier(Arc::new(AcceptAny(p)))
        .with_no_client_auth();
    cfg.alpn_protocols = vec![ALPN.to_vec()];
    Ok(cfg)
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Cc {
    Cubic,
    Bbr,
}

fn transport_config(cc: Cc) -> quinn::TransportConfig {
    let mut tc = quinn::TransportConfig::default();
    if cc == Cc::Bbr {
        tc.congestion_controller_factory(Arc::new(quinn::congestion::BbrConfig::default()));
    }
    tc
}

// ---------------------------------------------------------------------------------------------
// Framing
// ---------------------------------------------------------------------------------------------

fn frame(kind: u16, payload: &[u8]) -> Vec<u8> {
    let mut f = Vec::with_capacity(6 + payload.len());
    f.extend_from_slice(&u32::try_from(payload.len() + 2).unwrap().to_le_bytes());
    f.extend_from_slice(&kind.to_le_bytes());
    f.extend_from_slice(payload);
    f
}

fn probe_frame(seq: u32, t_ns: u64) -> Vec<u8> {
    let mut p = [0u8; 12];
    p[..4].copy_from_slice(&seq.to_le_bytes());
    p[4..].copy_from_slice(&t_ns.to_le_bytes());
    frame(PROBE, &p)
}

/// Reads one frame into `buf` and returns its type.
async fn read_frame<R: AsyncRead + Unpin>(r: &mut R, buf: &mut Vec<u8>) -> Result<u16> {
    let mut hdr = [0u8; 6];
    r.read_exact(&mut hdr).await?;
    let len = u32::from_le_bytes(hdr[..4].try_into()?) as usize;
    if !(2..=CHUNK_SIZE + 64).contains(&len) {
        return Err(format!("bad frame length {len}").into());
    }
    let kind = u16::from_le_bytes(hdr[4..6].try_into()?);
    buf.resize(len - 2, 0);
    r.read_exact(buf).await?;
    Ok(kind)
}

/// Writes every queued frame in FIFO order (one writer per channel).
async fn writer<W: AsyncWrite + Unpin>(mut w: W, mut rx: mpsc::Receiver<Vec<u8>>) -> Result<()> {
    while let Some(f) = rx.recv().await {
        w.write_all(&f).await?;
        if f.len() < 64 {
            w.flush().await?;
        }
    }
    Ok(())
}

// ---------------------------------------------------------------------------------------------
// Rate control
// ---------------------------------------------------------------------------------------------

const PACE_SLICE: usize = 16 * 1024;
/// Credit a paced sender may accumulate while idle, so it cannot burst afterwards.
const MAX_CREDIT: Duration = Duration::from_millis(2);

/// Paces writes to the rate in `rate_kbps` (0 = unlimited), re-read for every slice.
struct Pacer {
    rate_kbps: Arc<AtomicU64>,
    due: Instant,
}

impl Pacer {
    fn new(rate_kbps: Arc<AtomicU64>) -> Self {
        Self {
            rate_kbps,
            due: Instant::now(),
        }
    }

    async fn write<W: AsyncWrite + Unpin>(&mut self, w: &mut W, data: &[u8]) -> Result<()> {
        for part in data.chunks(PACE_SLICE) {
            let kbps = self.rate_kbps.load(Ordering::Relaxed);
            if kbps == 0 {
                w.write_all(part).await?;
                continue;
            }
            let now = Instant::now();
            if self.due + MAX_CREDIT < now {
                self.due = now - MAX_CREDIT;
            }
            self.due += Duration::from_secs_f64(part.len() as f64 * 8.0 / (kbps as f64 * 1000.0));
            tokio::time::sleep_until(self.due.into()).await;
            w.write_all(part).await?;
        }
        Ok(())
    }
}

const START_KBPS: f64 = 10_000.0;
const MIN_KBPS: f64 = 1_000.0;
const MAX_KBPS: f64 = 1_000_000.0;
const TICK: Duration = Duration::from_millis(10);
const BASE_WINDOW: Duration = Duration::from_secs(10);

/// Delay-based bulk rate controller (LEDBAT-like, on rate instead of window).
///
/// queueing delay = min(last 4 RTTs) - min(RTT over the last 10 s).
/// Above target: multiplicative decrease (x0.8), at most once per RTT.
/// Below target: increase proportional to the remaining headroom.
struct Controller {
    target_ns: u64,
    rate_kbps: f64,
    recent: VecDeque<u64>,
    base: VecDeque<(Instant, u64)>,
    last_tick: Instant,
    last_decrease: Instant,
}

impl Controller {
    fn new(target: Duration) -> Self {
        let now = Instant::now();
        Self {
            target_ns: u64::try_from(target.as_nanos()).unwrap_or(u64::MAX),
            rate_kbps: START_KBPS,
            recent: VecDeque::with_capacity(4),
            base: VecDeque::new(),
            last_tick: now,
            last_decrease: now,
        }
    }

    /// Feeds one RTT sample; returns the rate to use from now on.
    fn on_rtt(&mut self, rtt_ns: u64, now: Instant) -> f64 {
        if self.recent.len() == 4 {
            self.recent.pop_front();
        }
        self.recent.push_back(rtt_ns);
        match self.base.back_mut() {
            Some((start, min)) if now.duration_since(*start) < Duration::from_secs(1) => {
                *min = (*min).min(rtt_ns);
            }
            _ => self.base.push_back((now, rtt_ns)),
        }
        while self
            .base
            .front()
            .is_some_and(|(start, _)| now.duration_since(*start) > BASE_WINDOW)
        {
            self.base.pop_front();
        }

        if now.duration_since(self.last_tick) < TICK {
            return self.rate_kbps;
        }
        self.last_tick = now;

        let filtered = self.recent.iter().copied().min().unwrap_or(rtt_ns);
        let base = self.base.iter().map(|(_, m)| *m).min().unwrap_or(filtered);
        let queueing = filtered.saturating_sub(base);
        if queueing > self.target_ns {
            let hold = Duration::from_nanos(filtered.max(20_000_000));
            if now.duration_since(self.last_decrease) >= hold {
                self.rate_kbps *= 0.8;
                self.last_decrease = now;
            }
        } else {
            let headroom = 1.0 - queueing as f64 / self.target_ns as f64;
            self.rate_kbps += headroom * (self.rate_kbps * 0.03).max(250.0);
        }
        self.rate_kbps = self.rate_kbps.clamp(MIN_KBPS, MAX_KBPS);
        self.rate_kbps
    }
}

// ---------------------------------------------------------------------------------------------
// Server
// ---------------------------------------------------------------------------------------------

/// Echoes probe frames, discards everything else. With `reverse_bulk`, the server also streams
/// unpaced bulk chunks back through the same FIFO writer (single-connection baseline).
async fn serve_control<R, W>(mut r: R, w: W, reverse_bulk: bool) -> Result<()>
where
    R: AsyncRead + Unpin,
    W: AsyncWrite + Unpin + Send + 'static,
{
    let (tx, rx) = mpsc::channel::<Vec<u8>>(4);
    spawn_logged("control writer".into(), writer(w, rx));
    if reverse_bulk {
        let tx = tx.clone();
        tokio::spawn(async move {
            let chunk = frame(CHUNK, &vec![0x5Au8; CHUNK_SIZE]);
            while tx.send(chunk.clone()).await.is_ok() {}
        });
    }
    let mut buf = Vec::new();
    loop {
        let kind = read_frame(&mut r, &mut buf).await?;
        if kind == PROBE {
            tx.send(frame(PROBE, &buf))
                .await
                .map_err(|_| "control writer gone")?;
        }
    }
}

/// Streams bulk chunks paced to the rate the client sends (u32 kbit/s messages, 0 = unlimited),
/// until the peer goes away.
async fn send_bulk<R, W>(mut r: R, mut w: W) -> Result<()>
where
    R: AsyncRead + Unpin + Send + 'static,
    W: AsyncWrite + Unpin,
{
    let rate = Arc::new(AtomicU64::new(u64::from(r.read_u32_le().await?)));
    let updates = rate.clone();
    tokio::spawn(async move {
        while let Ok(kbps) = r.read_u32_le().await {
            updates.store(u64::from(kbps), Ordering::Relaxed);
        }
    });
    let chunk = frame(CHUNK, &vec![0x5Au8; CHUNK_SIZE]);
    let mut pacer = Pacer::new(rate);
    loop {
        pacer.write(&mut w, &chunk).await?;
    }
}

async fn drain<R: AsyncRead + Unpin>(mut r: R) -> Result<()> {
    let mut buf = vec![0u8; 1 << 20];
    while r.read(&mut buf).await? > 0 {}
    Ok(())
}

async fn server(bind: SocketAddr) -> Result<()> {
    T0.get_or_init(Instant::now);
    let tls = server_tls()?;
    let acceptor = tokio_rustls::TlsAcceptor::from(Arc::new(tls.clone()));
    let listener = TcpListener::bind(bind).await?;

    // QUIC: Cubic on the base port, BBR on base port + 1.
    for (cc, port) in [(Cc::Cubic, bind.port()), (Cc::Bbr, bind.port() + 1)] {
        let quic_cfg = quinn::crypto::rustls::QuicServerConfig::try_from(tls.clone())?;
        let mut cfg = quinn::ServerConfig::with_crypto(Arc::new(quic_cfg));
        cfg.transport_config(Arc::new(transport_config(cc)));
        let endpoint = quinn::Endpoint::server(cfg, SocketAddr::new(bind.ip(), port))?;
        spawn_quic_server(endpoint, cc);
    }
    log(&format!(
        "listening on tcp {bind}, udp {bind} (quic cubic), udp port {} (quic bbr)",
        bind.port() + 1
    ));

    loop {
        let (sock, peer) = listener.accept().await?;
        sock.set_nodelay(true)?;
        let acceptor = acceptor.clone();
        spawn_logged(format!("tcp {peer}"), async move {
            let mut tls = acceptor.accept(sock).await?;
            let role = tls.read_u8().await?;
            log(&format!("tcp {peer} role {role} open"));
            let result = match role {
                ROLE_SINGLE | ROLE_CONTROL | ROLE_SINGLE_REVERSE => {
                    let (r, w) = tokio::io::split(tls);
                    serve_control(r, w, role == ROLE_SINGLE_REVERSE).await
                }
                ROLE_BULK => drain(tls).await,
                ROLE_BULK_REVERSE => {
                    let (r, w) = tokio::io::split(tls);
                    send_bulk(r, w).await
                }
                other => Err(format!("unknown role {other}").into()),
            };
            log(&format!("tcp {peer} role {role} closed"));
            result.map_err(|e| format!("role {role}: {e}").into())
        });
    }
}

fn spawn_quic_server(endpoint: quinn::Endpoint, cc: Cc) {
    tokio::spawn(async move {
        while let Some(incoming) = endpoint.accept().await {
            spawn_logged(format!("quic {cc:?}"), async move {
                let conn = incoming.await?;
                let peer = conn.remote_address();
                log(&format!("quic {cc:?} {peer} open"));
                let c2 = conn.clone();
                tokio::spawn(async move {
                    while let Ok(r) = c2.accept_uni().await {
                        spawn_logged(format!("quic {peer} bulk drain"), drain(r));
                    }
                });
                while let Ok((s, mut r)) = conn.accept_bi().await {
                    spawn_logged(format!("quic {peer} stream"), async move {
                        match r.read_u8().await? {
                            ROLE_CONTROL => serve_control(r, s, false).await,
                            ROLE_BULK_REVERSE => send_bulk(r, s).await,
                            other => Err(format!("unknown stream role {other}").into()),
                        }
                    });
                }
                log(&format!(
                    "quic {cc:?} {peer} closed: {}",
                    conn.close_reason()
                        .map_or_else(|| "?".into(), |e| e.to_string())
                ));
                Ok(())
            });
        }
    });
}

// ---------------------------------------------------------------------------------------------
// Client
// ---------------------------------------------------------------------------------------------

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Transport {
    Tcp1,
    Tcp2,
    Quic,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Bulk {
    None,
    /// client -> server, same direction as input
    Saturate,
    /// server -> client, opposite to input
    Reverse,
}

#[derive(Clone, Copy, PartialEq, Debug)]
enum BulkRate {
    /// Mbit/s; 0 = unlimited
    Fixed(u32),
    Adaptive,
}

struct Args {
    connect: SocketAddr,
    transport: Transport,
    rate: u64,
    secs: u64,
    bulk: Bulk,
    bulk_rate: BulkRate,
    target: Duration,
    cc: Cc,
    mss: Option<u32>,
    label: String,
}

struct Shared {
    start: Instant,
    rtts: Mutex<Vec<u64>>,
    sent: AtomicU64,
    bulk_bytes: AtomicU64,
    measuring: AtomicBool,
    stop: AtomicBool,
    /// Current bulk rate (kbit/s, 0 = unlimited); read by pacers and the rate signaller.
    rate_kbps: Arc<AtomicU64>,
    controller: Option<Mutex<Controller>>,
    /// Controller rate sampled every tick while measuring (kbit/s).
    rate_trace: Mutex<Vec<f64>>,
}

impl Shared {
    fn on_rtt(&self, rtt_ns: u64) {
        if let Some(c) = &self.controller {
            let kbps = c.lock().unwrap().on_rtt(rtt_ns, Instant::now());
            self.rate_kbps.store(kbps as u64, Ordering::Relaxed);
        }
    }
}

async fn tcp_connect(addr: SocketAddr, mss: Option<u32>) -> Result<TcpStream> {
    let sock = socket2::Socket::new(
        socket2::Domain::for_address(addr),
        socket2::Type::STREAM,
        Some(socket2::Protocol::TCP),
    )?;
    sock.set_tcp_nodelay(true)?;
    sock.connect(&addr.into())?;
    #[cfg(not(windows))]
    if let Some(m) = mss {
        sock.set_tcp_mss(m)?;
    }
    #[cfg(windows)]
    let _ = mss;
    sock.set_nonblocking(true)?;
    Ok(TcpStream::from_std(sock.into())?)
}

async fn tls_connect(a: &Args, role: u8) -> Result<tokio_rustls::client::TlsStream<TcpStream>> {
    let tcp = tcp_connect(a.connect, a.mss).await?;
    let connector = tokio_rustls::TlsConnector::from(Arc::new(client_tls()?));
    let mut tls = connector
        .connect(ServerName::try_from("portalswitch")?, tcp)
        .await?;
    tls.write_u8(role).await?;
    tls.flush().await?;
    Ok(tls)
}

/// Reads echoed probes, records RTTs of measured probes and feeds every RTT to the controller.
/// Also counts bulk chunks arriving on the same connection (tcp1 reverse).
async fn read_echoes<R: AsyncRead + Unpin>(mut r: R, sh: Arc<Shared>) -> Result<()> {
    let mut buf = Vec::new();
    loop {
        let kind = read_frame(&mut r, &mut buf).await?;
        if kind == CHUNK && sh.measuring.load(Ordering::Relaxed) {
            sh.bulk_bytes
                .fetch_add(buf.len() as u64 + 6, Ordering::Relaxed);
        }
        if kind == PROBE && buf.len() >= 12 {
            let now = u64::try_from(sh.start.elapsed().as_nanos())?;
            let t = u64::from_le_bytes(buf[4..12].try_into()?);
            let seq = u32::from_le_bytes(buf[..4].try_into()?);
            let rtt = now - t;
            sh.on_rtt(rtt);
            if seq & MEASURED != 0 {
                sh.rtts.lock().unwrap().push(rtt);
            }
        }
    }
}

async fn bulk_into_queue(tx: mpsc::Sender<Vec<u8>>, sh: Arc<Shared>) {
    let chunk = frame(CHUNK, &vec![0xA5u8; CHUNK_SIZE]);
    while !sh.stop.load(Ordering::Relaxed) {
        if tx.send(chunk.clone()).await.is_err() {
            break;
        }
        if sh.measuring.load(Ordering::Relaxed) {
            sh.bulk_bytes
                .fetch_add(chunk.len() as u64, Ordering::Relaxed);
        }
    }
}

/// Reads raw bulk bytes sent by the server and counts them.
async fn count_drain<R: AsyncRead + Unpin>(mut r: R, sh: Arc<Shared>) -> Result<()> {
    let mut buf = vec![0u8; 1 << 20];
    loop {
        let n = r.read(&mut buf).await?;
        if n == 0 {
            return Ok(());
        }
        if sh.measuring.load(Ordering::Relaxed) {
            sh.bulk_bytes.fetch_add(n as u64, Ordering::Relaxed);
        }
    }
}

async fn bulk_direct<W: AsyncWrite + Unpin>(mut w: W, sh: Arc<Shared>) -> Result<()> {
    let chunk = frame(CHUNK, &vec![0xA5u8; CHUNK_SIZE]);
    let mut pacer = Pacer::new(sh.rate_kbps.clone());
    while !sh.stop.load(Ordering::Relaxed) {
        pacer.write(&mut w, &chunk).await?;
        if sh.measuring.load(Ordering::Relaxed) {
            sh.bulk_bytes
                .fetch_add(chunk.len() as u64, Ordering::Relaxed);
        }
    }
    Ok(())
}

/// Reverse bulk: tells the server the initial rate, then every rate change (u32 kbit/s).
async fn signal_rate<W: AsyncWrite + Unpin>(mut w: W, sh: Arc<Shared>) -> Result<()> {
    let mut last = sh.rate_kbps.load(Ordering::Relaxed);
    w.write_u32_le(u32::try_from(last)?).await?;
    w.flush().await?;
    while !sh.stop.load(Ordering::Relaxed) {
        tokio::time::sleep(TICK).await;
        let now = sh.rate_kbps.load(Ordering::Relaxed);
        if now != last {
            w.write_u32_le(u32::try_from(now)?).await?;
            w.flush().await?;
            last = now;
        }
    }
    // Keep the stream open until the run ends; the process exit closes it.
    std::future::pending::<()>().await;
    Ok(())
}

async fn client(a: Args) -> Result<String> {
    let initial_kbps = match a.bulk_rate {
        BulkRate::Fixed(mbps) => u64::from(mbps) * 1000,
        BulkRate::Adaptive => START_KBPS as u64,
    };
    let sh = Arc::new(Shared {
        start: Instant::now(),
        rtts: Mutex::new(Vec::new()),
        sent: AtomicU64::new(0),
        bulk_bytes: AtomicU64::new(0),
        measuring: AtomicBool::new(false),
        stop: AtomicBool::new(false),
        rate_kbps: Arc::new(AtomicU64::new(initial_kbps)),
        controller: (a.bulk_rate == BulkRate::Adaptive)
            .then(|| Mutex::new(Controller::new(a.target))),
        rate_trace: Mutex::new(Vec::new()),
    });

    // Control queue: the probe loop never blocks on the socket directly.
    let (ctl_tx, ctl_rx) = mpsc::channel::<Vec<u8>>(4096);
    let ctl_writer: JoinHandle<Result<()>>;
    let mut quic = None;

    match a.transport {
        Transport::Tcp1 => {
            let role = if a.bulk == Bulk::Reverse {
                ROLE_SINGLE_REVERSE
            } else {
                ROLE_SINGLE
            };
            let (r, w) = tokio::io::split(tls_connect(&a, role).await?);
            tokio::spawn(read_echoes(r, sh.clone()));
            // Single FIFO shared by probes and bulk: the naive design (never paced).
            let (tx, rx) = mpsc::channel::<Vec<u8>>(4);
            ctl_writer = tokio::spawn(writer(w, rx));
            let fwd = tx.clone();
            let mut ctl_rx = ctl_rx;
            tokio::spawn(async move {
                while let Some(f) = ctl_rx.recv().await {
                    if fwd.send(f).await.is_err() {
                        break;
                    }
                }
            });
            if a.bulk == Bulk::Saturate {
                tokio::spawn(bulk_into_queue(tx, sh.clone()));
            }
        }
        Transport::Tcp2 => {
            let (r, w) = tokio::io::split(tls_connect(&a, ROLE_CONTROL).await?);
            tokio::spawn(read_echoes(r, sh.clone()));
            ctl_writer = tokio::spawn(writer(w, ctl_rx));
            match a.bulk {
                Bulk::Saturate => {
                    let bulk = tls_connect(&a, ROLE_BULK).await?;
                    tokio::spawn(bulk_direct(bulk, sh.clone()));
                }
                Bulk::Reverse => {
                    let (br, bw) = tokio::io::split(tls_connect(&a, ROLE_BULK_REVERSE).await?);
                    tokio::spawn(signal_rate(bw, sh.clone()));
                    tokio::spawn(count_drain(br, sh.clone()));
                }
                Bulk::None => {}
            }
        }
        Transport::Quic => {
            let qcfg = quinn::crypto::rustls::QuicClientConfig::try_from(client_tls()?)?;
            let mut ccfg = quinn::ClientConfig::new(Arc::new(qcfg));
            ccfg.transport_config(Arc::new(transport_config(a.cc)));
            let mut ep = quinn::Endpoint::client("0.0.0.0:0".parse()?)?;
            ep.set_default_client_config(ccfg);
            let mut target = a.connect;
            if a.cc == Cc::Bbr {
                target.set_port(target.port() + 1);
            }
            let conn = ep.connect(target, "portalswitch")?.await?;
            let (mut s, r) = conn.open_bi().await?;
            s.set_priority(100)?;
            s.write_all(&[ROLE_CONTROL]).await?;
            tokio::spawn(read_echoes(r, sh.clone()));
            ctl_writer = tokio::spawn(writer(s, ctl_rx));
            match a.bulk {
                Bulk::Saturate => {
                    let u = conn.open_uni().await?;
                    u.set_priority(0)?;
                    tokio::spawn(bulk_direct(u, sh.clone()));
                }
                Bulk::Reverse => {
                    let (mut bs, br) = conn.open_bi().await?;
                    bs.write_all(&[ROLE_BULK_REVERSE]).await?;
                    tokio::spawn(signal_rate(bs, sh.clone()));
                    tokio::spawn(count_drain(br, sh.clone()));
                }
                Bulk::None => {}
            }
            quic = Some((ep, conn));
        }
    }

    // Probes run during warm-up too (they drive the controller) but are only measured after it.
    let warmup = if a.bulk_rate == BulkRate::Adaptive {
        Duration::from_secs(3)
    } else {
        Duration::from_secs(1)
    };
    let period = Duration::from_nanos(1_000_000_000 / a.rate);
    let mut tick = tokio::time::interval(period);
    tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    let warmup_end = Instant::now() + warmup;
    let mut measure_start = None;
    let mut last_trace = Instant::now();
    // Wall-clock cap: a blocked shared queue (tcp1) must not stretch a run indefinitely.
    let cap = warmup + Duration::from_secs(a.secs * 4);
    let started = Instant::now();
    let mut i: u32 = 0;
    loop {
        tick.tick().await;
        let now = Instant::now();
        if now >= warmup_end && measure_start.is_none() {
            measure_start = Some(now);
            sh.measuring.store(true, Ordering::Relaxed);
        }
        if let Some(ms) = measure_start {
            if now.duration_since(ms) >= Duration::from_secs(a.secs)
                || now.duration_since(started) > cap
            {
                break;
            }
            if now.duration_since(last_trace) >= TICK {
                last_trace = now;
                sh.rate_trace
                    .lock()
                    .unwrap()
                    .push(sh.rate_kbps.load(Ordering::Relaxed) as f64);
            }
        }
        let t = u64::try_from(sh.start.elapsed().as_nanos())?;
        let measured = measure_start.is_some();
        let seq = (i & !MEASURED) | if measured { MEASURED } else { 0 };
        i = i.wrapping_add(1);
        if ctl_tx.send(probe_frame(seq, t)).await.is_err() {
            let cause = match ctl_writer.await {
                Ok(Err(e)) => e.to_string(),
                Ok(Ok(())) => "writer ended".into(),
                Err(e) => e.to_string(),
            };
            return Err(format!("control channel failed: {cause}").into());
        }
        if measured {
            sh.sent.fetch_add(1, Ordering::Relaxed);
        }
    }
    let elapsed = measure_start.map_or(0.0, |ms| ms.elapsed().as_secs_f64());
    sh.measuring.store(false, Ordering::Relaxed);
    sh.stop.store(true, Ordering::Relaxed);
    // Grace period for in-flight echoes.
    tokio::time::sleep(Duration::from_secs(3)).await;
    // Close QUIC explicitly so the server stops sending before the next run starts.
    if let Some((ep, conn)) = quic.take() {
        conn.close(0u32.into(), b"done");
        let _ = tokio::time::timeout(Duration::from_secs(2), ep.wait_idle()).await;
    }

    let mut v = sh.rtts.lock().unwrap().clone();
    v.sort_unstable();
    let pct = |p: f64| {
        if v.is_empty() {
            0.0
        } else {
            v[((v.len() - 1) as f64 * p).round() as usize] as f64 / 1000.0
        }
    };
    let trace = sh.rate_trace.lock().unwrap().clone();
    let avg_rate_mbps = if trace.is_empty() {
        0.0
    } else {
        trace.iter().sum::<f64>() / trace.len() as f64 / 1000.0
    };
    let sent = sh.sent.load(Ordering::Relaxed);
    let mbps = sh.bulk_bytes.load(Ordering::Relaxed) as f64 * 8.0 / elapsed.max(1e-9) / 1e6;
    let bulk_rate = match a.bulk_rate {
        BulkRate::Fixed(m) => m.to_string(),
        BulkRate::Adaptive => "\"adaptive\"".into(),
    };
    Ok(format!(
        "{{\"label\":\"{}\",\"transport\":\"{:?}\",\"bulk\":\"{:?}\",\"bulk_rate_mbps\":{},\"target_ms\":{},\"cc\":\"{:?}\",\"rate\":{},\"secs\":{},\"sent\":{},\"received\":{},\"rtt_us\":{{\"p50\":{:.0},\"p95\":{:.0},\"p99\":{:.0},\"p999\":{:.0},\"max\":{:.0}}},\"bulk_mbps\":{:.0},\"controller_avg_mbps\":{:.1}}}",
        a.label,
        a.transport,
        a.bulk,
        bulk_rate,
        a.target.as_millis(),
        a.cc,
        a.rate,
        a.secs,
        sent,
        v.len(),
        pct(0.50),
        pct(0.95),
        pct(0.99),
        pct(0.999),
        v.last().map_or(0.0, |&x| x as f64 / 1000.0),
        mbps,
        avg_rate_mbps
    ))
}

// ---------------------------------------------------------------------------------------------
// CLI
// ---------------------------------------------------------------------------------------------

fn opt<'a>(args: &'a [String], name: &str) -> Option<&'a str> {
    args.iter()
        .position(|a| a == name)
        .and_then(|i| args.get(i + 1))
        .map(String::as_str)
}

const USAGE: &str = "usage: channels server [--bind ADDR]
       channels client --connect ADDR --transport tcp1|tcp2|quic [--rate HZ] [--secs N]
                       [--bulk none|saturate|reverse] [--bulk-rate MBPS|adaptive]
                       [--target-ms MS] [--cc cubic|bbr] [--mss N] [--label TEXT]";

#[tokio::main]
async fn main() -> Result<()> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    match args.first().map(String::as_str) {
        Some("server") => {
            let bind = opt(&args, "--bind").unwrap_or("0.0.0.0:47000").parse()?;
            server(bind).await
        }
        Some("client") => {
            let a = Args {
                connect: opt(&args, "--connect")
                    .ok_or("--connect required")?
                    .parse()?,
                transport: match opt(&args, "--transport").unwrap_or("tcp2") {
                    "tcp1" => Transport::Tcp1,
                    "tcp2" => Transport::Tcp2,
                    "quic" => Transport::Quic,
                    other => return Err(format!("unknown transport {other}").into()),
                },
                rate: opt(&args, "--rate").unwrap_or("1000").parse()?,
                secs: opt(&args, "--secs").unwrap_or("10").parse()?,
                bulk: match opt(&args, "--bulk").unwrap_or("none") {
                    "none" => Bulk::None,
                    "saturate" => Bulk::Saturate,
                    "reverse" => Bulk::Reverse,
                    other => return Err(format!("unknown bulk mode {other}").into()),
                },
                bulk_rate: match opt(&args, "--bulk-rate").unwrap_or("0") {
                    "adaptive" => BulkRate::Adaptive,
                    n => BulkRate::Fixed(n.parse()?),
                },
                target: Duration::from_millis(opt(&args, "--target-ms").unwrap_or("4").parse()?),
                cc: match opt(&args, "--cc").unwrap_or("cubic") {
                    "cubic" => Cc::Cubic,
                    "bbr" => Cc::Bbr,
                    other => return Err(format!("unknown cc {other}").into()),
                },
                mss: opt(&args, "--mss").map(str::parse).transpose()?,
                label: opt(&args, "--label").unwrap_or("").to_string(),
            };
            println!("{}", client(a).await?);
            Ok(())
        }
        _ => {
            eprintln!("{USAGE}");
            Ok(())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const MS: u64 = 1_000_000;

    fn feed(c: &mut Controller, rtt_ms: f64, from: Instant, steps: u32) -> f64 {
        let mut rate = 0.0;
        for k in 0..steps {
            let now = from + TICK * k;
            rate = c.on_rtt((rtt_ms * MS as f64) as u64, now);
        }
        rate
    }

    #[test]
    fn grows_while_rtt_stays_at_base() {
        let mut c = Controller::new(Duration::from_millis(4));
        let t = Instant::now();
        let rate = feed(&mut c, 3.0, t + TICK, 200);
        assert!(rate > START_KBPS * 10.0, "rate {rate}");
    }

    #[test]
    fn backs_off_when_queueing_exceeds_target() {
        let mut c = Controller::new(Duration::from_millis(4));
        let t = Instant::now();
        let high = feed(&mut c, 3.0, t + TICK, 200);
        let low = feed(&mut c, 30.0, t + TICK * 201, 100);
        assert!(low < high * 0.2, "high {high} low {low}");
        assert!(low >= MIN_KBPS);
    }

    #[test]
    fn holds_steady_near_target() {
        let mut c = Controller::new(Duration::from_millis(4));
        let t = Instant::now();
        feed(&mut c, 3.0, t + TICK, 100);
        // Queueing exactly at target: no headroom, no decrease. Let the min-of-4 filter
        // flush the 3 ms samples first.
        feed(&mut c, 7.0, t + TICK * 101, 5);
        let before = c.rate_kbps;
        let after = feed(&mut c, 7.0, t + TICK * 106, 50);
        assert!(
            (after - before).abs() / before < 0.05,
            "before {before} after {after}"
        );
    }
}
