//! Unified `nxrates.yml` schema — single source of truth shared by every
//! offline tool that consumes the pipeline configuration.
//!
//! Consolidated schema: each offline tool used to carry a near-identical
//! private copy of the `series.{renko,vol,calibration,pipeline}` schema
//! (renko_from_idx, nxr_calibrate, renko_trailing_from_idx, mtf_sweep,
//! generate_renko_from_ticks, fetch_crypto_history). They were not strictly
//! identical — some held a `target_bpd_by_class` table, some used `i64` vs
//! `usize` for `k_fit_windows_days`, some omitted `cexs`. The union here
//! supersedes all of them; bins import [`PipelineYml`] and access the
//! fields they need.
//!
//! `#[serde(default)]` is used on the leaf-level fields that only some
//! bins exercise so individual `nxrates.yml` files can omit them without
//! serde rejecting the parse. The required field set is the intersection
//! of what every bin needs.

use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

use crate::vol::VolConfig;

/// Default min 24h USD volume below which the core weights builder skips
/// emitting a synth injection rule (stablecoin- or USD-quoted pairs).
/// Was: hardcoded `100_000.0` at `core/src/weights.rs:190,211`. Phase
/// 59.R3.C2.O1 (2026-05-30).
pub const DEFAULT_MIN_VOLUME_INJECTION_USD: f64 = 100_000.0;

/// Default Binance historical-archive URL prefixes.
/// Fallback for `cexs.exchanges.binance.archive_url_template.*` when unset.
/// Phase 59.R3.C2.O4 (2026-05-30).
pub const DEFAULT_ARCHIVE_URL_BINANCE_MONTHLY: &str =
    "https://data.binance.vision/data/spot/monthly/aggTrades/{sym}/";
pub const DEFAULT_ARCHIVE_URL_BINANCE_DAILY: &str =
    "https://data.binance.vision/data/spot/daily/aggTrades/{sym}/";
pub const DEFAULT_ARCHIVE_URL_BINANCE_PROBE: &str =
    "https://data.binance.vision/data/spot/monthly/aggTrades/{sym}/{sym}-aggTrades-{y:04}-{m:02}.zip";

/// Default Bybit historical-archive URL prefixes.
pub const DEFAULT_ARCHIVE_URL_BYBIT_MONTHLY: &str = "https://public.bybit.com/spot/{sym}/";
pub const DEFAULT_ARCHIVE_URL_BYBIT_DAILY: &str = "https://public.bybit.com/spot/{sym}/";
pub const DEFAULT_ARCHIVE_URL_BYBIT_PROBE: &str =
    "https://public.bybit.com/trading/{sym}/{sym}{y:04}-{m:02}.csv.gz";

/// Default Bitget historical-archive URL prefix. Bitget exposes only a
/// daily (per-sequence) bucket — no monthly archives. Phase 59.R3.C3.O1
/// (2026-05-30).
pub const DEFAULT_ARCHIVE_URL_BITGET_MONTHLY: &str = "";
pub const DEFAULT_ARCHIVE_URL_BITGET_DAILY: &str =
    "https://img.bitgetimg.com/online/trades/SPBL/{sym}/";
pub const DEFAULT_ARCHIVE_URL_BITGET_PROBE: &str =
    "https://img.bitgetimg.com/online/trades/SPBL/{sym}/{ds}_{seq:03}.zip";

/// Default OKX historical-archive URL prefixes. OKX exposes both monthly
/// and daily traderecords buckets. Phase 59.R3.C3.O1 (2026-05-30).
pub const DEFAULT_ARCHIVE_URL_OKX_MONTHLY: &str =
    "https://static.okx.com/cdn/okex/traderecords/trades/monthly/{y:04}{m:02}/";
pub const DEFAULT_ARCHIVE_URL_OKX_DAILY: &str =
    "https://static.okx.com/cdn/okex/traderecords/trades/daily/{ds}/";
pub const DEFAULT_ARCHIVE_URL_OKX_PROBE: &str =
    "https://static.okx.com/cdn/okex/traderecords/trades/monthly/{y:04}{m:02}/{sym}-trades-{y:04}-{m:02}.zip";

/// Top-level wrapper matching the layout of `nxrates.yml`.
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct PipelineYml {
    #[serde(default)]
    pub cexs: CexsYml,
    pub series: SeriesYml,
    #[serde(default)]
    pub network: NetworkYml,
    #[serde(default)]
    pub server: ServerYml,
    /// Offline-tool / runtime operator-facing pipeline knobs that aren't
    /// part of the canonical core data path. Currently:
    ///   - `sweep.pairs`: the `mtf_sweep` calibration sweep universe (was:
    ///     hardcoded `series-factory/src/bin/mtf_sweep.rs::SWEEP_PAIRS`).
    #[serde(default)]
    pub pipeline: PipelineSectionYml,
    /// Synth-pair registry. Currently:
    ///   - `initial_pairs`: launch synth-pair list (was: hardcoded
    ///     `sdk/rust/src/synth/pairs.rs::INITIAL_SYNTH_PAIRS`).
    #[serde(default)]
    pub synths: SynthsYml,
    /// Stablecoin allowlist for the FX/metal/stablecoin auto-cross
    /// triangulator (`core::triangulator::build_auto_cross_rules`).
    #[serde(default)]
    pub triangulation: TriangulationYml,
    /// Runtime tuning knobs for the forwarders, fx provider server, and core
    /// REST/WS layer. Phase 59.R3.C2.O5 (2026-05-30) — was hardcoded
    /// `const FORWARDER_HEARTBEAT_SECS / PROVIDER_STALE_SECS / FRAME_BUF_MAX
    /// / STALE_SECS / REFRESH_OFFSET_SECS / FLUSH_MS` in
    /// `crypto/src/main.rs`, `crypto/src/bin/fx.rs`,
    /// `core/src/server/{rest,mod,ws}.rs`.
    #[serde(default)]
    pub runtime: RuntimeYml,
    /// Oracle price relays (`nxr-oracle` forwarder). Keyed by market-provider
    /// name (must exist in `mitch/ids/market-providers.csv`, e.g. `pyth`).
    #[serde(default)]
    pub oracles: OraclesYml,
    /// BTR DEX signed-quote endpoint (`/v1/quote/signed`). Absent = disabled.
    /// Domain binds to the DEPLOYED ExternalOracle (chain_id + address) —
    /// per-deployment config, never hardcoded. Signing key: env
    /// `NXR_SIGNER_KEY` (required when this block is present; never logged).
    #[serde(default)]
    pub signed_quotes: Option<SignedQuotesYml>,
}

