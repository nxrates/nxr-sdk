//! Shared schema for `ticker-params.json` - the file produced by `nxr-weights`
//! and consumed by the aggregator's weights loader. Both sides derive from the
//! same struct so drift is impossible at compile time.

use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct WeightsFile {
    #[serde(default)]
    pub generated_at: String,
    /// Stablecoin to USD rates for quote triangulation.
    #[serde(default)]
    pub stable_rates: BTreeMap<String, f64>,
    /// cmc_slug -> { "BTC/USDT": volume_usd, ... }
    #[serde(default)]
    pub pair_volumes: BTreeMap<String, BTreeMap<String, f64>>,
    /// cmc_slug -> { mitch_id, name, weight }
    #[serde(default)]
    pub exchanges: BTreeMap<String, ExchangeMeta>,
    /// FX provider defaults: mitch_id (as string) -> raw weight.
    /// Applied when a provider has no per-pair override for a ticker.
    /// Phase 60.π rename: was `broker_defaults`. Serde alias retained for
    /// transition window (existing ticker-params.json files still parse).
    #[serde(default, alias = "broker_defaults")]
    pub provider_defaults: BTreeMap<String, f64>,
    /// FX provider per-pair overrides: mitch_id (as string) -> pair ("EUR/USD") -> raw weight.
    /// Hardcoded in config.yml (FX venues have no public volume feed, so these
    /// express prime-provider quality rankings per instrument rather than measured
    /// volume). Falls back to `provider_defaults` when a pair is not listed.
    /// Phase 60.π rename: was `broker_pair_weights`. Serde alias retained.
    #[serde(default, alias = "broker_pair_weights")]
    pub provider_pair_weights: BTreeMap<String, BTreeMap<String, f64>>,
    /// Calibrated Renko `multiplier` per ticker (output of `nxr-calibrate`).
    /// Key = ticker_id as a decimal string (JSON object keys must be strings).
    /// Consumed by the aggregator's renko bar emitter at hot-reload time.
    /// Missing entries fall back to the per-ticker prior or the `config.yml`
    /// default multiplier — never panic.
    #[serde(default)]
    pub renko_k_per_ticker: BTreeMap<String, f64>,
    /// Unix-seconds timestamp of the last successful `nxr-calibrate` run.
    /// Optional so legacy weights files (pre-calibration era) still parse.
    #[serde(default)]
    pub calibrated_at: Option<u64>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ExchangeMeta {
    pub mitch_id: u16,
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub weight: f64,
}
