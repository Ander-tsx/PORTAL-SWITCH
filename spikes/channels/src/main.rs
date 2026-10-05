//! Spike: input latency while bulk data saturates the link, for three transports.
//!
//!   channels server [--bind 0.0.0.0:47000]
//!   channels client --connect HOST:47000 --transport tcp1|tcp2|quic
//!                   [--rate 1000] [--secs 10] [--bulk none|saturate|reverse] [--bulk-rate MBPS] [--cc cubic|bbr] [--mss N] [--label TEXT]
//!
//! tcp1: one TCP+TLS connection carrying probes and bulk through one FIFO writer (baseline).
//! tcp2: control and bulk on separate TCP+TLS connections.
//! quic: one QUIC connection, control on a high-priority bidi stream, bulk on a uni stream.
//!
//! The client sends small probe frames at `rate` Hz on the control channel; the server echoes
//! them. RTT is measured on the client clock only. Frames follow protocol v1 framing
//! (u32 length, u16 type, payload).

use std::net::SocketAddr;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use rustls::client::danger::{HandshakeSignatureValid, ServerCertVerified, ServerCertVerifier};
use rustls::crypto::CryptoProvider;
use rustls::pki_types::{CertificateDer, PrivateKeyDer, PrivatePkcs8KeyDer, ServerName, UnixTime};
use rustls::{DigitallySignedStruct, SignatureScheme};
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::mpsc;

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

// ---------------------------------------------------------------------------------------------
// Framing helpers
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

/// Reads one frame; returns (type, payload). `buf` is reused.
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

/// Paces writes to a fixed rate (0 = unlimited) by sleeping until the byte budget allows them.
struct Pacer {
    start: Instant,
    bytes: u64,
    rate_bps: f64,
}

const PACE_SLICE: usize = 16 * 1024;

impl Pacer {
    fn new(rate_mbps: u32) -> Self {
        Self {
            start: Instant::now(),
            bytes: 0,
            rate_bps: f64::from(rate_mbps) * 1e6,
        }
    }

    async fn write<W: AsyncWrite + Unpin>(&mut self, w: &mut W, data: &[u8]) -> Result<()> {
        if self.rate_bps == 0.0 {
            w.write_all(data).await?;
            return Ok(());
        }
        for part in data.chunks(PACE_SLICE) {
            self.bytes += part.len() as u64;
            let due = self.start + Duration::from_secs_f64(self.bytes as f64 * 8.0 / self.rate_bps);
            tokio::time::sleep_until(due.into()).await;
            w.write_all(part).await?;
        }
        Ok(())
    }
}

fn transport_config(cc: Cc) -> quinn::TransportConfig {
    let mut tc = quinn::TransportConfig::default();
    if cc == Cc::Bbr {
        tc.congestion_controller_factory(Arc::new(quinn::congestion::BbrConfig::default()));
    }
    tc
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Cc {
    Cubic,
    Bbr,
}

// ---------------------------------------------------------------------------------------------
// Server
// ---------------------------------------------------------------------------------------------

/// Echoes probe frames, discards everything else. With `reverse_bulk`, the server also streams
/// bulk chunks back through the same FIFO writer (single-connection baseline, reverse direction).
async fn serve_control<R, W>(mut r: R, w: W, reverse_bulk: bool) -> Result<()>
where
    R: AsyncRead + Unpin,
    W: AsyncWrite + Unpin + Send + 'static,
{
    let (tx, rx) = mpsc::channel::<Vec<u8>>(4);
    tokio::spawn(writer(w, rx));
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
            tx.send(frame(PROBE, &buf)).await?;
        }
    }
}

