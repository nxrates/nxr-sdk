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