/// `signed_quotes:` block — NXR-signed EIP-712 quote blobs for the BTR DEX
/// `ExternalOracle.batchPushSigned` relay path. Wire format is the FROZEN
/// spec `btr/dex/ORACLE_SIGNED_PUSH_SPEC.md` (24 B/feed packed records).
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct SignedQuotesYml {
    /// Deployed ExternalOracle address (0x-hex, 20 bytes) — EIP-712
    /// `verifyingContract`.
    pub oracle: String,
    /// EIP-712 domain chainId of the deployment.
    pub chain_id: u64,
    /// Blob rebuild floor (ms). Requests inside the window get the cached
    /// blob. Default 250.
    #[serde(default)]
    pub min_interval_ms: Option<u64>,
    /// Trailing 30 m bars for the Parkinson σ (mirrors keeper
    /// `nxr.vol_lookback_bars`). Default 48.
    #[serde(default)]
    pub sigma_lookback_bars: Option<u32>,
    /// Reject marks whose last aggregator emit is older than this (s).
    /// Default 120.
    #[serde(default)]
    pub mark_max_age_s: Option<u64>,
    /// DEX feed subset. `idx` MUST equal the feed's position in the on-chain
    /// append-only `feedIds[]` (idx never remaps); keeper cross-checks
    /// `feedIds(idx) == feed_id` at startup.
    pub feeds: Vec<SignedFeedYml>,
}

/// One signed-quote feed: on-chain `feedIds[]` index → NXR symbol.
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct SignedFeedYml {
    /// Position in the on-chain `feedIds[]` array.
    pub idx: u16,
    /// NXR symbol whose mark/σ/CI back the feed (e.g. `BTC-USDC`).
    pub symbol: String,
    /// Optional bridge symbol: pushed mark = `mark(symbol) × mark(quote_via)`
    /// (e.g. `CAKE-USDT × USDT-USDC`), CI/σ composed in quadrature — mirrors
    /// keeper `quote_via` (ORC-04).
    #[serde(default)]
    pub quote_via: Option<String>,
}

/// `oracles:` block — push-based oracle relay providers consumed by the
/// `nxr-oracle` forwarder (Pyth via self-hosted Hermes SSE today).
#[derive(Debug, Clone, Default, Deserialize, Serialize)]
pub struct OraclesYml {
    /// Oracle forwarder flush cadence (ms). Pythnet publishes ~400 ms slots,
    /// so oracles run their own (slower) cadence than the CEX 200 ms one.
    /// Default 1000 when absent.
    #[serde(default)]
    pub aggregation_interval_ms: Option<u64>,
    /// Relay catalog re-scan period (secs) for `watch` auto-onboarding.
    /// Default 3600 when absent.
    #[serde(default)]
    pub catalog_refresh_secs: Option<u64>,
    #[serde(default)]
    pub providers: BTreeMap<String, OracleProviderYml>,
}

/// `oracles.providers.<name>:` — one relay endpoint + its feed manifest.
#[derive(Debug, Clone, Default, Deserialize, Serialize)]
pub struct OracleProviderYml {
    /// Relay base URL (Hermes: `http://host:8080`; forwarder appends
    /// `/v2/updates/price/stream`). Env `NXR_ORACLE_URL_<NAME>` overrides.
    #[serde(default)]
    pub url: String,
    /// Canonical "BASE/QUOTE" → provider feed id (Pyth: hex, no 0x).
    #[serde(default)]
    pub symbols: BTreeMap<String, String>,
    /// Coming-soon feeds: canonical NXR symbol → provider CATALOG symbol
    /// (Pyth: e.g. "Metal.XCU/USD"). The forwarder re-scans the relay
    /// catalog every `catalog_refresh_secs` and auto-subscribes the moment
    /// a watched feed goes live. Keys must resolve strictly at boot.
    #[serde(default)]
    pub watch: BTreeMap<String, String>,
    /// Transport: "" = Hermes SSE (default), "lazer" = Pyth Lazer WS
    /// (JSON subscribe, Bearer token via env `NXR_ORACLE_TOKEN_<NAME>`).
    /// Lazer `symbols` values are DECIMAL Lazer feed ids, not hex.
    #[serde(default)]
    pub kind: String,
    /// Lazer subscription channel (e.g. "fixed_rate@200ms"). NEVER
    /// `real_time`: only ~30 stable feeds carry it and `ignoreInvalidFeeds`
    /// silently drops the rest (verified 2026-07-12 profiling).
    #[serde(default)]
    pub channel: String,
    /// Lazer stream endpoints, ALL consumed concurrently with first-arrival
    /// dedup on `feedUpdateTimestamp` (any endpoint may die; no gap).
    /// Overrides `url` when non-empty.
    #[serde(default)]
    pub urls: Vec<String>,
}

