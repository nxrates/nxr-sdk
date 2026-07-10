//! Transport-agnostic frame sinks and sources used by the SDK publisher and
//! consumer.
//!
//! The SDK publisher sends each outbound frame to every registered sink (e.g.
//! two redundant UDP multicast groups for CME-style A/B arbitration). The SDK
//! consumer reads from one or more sources and dedupes by `(mts, sequence)`.
//! Both halves are parametric over the transport: UDP multicast today, but
//! the same traits are intended to cover TCP unicast and future extensions
//! without touching the publisher/consumer core.
//!
//! Transport implementations MUST be `Send + Sync` so they can be shared
//! across tasks and held behind a `tokio::spawn`.

use std::future::Future;
use std::net::{Ipv4Addr, SocketAddr};

use anyhow::Result;
use socket2::{Domain, Protocol, Socket, Type};
use tokio::net::UdpSocket;
use tracing::{info, warn};

/// Async sink that writes an opaque byte frame to a transport.
///
/// The frame contents are a producer's responsibility; this trait is the
/// narrow waist every transport must implement. Errors surface per-send so
/// the publisher can throttle logging without tearing down the socket.
pub trait FrameSink: Send + Sync {
    /// Send a single frame. The implementation may be lossy (UDP) or reliable
    /// (TCP); callers treat transient errors as transport-specific noise and
    /// keep running.
    fn send(&self, bytes: &[u8]) -> impl Future<Output = Result<()>> + Send;

    /// Short label used in logs and metrics (`"A"`, `"B"`, `"ws"`). Must be
    /// stable for the lifetime of the sink.
    fn label(&self) -> &'static str;
}

/// Async source that yields opaque byte frames from a transport.
///
/// Each `recv` returns one complete frame. Implementations are responsible
/// for framing: UDP datagrams are already frame-sized, TCP readers must
/// buffer and split on their wire boundary.
pub trait FrameSource: Send {
    /// Read the next frame. Returns `None` when the transport closes.
    fn recv(&mut self) -> impl Future<Output = Result<Option<Vec<u8>>>> + Send;

    /// Short label for logs and metrics.
    fn label(&self) -> &'static str;
}

// =============================================================================
// UDP MULTICAST SINK
// =============================================================================

/// Target SO_SNDBUF used by [`UdpMulticastSink`]. Sized for ~100 ms of
/// sustained 1M msg/s at 56 B/frame; the kernel silently clamps against
/// `net.core.wmem_max` so startup logs show the requested value for ops
/// visibility.
pub const UDP_SEND_BUFFER_BYTES: usize = 6 * 1024 * 1024;

/// UDP multicast sink: one socket per redundant feed (A and B run two
/// independent instances on distinct multicast groups).
///
/// Sends use `send_to` against a fixed `SocketAddr`; the kernel routes to the
/// right multicast group based on the destination address. `IP_MULTICAST_LOOP`
/// is disabled so the sender does not receive its own datagrams, and
/// `IP_MULTICAST_TTL = 1` keeps traffic LAN-local (consumers must share the
/// broadcast domain).
pub struct UdpMulticastSink {
    socket: UdpSocket,
    dest: SocketAddr,
    label: &'static str,
}

impl UdpMulticastSink {
    /// Bind a nonblocking UDP socket on an ephemeral local port and prepare it
    /// for multicast sends to `dest`. `label` should be `"A"` or `"B"` for the
    /// redundant feed arbitration pattern.
    pub fn bind(dest: SocketAddr, label: &'static str) -> Result<Self> {
        let raw = Socket::new(Domain::IPV4, Type::DGRAM, Some(Protocol::UDP))?;
        raw.set_nonblocking(true)?;
        raw.set_multicast_ttl_v4(1)?;
        raw.set_multicast_loop_v4(false)?;
        if let Err(e) = raw.set_send_buffer_size(UDP_SEND_BUFFER_BYTES) {
            warn!(%e, requested = UDP_SEND_BUFFER_BYTES, label, "SO_SNDBUF tuning failed; kernel default used");
        }
        raw.bind(&SocketAddr::from(([0, 0, 0, 0], 0)).into())?;
        let socket = UdpSocket::from_std(raw.into())?;
        info!(%dest, label, send_buf = UDP_SEND_BUFFER_BYTES, "UDP multicast sink ready");
        Ok(Self { socket, dest, label })
    }
}

impl FrameSink for UdpMulticastSink {
    fn send(&self, bytes: &[u8]) -> impl Future<Output = Result<()>> + Send {
        let socket = &self.socket;
        let dest = self.dest;
        async move {
            socket.send_to(bytes, dest).await?;
            Ok(())
        }
    }

