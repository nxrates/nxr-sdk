//! Consumer transports: REST, WebSocket binary, UDP multicast.
//!
//! | Transport | Type | Latency | Use case |
//! |-----------|------|---------|----------|
//! | [`NxrClient`] | REST/JSON | ~10ms | Metadata, snapshots, health checks |
//! | [`WsStream`] | WebSocket binary | ~1ms | Real-time streaming over internet |
//! | [`MulticastStream`] | UDP multicast | ~5µs | Cross-host LAN raw MITCH frames |

use std::collections::HashMap;
use std::marker::PhantomData;
use std::net::Ipv4Addr;

use anyhow::{Result, Context};
use bytemuck::Pod;
use futures_util::StreamExt;
use serde::Deserialize;
use tokio::net::UdpSocket;
use tokio_tungstenite::tungstenite::protocol::Message;
use tracing::{error, info, warn};

/// Default multicast group for NXR index snapshots.
pub const DEFAULT_MCAST_ADDR: Ipv4Addr = Ipv4Addr::new(239, 0, 42, 1);
/// Default multicast port for index snapshots.
pub const DEFAULT_MCAST_PORT: u16 = 40006;

// ─── REST client ────────────────────────────────────────────────────────────

/// Ticker snapshot from `/v1/tickers`.
#[derive(Debug, Clone, Deserialize)]
pub struct TickerResponse {
    pub ticker_id: u64,
    pub mid: f64,
    pub bid: f64,
    pub ask: f64,
    pub ci: f64,
    pub confidence: u8,
}

/// NX Rates REST client for metadata and point-in-time queries.
///
/// For live streaming data, use [`WsStream`] or [`MulticastStream`] instead.
pub struct NxrClient {
    base_url: String,
}

impl NxrClient {
    /// Create a new REST client with a base URL (e.g. `"https://api.nxrates.io"`).
    pub fn new(base_url: &str) -> Self {
        Self { base_url: base_url.trim_end_matches('/').to_string() }
    }

    /// Create a REST client from host and port (e.g. `"nxr-svc", 40004`).
    pub fn from_host(host: &str, port: u16) -> Self {
        Self { base_url: format!("http://{}:{}", host, port) }
    }

    /// Resolve a unified symbol (e.g. "BTC/USDT") to its MITCH ticker_id.
    pub async fn resolve(&self, symbol: &str) -> Result<u64> {
        let map = self.symbols().await?;
        map.get(symbol)
            .copied()
            .ok_or_else(|| anyhow::anyhow!("symbol {symbol} not found in NXR"))
    }

    /// Fetch the full symbol → ticker_id map.
    pub async fn symbols(&self) -> Result<HashMap<String, u64>> {
        Ok(reqwest::get(format!("{}/v1/symbols", self.base_url)).await?.json().await?)
    }

    /// Fetch provider_id → name map.
    pub async fn providers(&self) -> Result<HashMap<u16, String>> {
        Ok(reqwest::get(format!("{}/v1/providers", self.base_url)).await?.json().await?)
    }

    /// Get all active ticker snapshots.
    pub async fn tickers(&self) -> Result<Vec<TickerResponse>> {
        Ok(reqwest::get(format!("{}/v1/tickers", self.base_url)).await?.json().await?)
    }

    /// Health check: returns `true` if the NXR service is up.
    pub async fn is_healthy(&self) -> bool {
        reqwest::get(format!("{}/health", self.base_url))
            .await
            .map(|r| r.status().is_success())
            .unwrap_or(false)
    }
}

// ─── WebSocket binary stream ────────────────────────────────────────────────

/// WS binary frame type tags.
const WS_TYPE_INDEX: u8 = 1;
const WS_TYPE_TICK: u8 = 2;

/// Number of f64 fields per Index record in a WS frame.
const WS_INDEX_STRIDE: usize = 9;
/// Number of f64 fields per Tick record in a WS frame.
const WS_TICK_STRIDE: usize = 6;

/// Decoded Index record from a WebSocket binary frame.
#[derive(Debug, Clone, Copy)]
pub struct WsIndex {
    pub ts_ms: f64,
    pub ticker: f64,
    pub mid: f64,
    pub bid: f64,
    pub ask: f64,
    pub ci: f64,
    pub confidence: f64,
    pub accepted: f64,
    pub rejected: f64,
}

/// Decoded Tick record from a WebSocket binary frame.
#[derive(Debug, Clone, Copy)]
pub struct WsTick {
    pub ts_ms: f64,
    pub ticker: f64,
    pub bid: f64,
    pub ask: f64,
    pub provider: f64,
    pub flags: f64,
}

/// A decoded WebSocket message containing either Index or Tick records.
#[derive(Debug, Clone)]
pub enum WsMessage {
    Index(Vec<WsIndex>),
    Tick(Vec<WsTick>),
}

type WsStreamInner = tokio_tungstenite::WebSocketStream<
    tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>,
>;

/// Real-time binary market data stream over WebSocket.
///
/// Connects to an NXR WebSocket endpoint and decodes binary frames into
/// [`WsIndex`] and [`WsTick`] records.
pub struct WsStream {
    inner: WsStreamInner,
}