/// `runtime:` block — forwarder + server tuning knobs. All `Option<…>`
/// fields fall back to their per-callsite `DEFAULT_RUNTIME_*` constant when
/// the YAML is silent (so old `config.yml` files keep working).
#[derive(Debug, Clone, Default, Deserialize, Serialize)]
pub struct RuntimeYml {
    /// Forwarder heartbeat cadence (seconds). Was: hardcoded
    /// `FORWARDER_HEARTBEAT_SECS = 5` in `crypto/src/main.rs:34`.
    #[serde(default)]
    pub forwarder_heartbeat_secs: Option<u64>,
    /// REST `/health` stale-forwarder threshold (seconds). Was: hardcoded
    /// `STALE_SECS = 20` in `core/src/server/rest.rs:786`.
    #[serde(default)]
    pub health_stale_secs: Option<u64>,
    /// Daily-refresh wait offset past UTC midnight (seconds). Was: hardcoded
    /// `REFRESH_OFFSET_SECS = 30` in `core/src/server/mod.rs:232`.
    #[serde(default)]
    pub daily_refresh_offset_secs: Option<u64>,
    /// WS flush cadence (ms). Was: hardcoded `FLUSH_MS = 100` in
    /// `core/src/server/ws.rs:92`.
    #[serde(default)]
    pub ws_flush_ms: Option<u64>,
}

/// Per-callsite runtime defaults. Mirror the prior `const` values so
/// YAML-silent configs preserve audit-frozen behaviour.
pub const DEFAULT_RUNTIME_FORWARDER_HEARTBEAT_SECS: u64 = 5;
pub const DEFAULT_RUNTIME_PROVIDER_STALE_SECS: u64 = 30;
pub const DEFAULT_RUNTIME_FRAME_BUF_MAX: usize = 2 * 1024 * 1024;
pub const DEFAULT_RUNTIME_HEALTH_STALE_SECS: u64 = 20;
pub const DEFAULT_RUNTIME_DAILY_REFRESH_OFFSET_SECS: u64 = 30;
pub const DEFAULT_RUNTIME_WS_FLUSH_MS: u64 = 200;

/// `pipeline:` block — offline-tool operator-facing knobs (sweep universe,
/// future: per-bin overrides). Distinct from `series.pipeline:` (which holds
/// replay/backfill knobs).
#[derive(Debug, Clone, Default, Deserialize, Serialize)]
pub struct PipelineSectionYml {
    #[serde(default)]
    pub sweep: SweepYml,
}

/// `pipeline.sweep:` — `mtf_sweep` bin universe.
#[derive(Debug, Clone, Default, Deserialize, Serialize)]
pub struct SweepYml {
    /// `<BASE>/<QUOTE>` pair list for the `mtf_sweep` calibration sweep.
    /// Empty ⇒ fall back to [`crate::synth::pairs::DEFAULT_SWEEP_PAIRS`].
    #[serde(default)]
    pub pairs: Vec<PairSpec>,
}

/// One `<BASE>/<QUOTE>` spec — yaml-friendly version of `(&str, &str)`.
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct PairSpec {
    pub base: String,
    pub quote: String,
}

/// `synths:` block — optional manual override for synth-pipeline pairs.
/// Leave `initial_pairs` empty: the live kernel derives crosses from
/// `cexs.cross_pairs` via [`crate::synth::cross_expand`].
#[derive(Debug, Clone, Default, Deserialize, Serialize)]
pub struct SynthsYml {
    #[serde(default)]
    pub initial_pairs: Vec<SynthPairYml>,
}

/// YAML-side mirror of [`crate::synth::pairs::SynthPairSpec`] — owned strings
/// so deserialization works without `'static` lifetime gymnastics.
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct SynthPairYml {
    pub synth_sym: String,
    pub base_sym: String,
    pub quote_sym: String,
}

/// `triangulation:` block — the ONE piece of the auto-cross-triangulation
/// eligibility test that can't be derived from existing ticker_id metadata.
/// FX (any currency pair) and CM-spot (metals/commodities, excluding futures
/// like expirable gold contracts) are both auto-detected from the ticker_id's
/// own asset-class + instrument-type bits (see
/// `core::triangulator::build_auto_cross_rules`) — zero config needed there.
/// Stablecoins are all `AssetClass::CR` (same class as BTC/ETH/...), which
/// carries no sub-class distinguishing "pegged to USD" from "not" - hence
/// this explicit, small, YAML-owned list rather than a Rust-hardcoded one.
#[derive(Debug, Clone, Default, Deserialize, Serialize)]
pub struct TriangulationYml {
    /// Base symbols (no "/USD" suffix) of USD-pegged stablecoins eligible for
    /// automatic cross-triangulation against every other eligible leg
    /// (FX majors, metals, other stablecoins). Each must already be
    /// registered as `<SYM>/USD` under some oracle provider's `symbols:`.
    #[serde(default)]
    pub stablecoins: Vec<String>,
}

/// Source of the default `NXR_CONFIG` fallback path. Determines what
/// the resolver returns when the env var is unset.
///
/// - [`ConfigHint::Runtime`] — long-running cluster services (core sink,
///   crypto forwarders, REST/WS server). Default = `/etc/nxr/config.yml`
///   (matches container layout where the chart mounts the values yml).
/// - [`ConfigHint::Bin`] — offline tools / one-shot binaries
///   (`nxr_calibrate`, `merge_idx`, `mtf_sweep`, …). Default = `./config.yml`
///   (matches dev workflow: `cd nx-rates && cargo run --bin …`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConfigHint {
    Runtime,
    Bin,
}

impl ConfigHint {
    /// Default path applied when `NXR_CONFIG` is unset.
    pub const fn default_path(self) -> &'static str {
        match self {
            ConfigHint::Runtime => "/etc/nxr/config.yml",
            ConfigHint::Bin => "config.yml",
        }
    }
}

impl PipelineYml {
    /// Read and parse a pipeline-yaml file from disk. Single source of truth
    /// for the 6+ `serde_yaml::from_str(&fs::read_to_string(p)?)?` callsites
    /// in `series-factory/src/bin/*`. Uses `serde_yml` (the maintained fork);
    /// schema is forward-compatible with serde_yaml-emitted files because
    /// only the `Deserialize` derives are exercised.
    pub fn load(path: &std::path::Path) -> anyhow::Result<Self> {
        use anyhow::Context;
        let s = std::fs::read_to_string(path)
            .with_context(|| format!("read pipeline yaml {}", path.display()))?;
        serde_yml::from_str::<Self>(&s)
            .with_context(|| format!("parse pipeline yaml {}", path.display()))
    }

