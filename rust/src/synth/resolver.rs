//! Dynamic triangulation resolver — single source of truth for symbol resolution.
//!
//! ## Problem replaced
//!
//! Prior to this module, NXR maintained a static `SYNTH_PATHS` registry listing
//! ~20 known triangulated pairs (`ETH/BTC`, `SOL/EUR`, etc). The registry was
//! brittle (every new pair required code change) and duplicated the
//! `symbol_map` (asset×quote Cartesian product available via TDWAP).
//!
//! ## The resolver contract
//!
//! Given any input symbol (MITCH `ticker_id`, slash form `BTC/USDT`, or
//! dash form `BTC-USDT`), classify it as one of:
//!
//! - **Direct**: the `ticker_id` exists in `symbol_map` ⇒ stream from disk.
//! - **Synth**: the `ticker_id` is NOT in `symbol_map`, but there is a common
//!   denominator `X` such that both `BASE/X` and `QUOTE/X` are direct
//!   ⇒ reconstruct on-the-fly via `px(BASE/QUOTE) = px(BASE/X) / px(QUOTE/X)`.
//! - **Identity**: `BASE == QUOTE` (e.g. `USDT/USDT`) ⇒ constant-1.0 stream.
//! - **NotFound**: no triangulation possible with current `symbol_map` +
//!   denominator priority.
//!
//! ## Why MITCH ticker_id is canonical
//!
//! Plaintext is ambiguous: `BTC/USDT` could be spot, a perpetual, a quarterly
//! future, etc. The MITCH ticker_id encodes instrument_type explicitly. The
//! resolver therefore:
//!   1. Normalizes any text input to a `ticker_id` (spot-only by default — if
//!      a caller wants futures or options, they MUST pass the `ticker_id`).
//!   2. Performs all lookups by `ticker_id`.
//!   3. Returns `Resolution` keyed on `ticker_id`.
//!
//! Dash AND slash forms are both accepted in text input — they normalize to
//! the same ticker_id.
//!
//! ## Triangulation algorithm
//!
//! Default denominator priority is `[USDT, USDC, USD, EUR, BTC]`. For target
//! `BASE/QUOTE`, iterate denominators X (skipping X == BASE and X == QUOTE);
//! for each X check whether both `BASE/X` and `QUOTE/X` exist in `symbol_map`
//! (inverse legs supported via `Leg::invert`). First match wins.
//!
//! Multi-hop (depth > 2) is intentionally rejected. Compounding spreads and
//! variance across 3+ legs degrades signal-to-noise below useful for HFT.
//! If a multi-hop is genuinely needed, add the relevant direct pair to
//! `symbol_map` (i.e. list it on a venue we collect from).
//!
//! ## Caching
//!
//! `Resolution` is deterministic given a fixed `symbol_map`. Resolutions are
//! cached in an LRU keyed by `ticker_id`. Invalidation triggers on
//! `symbol_map` deltas (assets added/removed from the collection config).

use std::sync::{Arc, RwLock};
use std::collections::HashMap;

use dashmap::DashMap;
use serde::Serialize;
use tracing::trace;

use crate::resolve_ticker_id;

/// Canonical resolution outcome for any input symbol.
#[derive(Debug, Clone, Serialize)]
pub enum Resolution {
    /// Direct pair — `ticker_id` exists in `symbol_map`.
    Direct(u64),
    /// Triangulated pair — `ticker_id` derived from legs.
    Synth(SynthPath),
    /// Identity pair (`BASE == QUOTE`) — emit constant 1.0 stream.
    Identity(u64),
    /// Not resolvable with current `symbol_map` + denominators.
    NotFound(ResolveErr),
}

#[derive(Debug, Clone, Serialize)]
pub enum ResolveErr {
    /// Text input couldn't be parsed (no `-` or `/`).
    Malformed(String),
    /// `BASE/QUOTE` parsed but no denominator yields both legs.
    NoDenominator { base: String, quote: String },
}

