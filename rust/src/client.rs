//! NXR REST + WebSocket consumer client.
//!
//! A single high-level entry point for the v1 API. Two equivalent call styles:
//!
//! 1) **Object form** — single call, all opts explicit:
//! ```no_run
//! use nxr_sdk::client::{NxrClient, HistoryOpts, DataKind};
//! # async fn run() -> anyhow::Result<()> {
//! let c = NxrClient::new("https://api.nxrates.com");
//! let data = c.history(HistoryOpts {
//!     ticker: Some("BTC/USDT".into()),
//!     kind: Some(DataKind::Renko),
//!     limit: Some(500),
//!     ..Default::default()
//! }).await?;
//! # Ok(()) }
//! ```
//!
//! 2) **Chainable builder** — flows in conditionals:
//! ```no_run
//! # use nxr_sdk::client::NxrClient;
//! # async fn run() -> anyhow::Result<()> {
//! let c = NxrClient::new("https://api.nxrates.com");
//! let data = c.get().history().pair("ETH/USDC").renko().limit(500).fetch().await?;
//! # Ok(()) }
//! ```
//!
//! Smart defaults: missing quote → "USDT"; missing kind → "renko"; missing
//! instrument → "spot". MITCH binary is the wire default on data endpoints
//! (`Accept: application/octet-stream`); JSON only on metadata.
//!
//! Real-time stream:
//! ```no_run
//! # use nxr_sdk::client::NxrClient;
//! # async fn run() -> anyhow::Result<()> {
//! let c = NxrClient::new("https://api.nxrates.com");
//! let mut sub = c.subscribe(&["BTC/USDT".to_string()]).await?;
//! while let Some(rec) = sub.next().await? {
//!     println!("{} {} {}", rec.epoch_ms, rec.ticker, rec.bid);
//! }
//! # Ok(()) }
//! ```

use std::collections::HashMap;
use std::sync::Mutex;
use std::time::Duration;

use anyhow::{Context, Result, anyhow, bail};
use bytemuck::Pod;
use reqwest::header::{ACCEPT, HeaderValue};
use serde::{Deserialize, Serialize};

use crate::ipc::record::IndexRecord;
use crate::mitch::bar::Bar;
use crate::ws_client::{WsClient, WsIndex};

/// Default endpoint for the public API.
pub const DEFAULT_BASE_URL: &str = "https://api.nxrates.com";

/// Smart-default quote symbol when none is provided.
pub const DEFAULT_QUOTE: &str = "USDT";

/// Smart-default data kind when none is provided.
pub const DEFAULT_KIND: DataKind = DataKind::Renko;

// ── Public DTOs (subset that matters at the SDK boundary) ────────────────

/// `/v1/price/{ticker_id}` + `/v1/last` snapshot row.
#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct SnapshotResponse {
    pub ticker: u64,
    pub mid: f64,
    pub bid: f64,
    pub ask: f64,
    /// Confidence interval in micro basis points (relative to mid).
    pub ci: u16,
    pub confidence: u8,
}

/// Synth-leg `{ sym, exp }`. `exp ∈ {+1, -1}`.
#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct SynthLeg {
    pub sym: String,
    pub exp: i8,
}

/// Synth-path entry. `synth = Π leg_i^{e_i}`.
#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct SynthPath {
    pub sym: String,
    pub legs: Vec<SynthLeg>,
}


/// Disk shard window from `/v1/tickers/detail`.
#[derive(Clone, Debug, Default, Deserialize, Serialize)]
pub struct ShardWindow {
    pub first_date: Option<String>,
    pub last_date: Option<String>,
    pub count: u32,
}

/// Per-kind schema from `/v1/tickers/detail`.
#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct KindSchema {
    pub fields: Vec<String>,
    pub stride_bytes: u32,
    pub shards: ShardWindow,
}

/// One row of `/v1/tickers/detail`.
#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct TickerDetail {
    pub ticker_id: u64,
    pub ticker: String,
    pub base: String,
    pub quote: String,
    pub base_class: String,
    pub quote_class: String,
    pub instrument_type: String,
    pub native: bool,
    #[serde(default)]
    pub synth_legs: Option<Vec<SynthLeg>>,
    #[serde(default)]
    pub kinds: HashMap<String, KindSchema>,
}

