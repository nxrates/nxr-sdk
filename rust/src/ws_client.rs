//! WebSocket client for the NXR binary quote stream.
//!
//! Third-party paying subscribers connect to `/v1/stream` on the NXR HTTP
//! server and receive batched `Index` snapshots over a binary WebSocket. The
//! wire format is the mirror of `core::server::ws::build_index_frame`:
//!
//! ```text
//! [0]   u8  msg_type   1 = index_batch
//! [1]   u8  _pad
//! [2-3] u16 count      number of records (little-endian)
//! [4-7] u32 _pad       align payload to 8-byte boundary
//! [8+]  count x 9 x f64 (little-endian)
//!       fields: epoch_ms, ticker, mid, bid, ask, ci_ubp, confidence, accepted, rejected
//! ```
//!
//! The client opens the socket, decodes every frame on the caller's task, and
//! hands back typed [`WsIndex`] batches. It is a thin wrapper so clients can
//! drop in an alternate transport (mock, replay) without touching decode
//! logic.
//!
//! [`WsClient::subscribe`] narrows the connection to a chosen set of symbols,
//! crosses included: the server composes a subscribed cross itself, so a client
//! never triangulates. Without it the connection keeps receiving the whole
//! native map, unchanged.

use std::time::Duration;

use anyhow::{Context, Result, bail};
use futures_util::{SinkExt, StreamExt};
use tokio::net::TcpStream;
use tokio_tungstenite::{
    MaybeTlsStream, WebSocketStream, connect_async_with_config, tungstenite::protocol::Message,
};
use tracing::{debug, info, warn};

use crate::ws_config::ws_config;

pub use crate::ws_frame::{FRAME_HEADER_BYTES, INDEX_RECORD_BYTES, INDEX_STRIDE, MSG_INDEX};

/// Default WebSocket keepalive. The server has no idle timeout but NAT paths
/// often do; ping every 30 s so a stateful firewall keeps the connection open.
pub const DEFAULT_KEEPALIVE: Duration = Duration::from_secs(30);

/// Decoded `Index` snapshot as carried over the binary WS frame.
///
/// All integer-valued fields (`ticker`, `confidence`, `accepted`, `rejected`)
/// are stored as `f64` on the wire for zero-copy browser access; this struct
/// reifies them back into their native types.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct WsIndex {
    /// Observation timestamp (epoch milliseconds, derived from MitchHeader.mts).
    pub epoch_ms: u64,
    /// MITCH ticker ID.
    pub ticker: u64,
    /// Midpoint in quote-currency units. `(bid + ask) / 2` server-side.
    pub mid: f64,
    /// Best bid.
    pub bid: f64,
    /// Best ask.
    pub ask: f64,
    /// Confidence interval in micro basis points of mid (`ci_ubp`).
    pub ci_ubp: f64,
    /// Active provider count.
    pub confidence: u32,
    /// Accepted ticks in the aggregation window.
    pub accepted: u32,
    /// Rejected ticks in the aggregation window.
    pub rejected: u32,
}

/// Low-level binary-frame decoder. Returns the number of records decoded,
/// pushing each onto `out`. Returns `Err` on malformed frames; callers should
/// log and drop the offending message.
pub(crate) fn decode_index_frame(bytes: &[u8], out: &mut Vec<WsIndex>) -> Result<usize> {
    if bytes.len() < FRAME_HEADER_BYTES {
        bail!("frame too short: {} < {}", bytes.len(), FRAME_HEADER_BYTES);
    }
    let msg_type = bytes[0];
    if msg_type != MSG_INDEX {
        bail!("unexpected msg_type {}: want {}", msg_type, MSG_INDEX);
    }
    let count = u16::from_le_bytes([bytes[2], bytes[3]]) as usize;
    let expected = FRAME_HEADER_BYTES + count * INDEX_RECORD_BYTES;
    if bytes.len() != expected {
        bail!(
            "frame size mismatch: got {} bytes for count={}, expected {}",
            bytes.len(),
            count,
            expected
        );
    }

    out.reserve(count);
    let mut off = FRAME_HEADER_BYTES;
    for _ in 0..count {
        let lanes: &[u8] = &bytes[off..off + INDEX_RECORD_BYTES];
        let f = |i: usize| -> f64 {
            let s = i * 8;
            f64::from_le_bytes(lanes[s..s + 8].try_into().unwrap())
        };
        out.push(WsIndex {
            epoch_ms: f(0) as u64,
            ticker: f(1) as u64,
            mid: f(2),
            bid: f(3),
            ask: f(4),
            ci_ubp: f(5),
            confidence: f(6) as u32,
            accepted: f(7) as u32,
            rejected: f(8) as u32,
        });
        off += INDEX_RECORD_BYTES;
    }
    Ok(count)
}