/// Streams bulk chunks at `rate_mbps` (0 = unlimited) until the peer goes away.
async fn send_bulk<W: AsyncWrite + Unpin>(mut w: W, rate_mbps: u32) -> Result<()> {
    let chunk = frame(CHUNK, &vec![0x5Au8; CHUNK_SIZE]);
    let mut pacer = Pacer::new(rate_mbps);
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
    let tls = server_tls()?;
    let acceptor = tokio_rustls::TlsAcceptor::from(Arc::new(tls.clone()));
    let listener = TcpListener::bind(bind).await?;

    // QUIC: Cubic on the base port, BBR on base port + 1.
    for (cc, port) in [(Cc::Cubic, bind.port()), (Cc::Bbr, bind.port() + 1)] {
        let quic_cfg = quinn::crypto::rustls::QuicServerConfig::try_from(tls.clone())?;
        let mut cfg = quinn::ServerConfig::with_crypto(Arc::new(quic_cfg));
        cfg.transport_config(Arc::new(transport_config(cc)));
        let endpoint = quinn::Endpoint::server(cfg, SocketAddr::new(bind.ip(), port))?;
        spawn_quic_server(endpoint);
    }
    eprintln!(
        "listening on tcp {bind}, udp {bind} (quic cubic), udp port {} (quic bbr)",
        bind.port() + 1
    );

    loop {
        let (sock, _) = listener.accept().await?;
        sock.set_nodelay(true)?;
        let acceptor = acceptor.clone();
        tokio::spawn(async move {
            let mut tls = acceptor.accept(sock).await?;
            let role = tls.read_u8().await?;
            match role {
                ROLE_SINGLE | ROLE_CONTROL | ROLE_SINGLE_REVERSE => {
                    let (r, w) = tokio::io::split(tls);
                    serve_control(r, w, role == ROLE_SINGLE_REVERSE).await
                }
                ROLE_BULK => drain(tls).await,
                ROLE_BULK_REVERSE => {
                    let rate = tls.read_u32_le().await?;
                    send_bulk(tls, rate).await
                }
                other => Err(format!("unknown role {other}").into()),
            }
        });
    }
}

