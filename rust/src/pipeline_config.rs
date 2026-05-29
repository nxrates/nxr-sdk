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
