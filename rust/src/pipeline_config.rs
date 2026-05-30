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

use crate::parkinson::VolConfig;

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

/// Top-level wrapper matching the layout of `nxrates.yml`.
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct PipelineYml {
    #[serde(default)]
    pub cexs: CexsYml,
    pub series: SeriesYml,
    #[serde(default)]
    pub forex: ForexYml,
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
    /// Runtime tuning knobs for the forwarders, fx broker server, and core
    /// REST/WS layer. Phase 59.R3.C2.O5 (2026-05-30) — was hardcoded
    /// `const FORWARDER_HEARTBEAT_SECS / BROKER_STALE_SECS / FRAME_BUF_MAX
    /// / STALE_SECS / REFRESH_OFFSET_SECS / FLUSH_MS` in
    /// `crypto/src/main.rs`, `crypto/src/bin/fx.rs`,
    /// `core/src/server/{rest,mod,ws}.rs`.
    #[serde(default)]
    pub runtime: RuntimeYml,
}

/// `runtime:` block — forwarder + server tuning knobs. All `Option<…>`
/// fields fall back to their per-callsite `DEFAULT_RUNTIME_*` constant when
/// the YAML is silent (so old `config.yml` files keep working).
#[derive(Debug, Clone, Default, Deserialize, Serialize)]
pub struct RuntimeYml {
    /// Forwarder heartbeat cadence (seconds). Was: hardcoded
    /// `FORWARDER_HEARTBEAT_SECS = 5` in `crypto/src/main.rs:34` and
    /// `crypto/src/bin/fx.rs:52`.
    #[serde(default)]
    pub forwarder_heartbeat_secs: Option<u64>,
    /// FX broker liveness staleness threshold (seconds). Was: hardcoded
    /// `BROKER_STALE_SECS = 30` in `crypto/src/bin/fx.rs:48`.
    #[serde(default)]
    pub broker_stale_secs: Option<u64>,
    /// Cap on accumulated TCP input awaiting frame parsing (bytes). Defense
    /// in depth against runaway peers. Was: hardcoded
    /// `FRAME_BUF_MAX = 2 * 1024 * 1024` in `crypto/src/bin/fx.rs:254`.
    #[serde(default)]
    pub frame_buf_max: Option<usize>,
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
pub const DEFAULT_RUNTIME_BROKER_STALE_SECS: u64 = 30;
pub const DEFAULT_RUNTIME_FRAME_BUF_MAX: usize = 2 * 1024 * 1024;
pub const DEFAULT_RUNTIME_HEALTH_STALE_SECS: u64 = 20;
pub const DEFAULT_RUNTIME_DAILY_REFRESH_OFFSET_SECS: u64 = 30;
pub const DEFAULT_RUNTIME_WS_FLUSH_MS: u64 = 100;

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

/// `synths:` block — synth-pair registry.
#[derive(Debug, Clone, Default, Deserialize, Serialize)]
pub struct SynthsYml {
    /// Initial / launch synth pairs. Empty ⇒ fall back to
    /// [`crate::synth::pairs::DEFAULT_INITIAL_SYNTH_PAIRS`].
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

/// `forex:` block — non-CEX broker-forwarded symbols + broker metadata.
/// `symbols` is the slash-separated FX pairs list scraped by nxr-fx /
/// MT4 forwarders (was hardcoded in `weights/src/scraper/fx.rs` originally).
/// `broker_symbols` is the no-slash multi-asset list (FX + commodities +
/// indices + crypto CFDs) the broker forwarders subscribe to and the core
/// sink pre-registers in `symbol_map`. Was: `core::main::FX_SYMBOLS`
/// (phase 59.R2C.1).
#[derive(Debug, Clone, Default, Deserialize, Serialize)]
pub struct ForexYml {
    #[serde(default)]
    pub symbols: Vec<String>,
    #[serde(default)]
    pub broker_symbols: Vec<String>,
    #[serde(default)]
    pub brokers: BTreeMap<String, FxBrokerYml>,
}

/// `forex.brokers.<name>:` per-broker mitch_id + weight + per-pair overrides.
/// Was: `weights::config::FxBrokerEntry`.
#[derive(Debug, Clone, Default, Deserialize, Serialize)]
pub struct FxBrokerYml {
    #[serde(default)]
    pub mitch_id: u16,
    #[serde(default)]
    pub weight: f64,
    /// Per-pair weight overrides keyed by canonical "BASE/QUOTE".
    /// Empty ⇒ broker default applies to every pair.
    #[serde(default)]
    pub pair_weights: BTreeMap<String, f64>,
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
#[derive(Debug, Clone, Default, Deserialize, Serialize)]
pub struct BarsMcastYml {
    #[serde(default)]
    pub s10_addr: Option<String>,
    #[serde(default)]
    pub s10_port: Option<u16>,
    #[serde(default)]
    pub s10_synth_addr: Option<String>,
    #[serde(default)]
    pub s10_synth_port: Option<u16>,
    #[serde(default)]
    pub renko_addr: Option<String>,
    #[serde(default)]
    pub renko_port: Option<u16>,
    #[serde(default)]
    pub renko_synth_addr: Option<String>,
    #[serde(default)]
    pub renko_synth_port: Option<u16>,
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
    /// Cross-currency / cross-base crypto pairs forwarded by upstream brokers
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

/// `series.calibration:` block. Mirrors `series_factory::bar_construction::
/// calibrate::CalibrationConfig` plus the per-class target table.
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct CalibrationYml {
    pub target_bpd: f64,
    pub k_fit_windows_days: Vec<usize>,
    pub min_window_days: usize,
    pub max_rounds: usize,
    pub tolerance: f64,
    pub mult_bounds: [f64; 2],
    #[serde(default)]
    pub target_bpd_by_class: BTreeMap<String, ClassTarget>,
}

impl CalibrationYml {
    /// Resolve `target_bpd` for a given asset-class key (e.g. "crypto_major",
    /// "fx_cross"). Falls back to the `default` table entry, then to the
    /// flat top-level `target_bpd`. `None` ⇒ explicit skip via sentinel.
    pub fn target_for_class(&self, class_key: &str) -> Option<f64> {
        if let Some(t) = self.target_bpd_by_class.get(class_key) {
            return t.resolved();
        }
        if let Some(t) = self.target_bpd_by_class.get("default") {
            return t.resolved();
        }
        Some(self.target_bpd)
    }
}

/// Per-class entry in `target_bpd_by_class`. Either a numeric bpd target or
/// a sentinel string (e.g. `"skip"`).
#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(untagged)]
pub enum ClassTarget {
    Bpd(f64),
    Sentinel(String),
}

impl ClassTarget {
    /// `None` ⇒ skip this class; `Some(v)` ⇒ use `v` as target bpd.
    pub fn resolved(&self) -> Option<f64> {
        match self {
            ClassTarget::Bpd(v) if *v > 0.0 => Some(*v),
            ClassTarget::Bpd(_) => None,
            ClassTarget::Sentinel(s) if s.eq_ignore_ascii_case("skip") => None,
            ClassTarget::Sentinel(_) => None,
        }
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
}