/// `/v1/tickers/detail` response wrapper.
#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct TickersDetailResponse {
    pub idx_aggregation_ms: u64,
    pub count: usize,
    pub tickers: Vec<TickerDetail>,
}

impl TickersDetailResponse {
    /// Lookup a row by canonical "BASE/QUOTE" string.
    pub fn by_ticker(&self, ticker: &str) -> Option<&TickerDetail> {
        self.tickers.iter().find(|t| t.ticker == ticker)
    }
}

/// Live ticker snapshot from `/v1/tickers`.
#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct TickerSnapshotJson {
    pub symbol: String,
    pub ticker: u64,
    pub ts_ms: i64,
    pub bid: f64,
    pub ask: f64,
    pub mid: f64,
    pub ci_ubp: u32,
    pub confidence: u8,
    pub accepted: u8,
    pub rejected: u8,
}

/// `/v1/synth/tick/{sym}` response.
#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct SynthTickJson {
    pub sym: String,
    pub bid: f64,
    pub ask: f64,
    pub mid: f64,
    pub conf: u16,
}

// ── Range / history options ──────────────────────────────────────────────

/// Range query options for time-bounded endpoints.
#[derive(Clone, Debug, Default)]
pub struct RangeOpts {
    pub from: Option<i64>,
    pub to: Option<i64>,
    pub limit: Option<usize>,
    pub cursor: Option<i64>,
}

/// Data kind discriminator for `history()`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DataKind {
    /// Raw 56 B `IndexRecord` stream.
    Idx,
    /// 96 B `Bar` (kline / S10 OHLC w/ microstructure).
    Kline,
    /// 96 B `Bar` (renko bricks).
    Renko,
}

impl DataKind {
    /// String form used in URL paths.
    pub fn as_str(&self) -> &'static str {
        match self {
            DataKind::Idx => "idx",
            DataKind::Kline => "kline",
            DataKind::Renko => "renko",
        }
    }
}

/// Object-form `history()` options.
#[derive(Clone, Debug, Default)]
pub struct HistoryOpts {
    /// Pair string ("BTC/USDT", "BTC-USDT", or bare "BTC").
    pub ticker: Option<String>,
    /// Base symbol (required if `ticker` is omitted).
    pub base: Option<String>,
    /// Quote symbol (defaults to "USDT").
    pub quote: Option<String>,
    /// Data kind (defaults to `Renko`).
    pub kind: Option<DataKind>,
    /// Instrument type — only "spot" supported today.
    pub instrument_type: Option<String>,
    pub from: Option<i64>,
    pub to: Option<i64>,
    pub limit: Option<usize>,
    pub cursor: Option<i64>,
}

/// `history()` return — discriminated union over the requested data kind.
#[derive(Clone, Debug)]
pub enum HistoryData {
    Idx(Vec<IndexRecord>),
    Bars { kind: DataKind, bars: Vec<Bar> },
}

// ── Client ───────────────────────────────────────────────────────────────

/// REST + WebSocket consumer client for the NXR v1 API.
#[derive(Clone)]
pub struct NxrClient {
    base_url: String,
    http: reqwest::Client,
    api_key: Option<String>,
    detail_cache: std::sync::Arc<Mutex<Option<TickersDetailResponse>>>,
    symbol_to_id: std::sync::Arc<Mutex<HashMap<String, u64>>>,
}

impl NxrClient {
    /// Construct a client. Pass an empty string to use [`DEFAULT_BASE_URL`].
    pub fn new(base_url: impl Into<String>) -> Self {
        let raw = base_url.into();
        let url = if raw.is_empty() {
            DEFAULT_BASE_URL.to_string()
        } else {
            raw.trim_end_matches('/').to_string()
        };
        let http = reqwest::Client::builder()
            .user_agent(concat!("nxr-sdk-rust/", env!("CARGO_PKG_VERSION")))
            .timeout(Duration::from_secs(30))
            .build()
            .expect("reqwest client build");
        Self {
            base_url: url,
            http,
            api_key: None,
            detail_cache: Default::default(),
            symbol_to_id: Default::default(),
        }
    }

