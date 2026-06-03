//! 1:1 wrapped spot bases → canonical index bases for MITCH ticker resolution.
//!
//! Distinct from `cexs.aliases` (weights scraper symbol strings) and
//! per-exchange wire aliases. Used so `CBBTC/USDC` and `BTC/USDC` share the
//! same `ticker_id`, index shards, and renko series.

use std::collections::BTreeMap;
use std::sync::LazyLock;

use crate::pipeline_config::{ConfigHint, PipelineYml};
use crate::ticker::split_pair_multi;

/// Default 1:1 spot wrappers when YAML is absent (CI, dev). Production
/// `config.yml` merges/overrides these entries at startup.
const DEFAULT_PRICE_CANONICAL: &[(&str, &str)] = &[
    ("CBBTC", "BTC"),
    ("WBTC", "BTC"),
    ("TBTC", "BTC"),
    ("BTCB", "BTC"),
    ("BBTC", "BTC"),
];

static MAP: LazyLock<BTreeMap<String, String>> = LazyLock::new(load_map);

static QUOTES: LazyLock<Vec<String>> = LazyLock::new(load_quotes);

fn load_map() -> BTreeMap<String, String> {
    let mut out: BTreeMap<String, String> = DEFAULT_PRICE_CANONICAL
        .iter()
        .map(|(w, c)| (w.to_string(), c.to_string()))
        .collect();
    if let Ok(yml) = PipelineYml::load_default(ConfigHint::Runtime) {
        for (wrapper, canonical) in yml.cexs.price_canonical {
            if wrapper.is_empty() || canonical.is_empty() {
                continue;
            }
            out.insert(wrapper.to_uppercase(), canonical.to_uppercase());
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
                "USD".into(),
                "EUR".into(),
                "GBP".into(),
                "AUD".into(),
            ]
        })
}

/// Map a base symbol to its canonical index base (e.g. `CBBTC` → `BTC`).
/// Unknown symbols pass through uppercased.
#[inline]
pub fn canonical_price_base(base: &str) -> String {
    let key = base.trim().to_uppercase();
    MAP.get(&key).cloned().unwrap_or(key)
}

/// Rewrite the base leg of `BASE/QUOTE` or `BASE-QUOTE` when listed in
/// `cexs.price_canonical`. Quote leg is unchanged.
pub fn canonical_price_pair(pair: &str) -> String {
    let Some((base, quote)) = split_pair_multi(pair, &['/', '-']) else {
        return pair.trim().to_uppercase();
    };
    format!("{}/{}", canonical_price_base(base), quote.to_uppercase())
}

/// `true` when `pair`'s base is a configured wrapper (e.g. `CBBTC/USDC`).
#[inline]
pub fn is_price_alias_pair(pair: &str) -> bool {
    let Some((base, _)) = split_pair_multi(pair, &['/', '-']) else {
        return false;
    };
    MAP.contains_key(&base.trim().to_uppercase())
}

/// Pairs to pre-register in the core `symbol_map` (wrapper × quote list).
pub fn alias_pairs_to_register() -> Vec<String> {
    let mut out = Vec::new();
    for wrapper in MAP.keys() {
        for quote in QUOTES.iter() {
            out.push(format!("{wrapper}/{quote}"));
        }
    }
    out
}

/// Reload map from disk (tests only).
#[cfg(test)]
pub fn reload_for_test() {
    // LazyLock is not resettable; tests use the live config.yml when present.
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn canonical_base_from_config_or_passthrough() {
        let canon = canonical_price_base("CBBTC");
        // When config.yml is present in the nx-rates tree, CBBTC→BTC.
        if MAP.contains_key("CBBTC") {
            assert_eq!(canon, "BTC");
        } else {
            assert_eq!(canon, "CBBTC");
        }
    }

    #[test]
    fn canonical_pair_rewrites_base_only() {
        let out = canonical_price_pair("cbbtc-usdc");
        if MAP.contains_key("CBBTC") {
            assert_eq!(out, "BTC/USDC");
        } else {
            assert_eq!(out, "CBBTC/USDC");
        }
    }
}