/// Connected WebSocket client that yields decoded [`WsIndex`] batches.
///
/// Construct with [`WsClient::connect`] and call [`WsClient::next_batch`] in a
/// loop. The underlying socket is a `tokio-tungstenite` stream so the caller
/// can reuse the same tokio runtime as the rest of their app.
pub struct WsClient {
    inner: WebSocketStream<MaybeTlsStream<TcpStream>>,
    keepalive: Duration,
    last_ping: tokio::time::Instant,
    /// Binary frames received while awaiting a subscription ack. Drained by
    /// [`WsClient::next_batch`] before the socket is read again, so subscribing
    /// mid-stream never drops a tick.
    pending: std::collections::VecDeque<Vec<u8>>,
}

/// Persistent random device id sent as `client_id` on `/v1/stream`. The
/// anonymous WS cap is per client id (2) with a higher per-IP ceiling, so
/// users behind one NAT do not starve each other. Order: env `NXR_CLIENT_ID`,
/// `~/.nxr/client_id`, else a fresh id persisted there (best effort).
pub fn client_id() -> String {
    let valid = |s: &str| {
        (8..=64).contains(&s.len())
            && s.bytes()
                .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
    };
    if let Ok(v) = std::env::var("NXR_CLIENT_ID")
        && valid(&v)
    {
        return v;
    }
    let path = std::env::var_os("HOME").map(|h| std::path::Path::new(&h).join(".nxr/client_id"));
    if let Some(Ok(id)) = path.as_ref().map(std::fs::read_to_string) {
        let id = id.trim();
        if valid(id) {
            return id.to_string();
        }
    }
    use std::hash::{BuildHasher, Hasher};
    let rnd = |salt: u64| {
        let mut h = std::collections::hash_map::RandomState::new().build_hasher();
        h.write_u64(salt);
        h.finish()
    };
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_nanos() as u64);
    let id = format!("{:016x}{:016x}", rnd(nanos), rnd(!nanos));
    if let Some(p) = path {
        if let Some(dir) = p.parent() {
            let _ = std::fs::create_dir_all(dir);
        }
        let _ = std::fs::write(&p, &id);
    }
    id
}

/// Append `client_id=<id>` to `url` unless it already carries one.
pub fn with_client_id(url: &str, id: &str) -> String {
    if url.contains("client_id=") {
        return url.to_string();
    }
    let sep = if url.contains('?') { '&' } else { '?' };
    format!("{url}{sep}client_id={id}")
}