impl WsStream {
    /// Connect to a WebSocket URL (e.g. `"wss://ws.nxrates.io/v1/stream"`).
    pub async fn connect(url: &str) -> Result<Self> {
        let (ws, _resp) = tokio_tungstenite::connect_async(url)
            .await
            .with_context(|| format!("failed to connect to {url}"))?;
        info!(url, "websocket connected");
        Ok(Self { inner: ws })
    }

    /// Receive the next decoded message. Returns `None` when the connection closes.
    pub async fn recv(&mut self) -> Option<WsMessage> {
        loop {
            let msg = self.inner.next().await?;
            match msg {
                Ok(Message::Binary(data)) => {
                    match Self::decode_frame(&data) {
                        Some(m) => return Some(m),
                        None => continue,
                    }
                }
                Ok(Message::Ping(_)) => continue,
                Ok(Message::Close(_)) => return None,
                Ok(_) => continue,
                Err(e) => {
                    error!(%e, "websocket error");
                    return None;
                }
            }
        }
    }

    /// Decode a binary frame into a [`WsMessage`].
    ///
    /// Frame layout: `[u8 type][u8 pad][u16le count][4B reserved][count * stride * 8B f64s]`
    fn decode_frame(data: &[u8]) -> Option<WsMessage> {
        if data.len() < 8 {
            warn!(len = data.len(), "ws frame too short for header");
            return None;
        }

        let msg_type = data[0];
        let count = u16::from_le_bytes([data[2], data[3]]) as usize;
        let payload = &data[8..];

        match msg_type {
            WS_TYPE_INDEX => {
                let expected = count * WS_INDEX_STRIDE * 8;
                if payload.len() < expected {
                    warn!(count, expected, actual = payload.len(), "ws index frame truncated");
                    return None;
                }
                let records = (0..count)
                    .map(|i| {
                        let base = i * WS_INDEX_STRIDE * 8;
                        let f = |j: usize| f64::from_le_bytes(
                            payload[base + j * 8..base + (j + 1) * 8]
                                .try_into()
                                .unwrap(),
                        );
                        WsIndex {
                            ts_ms: f(0),
                            ticker: f(1),
                            mid: f(2),
                            bid: f(3),
                            ask: f(4),
                            ci: f(5),
                            confidence: f(6),
                            accepted: f(7),
                            rejected: f(8),
                        }
                    })
                    .collect();
                Some(WsMessage::Index(records))
            }
            WS_TYPE_TICK => {
                let expected = count * WS_TICK_STRIDE * 8;
                if payload.len() < expected {
                    warn!(count, expected, actual = payload.len(), "ws tick frame truncated");
                    return None;
                }
                let records = (0..count)
                    .map(|i| {
                        let base = i * WS_TICK_STRIDE * 8;
                        let f = |j: usize| f64::from_le_bytes(
                            payload[base + j * 8..base + (j + 1) * 8]
                                .try_into()
                                .unwrap(),
                        );
                        WsTick {
                            ts_ms: f(0),
                            ticker: f(1),
                            bid: f(2),
                            ask: f(3),
                            provider: f(4),
                            flags: f(5),
                        }
                    })
                    .collect();
                Some(WsMessage::Tick(records))
            }
            other => {
                warn!(msg_type = other, "unknown ws frame type");
                None
            }
        }
    }
}

// ─── UDP multicast consumer ─────────────────────────────────────────────────

/// Receives fixed-size `T: Pod` datagrams from a UDP multicast group.
///
/// Generic over any MITCH type: the same struct works for `Index` (40B),
/// `Tick` (32B), or `Bar` (128B) depending on which multicast group you
/// bind to.
pub struct MulticastStream<T: Pod + Send + 'static> {
    socket: UdpSocket,
    _marker: PhantomData<T>,
}

impl<T: Pod + Send + 'static> MulticastStream<T> {
    /// Bind to a multicast group and start receiving `T`-sized datagrams.
    pub async fn bind(addr: Ipv4Addr, port: u16) -> Result<Self> {
        let socket = UdpSocket::bind(("0.0.0.0", port)).await?;
        socket.join_multicast_v4(addr, Ipv4Addr::UNSPECIFIED)?;
        info!(%addr, port, size = std::mem::size_of::<T>(), "multicast joined");
        Ok(Self { socket, _marker: PhantomData })
    }

    /// Bind to the default NXR index multicast group (`239.0.42.1:40006`).
    pub async fn bind_default() -> Result<Self> {
        Self::bind(DEFAULT_MCAST_ADDR, DEFAULT_MCAST_PORT).await
    }

    /// Receive one record. Returns `None` on unrecoverable socket error.
    /// Silently drops datagrams that don't match `size_of::<T>()`.
    pub async fn recv(&self) -> Option<T> {
        let expected = std::mem::size_of::<T>();
        let mut buf = vec![0u8; expected + 1];
        loop {
            match self.socket.recv_from(&mut buf).await {
                Ok((n, _)) if n == expected => {
                    return Some(*bytemuck::from_bytes(&buf[..expected]));
                }
                Ok((n, peer)) => {
                    warn!(n, expected, %peer, "unexpected datagram size");
                }
                Err(e) => {
                    error!(%e, "multicast recv error");
                    return None;
                }
            }
        }
    }
}