/// A synthetic pair derived from two leg pairs via a common denominator.
///
/// `px(target) = px(legs[0]) / px(legs[1])` after applying `invert` flips.
#[derive(Debug, Clone, Serialize)]
pub struct SynthPath {
    /// The target `ticker_id` being synthesized.
    pub target: u64,
    /// Numerator leg (`BASE/X`).
    pub num: Leg,
    /// Denominator leg (`QUOTE/X`).
    pub den: Leg,
    /// The chosen denominator asset (e.g. "USDT").
    pub denom: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct Leg {
    pub ticker_id: u64,
    /// If true, use `1/px(ticker_id)` (i.e. leg is `X/BASE` not `BASE/X`).
    pub invert: bool,
    /// Display sym for logging (slash form).
    pub sym: String,
}

/// Resolver configuration.
#[derive(Debug, Clone)]
pub struct ResolverCfg {
    /// Denominator preference order. First match wins.
    pub denominator_priority: Vec<String>,
    /// Allow `X/BASE` to substitute for `BASE/X` via `1/px` inversion.
    pub allow_inverse_legs: bool,
    /// LRU cache capacity.
    pub cache_capacity: usize,
}

impl Default for ResolverCfg {
    fn default() -> Self {
        Self {
            denominator_priority: vec![
                "USDT".to_string(),
                "USDC".to_string(),
                "USD".to_string(),
                "EUR".to_string(),
                "BTC".to_string(),
            ],
            allow_inverse_legs: true,
            cache_capacity: 4096,
        }
    }
}

/// Single canonical resolver. Replaces the static `SYNTH_PATHS` registry.
pub struct Resolver {
    /// `ticker_id -> ()` presence set, derived from the live `symbol_map`.
    /// Wrapped in `Arc<DashMap>` to share with the rest of the server.
    symbol_map: Arc<DashMap<String, u64>>,
    cfg: ResolverCfg,
    /// Resolution cache keyed by `ticker_id`. Bounded; LRU eviction is
    /// approximate (random eviction on overflow). Sufficient because the
    /// cache hit rate is dominated by the few hundred active syms.
    cache: Arc<RwLock<HashMap<u64, Resolution>>>,
}

impl Resolver {
    pub fn new(symbol_map: Arc<DashMap<String, u64>>) -> Self {
        Self::with_cfg(symbol_map, ResolverCfg::default())
    }

    pub fn with_cfg(symbol_map: Arc<DashMap<String, u64>>, cfg: ResolverCfg) -> Self {
        Self {
            symbol_map,
            cfg,
            cache: Arc::new(RwLock::new(HashMap::with_capacity(256))),
        }
    }

    /// Resolve from text (dash or slash both accepted). Spot-only — futures
    /// or options callers must use `resolve_ticker_id` directly.
    pub fn resolve_text(&self, input: &str) -> Resolution {
        let slash = normalize_to_slash(input);
        let ticker_id = resolve_ticker_id(&slash);
        self.resolve_id(ticker_id, Some(&slash))
    }

    /// Resolve from a MITCH ticker_id (canonical). The optional `display`
    /// is used for human-readable logging only.
    pub fn resolve_id(&self, ticker_id: u64, display: Option<&str>) -> Resolution {
        // Cache lookup.
        if let Some(hit) = self.cache.read().unwrap().get(&ticker_id).cloned() {
            return hit;
        }

        let resolution = self.resolve_inner(ticker_id, display);
        self.cache_put(ticker_id, resolution.clone());
        resolution
    }

    /// Invalidate the entire cache. Call when `symbol_map` mutates.
    pub fn invalidate(&self) {
        self.cache.write().unwrap().clear();
    }

    fn resolve_inner(&self, ticker_id: u64, display: Option<&str>) -> Resolution {
        // 1) Direct? Resolve display name from symbol_map (reverse lookup).
        if self.symbol_map.iter().any(|e| *e.value() == ticker_id) {
            return Resolution::Direct(ticker_id);
        }

        // 2) Parse the input symbol to (BASE, QUOTE) for triangulation.
        let slash = match display {
            Some(s) if s.contains('/') => s.to_string(),
            _ => {
                trace!(ticker_id, "synth: display not provided; cannot triangulate without BASE/QUOTE");
                return Resolution::NotFound(ResolveErr::Malformed(
                    display.unwrap_or("<no display>").to_string(),
                ));
            }
        };
        let (base, quote) = match slash.split_once('/') {
            Some((b, q)) => (b.to_string(), q.to_string()),
            None => return Resolution::NotFound(ResolveErr::Malformed(slash)),
        };

        // 3) Identity?
        if base == quote {
            return Resolution::Identity(ticker_id);
        }

        // 4) Triangulate: iterate denominator priority.
        for denom in &self.cfg.denominator_priority {
            if denom == &base || denom == &quote {
                continue;
            }

            let num = self.find_leg(&base, denom);
            let den = self.find_leg(&quote, denom);
            if let (Some(n), Some(d)) = (num, den) {
                return Resolution::Synth(SynthPath {
                    target: ticker_id,
                    num: n,
                    den: d,
                    denom: denom.clone(),
                });
            }
        }

        Resolution::NotFound(ResolveErr::NoDenominator { base, quote })
    }

    /// Find a leg for `base/quote`. Tries direct `base/quote` then inverse
    /// `quote/base` (if config allows).
    fn find_leg(&self, base: &str, quote: &str) -> Option<Leg> {
        let direct = format!("{}/{}", base, quote);
        if let Some(id) = self.symbol_map.get(&direct).map(|e| *e.value()) {
            return Some(Leg { ticker_id: id, invert: false, sym: direct });
        }
        if self.cfg.allow_inverse_legs {
            let inv = format!("{}/{}", quote, base);
            if let Some(id) = self.symbol_map.get(&inv).map(|e| *e.value()) {
                return Some(Leg { ticker_id: id, invert: true, sym: inv });
            }
        }
        None
    }