/// `scheme://host[:port]` only (default port dropped, IPv6 brackets kept). Allowlist rebuild like
/// `btr_rpc::redact_url`, stricter: only validated pieces are echoed, and any `@` after the scheme
/// (userinfo, or a path/query `@` that may hide a raw password) redacts the whole authority.
pub(crate) fn redact_url(url: &str) -> String {
    const NONE: &str = "<redacted-url>";
    let Some((scheme, rest)) = url.split_once("://") else { return NONE.into() };
    let mut sc = scheme.bytes();
    if !sc.next().is_some_and(|b| b.is_ascii_alphabetic())
        || !sc.all(|b| b.is_ascii_alphanumeric() || matches!(b, b'+' | b'.' | b'-'))
    {
        return NONE.into();
    }
    let scheme = scheme.to_ascii_lowercase();
    let hidden = format!("{scheme}://<redacted>");
    let auth = rest.split(['/', '?', '#']).next().unwrap_or("");
    if rest.contains(['@', '\\']) {
        return hidden;
    }
    let (host, port) = if let Some(v6) = auth.strip_prefix('[') {
        match v6.split_once(']') {
            Some((h, t)) if !h.is_empty() && h.bytes().all(|b| b.is_ascii_hexdigit() || matches!(b, b':' | b'.')) => {
                (format!("[{}]", h.to_ascii_lowercase()), t.strip_prefix(':').map(str::to_string).or_else(|| t.is_empty().then(String::new)))
            }
            _ => return hidden,
        }
    } else {
        match auth.rsplit_once(':') {
            Some((h, p)) => (h.to_ascii_lowercase(), Some(p.to_string())),
            None => (auth.to_ascii_lowercase(), None),
        }
    };
    let host_ok = host.starts_with('[')
        || (!host.is_empty() && host.bytes().all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'-')));
    let port = port.unwrap_or_default();
    let port_ok = port.bytes().all(|b| b.is_ascii_digit());
    if !host_ok || !port_ok {
        return hidden;
    }
    let dflt = match scheme.as_str() {
        "http" | "ws" => "80",
        "https" | "wss" => "443",
        _ => "",
    };
    if port.is_empty() || port == dflt {
        format!("{scheme}://{host}")
    } else {
        format!("{scheme}://{host}:{port}")
    }
}

impl WsClient {
    /// Open a WebSocket to `url` (e.g. `ws://nxr-svc:40004/v1/stream`). A
    /// persistent [`client_id`] is appended unless the url already has one.
    pub async fn connect(url: &str) -> Result<Self> {
        let url = &with_client_id(url, &client_id());
        // Same inbound caps and TCP_NODELAY as the CEX forwarder: one config,
        // one place to audit (nxr_sdk::ws_config).
        let (ws, _response) = connect_async_with_config(url, Some(ws_config()), true)
            .await
            .with_context(|| format!("connect {}", redact_url(url)))?;
        info!(url = %redact_url(url), "NXR WS client connected");
        Ok(Self {
            inner: ws,
            keepalive: DEFAULT_KEEPALIVE,
            last_ping: tokio::time::Instant::now(),
            pending: std::collections::VecDeque::new(),
        })
    }

    /// Ask for exactly `ids` (crosses included) instead of the whole native
    /// map, and return the ids the server accepted.
    ///
    /// A cross arrives ALREADY COMPOSED, identical to `GET /v1/price` for the
    /// same instant: never triangulate a stream client-side. An id the server
    /// cannot route is reported in the ack and is NOT subscribed, so a typo
    /// fails loudly instead of never delivering. Subscribing nothing (or
    /// unsubscribing everything) leaves the connection on the full broadcast.
    pub async fn subscribe(&mut self, ids: &[&str]) -> Result<Vec<String>> {
        self.send_sub("sub", ids).await
    }

    /// Drop `ids` from this connection's set. Emptying it returns the
    /// connection to the all-ticker broadcast.
    pub async fn unsubscribe(&mut self, ids: &[&str]) -> Result<Vec<String>> {
        self.send_sub("unsub", ids).await
    }

    async fn send_sub(&mut self, op: &str, ids: &[&str]) -> Result<Vec<String>> {
        // `filter: true` is the OPT-IN into the filtered lane. A `sub` without
        // it is accounting-only and leaves the client on the full broadcast.
        let env =
            serde_json::json!({ "type": op, "kind": "idx", "ids": ids, "filter": true }).to_string();
        self.inner
            .send(Message::Text(env.into()))
            .await
            .context("WS send sub")?;
        // Read to the ack, stashing any binary frame that overtakes it.
        loop {
            match self.inner.next().await {
                Some(Ok(Message::Text(t))) => {
                    let ack: serde_json::Value =
                        serde_json::from_str(&t).context("WS sub ack parse")?;
                    if let Some(e) = ack.get("error").and_then(|v| v.as_str()) {
                        bail!("WS sub rejected: {e}");
                    }
                    for r in ack["rejected"].as_array().unwrap_or(&Vec::new()) {
                        warn!(id = %r["id"], err = %r["error"], "WS: id not subscribed");
                    }
                    return Ok(ack["subscribed"]
                        .as_array()
                        .map(|a| {
                            a.iter()
                                .filter_map(|v| v.as_str().map(str::to_string))
                                .collect()
                        })
                        .unwrap_or_default());
                }
                Some(Ok(Message::Binary(data))) => self.pending.push_back(data.into()),
                Some(Ok(Message::Ping(p))) => {
                    self.inner.send(Message::Pong(p)).await.ok();
                }
                Some(Ok(Message::Close(_))) | None => bail!("WS closed awaiting sub ack"),
                Some(Err(e)) => return Err(e).context("WS recv"),
                _ => {}
            }
        }
    }