    fn label(&self) -> &'static str {
        self.label
    }
}

// =============================================================================
// UDP RECEIVE TUNING (shared by multicast and unicast listeners)
// =============================================================================

/// Target SO_RCVBUF for any UDP listener in this stack. Matches the sink so
/// the receive path can absorb bursts from a saturated sender without drops.
/// Requires `net.core.rmem_max >= UDP_RECV_BUFFER_BYTES` on Linux; the kernel
/// silently clamps otherwise, so callers log the REQUESTED value for ops
/// visibility rather than the actual kernel-granted size.
pub const UDP_RECV_BUFFER_BYTES: usize = 6 * 1024 * 1024;

/// Build a nonblocking IPv4 UDP socket with receive-side tuning applied:
/// SO_REUSEADDR, SO_REUSEPORT (Unix), SO_RCVBUF=[`UDP_RECV_BUFFER_BYTES`], then
/// bind to `bind_addr`. Used by both multicast joins and unicast listeners so
/// there is exactly one place to audit receive-path tuning.
///
/// Caller is responsible for any further setup (e.g. `join_multicast_v4`)
/// before wrapping the returned socket in `tokio::net::UdpSocket::from_std`.
fn build_rx_socket(bind_addr: SocketAddr, label: &str) -> Result<Socket> {
    let raw = Socket::new(Domain::IPV4, Type::DGRAM, Some(Protocol::UDP))?;
    raw.set_nonblocking(true)?;
    raw.set_reuse_address(true)?;
    // SO_REUSEPORT lets a restarted consumer bind the same port without
    // waiting out the prior instance's TIME_WAIT. Unix-only; best-effort warn.
    #[cfg(unix)]
    if let Err(e) = raw.set_reuse_port(true) {
        warn!(%e, label, "SO_REUSEPORT not supported; fast-restart bind may fail");
    }
    if let Err(e) = raw.set_recv_buffer_size(UDP_RECV_BUFFER_BYTES) {
        warn!(%e, requested = UDP_RECV_BUFFER_BYTES, label, "SO_RCVBUF tuning failed; kernel default used");
    }
    raw.bind(&bind_addr.into())?;
    Ok(raw)
}

/// Bind a tuned UDP unicast listener on `0.0.0.0:port` and return a ready
/// `tokio::net::UdpSocket`. Matches the tuning applied by
/// [`UdpMulticastSource::join`] so multicast and unicast ingest paths have
/// identical buffer sizes, reuse semantics, and logging.
pub fn bind_udp_listener(port: u16, label: &'static str) -> Result<UdpSocket> {
    let bind_addr = SocketAddr::from(([0, 0, 0, 0], port));
    let raw = build_rx_socket(bind_addr, label)?;
    let socket = UdpSocket::from_std(raw.into())?;
    info!(%bind_addr, label, recv_buf = UDP_RECV_BUFFER_BYTES, "UDP unicast listener ready");
    Ok(socket)
}

// =============================================================================
// UDP MULTICAST SOURCE
// =============================================================================

/// UDP multicast receiver bound to a specific multicast group. The consumer
/// runs one instance per redundant feed (A and B) and dedupes downstream.
pub struct UdpMulticastSource {
    socket: UdpSocket,
    buf: Vec<u8>,
    label: &'static str,
}

impl UdpMulticastSource {
    /// Bind, join the multicast group on the requested interface, and prepare
    /// the socket for `recv_from`. Pass `interface = Ipv4Addr::UNSPECIFIED`
    /// for the default interface.
    ///
    /// `max_frame` caps the read buffer. Anything larger than the biggest
    /// MITCH frame you plan to consume is fine; 2048 comfortably holds the
    /// order-book frame (`2088` minus one allocation on overflow).
    pub fn join(
        group: Ipv4Addr,
        port: u16,
        interface: Ipv4Addr,
        label: &'static str,
        max_frame: usize,
    ) -> Result<Self> {
        let bind_addr = SocketAddr::from(([0, 0, 0, 0], port));
        let raw = build_rx_socket(bind_addr, label)?;
        raw.join_multicast_v4(&group, &interface)?;
        let socket = UdpSocket::from_std(raw.into())?;
        info!(%group, port, %interface, label, recv_buf = UDP_RECV_BUFFER_BYTES, "UDP multicast source ready");
        Ok(Self {
            socket,
            buf: vec![0u8; max_frame],
            label,
        })
    }
}