    /// Resolve the canonical NXR config path: env `NXR_CONFIG` if set,
    /// else `hint.default_path()`. Single SDK home for the 8+ ad-hoc
    /// `std::env::var("NXR_CONFIG").unwrap_or_else(...)` callsites in
    /// `crypto/`, `core/`, and `series-factory/bin/*`.
    pub fn resolve_path(hint: ConfigHint) -> std::path::PathBuf {
        std::env::var("NXR_CONFIG")
            .map(std::path::PathBuf::from)
            .unwrap_or_else(|_| std::path::PathBuf::from(hint.default_path()))
    }

    /// Load using the canonical NXR_CONFIG resolution policy. Phase 59.R3.L1.
    pub fn load_default(hint: ConfigHint) -> anyhow::Result<Self> {
        Self::load(&Self::resolve_path(hint))
    }
}

/// `cexs.exchanges.<name>:` per-exchange metadata. New fields are wholly
/// optional so legacy configs (`mitch_id` + `weight` + `cmc_slug` only) keep
/// parsing. WS / REST URLs were hardcoded in
/// `crypto/src/exchange/*.rs` until phase 59.R2C.4; once moved to YAML
/// the per-handler struct factory reads them at startup.
#[derive(Debug, Clone, Default, Deserialize, Serialize)]
pub struct ExchangeYml {
    #[serde(default)]
    pub mitch_id: Option<u16>,
    #[serde(default)]
    pub cmc_slug: Option<String>,
    #[serde(default)]
    pub weight: Option<f64>,
    /// WebSocket endpoint (audit-frozen URL — preserved from prior hardcode).
    #[serde(default)]
    pub ws_url: Option<String>,
    /// REST markets / exchangeInfo endpoint (audit-frozen).
    #[serde(default)]
    pub rest_url: Option<String>,
    /// Per-exchange quote-asset suffix list used by `normalize_symbol`
    /// (suffix-split mode). Was hardcoded in 13 adapters
    /// (`crypto/src/exchange/{binance,bybit,bitget,...}.rs::quote_suffixes`).
    /// Empty ⇒ fall back to `nxr_sdk::resolve::DEFAULT_QUOTE_SUFFIXES`.
    /// Phase 59.R3.H1 (2026-05-30).
    #[serde(default)]
    pub quote_suffixes: Vec<String>,
    /// Fallback ticker list used by `parse_markets_response` when REST returns
    /// a malformed / geo-blocked response. Was hardcoded in
    /// `crypto/src/exchange/{bullish,bitunix,weex}.rs` (phase 59.R3.C2.O2,
    /// 2026-05-30). Empty ⇒ adapter receives an empty market list (failing
    /// loudly is preferable to silently shipping a stale audit-frozen set).
    #[serde(default)]
    pub fallback_markets: Vec<String>,
    /// Per-exchange symbol-alias map for `normalize_symbol` / `format_symbol`.
    /// Keyed by the exchange-native code → canonical NXR code (e.g. Kraken
    /// `XBT → BTC`, Bitfinex `UST → USDT`). `format_symbol` walks the same
    /// map in reverse. Was hardcoded `.replace("XBT","BTC")` and
    /// `("UST","USDT")` literal arrays in `kraken.rs` / `bitfinex.rs`
    /// (phase 59.R3.C2.O3, 2026-05-30). Distinct from the top-level
    /// `cexs.aliases` map which the weights scraper uses globally.
    #[serde(default)]
    pub aliases: BTreeMap<String, String>,
    /// Historical-archive URL template(s) used by `series-factory` sources.
    /// Was hardcoded in `series-factory/src/sources/{binance,bybit}.rs` and
    /// `series-factory/src/bin/fetch_crypto_history.rs` (phase 59.R3.C2.O4,
    /// 2026-05-30). `None` ⇒ fall back to the per-source `DEFAULT_ARCHIVE_URL_*`
    /// const.
    #[serde(default)]
    pub archive_url_template: Option<ArchiveUrlTemplate>,
    /// Canonical "BASE/QUOTE" pairs to drop from this exchange's resolved
    /// symbol list even when present in both `NXR_SYMBOLS` and the
    /// exchange's own live markets response. For cross-exchange ticker
    /// collisions: a short/generic base symbol (e.g. "U") can denote two
    /// completely unrelated assets on different exchanges (confirmed
    /// 2026-07-12: Bybit's & XT.com's "U" is an unrelated ~$0.0003 token,
    /// not United Stables the ~$1 stablecoin every other connected exchange
    /// lists under the same ticker) - mixing them into one VWAP composite
    /// silently corrupts the aggregate. Checked in `client.rs::get_symbols`
    /// after the existing supported-set intersection.
    #[serde(default)]
    pub exclude_symbols: Vec<String>,
}

/// Per-exchange historical-archive URL prefixes. Each field is the URL stem
/// up to (and including) the trailing `/`; the adapter appends
/// `{filename}` (already `format!`-built with sym/date) to compose the full
/// download URL. `None` fields fall back to the per-source default const.
///
/// `monthly` / `daily` are used by the bulk-fetch adapters
/// (`sources/{binance,bybit}.rs`); `probe` is used by `fetch_crypto_history`
/// to HEAD-probe coverage (bybit's probe path differs from its bulk path).
#[derive(Debug, Clone, Default, Deserialize, Serialize)]
pub struct ArchiveUrlTemplate {
    /// Monthly archive URL prefix. Interpolates `{sym}` for the symbol token.
    /// Example: `https://data.binance.vision/data/spot/monthly/aggTrades/{sym}/`.
    #[serde(default)]
    pub monthly: Option<String>,
    /// Daily archive URL prefix. Interpolates `{sym}`.
    #[serde(default)]
    pub daily: Option<String>,
    /// Probe URL template — used by `fetch_crypto_history` coverage probe.
    /// Interpolates `{sym}`, `{y}`, `{m}` (zero-padded year/month).
    #[serde(default)]
    pub probe: Option<String>,
}

