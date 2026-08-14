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
    MaybeTlsStream, WebSocketStream, connect_async, tungstenite::protocol::Message,
};
use tracing::{debug, info, warn};

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

impl WsClient {
    /// Open a WebSocket to `url` (e.g. `ws://nxr-svc:40004/v1/stream`).
    pub async fn connect(url: &str) -> Result<Self> {
        let (ws, _response) = connect_async(url)
            .await
            .with_context(|| format!("connect {}", url))?;
        info!(%url, "NXR WS client connected");
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
        let env = serde_json::json!({ "type": op, "kind": "idx", "ids": ids }).to_string();
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
                epoch_ms: 1_700_000_000_000 + i as u64 * 100,
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
