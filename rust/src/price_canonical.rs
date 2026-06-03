//! Index symbol aliases for MITCH ticker resolution (both pair legs).
//!
//! One map (`cexs.price_canonical`) drives alias → canonical index symbol.
//! Applied to **base and quote** so `ETH/DAI`, `WETH/USDT`, and `BTC/USDT0`
//! share shards with `ETH/USDS`, `ETH/USDT`, `BTC/USDT`.
//!
//! Distinct from:
//! - `cexs.aliases` — weights scraper string normalization (XBT→BTC)
//! - MITCH asset IDs — on-chain / oracle identity unchanged
//! - `wrapperOf` in BTR SDK — UI search; LSTs stay index-distinct
//!
//! ## Kinds (API `alias_kind`)
//! - `wrapper_1to1` — fungible wrap (WETH→ETH, WBTC→BTC)
//! - `stable_synonym` — deprecated/bridged ticker (DAI→USDS, USDT0→USDT)

use std::collections::{BTreeMap, BTreeSet};

use std::sync::LazyLock;

use crate::pipeline_config::{ConfigHint, PipelineYml};
use crate::ticker::split_pair_multi;

/// Default aliases when YAML is absent (CI, dev). Production config merges on top.
const DEFAULT_PRICE_CANONICAL: &[(&str, &str)] = &[
    // BTC 1:1 wrappers
    ("CBBTC", "BTC"),
    ("WBTC", "BTC"),
    ("TBTC", "BTC"),
    ("BTCB", "BTC"),
    ("BBTC", "BTC"),
    // Native / gas-token wraps
    ("WETH", "ETH"),
    ("WBNB", "BNB"),
    ("WSOL", "SOL"),
    // Stable / bridged synonyms (index series only)
    ("DAI", "USDS"),
    ("USDT0", "USDT"),
];

const STABLE_SYNONYMS: &[&str] = &["DAI", "USDT0"];

const DEFAULT_INDEX_MAJORS: &[&str] = &[
    "BTC", "ETH", "SOL", "BNB", "XRP", "ADA", "DOGE", "AVAX", "LINK", "DOT", "LTC", "TRX",
    "SUI", "HYPE", "UNI", "AAVE", "NEAR", "PEPE", "SHIB",
];

static MAP: LazyLock<BTreeMap<String, String>> = LazyLock::new(load_map);

static QUOTES: LazyLock<Vec<String>> = LazyLock::new(load_quotes);

static MAJORS: LazyLock<Vec<String>> = LazyLock::new(load_majors);

fn load_map() -> BTreeMap<String, String> {
    let mut out: BTreeMap<String, String> = DEFAULT_PRICE_CANONICAL
        .iter()
        .map(|(w, c)| (w.to_string(), c.to_string()))
        .collect();
    if let Ok(yml) = PipelineYml::load_default(ConfigHint::Runtime) {
        for (alias, canonical) in yml.cexs.price_canonical {
            if alias.is_empty() || canonical.is_empty() {
                continue;
            }
            out.insert(alias.to_uppercase(), canonical.to_uppercase());
        }
    }
    out
}

fn load_quotes() -> Vec<String> {
    PipelineYml::load_default(ConfigHint::Runtime)
        .ok()
        .map(|y| {
            y.cexs
                .price_canonical_quotes
                .into_iter()
                .map(|q| q.to_uppercase())
                .collect()
        })
        .filter(|v: &Vec<String>| !v.is_empty())
        .unwrap_or_else(|| {
            vec![
                "USDT".into(),
                "USDC".into(),
                "USDS".into(),
                "DAI".into(),
                "USDT0".into(),
                "USD".into(),
                "EUR".into(),
                "GBP".into(),
                "AUD".into(),
            ]
        })
}

fn load_majors() -> Vec<String> {
    PipelineYml::load_default(ConfigHint::Runtime)
        .ok()
        .map(|y| {
            if y.cexs.crypto_majors.is_empty() {
                DEFAULT_INDEX_MAJORS.iter().map(|s| s.to_string()).collect()
            } else {
                y.cexs.crypto_majors.iter().map(|s| s.to_uppercase()).collect()
            }
        })
        .unwrap_or_else(|| DEFAULT_INDEX_MAJORS.iter().map(|s| s.to_string()).collect())
}

/// Map a symbol (base or quote) to its canonical index symbol.
#[inline]
pub fn canonical_price_symbol(sym: &str) -> String {
    let key = sym.trim().to_uppercase();
    MAP.get(&key).cloned().unwrap_or(key)
}

/// Back-compat alias — same as [`canonical_price_symbol`].
#[inline]
pub fn canonical_price_base(base: &str) -> String {
    canonical_price_symbol(base)
}

/// Rewrite both legs of `BASE/QUOTE` or `BASE-QUOTE`.
pub fn canonical_price_pair(pair: &str) -> String {
    let Some((base, quote)) = split_pair_multi(pair, &['/', '-']) else {
        return canonical_price_symbol(pair);
    };
    format!(
        "{}/{}",
        canonical_price_symbol(base),
        canonical_price_symbol(quote)
    )
}

/// `true` when either leg is a configured alias.
#[inline]
pub fn is_price_alias_pair(pair: &str) -> bool {
    let Some((base, quote)) = split_pair_multi(pair, &['/', '-']) else {
        return MAP.contains_key(&pair.trim().to_uppercase());
    };
    let b = base.trim().to_uppercase();
    let q = quote.trim().to_uppercase();
    MAP.contains_key(&b) || MAP.contains_key(&q)
}

/// API metadata: stable synonym vs 1:1 wrapper.
pub fn alias_kind_for_pair(pair: &str) -> &'static str {
    let Some((base, quote)) = split_pair_multi(pair, &['/', '-']) else {
        return alias_kind_for_symbol(pair);
    };
    let b = base.trim().to_uppercase();
    let q = quote.trim().to_uppercase();
    if STABLE_SYNONYMS.contains(&b.as_str()) || STABLE_SYNONYMS.contains(&q.as_str()) {
        "stable_synonym"
    } else {
        "wrapper_1to1"
    }
}

fn alias_kind_for_symbol(sym: &str) -> &'static str {
    if STABLE_SYNONYMS.contains(&sym.trim().to_uppercase().as_str()) {
        "stable_synonym"
    } else {
        "wrapper_1to1"
    }
}

/// Pairs to pre-register in the core `symbol_map`.
pub fn alias_pairs_to_register() -> Vec<String> {
    let mut out = BTreeSet::new();
    for alias in MAP.keys() {
        // alias as base × standard quotes (CBBTC/USDC, WETH/USDT, …)
        for q in QUOTES.iter() {
            out.insert(format!("{alias}/{q}"));
        }
        // alias as quote × liquid majors (ETH/DAI, BTC/USDT0, …)
        for major in MAJORS.iter() {
            out.insert(format!("{major}/{alias}"));
        }
    }
    out.into_iter().collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn canonical_both_legs() {
        assert_eq!(canonical_price_pair("WETH/USDT0"), "ETH/USDT");
        assert_eq!(canonical_price_pair("ETH/DAI"), "ETH/USDS");
    }

    #[test]
    fn stable_synonym_kind() {
        assert_eq!(alias_kind_for_pair("ETH/DAI"), "stable_synonym");
        assert_eq!(alias_kind_for_pair("WETH/USDT"), "wrapper_1to1");
    }

    #[test]
    fn usdc_not_aliased() {
        assert_eq!(canonical_price_symbol("USDC"), "USDC");
    }
}