    /// Default-endpoint constructor — `NxrClient::default()` ≡ `NxrClient::new(DEFAULT_BASE_URL)`.
    pub fn default() -> Self {
        Self::new(DEFAULT_BASE_URL)
    }

    /// Attach an API key (`X-NXR-Key`) for paid plan access.
    pub fn with_api_key(mut self, key: impl Into<String>) -> Self {
        let k = key.into();
        self.api_key = if k.is_empty() { None } else { Some(k) };
        self
    }

    /// Set a custom HTTP timeout (default 30 s).
    pub fn with_timeout(mut self, t: Duration) -> Self {
        self.http = reqwest::Client::builder()
            .user_agent(concat!("nxr-sdk-rust/", env!("CARGO_PKG_VERSION")))
            .timeout(t)
            .build()
            .expect("reqwest client build");
        self
    }

    /// Read-only access to the base URL.
    pub fn base_url(&self) -> &str {
        &self.base_url
    }

    // ── Metadata endpoints (JSON) ────────────────────────────────────────

    /// `GET /health` — full JSON liveness body.
    pub async fn health(&self) -> Result<serde_json::Value> {
        self.json_get("/health").await
    }

    /// `GET /v1/providers` — provider_id → name map.
    pub async fn providers(&self) -> Result<HashMap<u16, String>> {
        // Server returns `{ "<id>": "<name>" }` with string keys; parse via a helper.
        let raw: HashMap<String, String> = self.json_get("/v1/providers").await?;
        let mut out = HashMap::with_capacity(raw.len());
        for (k, v) in raw {
            if let Ok(id) = k.parse::<u16>() {
                out.insert(id, v);
            }
        }
        Ok(out)
    }

    /// `GET /v1/tickers` — live snapshot for every ticker.
    pub async fn tickers(&self) -> Result<Vec<TickerSnapshotJson>> {
        self.json_get("/v1/tickers").await
    }

    /// `GET /v1/tickers/detail` — universal integrator inventory (cached).
    pub async fn tickers_detail(&self) -> Result<TickersDetailResponse> {
        if let Some(d) = self.detail_cache.lock().ok().and_then(|g| g.clone()) {
            return Ok(d);
        }
        let d: TickersDetailResponse = self.json_get("/v1/tickers/detail").await?;
        if let Ok(mut g) = self.detail_cache.lock() {
            *g = Some(d.clone());
        }
        if let Ok(mut g) = self.symbol_to_id.lock() {
            g.clear();
            for t in &d.tickers {
                if t.ticker_id != 0 {
                    g.insert(t.ticker.clone(), t.ticker_id);
                }
            }
        }
        Ok(d)
    }

    /// Force-refresh the cached `/v1/tickers/detail`.
    pub async fn tickers_detail_refresh(&self) -> Result<TickersDetailResponse> {
        if let Ok(mut g) = self.detail_cache.lock() {
            *g = None;
        }
        self.tickers_detail().await
    }

    /// `GET /v1/price/{ticker_id}`.
    pub async fn price(&self, ticker_id: u64) -> Result<Option<SnapshotResponse>> {
        self.json_get(&format!("/v1/price/{ticker_id}")).await
    }

    /// `GET /v1/last?symbols=<id>,<id>,...`.
    pub async fn last(&self, ticker_ids: &[u64]) -> Result<Vec<SnapshotResponse>> {
        if ticker_ids.is_empty() {
            return Ok(vec![]);
        }
        let csv = ticker_ids
            .iter()
            .map(|x| x.to_string())
            .collect::<Vec<_>>()
            .join(",");
        let path = format!("/v1/last?symbols={}", urlencoding(&csv));
        self.json_get(&path).await
    }

    /// `GET /v1/synth/paths` — static synth registry.
    pub async fn synth_paths(&self) -> Result<Vec<SynthPath>> {
        self.json_get("/v1/synth/paths").await
    }

    /// `GET /v1/synth/tick/{sym}` — instantaneous synth tick.
    pub async fn synth_tick(&self, sym: &str) -> Result<SynthTickJson> {
        self.json_get(&format!("/v1/synth/tick/{}", url_sym(sym))).await
    }

