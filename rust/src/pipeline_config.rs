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
    pub brokers: BTreeMap<String, serde_yml::Value>,
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
