//! Market provider resolution - lookup functions for exchange/venue names.
//!
//! Wraps the generated constants in mitch with convenient lookup APIs.
//! Types (MarketProvider, ProviderMatch) remain defined in mitch.

use mitch::constants::{resolve_market_providers, market_providers_by_id};
use mitch::market_providers::MarketProvider;

/// Get market provider by numeric ID.
pub fn get_market_provider_by_id(id: u16) -> Option<MarketProvider> {
    market_providers_by_id(id as u64).map(MarketProvider::from)
}

/// Get market provider ID by exact name match.
pub fn get_market_provider_id_by_name(name: &str) -> Option<u16> {
    resolve_market_providers(name).map(|entry| entry.id as u16)
}

/// Providers HARD-EXCLUDED from all aggregation and index construction
/// (operator decision 2026-07-04). These venues published fabricated L1 sizes
/// (24h turnover as top-of-book qty: Biconomy=71, Coinstore=361, CoinW=371).
/// NOTE (red-team verified 2026-07-04): TDWAP price weighting is
/// base_weight × decay — fake sizes never moved the published bid/ask/spread;
/// they corrupt the SUMMED volume fields (vbid/vask) and everything derived
/// from them (vol_imbalance/OFI, volume features). Exclusion is a DATA-HYGIENE
/// decision: venues that fabricate L1 fields are untrusted everywhere.
/// Enforced at every layer: forwarder spawn, live aggregation, offline
/// re-aggregation — single source of truth here.
pub const EXCLUDED_PROVIDERS: [u16; 3] = [71, 361, 371];

/// True if this provider is hard-excluded from aggregation/index construction.
#[inline]
pub fn is_excluded_provider(id: u16) -> bool {
    EXCLUDED_PROVIDERS.contains(&id)
}