    /// `GET /v1/integrity/{sym}` — shard-integrity diagnostics. Returns the
    /// parsed JSON; HTTP 503 surfaces as an error with the body excerpt.
    pub async fn integrity(&self, sym: &str, kind: Option<&str>) -> Result<serde_json::Value> {
        let mut path = format!("/v1/integrity/{}", url_sym(sym));
        if let Some(k) = kind {
            path.push_str(&format!("?kind={}", urlencoding(k)));
        }
        self.json_get(&path).await
    }

    // ── Data endpoints (octet-stream MITCH binary) ────────────────────────

    /// `GET /v1/idx/{sym}` — raw 56 B `IndexRecord` stream, zero-copy decoded.
    pub async fn idx(&self, sym: &str, opts: &RangeOpts) -> Result<Vec<IndexRecord>> {
        let path = format!("/v1/idx/{}{}", url_sym(sym), build_range(opts, None));
        let bytes = self.bytes_get(&path).await?;
        decode_pod_slice::<IndexRecord>(&bytes)
    }

    /// `GET /v1/bars/{sym}/{kind}` — 96 B `Bar` stream, decoded into a `Vec<Bar>`.
    pub async fn bars(&self, sym: &str, kind: BarKindParam, opts: &RangeOpts) -> Result<Vec<Bar>> {
        let path = format!(
            "/v1/bars/{}/{}{}",
            url_sym(sym),
            kind.as_str(),
            build_range(opts, None)
        );
        let bytes = self.bytes_get(&path).await?;
        decode_pod_slice::<Bar>(&bytes)
    }

    /// Unified `history()` — discriminated return by `kind`.
    ///
    /// Smart defaults applied:
    /// * quote → "USDT"
    /// * kind → `Renko`
    /// * instrument → "spot"
    pub async fn history(&self, opts: HistoryOpts) -> Result<HistoryData> {
        let (ticker, _quote) = resolve_bq(&opts)?;
        let kind = opts.kind.unwrap_or(DEFAULT_KIND);
        let instrument = opts
            .instrument_type
            .as_deref()
            .unwrap_or("spot")
            .to_ascii_lowercase();
        if instrument != "spot" {
            bail!("instrument_type={} not supported (spot only)", instrument);
        }
        let range = RangeOpts {
            from: opts.from,
            to: opts.to,
            limit: opts.limit,
            cursor: opts.cursor,
        };
        match kind {
            DataKind::Idx => {
                let rs = self.idx(&ticker, &range).await?;
                Ok(HistoryData::Idx(rs))
            }
            DataKind::Kline => Ok(HistoryData::Bars {
                kind,
                bars: self.bars(&ticker, BarKindParam::Kline, &range).await?,
            }),
            DataKind::Renko => Ok(HistoryData::Bars {
                kind,
                bars: self.bars(&ticker, BarKindParam::Renko, &range).await?,
            }),
        }
    }

