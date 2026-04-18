//! Shared schema for `ticker-params.json` — the file produced by `nxr-weights`
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
    /// FX broker defaults: mitch_id (as string) -> raw weight.
    #[serde(default)]
    pub broker_defaults: BTreeMap<String, f64>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ExchangeMeta {
    pub mitch_id: u16,
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub weight: f64,
}