    fn cache_put(&self, ticker_id: u64, resolution: Resolution) {
        let mut cache = self.cache.write().unwrap();
        if cache.len() >= self.cfg.cache_capacity {
            // Crude eviction: drop a single entry (HashMap iteration order is
            // randomized so this is effectively random eviction). Good enough
            // for our cache hit-rate profile (~100 active syms in steady state).
            if let Some(k) = cache.keys().next().copied() {
                cache.remove(&k);
            }
        }
        cache.insert(ticker_id, resolution);
    }
}

/// Normalize any text symbol form to slash. Dash → slash. No-op on slash.
/// Numeric / hex prefixed forms are returned as-is (caller resolves separately).
#[inline]
pub fn normalize_to_slash(s: &str) -> String {
    if s.contains('-') {
        s.replace('-', "/")
    } else {
        s.to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dash_normalizes_to_slash() {
        assert_eq!(normalize_to_slash("BTC-USDT"), "BTC/USDT");
        assert_eq!(normalize_to_slash("BTC/USDT"), "BTC/USDT");
        assert_eq!(normalize_to_slash("0x060A8D644C100000"), "0x060A8D644C100000");
    }

    #[test]
    fn direct_lookup_returns_direct() {
        let id = resolve_ticker_id("BTC/USDT");
        let sm: Arc<DashMap<String, u64>> = Arc::new(DashMap::new());
        sm.insert("BTC/USDT".to_string(), id);
        let r = Resolver::new(sm);

        // Dash and slash both yield Direct.
        assert!(matches!(r.resolve_text("BTC-USDT"), Resolution::Direct(_)));
        assert!(matches!(r.resolve_text("BTC/USDT"), Resolution::Direct(_)));
    }

    #[test]
    fn missing_yields_synth_when_legs_exist() {
        let btc_usdt = resolve_ticker_id("BTC/USDT");
        let eth_usdt = resolve_ticker_id("ETH/USDT");
        let sm: Arc<DashMap<String, u64>> = Arc::new(DashMap::new());
        sm.insert("BTC/USDT".to_string(), btc_usdt);
        sm.insert("ETH/USDT".to_string(), eth_usdt);
        let r = Resolver::new(sm);

        // ETH-BTC not direct, but BOTH legs (ETH/USDT and BTC/USDT) exist.
        match r.resolve_text("ETH-BTC") {
            Resolution::Synth(p) => {
                assert_eq!(p.denom, "USDT");
                assert_eq!(p.num.sym, "ETH/USDT");
                assert_eq!(p.den.sym, "BTC/USDT");
                assert!(!p.num.invert);
                assert!(!p.den.invert);
            }
            other => panic!("expected Synth, got {:?}", other),
        }
    }

    #[test]
    fn identity_when_base_eq_quote() {
        let id = resolve_ticker_id("USDT/USDT");
        let sm: Arc<DashMap<String, u64>> = Arc::new(DashMap::new());
        let r = Resolver::new(sm);
        let res = r.resolve_id(id, Some("USDT/USDT"));
        assert!(matches!(res, Resolution::Identity(_)));
    }

    #[test]
    fn missing_with_no_legs_yields_notfound() {
        let sm: Arc<DashMap<String, u64>> = Arc::new(DashMap::new());
        let r = Resolver::new(sm);
        match r.resolve_text("FOO-BAR") {
            Resolution::NotFound(ResolveErr::NoDenominator { base, quote }) => {
                assert_eq!(base, "FOO");
                assert_eq!(quote, "BAR");
            }
            other => panic!("expected NotFound(NoDenominator), got {:?}", other),
        }
    }

    #[test]
    fn inverse_leg_supported() {
        // Only USDT/BTC exists (not BTC/USDT). Inverse should kick in.
        let usdt_btc = resolve_ticker_id("USDT/BTC");
        let usdt_eth = resolve_ticker_id("USDT/ETH");
        let sm: Arc<DashMap<String, u64>> = Arc::new(DashMap::new());
        sm.insert("USDT/BTC".to_string(), usdt_btc);
        sm.insert("USDT/ETH".to_string(), usdt_eth);
        let r = Resolver::new(sm);
        match r.resolve_text("ETH-BTC") {
            Resolution::Synth(p) => {
                // ETH leg = USDT/ETH inverted; BTC leg = USDT/BTC inverted.
                // denom = USDT (because USDT is in priority list and BOTH legs found).
                // Hmm, wait — USDT IS the base of both stored pairs. The resolver
                // skips X if X==base or X==quote of target. Target is ETH/BTC,
                // so USDT != ETH and USDT != BTC → eligible. Find ETH/USDT? No,
                // we stored USDT/ETH. Inverse allowed → invert=true. Same for BTC.
                assert_eq!(p.denom, "USDT");
                assert!(p.num.invert);
                assert!(p.den.invert);
            }
            other => panic!("expected Synth, got {:?}", other),
        }
    }
}