    /// Chainable builder root: `client.get().history()...`.
    pub fn get(&self) -> HistoryRoot<'_> {
        HistoryRoot { client: self }
    }

    // ── WebSocket subscriber ─────────────────────────────────────────────

    /// Subscribe to the live index stream from `/v1/stream`. Returns a
    /// [`WsStream`] that yields decoded [`WsIndex`] records on `.next()`.
    ///
    /// `tickers` filters records client-side by ticker_id (resolved from the
    /// cached `/v1/tickers/detail`). Pass an empty slice to receive every
    /// record.
    pub async fn subscribe(&self, tickers: &[String]) -> Result<WsStream> {
        let ws_url = ws_from_http(&self.base_url) + "/v1/stream";
        let inner = WsClient::connect(&ws_url).await?;
        let allow_ids = if tickers.is_empty() {
            None
        } else {
            // Best-effort resolve via cached detail map.
            if self
                .symbol_to_id
                .lock()
                .map(|g| g.is_empty())
                .unwrap_or(true)
            {
                let _ = self.tickers_detail().await; // populates cache; ignore errors
            }
            let g = self.symbol_to_id.lock().ok();
            let mut ids = Vec::with_capacity(tickers.len());
            if let Some(map) = g {
                for t in tickers {
                    if let Some(id) = map.get(t) {
                        ids.push(*id);
                    }
                }
            }
            if ids.is_empty() { None } else { Some(ids) }
        };
        Ok(WsStream {
            inner,
            allow_ids,
            buffer: Vec::new(),
        })
    }

    // ── Internals ────────────────────────────────────────────────────────

    async fn json_get<T: for<'de> Deserialize<'de>>(&self, path: &str) -> Result<T> {
        let url = format!("{}{}", self.base_url, path);
        let mut req = self
            .http
            .get(&url)
            .header(ACCEPT, HeaderValue::from_static("application/json"));
        if let Some(k) = &self.api_key {
            req = req.header("X-NXR-Key", k);
        }
        let r = req
            .send()
            .await
            .with_context(|| format!("GET {}", url))?;
        let status = r.status();
        if !status.is_success() {
            let body = r.text().await.unwrap_or_default();
            bail!(
                "NXR {path}: HTTP {status}{}",
                if body.is_empty() {
                    String::new()
                } else {
                    format!(" — {}", body.chars().take(200).collect::<String>())
                }
            );
        }
        r.json::<T>().await.with_context(|| format!("decode {}", path))
    }

    async fn bytes_get(&self, path: &str) -> Result<bytes::Bytes> {
        let url = format!("{}{}", self.base_url, path);
        let mut req = self
            .http
            .get(&url)
            .header(ACCEPT, HeaderValue::from_static("application/octet-stream"));
        if let Some(k) = &self.api_key {
            req = req.header("X-NXR-Key", k);
        }
        let r = req
            .send()
            .await
            .with_context(|| format!("GET {}", url))?;
        let status = r.status();
        if !status.is_success() {
            let body = r.text().await.unwrap_or_default();
            bail!(
                "NXR {path}: HTTP {status}{}",
                if body.is_empty() {
                    String::new()
                } else {
                    format!(" — {}", body.chars().take(200).collect::<String>())
                }
            );
        }
        Ok(r.bytes().await.with_context(|| format!("bytes {}", path))?)
    }
}

impl Default for NxrClient {
    fn default() -> Self {
        Self::new(DEFAULT_BASE_URL)
    }
}

/// `bars()` kind argument. Wraps `DataKind` to forbid `Idx` at the type level.
#[derive(Clone, Copy, Debug)]
pub enum BarKindParam {
    Kline,
    Renko,
}

impl BarKindParam {
    fn as_str(&self) -> &'static str {
        match self {
            BarKindParam::Kline => "kline",
            BarKindParam::Renko => "renko",
        }
    }
}

// ── Chainable history builder ────────────────────────────────────────────

/// Chainable-builder root returned by [`NxrClient::get`].
pub struct HistoryRoot<'a> {
    client: &'a NxrClient,
}

impl<'a> HistoryRoot<'a> {
    /// Open a [`HistoryBuilder`] for fluent chaining.
    pub fn history(&self) -> HistoryBuilder<'a> {
        HistoryBuilder {
            client: self.client,
            opts: HistoryOpts::default(),
        }
    }
}

/// Chainable history-fetch builder. Terminal: `.fetch().await`.
pub struct HistoryBuilder<'a> {
    client: &'a NxrClient,
    opts: HistoryOpts,
}

impl<'a> HistoryBuilder<'a> {
    pub fn pair(mut self, t: impl Into<String>) -> Self {
        self.opts.ticker = Some(t.into());
        self
    }
    pub fn ticker(self, t: impl Into<String>) -> Self {
        self.pair(t)
    }
    pub fn base(mut self, b: impl Into<String>) -> Self {
        self.opts.base = Some(b.into());
        self
    }
    pub fn quote(mut self, q: impl Into<String>) -> Self {
        self.opts.quote = Some(q.into());
        self
    }
    pub fn kind(mut self, k: DataKind) -> Self {
        self.opts.kind = Some(k);
        self
    }
    pub fn idx(mut self) -> Self {
        self.opts.kind = Some(DataKind::Idx);
        self
    }
    pub fn kline(mut self) -> Self {
        self.opts.kind = Some(DataKind::Kline);
        self
    }
    pub fn renko(mut self) -> Self {
        self.opts.kind = Some(DataKind::Renko);
        self
    }
    pub fn from(mut self, ms: i64) -> Self {
        self.opts.from = Some(ms);
        self
    }
    pub fn to(mut self, ms: i64) -> Self {
        self.opts.to = Some(ms);
        self
    }
    pub fn limit(mut self, n: usize) -> Self {
        self.opts.limit = Some(n);
        self
    }
    pub fn cursor(mut self, ms: i64) -> Self {
        self.opts.cursor = Some(ms);
        self
    }
    /// Execute the request.
    pub async fn fetch(self) -> Result<HistoryData> {
        self.client.history(self.opts).await
    }
}