fn spawn_quic_server(endpoint: quinn::Endpoint) {
    tokio::spawn(async move {
        while let Some(incoming) = endpoint.accept().await {
            tokio::spawn(async move {
                let conn = incoming.await?;
                let c2 = conn.clone();
                tokio::spawn(async move {
                    while let Ok(r) = c2.accept_uni().await {
                        tokio::spawn(drain(r));
                    }
                });
                while let Ok((s, mut r)) = conn.accept_bi().await {
                    tokio::spawn(async move {
                        match r.read_u8().await? {
                            ROLE_CONTROL => serve_control(r, s, false).await,
                            ROLE_BULK_REVERSE => {
                                let rate = r.read_u32_le().await?;
                                send_bulk(s, rate).await
                            }
                            other => Err(format!("unknown stream role {other}").into()),
                        }
                    });
                }
                Ok::<_, BoxError>(())
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

struct Args {
    connect: SocketAddr,
    transport: Transport,
    rate: u64,
    secs: u64,
    bulk: Bulk,
    bulk_rate: u32,
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

/// Reads echoed probes and records their RTT.
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
            if seq & 0x8000_0000 != 0 {
                sh.rtts.lock().unwrap().push(now - t);
            }
        }
    }
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

async fn bulk_direct<W: AsyncWrite + Unpin>(mut w: W, sh: Arc<Shared>, rate: u32) -> Result<()> {
    let chunk = frame(CHUNK, &vec![0xA5u8; CHUNK_SIZE]);
    let mut pacer = Pacer::new(rate);
    while !sh.stop.load(Ordering::Relaxed) {
        pacer.write(&mut w, &chunk).await?;
        if sh.measuring.load(Ordering::Relaxed) {
            sh.bulk_bytes
                .fetch_add(chunk.len() as u64, Ordering::Relaxed);
        }
    }
    Ok(())
}

async fn client(a: Args) -> Result<String> {
    let sh = Arc::new(Shared {
        start: Instant::now(),
        rtts: Mutex::new(Vec::new()),
        sent: AtomicU64::new(0),
        bulk_bytes: AtomicU64::new(0),
        measuring: AtomicBool::new(false),
        stop: AtomicBool::new(false),
    });

    // Control queue: the probe loop never blocks on the socket directly.
    let (ctl_tx, ctl_rx) = mpsc::channel::<Vec<u8>>(4096);
    let mut _quic_keepalive = None;

    match a.transport {
        Transport::Tcp1 => {
            let role = if a.bulk == Bulk::Reverse {
                ROLE_SINGLE_REVERSE
            } else {
                ROLE_SINGLE
            };
            let (r, w) = tokio::io::split(tls_connect(&a, role).await?);
            tokio::spawn(read_echoes(r, sh.clone()));
            // Single FIFO shared by probes and bulk: the naive design.
            let (tx, rx) = mpsc::channel::<Vec<u8>>(4);
            tokio::spawn(writer(w, rx));
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
            tokio::spawn(writer(w, ctl_rx));
            match a.bulk {
                Bulk::Saturate => {
                    let bulk = tls_connect(&a, ROLE_BULK).await?;
                    tokio::spawn(bulk_direct(bulk, sh.clone(), a.bulk_rate));
                }
                Bulk::Reverse => {
                    let mut bulk = tls_connect(&a, ROLE_BULK_REVERSE).await?;
                    bulk.write_u32_le(a.bulk_rate).await?;
                    bulk.flush().await?;
                    tokio::spawn(count_drain(bulk, sh.clone()));
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
            tokio::spawn(writer(s, ctl_rx));
            match a.bulk {
                Bulk::Saturate => {
                    let u = conn.open_uni().await?;
                    u.set_priority(0)?;
                    tokio::spawn(bulk_direct(u, sh.clone(), a.bulk_rate));
                }
                Bulk::Reverse => {
                    let (mut bs, br) = conn.open_bi().await?;
                    bs.write_all(&[ROLE_BULK_REVERSE]).await?;
                    bs.write_all(&a.bulk_rate.to_le_bytes()).await?;
                    tokio::spawn(count_drain(br, sh.clone()));
                    // Keep the send half alive for the duration of the run.
                    std::mem::forget(bs);
                }
                Bulk::None => {}
            }
            _quic_keepalive = Some((ep, conn));
        }
    }

    // Warm-up: let bulk reach steady state before measuring.
    tokio::time::sleep(Duration::from_secs(1)).await;
    sh.measuring.store(true, Ordering::Relaxed);
    let measure_start = Instant::now();
    let period = Duration::from_nanos(1_000_000_000 / a.rate);
    let mut tick = tokio::time::interval(period);
    tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    let n = a.rate * a.secs;
    // Wall-clock cap: a blocked shared queue (tcp1) must not stretch a run indefinitely.
    let cap = Duration::from_secs(a.secs * 4);
    for i in 0..n {
        if measure_start.elapsed() > cap {
            break;
        }
        tick.tick().await;
        let t = u64::try_from(sh.start.elapsed().as_nanos())?;
        // High bit marks measured probes.
        let seq = u32::try_from(i)? | 0x8000_0000;
        ctl_tx.send(probe_frame(seq, t)).await?;
        sh.sent.fetch_add(1, Ordering::Relaxed);
    }
    let elapsed = measure_start.elapsed().as_secs_f64();
    sh.measuring.store(false, Ordering::Relaxed);
    sh.stop.store(true, Ordering::Relaxed);
    // Grace period for in-flight echoes.
    tokio::time::sleep(Duration::from_secs(3)).await;
    // Close QUIC explicitly so the server stops sending before the next run starts.
    if let Some((ep, conn)) = _quic_keepalive.take() {
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
    let sent = sh.sent.load(Ordering::Relaxed);
    let mbps = sh.bulk_bytes.load(Ordering::Relaxed) as f64 * 8.0 / elapsed / 1e6;
    Ok(format!(
        "{{\"label\":\"{}\",\"transport\":\"{:?}\",\"bulk\":\"{:?}\",\"bulk_rate_mbps\":{},\"cc\":\"{:?}\",\"rate\":{},\"secs\":{},\"sent\":{},\"received\":{},\"rtt_us\":{{\"p50\":{:.0},\"p95\":{:.0},\"p99\":{:.0},\"p999\":{:.0},\"max\":{:.0}}},\"bulk_mbps\":{:.0}}}",
        a.label,
        a.transport,
        a.bulk,
        a.bulk_rate,
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
        mbps
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

#[tokio::main]
async fn main() -> Result<()> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    match args.first().map(String::as_str) {
        Some("server") => {
            let bind = opt(&args, "--bind").unwrap_or("0.0.0.0:47000").parse()?;
            server(bind).await
        }
        Some("client") => {
            let transport = match opt(&args, "--transport").unwrap_or("tcp2") {
                "tcp1" => Transport::Tcp1,
                "tcp2" => Transport::Tcp2,
                "quic" => Transport::Quic,
                other => return Err(format!("unknown transport {other}").into()),
            };
            let a = Args {
                connect: opt(&args, "--connect")
                    .ok_or("--connect required")?
                    .parse()?,
                transport,
                rate: opt(&args, "--rate").unwrap_or("1000").parse()?,
                secs: opt(&args, "--secs").unwrap_or("10").parse()?,
                bulk: match opt(&args, "--bulk").unwrap_or("none") {
                    "none" => Bulk::None,
                    "saturate" => Bulk::Saturate,
                    "reverse" => Bulk::Reverse,
                    other => return Err(format!("unknown bulk mode {other}").into()),
                },
                bulk_rate: opt(&args, "--bulk-rate").unwrap_or("0").parse()?,
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
            eprintln!(
                "usage: channels server [--bind ADDR] | channels client --connect ADDR --transport tcp1|tcp2|quic [--rate HZ] [--secs N] [--bulk none|saturate|reverse] [--bulk-rate MBPS] [--cc cubic|bbr] [--mss N] [--label TEXT]"
            );
            Ok(())
        }
    }
}
