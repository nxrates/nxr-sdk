//! Expand a cross catalog into synth-pipeline triples `(synth, leg_a, leg_b)`.
//!
//! Crosses never persist `.idx`; the kernel composes leg streams in RAM and
//! materialises `.s10` + `.renko` only. Historical bars are rebuilt from the
//! leg `.idx` shards (`synth-backfill-from-idx`), not from a cross idx file.

use crate::pipeline_config::SynthPairYml;
use crate::resolve_ticker;
use mitch::common::InstrumentType;
use std::collections::HashSet;

/// Return synth-pipeline work items for all resolvable crosses.
///
/// Both legs are taken in `storage_quote`, the PUBLISHED denomination, so a
/// cross composes off the same `<asset>/<storage_quote>` primaries `core::storage`
/// materialises. It was a literal `{base}/USDT`, which quietly priced the peg at
/// 1 and pinned every cross to a basis the volume survey is free to move.
///
/// Skips:
/// - `BASE/<storage_quote>` primaries (`BASE` ∈ `primary_bases`)
/// - any `*/<storage_quote>` cross (that IS a primary, not a cross)
/// - crosses whose legs do not resolve
pub fn expand_cross_pairs(
    cross_pairs: &[String],
    primary_bases: &[String],
    storage_quote: &str,
) -> Vec<SynthPairYml> {
    let quote = storage_quote.to_uppercase();
    let primaries: HashSet<String> = primary_bases
        .iter()
        .map(|b| format!("{}/{quote}", b.to_uppercase()))
        .collect();

    let mut out = Vec::with_capacity(cross_pairs.len());
    let mut seen = HashSet::new();

    for raw in cross_pairs {
        let synth_sym = normalize_cross(raw);
        if synth_sym.is_empty() || !seen.insert(synth_sym.clone()) {
            continue;
        }
        if primaries.contains(&synth_sym) {
            continue;
        }
        let Some((base, quote)) = synth_sym.split_once('/') else {
            continue;
        };
        if quote == storage_quote {
            continue;
        }
        let Some((leg_a, leg_b)) = legs_for_cross(base, quote, storage_quote) else {
            continue;
        };
        if !leg_resolves(&leg_a) || !leg_resolves(&leg_b) {
            continue;
        }
        out.push(SynthPairYml {
            synth_sym,
            base_sym: leg_a,
            quote_sym: leg_b,
        });
    }
    out
}

/// Generate the full directed N×(N−1) crypto-cross catalog over `assets`.
///
/// Every ordered pair of distinct assets `A/B` (both directions — `A/B` and
/// `B/A` are distinct inverse crosses) becomes a cross string, bridged at
/// composition time by [`legs_for_cross`] onto the storage quote. This is the
/// "all crosses by default" input: feed the result to [`expand_cross_pairs`],
/// which drops any pair whose legs don't resolve. Assets are the single canonical crypto
/// universe (`cexs.assets`); no per-cross declaration.
pub fn all_crypto_crosses(assets: &[String]) -> Vec<String> {
    let up: Vec<String> = assets
        .iter()
        .map(|a| a.trim().to_uppercase())
        .filter(|a| !a.is_empty())
        .collect();
    let mut out = Vec::with_capacity(up.len().saturating_mul(up.len().saturating_sub(1)));
    for a in &up {
        for b in &up {
            if a != b {
                out.push(format!("{a}/{b}"));
            }
        }
    }
    out
}

fn normalize_cross(sym: &str) -> String {
    sym.trim()
        .to_uppercase()
        .replace('-', "/")
}

fn legs_for_cross(base: &str, quote: &str, storage_quote: &str) -> Option<(String, String)> {
    if base.is_empty() || quote.is_empty() || base == quote {
        return None;
    }
    Some((
        format!("{base}/{storage_quote}"),
        format!("{quote}/{storage_quote}"),
    ))
}

fn leg_resolves(sym: &str) -> bool {
    resolve_ticker(sym, InstrumentType::SPOT).is_ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn all_crypto_crosses_is_directed_nxn_minus_diag() {
        let assets: Vec<String> = ["BTC", "ETH", "SOL"].iter().map(|s| s.to_string()).collect();
        let out = all_crypto_crosses(&assets);
        assert_eq!(out.len(), 3 * 2); // N×(N-1), both directions, no A/A
        let set: HashSet<_> = out.iter().map(String::as_str).collect();
        assert!(set.contains("ETH/BTC") && set.contains("BTC/ETH")); // both directions
        assert!(!set.contains("BTC/BTC"));
        // Feeds expand_cross_pairs cleanly (legs resolve for real majors).
        let expanded = expand_cross_pairs(&out, &["BTC".into(), "ETH".into(), "SOL".into()], "USD");
        assert!(expanded.iter().any(|p| p.synth_sym == "ETH/BTC" && p.base_sym == "ETH/USD"));
    }

    #[test]
    fn skips_storage_quote_primaries_and_keeps_crypto_crosses() {
        let crosses = vec![
            "BTC/USD".into(),
            "ETH/BTC".into(),
            "BNB/ETH".into(),
            "PYUSD/USDC".into(),
            "EUR/USD".into(),
        ];
        let primaries = vec!["BTC".into(), "ETH".into(), "PYUSD".into()];
        let out = expand_cross_pairs(&crosses, &primaries, "USD");
        let syms: HashSet<_> = out.iter().map(|p| p.synth_sym.as_str()).collect();
        assert!(!syms.contains("BTC/USD"));
        assert!(!syms.contains("EUR/USD"));
        assert!(syms.contains("ETH/BTC"));
        assert!(syms.contains("BNB/ETH"));
        assert!(syms.contains("PYUSD/USDC"));
        let eth_btc = out.iter().find(|p| p.synth_sym == "ETH/BTC").unwrap();
        assert_eq!(eth_btc.base_sym, "ETH/USD");
        assert_eq!(eth_btc.quote_sym, "BTC/USD");
    }
}