/// `network:` block — UDP + listener cfg. Today these knobs are env-driven via
/// `NxrConfig::from_env()`; the schema is here so `PipelineYml::load` accepts a
/// YAML that documents the values. Reading them at runtime stays env-first
/// (operator policy: per-pod override without YAML re-render).
#[derive(Debug, Clone, Default, Deserialize, Serialize)]
pub struct NetworkYml {
    #[serde(default)]
    pub listen_port: Option<u16>,
    #[serde(default)]
    pub aggregation_interval_ms: Option<u64>,
    #[serde(default)]
    pub stale_threshold_ms: Option<u64>,
    #[serde(default)]
    pub heartbeat_interval_ms: Option<u64>,
    #[serde(default)]
    pub server_host: Option<String>,
    #[serde(default)]
    pub server_port: Option<u16>,
    #[serde(default)]
    pub multicast_addr_a: Option<String>,
    #[serde(default)]
    pub multicast_port_a: Option<u16>,
    #[serde(default)]
    pub multicast_addr_b: Option<String>,
    #[serde(default)]
    pub multicast_port_b: Option<u16>,
    /// Bar feeds — distinct multicast legs for s10 / renko / synth flavors.
    /// Was: `core::bars_s10::MCAST_S10_*` + `core::bars_renko::MCAST_RENKO_*`
    /// (phase 59.R2C.6).
    #[serde(default)]
    pub bars: BarsMcastYml,
}

/// `network.bars:` — per-feed multicast addr+port. Empty fields fall back to
/// the audit-frozen const values to keep deploy stable when YAML is silent.
/// 2026-06-01: synth shares the same leg as native (operator: "s10_synth and
/// s10 are the same object") — only 2 legs total now (s10, renko).
#[derive(Debug, Clone, Default, Deserialize, Serialize)]
pub struct BarsMcastYml {
    #[serde(default)]
    pub s10_addr: Option<String>,
    #[serde(default)]
    pub s10_port: Option<u16>,
    #[serde(default)]
    pub renko_addr: Option<String>,
    #[serde(default)]
    pub renko_port: Option<u16>,
}

/// `server:` block — REST/WS layer cfg. Phase 59.R2C.5 moved
/// `core::server::security` defaults here.
#[derive(Debug, Clone, Default, Deserialize, Serialize)]
pub struct ServerYml {
    /// Default CORS origins applied when `NXR_CORS_ORIGINS` is unset. Was:
    /// `core::server::security::DEFAULT_CORS_ORIGINS`.
    #[serde(default)]
    pub cors_origins: Vec<String>,
    /// Per-IP token-bucket + global concurrency caps. Was:
    /// `core::server::security::{DEFAULT_MAX_CONCURRENCY, DEFAULT_RL_BURST,
    /// DEFAULT_RL_PER_SEC}`.
    #[serde(default)]
    pub rate_limits: RateLimitsYml,
}

#[derive(Debug, Clone, Default, Deserialize, Serialize)]
pub struct RateLimitsYml {
    #[serde(default)]
    pub max_concurrency: Option<usize>,
    #[serde(default)]
    pub burst: Option<u32>,
    #[serde(default)]
    pub per_sec: Option<u64>,
}

/// `cexs:` block — exchange + asset metadata + classification lists.
///
/// CANONICAL HOME for soft / changeable asset lists. All previously
/// hardcoded `const` arrays in `core/`, `weights/`, `series-factory/bin/*`
/// now read from these fields (operator mandate 2026-05-29: NO hardcoded
/// vars; consolidated config). Empty `Vec` = falls back to sdk default
/// when callsite needs one (e.g. bridge_stables defaults to FNV-frozen
/// audit list).
#[derive(Debug, Clone, Default, Deserialize, Serialize)]
pub struct CexsYml {
    /// Min 24h USD volume for a volatile asset to be eligible (weights scraper).
    /// Was: `weights::config::default_min_volatile`.
    #[serde(default)]
    pub min_volume_volatile_usd: Option<f64>,
    /// Min 24h USD volume for a stablecoin to be eligible (weights scraper).
    /// Stablecoin lower bar is intentional (most stable liquidity is on-chain).
    /// Was: `weights::config::default_min_stable`.
    #[serde(default)]
    pub min_volume_stable_usd: Option<f64>,
    /// Min 24h USD volume threshold used by `core::weights` injection-rule
    /// builder (`load_file`) to decide whether to emit a synth injection for
    /// a stablecoin-quoted or USD-quoted pair. Was: hardcoded `100_000.0`
    /// literal at `core/src/weights.rs:190,211` (phase 59.R3.C2.O1,
    /// 2026-05-30). Distinct from the scraper's eligibility threshold above.
    /// `None` ⇒ falls back to [`DEFAULT_MIN_VOLUME_INJECTION_USD`].
    #[serde(default)]
    pub min_volume_injection_usd: Option<f64>,
    /// Weights scraper poll cadence (minutes). Was:
    /// `weights::config::default_scrape_interval`.
    #[serde(default)]
    pub scrape_interval_minutes: Option<u64>,
    /// Path to the rendered weights file consumed by the NXR collector.
    /// Was: `weights::config::default_weights_path`.
    #[serde(default)]
    pub weights_path: Option<String>,
    /// Ticker alias map: non-standard → canonical (e.g. `XBT → BTC`). Was:
    /// `weights::config::Cexs::aliases`.
    #[serde(default)]
    pub aliases: BTreeMap<String, String>,
    /// Stablecoins recognized at runtime (was: `core::weights::BRIDGE_STABLES`,
    /// `series_factory/renko_trailing::STABLE_SYMBOLS`, `mtf_sweep::STABLE_SYMBOLS`).
    #[serde(default)]
    pub stablecoins: Vec<String>,
    /// Bridge-quoted stables (subset of `stablecoins` used for synth-USD
    /// derivation). If empty, callers fall back to `stablecoins`.
    #[serde(default)]
    pub bridge_stables: Vec<String>,
    /// USD fiat quote aliases recognized by the synth-injection builder as
    /// the "raw USD" leg (distinct from on-chain stables). Was: hardcoded
    /// `"USD"` literal at `core/src/weights.rs:226` (phase 59.R3.C5.A3,
    /// 2026-05-30). When the quote is one of these, the builder emits a
    /// `target × bridge_stable/USD` (inverted) injection rule.
    /// Defaults to `["USD"]` if YAML key is absent.
    #[serde(default)]
    pub usd_aliases: Vec<String>,
    /// Major-cap crypto asset symbols (base side). Was:
    /// `series_factory/{nxr_calibrate,renko_trailing,mtf_sweep}::CRYPTO_MAJORS`.
    #[serde(default)]
    pub crypto_majors: Vec<String>,
    /// FX major currency symbols. Was: `series_factory::nxr_calibrate::FX_MAJORS`.
    #[serde(default)]
    pub fx_majors: Vec<String>,
    /// All scrape-able assets (input list for the weights scraper).
    #[serde(default)]
    pub assets: Vec<String>,
    /// Cross-currency / cross-base crypto pairs forwarded by upstream providers
    /// (e.g. `BTC/EUR`, `ETH/BTC`, `KZT/USDT`). Was: `core::main::CRYPTO_CROSS_PAIRS`
    /// (phase 59.R2C.2). The core sink pre-registers ticker IDs for each pair
    /// in `symbol_map` so they round-trip through the REST/WS API.
    #[serde(default)]
    pub cross_pairs: Vec<String>,
    /// Per-exchange metadata. Keyed by lowercase exchange name (e.g. `binance`).
    /// Mitch IDs canonical here (phase 59.R2C.3 dropped the Rust mirror).
    /// URLs canonical here (phase 59.R2C.4 dropped the per-handler hardcode).
    #[serde(default)]
    pub exchanges: BTreeMap<String, ExchangeYml>,
    /// Volume scraper config — URLs + intervals. Was: `weights::scraper::CMC_PAIRS_URL`.
    #[serde(default)]
    pub scraper: ScraperYml,
}