// ── WS stream ────────────────────────────────────────────────────────────

/// Active WebSocket subscription handle. Iterate via `next()`; drop or call
/// `close()` to terminate.
pub struct WsStream {
    inner: WsClient,
    allow_ids: Option<Vec<u64>>,
    buffer: Vec<WsIndex>,
}

impl WsStream {
    /// Yield the next filtered record. Returns `Ok(None)` when the server
    /// closes the stream.
    pub async fn next(&mut self) -> Result<Option<WsIndex>> {
        loop {
            if let Some(r) = self.buffer.pop() {
                return Ok(Some(r));
            }
            let batch = match self.inner.next_batch().await? {
                Some(b) => b,
                None => return Ok(None),
            };
            // Filter + reverse so pop() yields in original order.
            let filtered: Vec<WsIndex> = batch
                .into_iter()
                .filter(|r| match &self.allow_ids {
                    Some(ids) => ids.contains(&r.ticker),
                    None => true,
                })
                .collect();
            if filtered.is_empty() {
                continue;
            }
            self.buffer = filtered.into_iter().rev().collect();
        }
    }

    /// Close the underlying socket cleanly.
    pub async fn close(self) -> Result<()> {
        self.inner.close().await
    }
}

// ── URL / parsing helpers ────────────────────────────────────────────────

fn url_sym(sym: &str) -> String {
    // Server accepts dash form natively (no %-encoding required).
    sym.replace('/', "-")
}

fn build_range(opts: &RangeOpts, extra: Option<(&str, &str)>) -> String {
    let mut parts: Vec<String> = Vec::new();
    if let Some(v) = opts.from {
        parts.push(format!("from={}", v));
    }
    if let Some(v) = opts.to {
        parts.push(format!("to={}", v));
    }
    if let Some(v) = opts.limit {
        parts.push(format!("limit={}", v));
    }
    if let Some(v) = opts.cursor {
        parts.push(format!("cursor={}", v));
    }
    if let Some((k, v)) = extra {
        parts.push(format!("{}={}", k, v));
    }
    if parts.is_empty() {
        String::new()
    } else {
        format!("?{}", parts.join("&"))
    }
}

/// Owned-string ticker parser with `DEFAULT_QUOTE` fallback for unquoted
/// inputs. Delegates the actual split logic to
/// [`crate::ticker::split_pair_multi`] (the canonical 2-leg splitter) to
/// avoid drift between client.rs and the sdk's pair-split rules.
/// Phase 59.R3.C5.C5 (2026-05-30) — was an inline `for sep in ['/', '-', '_']`
/// loop that diverged subtly from `split_pair_multi` (e.g. did not reject
/// inputs with multiple separators).
fn parse_ticker(s: &str) -> (String, String) {
    let t = s.trim().to_uppercase();
    match crate::ticker::split_pair_multi(&t, &['/', '-', '_']) {
        Some((base, quote)) => (base.trim().to_string(), quote.trim().to_string()),
        None => (t, DEFAULT_QUOTE.to_string()),
    }
}

fn resolve_bq(opts: &HistoryOpts) -> Result<(String, String)> {
    if let Some(t) = &opts.ticker {
        let (b, q) = parse_ticker(t);
        let quote = opts.quote.clone().unwrap_or(q).to_uppercase();
        return Ok((format!("{}/{}", b, quote), quote));
    }
    let b = opts
        .base
        .clone()
        .ok_or_else(|| anyhow!("history() requires either ticker or base"))?
        .to_uppercase();
    let q = opts
        .quote
        .clone()
        .unwrap_or_else(|| DEFAULT_QUOTE.to_string())
        .to_uppercase();
    Ok((format!("{}/{}", b, q), q))
}