impl FrameSource for UdpMulticastSource {
    fn recv(&mut self) -> impl Future<Output = Result<Option<Vec<u8>>>> + Send {
        async move {
            // tokio's async UdpSocket::recv never surfaces WouldBlock to user
            // code — the runtime swallows it and reschedules the task. Any
            // error that actually reaches here is real (socket closed, ICMP
            // unreachable, interface down) and should propagate so the
            // supervisor can react instead of being laundered into an empty
            // frame that silently pollutes the dedup ring.
            let n = self.socket.recv(&mut self.buf).await?;
            Ok(Some(self.buf[..n].to_vec()))
        }
    }

    fn label(&self) -> &'static str {
        self.label
    }
}

// ─── Bar-feed multicast resolution (shared by bars_s10 + bars_renko) ─────

/// Parse `"a.b.c.d"` into `[u8;4]`. Returns `None` on malformed input.
///
/// Lifted from `core::bars_{s10,renko}` 2026-06-01 to kill the dup.
pub fn parse_v4_addr(s: &str) -> Option<[u8; 4]> {
    let parts: Vec<u8> = s.split('.').filter_map(|p| p.parse().ok()).collect();
    if parts.len() == 4 { Some([parts[0], parts[1], parts[2], parts[3]]) } else { None }
}

/// Bar-feed flavor for multicast resolution. Replaces the per-file
/// `DEFAULT_S10_*` / `DEFAULT_RENKO_*` const + `resolve_*_mcast()` pair.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BarKind {
    S10,
    Renko,
}

impl BarKind {
    /// Producer label used in metric tags. Synth and native share the same
    /// multicast leg + label (operator 2026-06-01: "s10_synth and s10 are
    /// the same object"). Consumers filter by ticker_id payload.
    #[inline]
    pub fn label(self) -> &'static str {
        match self {
            BarKind::S10 => "s10",
            BarKind::Renko => "renko",
        }
    }

    /// Resolve UDP multicast (addr, port) for this bar feed.
    ///
    /// Reads `NXR_CONFIG` once per process (OnceLock-cached). Falls back to
    /// audit-frozen defaults when YAML is silent or unparseable.
    pub fn resolve_mcast(self) -> ([u8; 4], u16) {
        use crate::pipeline_config::{BarsMcastYml, ConfigHint, PipelineYml};
        use std::sync::OnceLock;
        static CFG: OnceLock<BarsMcastYml> = OnceLock::new();
        let bars = CFG.get_or_init(|| {
            PipelineYml::load_default(ConfigHint::Runtime)
                .map(|p| p.network.bars)
                .unwrap_or_default()
        });
        match self {
            BarKind::S10 => (
                bars.s10_addr.as_deref().and_then(parse_v4_addr).unwrap_or([239, 0, 42, 3]),
                bars.s10_port.unwrap_or(40008),
            ),
            BarKind::Renko => (
                bars.renko_addr.as_deref().and_then(parse_v4_addr).unwrap_or([239, 0, 42, 4]),
                bars.renko_port.unwrap_or(40009),
            ),
        }
    }
}

/// Bar producer source: full-fanout `Native`, single-ticker `Synth(id)`, or
/// full-fanout `SynthAll` (many synth ids, one shared writer thread). All
/// three share the same multicast leg + label per BarKind.
///
/// `SynthAll` exists because the legacy `NXR_SYNTH_PIPELINE` used to spawn
/// one `Synth(id)` producer PER pair (main.rs looping `bars_s10::spawn` /
/// `bars_renko::spawn` once per synth_id) - each call opens its own
/// dedicated OS writer thread (`shard_writer::spawn_writer`) even though
/// that thread's `writers: FxHashMap<u64, _>` already supports many
/// tickers. RCA 2026-07-10: 52 synth pairs x 4 threads (s10 writer + s10
/// watchdog + renko writer + renko watchdog) = 208 extra OS threads,
/// pushing total process thread count to 229 - a strong contributor to the
/// live OOM crash-loop (glibc allocates per-thread malloc arenas; virtual
/// memory stayed ~constant ~11.5GB independent of the cgroup limit, the
/// signature of thread-driven arena bloat rather than a data leak). `Synth
/// (u64)` is kept for the existing single-ticker test coverage; the real
/// pipeline now spawns exactly ONE `SynthAll` producer (like `Native`) that
/// demuxes all synth ids through the same per-ticker `TickerState` map.
#[derive(Debug, Clone, Copy)]
pub enum BarSource {
    Native,
    Synth(u64),
    SynthAll,
}