/// `cexs.scraper:` block — endpoints + selectors for CEX volume scraping.
#[derive(Debug, Clone, Default, Deserialize, Serialize)]
pub struct ScraperYml {
    /// CoinMarketCap pairs endpoint. Defaults to the production CMC API
    /// when absent. Override here to test against a staging mirror or to
    /// pin against a specific API version without rebuilding.
    #[serde(default)]
    pub cmc_pairs_url: Option<String>,
}

/// `series:` block — pipeline-internal config.
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct SeriesYml {
    pub renko: RenkoYml,
    pub vol: VolConfig,
    pub calibration: CalibrationYml,
    pub pipeline: PipelineParamsYml,
}

/// `series.renko:` block. `max_pct` dropped 2026-05-24 (operator: markets be
/// markets); serde tolerates extra keys so a stale `max_pct:` in older yml
/// is silently ignored.
#[derive(Debug, Clone, Copy, Deserialize, Serialize)]
pub struct RenkoYml {
    pub min_pct: f32,
}

fn default_rolling_window_days() -> usize { 365 }
fn default_bracket_max_iters() -> usize { 12 }
fn default_accept_tol() -> f64 { 0.05 }

/// `series.calibration:` block. Mirrors `series_factory::bar_construction::
/// calibrate::CalibrationConfig` plus the per-class target table.
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct CalibrationYml {
    pub target_bpd: f64,
    /// Single trailing window (days) over which the per-UTC-day brick-count
    /// MEDIAN is computed and driven to `target_bpd`. Replaces the legacy MTF
    /// `k_fit_windows_days` blend — one window, one median, one engine
    /// (`docs/renko-methodology.md` §3). Default 365 (one full bull/bear/chop
    /// rotation; 2× the longest σ-blend window).
    #[serde(default = "default_rolling_window_days")]
    pub rolling_window_days: usize,
    pub min_window_days: usize,
    /// Iteration cap for the BOUNDED log-k bracket fallback inside
    /// `scale_to_target_k` (the wrong-tread safety net; methodology §4 step 6).
    /// Renamed from `max_rounds`. Default 12.
    #[serde(default = "default_bracket_max_iters", alias = "max_rounds")]
    pub bracket_max_iters: usize,
    /// Warm-start accept tolerance: the direct scale-to-target step accepts when
    /// `|median/target − 1| ≤ accept_tol`. ALSO the advisory achieved-err warn
    /// threshold (NOT a ticker-drop gate — methodology §4 step 8). Default 0.05.
    #[serde(default = "default_accept_tol", alias = "tolerance")]
    pub accept_tol: f64,
    pub mult_bounds: [f64; 2],
    /// Per-pair `target_bpd` overrides keyed by pair string (e.g. "USDC/USDT"
    /// → 50). Highest-priority lookup; reserve for genuine per-pair exceptions
    /// the class default can't express (a specific LSD/LST, a one-off regime).
    #[serde(default)]
    pub target_bpd_overrides: BTreeMap<String, f64>,
    /// Per-asset-class `target_bpd` defaults keyed by `AssetClassBucket::as_key`
    /// (e.g. "crypto_stable" → 50). Applied when no per-pair override matches,
    /// using the class detected by `asset_class::bucket_for_pair` (which reads
    /// MITCH wire bits + the `cexs.stablecoins` membership list). Lets every
    /// stable/stable pair inherit the low target by CLASS, so a newly listed
    /// stablecoin can't silently fall back to the flat 300 default just because
    /// nobody added it to the per-pair override list.
    #[serde(default)]
    pub target_bpd_by_class: BTreeMap<String, f64>,
    /// PART B4 (2026-06-09): per-pair FORCED renko-k escape hatch keyed by pair
    /// string (e.g. "BTC/USDT" → 0.42). When present and within
    /// `[K_FLOOR, K_MAX_SAFETY]` (no upper market ceiling — only the numeric
    /// safety cap), the calibrator EMITS this k directly and SKIPS the fit for
    /// that pair. Reserved for "structural-floor" tickers the
    /// staircase prevents the fit from landing within `RENKO_BPD_ACCEPT_TOL` of
    /// target (surfaced by the per-ticker `achieved_err` log). Empty by default —
    /// add entries only after the fit log shows a ticker's floor exceeds tol.
    #[serde(default)]
    pub renko_k_overrides: BTreeMap<String, f64>,
}

