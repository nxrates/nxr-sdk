use crate::resolve::{resolve_asset_in_class, resolve_ticker};
use dashmap::DashMap;
use mitch::ticker::forex_ticker;
use mitch::{AssetClass, InstrumentType};
use tracing::warn;

/// Split a canonical `BASE/QUOTE` pair string into its two legs.
///
/// Canonical NXR pair format = `"<BASE>/<QUOTE>"` (uppercase preserved by
/// caller — this fn does no case folding). Returns `None` when the input
/// has no `/`, has more than one `/`, or either side is empty.
///
/// Single SDK source of truth. Replaced ad-hoc splitting in `core::weights`,
/// `weights::main`, `series-factory::bin::nxr_calibrate`, `crypto::client`
/// (phase 59.R3.H4) and, in `crypto::exchange::upbit`, `format_symbol` then
/// `normalize_symbol` (which kept a hand-rolled `find('-')` until 2026-08-18).
#[inline]
pub fn split_pair(pair: &str) -> Option<(&str, &str)> {
    split_pair_multi(pair, &['/'])
}

/// Like [`split_pair`] but accepts any separator from `seps`. Used by CLI
/// arg parsers that accept both `BASE/QUOTE` and `BASE-QUOTE` (e.g.
/// `series-factory` bins, `mtf_sweep`). Returns `None` when no separator
/// matches, more than one separator is present, or either side is empty.
///
/// Phase 59.R3.C3.O4 (2026-05-30) — was duplicated as
/// `series_factory::split_pair` (find-+-slice) and an inline
/// `tok.split(['/', '-'])` in `mtf_sweep`.
#[inline]
pub fn split_pair_multi<'a>(pair: &'a str, seps: &[char]) -> Option<(&'a str, &'a str)> {
    let sep_ix = pair.find(|c: char| seps.contains(&c))?;
    let base = &pair[..sep_ix];
    let rest = &pair[sep_ix + 1..];
    if base.is_empty() || rest.is_empty() {
        return None;
    }
    // Reject inputs with more than one separator (matches `split_pair`'s
    // strict 2-leg contract).
    if rest.contains(|c: char| seps.contains(&c)) {
        return None;
    }
    Some((base, rest))
}

/// Resolve and cache MITCH ticker IDs from canonical symbol strings like "BTC/USDT".
pub struct TickerIdCache {
    cache: DashMap<String, u64>,
}

impl TickerIdCache {
    pub fn new() -> Self {
        Self {
            cache: DashMap::new(),
        }
    }

    #[inline]
    pub fn get_or_compute(&self, symbol: &str) -> u64 {
        *self
            .cache
            .entry(symbol.to_string())
            .or_insert_with(|| resolve_ticker_id(symbol))
    }

    pub fn preload_symbols(&self, symbols: &[String]) {
        for symbol in symbols {
            self.get_or_compute(symbol);
        }
    }
}

impl Default for TickerIdCache {
    fn default() -> Self {
        Self::new()
    }
}

/// Resolve a 6-char pure-alpha FX symbol (e.g. "USDJPY") using direct 3+3 split.
///
/// The generic `resolve_ticker` algorithm has a known issue with USD/EUR/GBP-prefixed
/// pairs: it detects the prefix as a "major quote currency" and reverses base/quote
/// (e.g. "USDJPY" -> JPY/USD instead of USD/JPY). This function bypasses that by
/// treating the first 3 chars as base and last 3 as quote, matching the 6-char
/// symbol form brokers quote.
fn resolve_fx6_ticker_id(symbol: &str) -> Option<u64> {
    let b = symbol.as_bytes();
    if b.len() != 6 || !b.iter().all(|c| c.is_ascii_alphabetic()) {
        return None;
    }
    let base = resolve_asset_in_class(&symbol[..3], 0.90, AssetClass::FX)?;
    let quote = resolve_asset_in_class(&symbol[3..], 0.90, AssetClass::FX)?;
    forex_ticker(
        base.asset.class_id,
        quote.asset.class_id,
        InstrumentType::SPOT,
        0,
    )
    .ok()
    .map(|t| t.raw)
}

/// Strict resolution: fx6 shortcut then full resolver, NO FNV fallback.
/// `None` = unresolvable symbol. Boot-time config validation (nxr-oracle)
/// uses this to fail loud instead of sharding under a phantom hash id.
pub fn try_resolve_ticker_id(symbol: &str) -> Option<u64> {
    // For 6-char pure-alpha FX pairs (EURUSD, USDJPY, USDCAD, ...), use a direct 3+3
    // base/quote split to match the 6-char broker symbol encoding.
    if let Some(id) = resolve_fx6_ticker_id(symbol) {
        return Some(id);
    }
    resolve_ticker(symbol, InstrumentType::SPOT)
        .ok()
        .map(|m| m.ticker.id)
}

pub fn resolve_ticker_id(symbol: &str) -> u64 {
    try_resolve_ticker_id(symbol).unwrap_or_else(|| {
        warn!(symbol, "mitch resolver failed, using FNV fallback");
        phantom_ticker_id(symbol)
    })
}

/// The FNV-1a id [`resolve_ticker_id`] falls back to when a symbol has no MITCH
/// id — a **phantom**: unique (so the ticker-id collision gate is blind to it),
/// but not a bit-packed `TickerId`, so anything decoding its class /
/// instrument-type bits reads hash noise.
///
/// Exposed so the shard migration (`series-factory/src/bin/migrate_phantom_ids`)
/// and the core boot check can compute a symbol's OLD directory name from ONE
/// definition. Never call this to mint an id for new data.
#[inline]
pub fn phantom_ticker_id(symbol: &str) -> u64 {
    fnv1a_64(symbol.as_bytes())
}

#[inline]
const fn fnv1a_64(data: &[u8]) -> u64 {
    const FNV_OFFSET: u64 = 14695981039346656037;
    const FNV_PRIME: u64 = 1099511628211;
    let mut hash = FNV_OFFSET;
    let mut i = 0;
    while i < data.len() {
        hash ^= data[i] as u64;
        hash = hash.wrapping_mul(FNV_PRIME);
        i += 1;
    }
    hash
}