    /// Override the ping cadence used to keep NAT sessions open. Default is
    /// 30 s; set higher if the path is firewall-free.
    pub fn with_keepalive(mut self, keepalive: Duration) -> Self {
        self.keepalive = keepalive;
        self
    }

    /// Await the next batch of `Index` snapshots. Returns `Ok(None)` when the
    /// server closes the stream. Malformed frames are logged and skipped so
    /// one bad message never tears down a long-lived subscription.
    pub async fn next_batch(&mut self) -> Result<Option<Vec<WsIndex>>> {
        loop {
            if let Some(data) = self.pending.pop_front() {
                let mut batch = Vec::new();
                match decode_index_frame(&data, &mut batch) {
                    Ok(_) => return Ok(Some(batch)),
                    Err(e) => {
                        warn!(err = %e, len = data.len(), "WS: dropping malformed frame");
                        continue;
                    }
                }
            }
            if self.last_ping.elapsed() >= self.keepalive {
                self.inner.send(Message::Ping(Vec::new().into())).await.ok();
                self.last_ping = tokio::time::Instant::now();
            }
            match self.inner.next().await {
                Some(Ok(Message::Binary(data))) => {
                    let mut batch = Vec::new();
                    match decode_index_frame(&data, &mut batch) {
                        Ok(_) => return Ok(Some(batch)),
                        Err(e) => {
                            warn!(err = %e, len = data.len(), "WS: dropping malformed frame");
                            continue;
                        }
                    }
                }
                Some(Ok(Message::Ping(p))) => {
                    self.inner.send(Message::Pong(p)).await.ok();
                }
                Some(Ok(Message::Pong(_))) | Some(Ok(Message::Text(_))) => {
                    debug!("WS: ignoring non-binary control frame");
                }
                Some(Ok(Message::Close(_))) | None => {
                    info!("NXR WS stream closed");
                    return Ok(None);
                }
                Some(Ok(Message::Frame(_))) => { /* raw frames never surface via next() */ }
                Some(Err(e)) => return Err(e).context("WS recv"),
            }
        }
    }