impl CalibrationYml {
    /// Resolve `target_bpd` for a given pair string (e.g. "BTC/USDT") WITHOUT
    /// class detection. Lookup: per-pair override → flat top-level `target_bpd`.
    /// Prefer `target_for_pair_classed` where the caller knows the asset class.
    pub fn target_for_pair(&self, pair: &str) -> f64 {
        self.target_bpd_overrides.get(pair).copied().unwrap_or(self.target_bpd)
    }

    /// Resolve `target_bpd` using the detected asset-class bucket.
    /// Lookup order: per-pair override → per-class default → flat `target_bpd`.
    /// `class_key` is `AssetClassBucket::as_key()` (e.g. "crypto_stable").
    /// Never skips — operator policy: the calibrator always has a target.
    pub fn target_for_pair_classed(&self, pair: &str, class_key: &str) -> f64 {
        if let Some(&v) = self.target_bpd_overrides.get(pair) {
            return v;
        }
        if let Some(&v) = self.target_bpd_by_class.get(class_key) {
            return v;
        }
        self.target_bpd
    }

    /// Assert the YAML `mult_bounds` are sane bracket bounds for the calibrator.
    ///
    /// LOWER bound is a hard floor: `mult_bounds[0]` MUST equal
    /// `renko::MULT_LOWER_BOUND` (= K_FLOOR). The bisection does NOT auto-expand
    /// downward — a too-flat asset that can't reach target even at K_FLOOR is
    /// legitimately dropped (operator directive 2026-06-09: "Maybe a minimum").
    ///
    /// UPPER bound is now only the INITIAL bracket HINT for the full-history
    /// log-k bisection — NOT a ceiling. The bisection auto-EXPANDS `k_hi` upward
    /// (doubling) when the median==target crossing sits above `mult_bounds[1]`,
    /// so a storming crypto calibrates beyond the old 4.0 wall (operator
    /// directive: "K should not have a max"). We therefore only require it to be
    /// a positive, above-the-floor seed — no equality to a fixed ceiling.
    ///
    /// Returns an error (callers `bail!`/`expect` at startup) rather than
    /// panicking inline, so config-load surfaces a clean diagnostic.
    pub fn assert_bounds_consistent(&self) -> Result<(), String> {
        use crate::renko::MULT_LOWER_BOUND;
        if (self.mult_bounds[0] - MULT_LOWER_BOUND).abs() > f64::EPSILON {
            return Err(format!(
                "calibration.mult_bounds[0]={} disagrees with renko::MULT_LOWER_BOUND={} (K_FLOOR)",
                self.mult_bounds[0], MULT_LOWER_BOUND
            ));
        }
        if !(self.mult_bounds[1] > self.mult_bounds[0]) {
            return Err(format!(
                "calibration.mult_bounds[1]={} must be > mult_bounds[0]={} (it seeds the \
                 bisection's INITIAL upper bracket; the search auto-expands it upward as needed)",
                self.mult_bounds[1], self.mult_bounds[0]
            ));
        }
        Ok(())
    }
}

/// `series.pipeline:` block — replay / backfill knobs.
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct PipelineParamsYml {
    pub bootstrap_days: i64,
    #[serde(default)]
    pub max_bars: usize,
    #[serde(default)]
    pub max_mem_gb: usize,
    #[serde(default)]
    pub exchanges: Vec<String>,
    #[serde(default)]
    pub pairs: Vec<String>,
    /// Disk-footprint guard knobs for the backfill orchestrator. Single-source
    /// YAML (operator mandate, no hardcoded magic in the binary). See
    /// `backfill-all`'s pre-flight headroom guard + monthly stream-and-delete.
    #[serde(default)]
    pub backfill: BackfillDiskYml,
}

/// `series.pipeline.backfill:` — disk-footprint guard knobs for `backfill-all`.
///
/// The orchestrator now fetches raw `.ticks` one calendar month at a time,
/// folds each month into the per-provider `.idx`, then deletes that month's
/// raw `.ticks` before fetching the next. Peak raw footprint is therefore
/// bounded to ~1 month × n_exchanges (not the full backfill range). These
/// knobs drive the pre-flight headroom guard that aborts the run if the
/// projected monthly raw peak would not fit the free space on the data PVC.
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct BackfillDiskYml {
    /// Conservative estimate of raw `.ticks` bytes produced per exchange per
    /// calendar day for a liquid pair (binance/bybit monthly aggTrades for a
    /// top pair run ~1-2 GiB/month ≈ 30-70 MiB/day; we size for the worst
    /// liquid pair). Used by the pre-flight guard to project the monthly peak.
    pub bytes_per_exchange_day: u64,
    /// Safety factor applied to free space in the headroom guard: the run
    /// aborts unless `projected_peak <= free_bytes * safety_factor`. `< 1.0`
    /// leaves a margin (e.g. 0.8 = require 25% headroom above the projection).
    pub headroom_safety_factor: f64,
}