fn ws_from_http(http: &str) -> String {
    if let Some(rest) = http.strip_prefix("https://") {
        format!("wss://{}", rest)
    } else if let Some(rest) = http.strip_prefix("http://") {
        format!("ws://{}", rest)
    } else {
        http.to_string()
    }
}

fn urlencoding(s: &str) -> String {
    // Minimal: only ',' needs encoding for CSV symbols list.
    s.replace(',', "%2C")
}

/// Reinterpret a contiguous bytes slice as `&[T]` and copy into a `Vec<T>`.
/// `T` is constrained to `bytemuck::Pod` (e.g. `IndexRecord`, `Bar`).
fn decode_pod_slice<T: Pod + Copy>(bytes: &[u8]) -> Result<Vec<T>> {
    let stride = std::mem::size_of::<T>();
    if stride == 0 {
        return Ok(vec![]);
    }
    let n = bytes.len() / stride;
    let aligned = &bytes[..n * stride];
    let slice: &[T] = bytemuck::try_cast_slice(aligned)
        .map_err(|e| anyhow!("bytemuck cast: {} (stride={}, len={})", e, stride, aligned.len()))?;
    Ok(slice.to_vec())
}

// ── Tests ────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_ticker_forms() {
        assert_eq!(parse_ticker("BTC/USDT"), ("BTC".into(), "USDT".into()));
        assert_eq!(parse_ticker("eth-usdc"), ("ETH".into(), "USDC".into()));
        assert_eq!(parse_ticker("BTC"), ("BTC".into(), "USDT".into()));
    }

    #[test]
    fn resolve_bq_smart_defaults() {
        let (t, q) = resolve_bq(&HistoryOpts {
            base: Some("BTC".into()),
            ..Default::default()
        })
        .unwrap();
        assert_eq!(t, "BTC/USDT");
        assert_eq!(q, "USDT");

        let (t, q) = resolve_bq(&HistoryOpts {
            ticker: Some("ETH/USDC".into()),
            ..Default::default()
        })
        .unwrap();
        assert_eq!(t, "ETH/USDC");
        assert_eq!(q, "USDC");

        // Explicit quote overrides ticker-implied quote.
        let (t, q) = resolve_bq(&HistoryOpts {
            ticker: Some("BTC/USDT".into()),
            quote: Some("USDC".into()),
            ..Default::default()
        })
        .unwrap();
        assert_eq!(t, "BTC/USDC");
        assert_eq!(q, "USDC");
    }

    #[test]
    fn build_range_strings() {
        assert_eq!(build_range(&RangeOpts::default(), None), "");
        assert_eq!(
            build_range(
                &RangeOpts {
                    limit: Some(10),
                    from: Some(1),
                    ..Default::default()
                },
                None
            ),
            "?from=1&limit=10"
        );
    }

    #[test]
    fn url_sym_normalisation() {
        assert_eq!(url_sym("BTC/USDT"), "BTC-USDT");
        assert_eq!(url_sym("BTC-USDT"), "BTC-USDT");
    }

    #[test]
    fn ws_url_conversion() {
        assert_eq!(ws_from_http("http://nxr:80"), "ws://nxr:80");
        assert_eq!(
            ws_from_http("https://api.nxrates.com"),
            "wss://api.nxrates.com"
        );
    }

    #[test]
    fn decode_pod_slice_roundtrip() {
        // Build 2 IndexRecord (Pod) then decode back via the helper.
        let rec: IndexRecord = bytemuck::Zeroable::zeroed();
        let mut bytes = Vec::with_capacity(std::mem::size_of::<IndexRecord>() * 2);
        bytes.extend_from_slice(bytemuck::bytes_of(&rec));
        bytes.extend_from_slice(bytemuck::bytes_of(&rec));
        let out: Vec<IndexRecord> = decode_pod_slice(&bytes).unwrap();
        assert_eq!(out.len(), 2);
    }

    #[test]
    fn nxr_client_default_uses_public_endpoint() {
        let c = NxrClient::default();
        assert_eq!(c.base_url(), "https://api.nxrates.com");
    }
}