    /// Close the socket cleanly.
    pub async fn close(mut self) -> Result<()> {
        self.inner.close(None).await.context("WS close")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn redact_url_keeps_scheme_host_port_only() {
        for (raw, want) in [
            ("wss://ws-api.kucoin.com/endpoint?token=SECRET&client_id=x", "wss://ws-api.kucoin.com"),
            ("ws://h:40004/v1/stream?k=S", "ws://h:40004"),
            ("ws://h:1#frag", "ws://h:1"),
            ("wss://h:443/p?t=S", "wss://h"),
            ("ws://H:80/", "ws://h"),
            ("ws://[::1]:40004/v1?t=S", "ws://[::1]:40004"),
            ("ws://[::1]/v1", "ws://[::1]"),
            // any `@` after the scheme, wherever it sits, hides the host too
            ("ws://u:p@h:40004/v1/stream?k=S", "ws://<redacted>"),
            ("wss://h/p?x=a@SECRET", "wss://<redacted>"),
            ("https://rpc.example/key@abc123", "https://<redacted>"),
            ("ws://[::1]/p?t=a@SECRET", "ws://<redacted>"),
            ("http://user:pa/ss@1.2.3.4:8080", "http://<redacted>"),
            ("http://h:x/p?a@S", "http://<redacted>"),
            ("ws://user:123/rest@host:9", "ws://<redacted>"),
            ("ws://h%40SECRET:1/", "ws://<redacted>"),
            ("ws://SECRET\\h:1/", "ws://<redacted>"),
            ("ws://[::1@SECRET]:1/", "ws://<redacted>"),
            ("host/?u=http://sec@x", "<redacted-url>"),
            ("tok3n", "<redacted-url>"),
            ("h:40004/p?t=S", "<redacted-url>"),
        ] {
            let got = redact_url(raw);
            assert_eq!(got, want, "{raw}");
            for secret in ["SECRET", "sec", "user", "123"] {
                assert!(!(raw.contains(secret) && want.contains("redacted") && got.contains(secret)), "{raw} leaked {secret}");
            }
        }
    }

    fn encode(records: &[WsIndex]) -> Vec<u8> {
        let mut buf = Vec::with_capacity(FRAME_HEADER_BYTES + records.len() * INDEX_RECORD_BYTES);
        buf.push(MSG_INDEX);
        buf.push(0);
        buf.extend_from_slice(&(records.len() as u16).to_le_bytes());
        buf.extend_from_slice(&[0u8; 4]);
        for r in records {
            for f in [
                r.epoch_ms as f64,
                r.ticker as f64,
                r.mid,
                r.bid,
                r.ask,
                r.ci_ubp,
                r.confidence as f64,
                r.accepted as f64,
                r.rejected as f64,
            ] {
                buf.extend_from_slice(&f.to_le_bytes());
            }
        }
        buf
    }

    #[test]
    fn decode_round_trip_single() {
        let input = vec![WsIndex {
            epoch_ms: 1_700_000_000_000,
            ticker: 0xABCDEF,
            mid: 50_000.5,
            bid: 50_000.0,
            ask: 50_001.0,
            ci_ubp: 12.5,
            confidence: 7,
            accepted: 42,
            rejected: 3,
        }];
        let bytes = encode(&input);
        let mut out = Vec::new();
        assert_eq!(decode_index_frame(&bytes, &mut out).unwrap(), 1);
        assert_eq!(out, input);
    }

    #[test]
    fn decode_round_trip_batch() {
        let input: Vec<WsIndex> = (0..8)
            .map(|i| WsIndex {
                epoch_ms: 1_700_000_000_000 + i * 100,
                ticker: 1000 + i,
                mid: 100.0 + i as f64,
                bid: 99.5 + i as f64,
                ask: 100.5 + i as f64,
                ci_ubp: 1.0,
                confidence: 5,
                accepted: 10,
                rejected: 0,
            })
            .collect();
        let bytes = encode(&input);
        let mut out = Vec::new();
        assert_eq!(decode_index_frame(&bytes, &mut out).unwrap(), 8);
        assert_eq!(out, input);
    }

    #[test]
    fn decode_rejects_wrong_msg_type() {
        let mut bytes = vec![0u8; FRAME_HEADER_BYTES];
        bytes[0] = 99;
        let mut out = Vec::new();
        assert!(decode_index_frame(&bytes, &mut out).is_err());
    }

    #[test]
    fn decode_rejects_truncated() {
        let bytes = vec![MSG_INDEX, 0, 1, 0, 0, 0, 0, 0];
        let mut out = Vec::new();
        assert!(decode_index_frame(&bytes, &mut out).is_err());
    }
}

#[cfg(test)]
mod client_id_tests {
    use super::*;

    #[test]
    fn appends_once() {
        let w = |u: &str| with_client_id(u, "abc");
        assert_eq!(w("ws://h/s"), "ws://h/s?client_id=abc");
        assert_eq!(w("ws://h/s?v=2"), "ws://h/s?v=2&client_id=abc");
        assert_eq!(w("ws://h/s?client_id=x"), "ws://h/s?client_id=x");
    }

    #[test]
    fn generated_id_is_valid_for_the_server() {
        let id = client_id();
        assert!((8..=64).contains(&id.len()));
        let ok = |b: u8| b.is_ascii_alphanumeric() || b == b'-' || b == b'_';
        assert!(id.bytes().all(ok));
    }
}