impl Default for BackfillDiskYml {
    fn default() -> Self {
        // Defaults sized for the launch universe (binance/bybit liquid pairs).
        // ~70 MiB/exch/day is a conservative upper bound for the heaviest
        // monthly aggTrades; safety_factor 0.8 keeps 20% slack on free space.
        Self {
            bytes_per_exchange_day: 70 * 1024 * 1024,
            headroom_safety_factor: 0.8,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cal() -> CalibrationYml {
        CalibrationYml {
            target_bpd: 300.0,
            rolling_window_days: 365,
            min_window_days: 30,
            bracket_max_iters: 12,
            accept_tol: 0.05,
            mult_bounds: [0.05, 4.0],
            target_bpd_overrides: BTreeMap::from([("USDC/USDT".to_string(), 50.0)]),
            target_bpd_by_class: BTreeMap::from([("crypto_stable".to_string(), 50.0)]),
            renko_k_overrides: BTreeMap::new(),
        }
    }

    #[test]
    fn classed_resolution_order() {
        let c = cal();
        // 1. explicit per-pair override wins over everything.
        assert_eq!(c.target_for_pair_classed("USDC/USDT", "crypto_stable"), 50.0);
        // 2. no override, but class default applies — the whole point: a stable
        //    pair NOT in the override list still gets 50 by class detection.
        assert_eq!(c.target_for_pair_classed("FDUSD/USDT", "crypto_stable"), 50.0);
        // 3. no override, no class default → flat default.
        assert_eq!(c.target_for_pair_classed("BTC/USDT", "crypto_major"), 300.0);
        // unclassified bucket also falls back to flat default.
        assert_eq!(c.target_for_pair_classed("FOO/BAR", "default"), 300.0);
    }

    #[test]
    fn non_classed_helper_unchanged() {
        let c = cal();
        // Legacy path: only the per-pair override map, no class layer.
        assert_eq!(c.target_for_pair("USDC/USDT"), 50.0);
        assert_eq!(c.target_for_pair("FDUSD/USDT"), 300.0); // not in override map
    }

    /// PART B4 (2026-06-09): the per-pair forced-k escape hatch. The calibrator
    /// binary short-circuits the fit when `renko_k_overrides` has the pair AND
    /// the k is in `[K_FLOOR, K_MAX_SAFETY]` (no upper market ceiling — operator
    /// directive: "K should not have a max"). This pins the field's
    /// deserialization + the exact bounds predicate the binary applies.
    #[test]
    fn renko_k_override_short_circuits_fit() {
        use crate::renko::{K_FLOOR, K_MAX_SAFETY};
        // Field deserializes from YAML and defaults to empty when absent.
        // "BIG/HI": 9.0 is now ACCEPTED (above the old 4.0 wall, below safety cap).
        let y: CalibrationYml = serde_yml::from_str(
            "target_bpd: 300\nrolling_window_days: 365\nmin_window_days: 30\n\
             bracket_max_iters: 12\naccept_tol: 0.05\nmult_bounds: [0.05, 4.0]\n\
             renko_k_overrides:\n  \"BTC/USDT\": 0.42\n  \"BAD/LOW\": 0.001\n  \"BIG/HI\": 9.0\n",
        ).expect("parse CalibrationYml with renko_k_overrides");

        // Present + in-bounds ⇒ the binary emits this k and skips the fit.
        let forced = y.renko_k_overrides.get("BTC/USDT").copied();
        assert_eq!(forced, Some(0.42));
        assert!((K_FLOOR..=K_MAX_SAFETY).contains(&forced.unwrap()),
            "in-bounds override short-circuits");

        // Below K_FLOOR is still ignored (the floor is preserved).
        assert!(!(K_FLOOR..=K_MAX_SAFETY)
            .contains(&y.renko_k_overrides["BAD/LOW"]), "below K_FLOOR ignored");
        // Above the OLD 4.0 ceiling is now ACCEPTED (no upper market cap).
        assert!((K_FLOOR..=K_MAX_SAFETY)
            .contains(&y.renko_k_overrides["BIG/HI"]), "k above old 4.0 wall now accepted");

        // Absent pair ⇒ no override ⇒ normal fit path.
        assert!(y.renko_k_overrides.get("ETH/USDT").is_none());

        // Default (field omitted) ⇒ empty map.
        let c = cal();
        assert!(c.renko_k_overrides.is_empty());
    }

    /// Lean calibration field set (2026-06-10, `docs/renko-methodology.md`):
    /// the new `rolling_window_days` / `accept_tol` / `bracket_max_iters` parse,
    /// the legacy `max_rounds`/`tolerance` keys still deserialize via serde
    /// `alias` (forward-compat with un-migrated yml), and omitted fields take the
    /// documented defaults (365 / 0.05 / 12).
    #[test]
    fn lean_calibration_fields_parse_with_defaults_and_aliases() {
        // New canonical keys.
        let y: CalibrationYml = serde_yml::from_str(
            "target_bpd: 300\nrolling_window_days: 365\nmin_window_days: 30\n\
             bracket_max_iters: 12\naccept_tol: 0.05\nmult_bounds: [0.05, 4.0]\n",
        ).expect("parse lean CalibrationYml");
        assert_eq!(y.rolling_window_days, 365);
        assert_eq!(y.bracket_max_iters, 12);
        assert!((y.accept_tol - 0.05).abs() < 1e-12);

        // Legacy aliases: an un-migrated yml using `max_rounds` / `tolerance`
        // still deserializes into the renamed fields.
        let legacy: CalibrationYml = serde_yml::from_str(
            "target_bpd: 300\nrolling_window_days: 365\nmin_window_days: 30\n\
             max_rounds: 20\ntolerance: 0.08\nmult_bounds: [0.05, 4.0]\n",
        ).expect("parse legacy-aliased CalibrationYml");
        assert_eq!(legacy.bracket_max_iters, 20, "max_rounds aliases bracket_max_iters");
        assert!((legacy.accept_tol - 0.08).abs() < 1e-12, "tolerance aliases accept_tol");

        // Omitted optional fields → documented defaults.
        let minimal: CalibrationYml = serde_yml::from_str(
            "target_bpd: 300\nmin_window_days: 30\nmult_bounds: [0.05, 4.0]\n",
        ).expect("parse minimal CalibrationYml");
        assert_eq!(minimal.rolling_window_days, 365);
        assert_eq!(minimal.bracket_max_iters, 12);
        assert!((minimal.accept_tol - 0.05).abs() < 1e-12);
    }
}
