//! Asset and ticker resolution engine.
//!
//! Moved from mitch (which should only define types + encoding) to the SDK
//! where business logic belongs. Provides:
//! - `resolve_ticker`: full symbol -> TickerMatch resolution with suffix stripping + quote detection
//! - `resolve_asset` / `resolve_asset_in_class`: fuzzy asset lookup with Jaro-Winkler scoring
//! - `get_asset_by_id` / `get_asset_by_global_id`: exact asset lookup by numeric ID

use crate::price_canonical::canonical_price_base;

use mitch::common::{AssetClass, InstrumentType, MitchError};
use mitch::constants::{
    COMMODITIES_DATA, CRYPTO_ASSETS_DATA, EQUITIES_DATA,
    FOREX_DATA, INDICES_DATA, SOVEREIGN_DEBT_DATA, DataEntry,
};
use mitch::ticker::{Asset, AssetMatch, Ticker, TickerMatch, TickerId, pack_asset};
use std::collections::HashMap;
use std::sync::LazyLock;

// ---- String normalization (internal) ----

/// Normalize a string for asset search: lowercase, strip business suffixes,
/// keep alphanumeric plus the ticker-significant sigils `+` and `-`.
///
/// `+`/`-` are LOAD-BEARING for disambiguation: stripping them collapsed the
/// alias "ETH+" → "eth" == bare "ETH", so `by_normalized` last-write-wins made
/// ETH/USDT resolve to "Ethereum Plus" (5701) instead of Ethereum (5801).
/// Keeping them keeps "eth+" and "eth" distinct keys (RCA ROOT1c, 2026-06-01).
fn normalize_asset_name(input: &str) -> String {
    let mut s = input.trim().to_lowercase();

    // Strip trailing punctuation that is NOT a ticker-significant sigil.
    // `+`/`-` are preserved here so "ETH+" survives as a distinct token.
    while s.ends_with(|c: char| c.is_ascii_punctuation() && c != '+' && c != '-') {
        s.pop();
    }

    // Strip common prefix
    if s.starts_with("the ") {
        s = s[4..].to_string();
    }

    // Strip business suffixes
    for suffix in &[
        " corporation", " company", " inc", " corp", " ltd", " llc",
        " limited", " group", " cie",
    ] {
        if s.ends_with(suffix) {
            s = s[..s.len() - suffix.len()].trim().to_string();
            break;
        }
    }

    s.chars().filter(|c| c.is_alphanumeric() || *c == '+' || *c == '-').collect()
}

// ---- Jaro-Winkler similarity (internal) ----

fn jaro_winkler_similarity(s1: &str, s2: &str) -> f64 {
    if s1 == s2 { return 1.0; }
    if s1.is_empty() || s2.is_empty() { return 0.0; }

    let jaro = jaro_similarity(s1, s2);
    let prefix_len = s1.chars().zip(s2.chars()).take(4).take_while(|(a, b)| a == b).count() as f64;
    jaro + 0.1 * prefix_len * (1.0 - jaro)
}

fn jaro_similarity(s1: &str, s2: &str) -> f64 {
    let c1: Vec<char> = s1.chars().collect();
    let c2: Vec<char> = s2.chars().collect();
    let (l1, l2) = (c1.len(), c2.len());

    if l1 == 0 && l2 == 0 { return 1.0; }
    if l1 == 0 || l2 == 0 { return 0.0; }

    let window = if l1.max(l2) <= 2 { 0 } else { l1.max(l2) / 2 - 1 };
    let mut m1 = vec![false; l1];
    let mut m2 = vec![false; l2];
    let mut matches = 0usize;

    for i in 0..l1 {
        let lo = i.saturating_sub(window);
        let hi = (i + window + 1).min(l2);
        for j in lo..hi {
            if m2[j] || c1[i] != c2[j] { continue; }
            m1[i] = true;
            m2[j] = true;
            matches += 1;
            break;
        }
    }
    if matches == 0 { return 0.0; }

    let mut transpositions = 0usize;
    let mut k = 0;
    for i in 0..l1 {
        if !m1[i] { continue; }
        while !m2[k] { k += 1; }
        if c1[i] != c2[k] { transpositions += 1; }
        k += 1;
    }

    let m = matches as f64;
    (m / l1 as f64 + m / l2 as f64 + (m - transpositions as f64 / 2.0) / m) / 3.0
}

// ---- Suffix stripping ----

fn strip_ticker_suffixes(symbol: &str) -> String {
    let mut s = symbol.to_lowercase();

    // Strip leading sigil
    for p in &["^", ".", "$", "#", "_"] {
        if s.starts_with(p) { s = s[1..].to_string(); break; }
    }

    // Two-pass suffix stripping for compound suffixes
    for _ in 0..2 {
        let mut changed = false;

        // Delimiter-based single-char suffixes
        for d in &["-", "_", ".", "$", "^", "#"] {
            if let Some(pos) = s.rfind(d)
                && matches!(&s[pos + 1..], "us" | "m" | "c" | "z" | "b" | "r" | "d" | "i")
            {
                s = s[..pos].to_string();
                changed = true;
                break;
            }
        }

        // Standalone word suffixes
        if !changed {
            for suf in &["usx", "mini", "micro", "cash", "spot", "ecn", "zero"] {
                if s.ends_with(suf) && s.len() > suf.len() {
                    s = s[..s.len() - suf.len()].to_string();
                    changed = true;
                    break;
                }
            }
        }

        // Trailing delimiters
        if !changed {
            for d in &["-", "_", ".", "$", "^", "#"] {
                if s.ends_with(d) && s.len() > 1 {
                    s = s[..s.len() - 1].to_string();
                    changed = true;
                    break;
                }
            }
        }

        if !changed { break; }
    }
    s
}

// ---- Asset resolver ----

struct AssetResolver {
    by_id: HashMap<(AssetClass, u16), Asset>,
    by_normalized: HashMap<String, Asset>,
    by_class: HashMap<AssetClass, Vec<Asset>>,
    all: Vec<Asset>,
}

impl AssetResolver {
    fn new() -> Self {
        let mut r = Self {
            by_id: HashMap::new(),
            by_normalized: HashMap::new(),
            by_class: HashMap::new(),
            all: Vec::new(),
        };
        r.load_class(AssetClass::CM, COMMODITIES_DATA);
        r.load_class(AssetClass::CR, CRYPTO_ASSETS_DATA);
        r.load_class(AssetClass::EQ, EQUITIES_DATA);
        r.load_class(AssetClass::FX, FOREX_DATA);
        r.load_class(AssetClass::IN, INDICES_DATA);
        r.load_class(AssetClass::SD, SOVEREIGN_DEBT_DATA);
        r
    }

    fn load_class(&mut self, class: AssetClass, data: &[DataEntry]) {
        let mut class_assets = Vec::new();
        for entry in data {
            let class_id = entry.id as u16;
            let asset = Asset {
                id: pack_asset(class, class_id),
                class_id,
                class,
                name: entry.name.to_string(),
                aliases: entry.aliases.to_string(),
            };

            self.by_id.insert((class, class_id), asset.clone());

            // Collision guard (RCA ROOT1c, 2026-06-01): no two assets within the
            // resolver may share a normalized name OR alias key. A duplicate key
            // pointing at a DIFFERENT (class, class_id) means last-write-wins
            // silently mis-resolves one of them (the "Ethereum Plus" 5701 vs
            // "Ethereum" 5801 incident). Panic at load so the boot CI gate /
            // collision SLA catches it instead of shipping a ghost id.
            let mut insert_checked = |norm: String, asset: &Asset| {
                if let Some(existing) = self.by_normalized.get(&norm)
                    && (existing.class, existing.class_id) != (asset.class, asset.class_id)
                {
                    // Hard gate scoped to CR↔CR (the collision SLA, RCA ROOT1c):
                    // two DISTINCT crypto assets sharing a normalized name/alias
                    // is the ghost-id bug (USDN→{Neutrino,Noble}, ETH→{Ethereum,
                    // Ethereum Plus}). Panic so the boot CI gate catches it.
                    //
                    // Cross-class ticker overlaps (e.g. DASH = crypto Dash AND
                    // equity DoorDash; SOLV = crypto Solv AND equity Solventum)
                    // are EXPECTED — a crypto and an equity may legitimately
                    // share a ticker. Those are disambiguated at lookup time by
                    // `class_filter` (crypto-quote pairs force base=CR), not at
                    // load time, so they only warn. Pre-existing intra-other-
                    // class dups (CM "HEATOIL") likewise warn (last-write-wins,
                    // unchanged legacy behaviour).
                    if existing.class == AssetClass::CR && asset.class == AssetClass::CR {
                        panic!(
                            "crypto asset normalized-key collision: '{}' maps to both \
                             {:?}/{} ({}) and {:?}/{} ({}) — two CR assets share a \
                             normalized name/alias; fix crypto-assets.csv (RCA ROOT1c)",
                            norm,
                            existing.class, existing.class_id, existing.name,
                            asset.class, asset.class_id, asset.name,
                        );
                    } else {
                        tracing::warn!(
                            key = %norm,
                            existing = %existing.name,
                            incoming = %asset.name,
                            "cross-class / non-crypto asset normalized-key collision (last-write-wins; disambiguated by class_filter at lookup)"
                        );
                    }
                }
                self.by_normalized.insert(norm, asset.clone());
            };

            let norm = normalize_asset_name(entry.name);
            if !norm.is_empty() {
                insert_checked(norm, &asset);
            }
            for alias in entry.aliases.split('|').filter(|s| !s.is_empty()) {
                let norm_alias = normalize_asset_name(alias);
                if !norm_alias.is_empty() {
                    insert_checked(norm_alias, &asset);
                }
            }

            class_assets.push(asset.clone());
            self.all.push(asset);
        }
        self.by_class.insert(class, class_assets);
    }

    fn find(&self, query: &str, min_confidence: f64, class_filter: Option<AssetClass>) -> Option<AssetMatch> {
        if query.trim().is_empty() { return None; }

        let cleaned = strip_ticker_suffixes(query);
        let norm = normalize_asset_name(&cleaned);

        // Exact normalized match
        if let Some(asset) = self.by_normalized.get(&norm)
            && class_filter.is_none_or(|c| c == asset.class)
        {
            return Some(AssetMatch { asset: asset.clone(), confidence: 1.0, matched_field: "exact".into() });
        }

        let candidates: Vec<&Asset> = match class_filter {
            Some(c) => self.by_class.get(&c)?.iter().collect(),
            None => self.all.iter().collect(),
        };

        // Exact alias match
        for asset in &candidates {
            if asset.aliases.split('|').any(|a| a == norm) {
                return Some(AssetMatch {
                    asset: (*asset).clone(),
                    confidence: 1.0,
                    matched_field: format!("Exact alias match on '{}'", norm),
                });
            }
        }

        // Fuzzy matching
        let mut best: Option<AssetMatch> = None;
        for asset in candidates {
            let name_sim = jaro_winkler_similarity(&norm, &normalize_asset_name(&asset.name));
            let mut best_sim = name_sim;
            let mut field = "name".to_string();

            for alias in asset.aliases.split('|').filter(|s| !s.is_empty()) {
                let sim = jaro_winkler_similarity(&norm, &normalize_asset_name(alias));
                if sim > best_sim {
                    best_sim = sim;
                    field = format!("alias:{}", alias);
                }
            }

            if best_sim >= min_confidence {
                let is_better = best.as_ref().is_none_or(|cur| {
                    best_sim > cur.confidence
                        || (best_sim == cur.confidence && asset.name.len() <= cur.asset.name.len())
                });
                if is_better {
                    best = Some(AssetMatch { asset: asset.clone(), confidence: best_sim, matched_field: field });
                }
            }
        }
        best
    }
}

static RESOLVER: LazyLock<AssetResolver> = LazyLock::new(AssetResolver::new);

// ---- Quote currency detection ----

/// Major quote currency symbols used by `detect_quote_currency` (lowercase
/// for direct comparison against `to_lowercase()` input). Single SDK source
/// of truth — superset of [`DEFAULT_QUOTE_SUFFIXES`] (the crypto-CEX side
/// uses a tighter subset to avoid mis-splitting non-crypto-quoted pairs).
/// Phase 59.R2D — was duplicated in `sdk::resolve::MAJOR_QUOTE_SYMBOLS`
/// (private) + `crypto::exchange::DEFAULT_QUOTE_SUFFIXES` (also private).
pub const MAJOR_QUOTE_SYMBOLS_LC: &[&str] = &[
    "usdt", "usdc", "usd", "eur", "gbp", "jpy", "cad", "aud", "chf", "btc", "eth",
];

/// Uppercase quote suffix list used by CEX symbol parsers
/// (`normalize_suffix("BTCUSDT", DEFAULT_QUOTE_SUFFIXES)` → "BTC/USDT").
/// Order is load-bearing: longer / more-specific suffixes first so
/// `USDT` matches before `USD`. Audit-frozen 2026-05-29 — adding new
/// quote bases requires a CEX adapter audit.
/// Phase 59.R2D moved this from `crypto::exchange::mod::DEFAULT_QUOTE_SUFFIXES`.
pub const DEFAULT_QUOTE_SUFFIXES: &[&str] = &["USDT", "USDC", "BTC", "ETH", "USD"];

/// Resolve a quote-side token to an Asset. Crypto majors (usdt/usdc/btc/eth)
/// and USD resolve cross-class; everything else is tried as an FX fiat code
/// FIRST (RCA ROOT1b, 2026-06-01) so `USDT/THB` → quote=THB(FX), not a
/// whole-pair fuzz that forced quote=USD and collapsed ~24 fiat pairs onto one
/// id. Exact match required (conf 1.0); no fuzzy on the quote side.
fn resolve_quote_token(token: &str) -> Option<Asset> {
    // FX fiat first for non-crypto-major tokens (THB, BRL, TRY, ...).
    let is_crypto_major = MAJOR_QUOTE_SYMBOLS_LC.contains(&token);
    if !is_crypto_major
        && let Some(m) = RESOLVER.find(token, 1.0, Some(AssetClass::FX))
        && m.confidence >= 1.0
    {
        return Some(m.asset);
    }
    // Crypto majors + USD (USD resolves in FX exact too).
    RESOLVER.find(token, 0.95, None).map(|m| m.asset)
}

fn detect_quote_currency(symbol: &str) -> Option<(Asset, String, String)> {
    let lower = symbol.to_lowercase();

    // Explicit-delimiter split takes priority: the RIGHT side is the quote.
    // Robust for slash/dash/underscore pairs (e.g. "USDT/THB" → base "usdt",
    // quote "thb"-FX) where a blind major-suffix scan mis-splits.
    for d in ['/', '_', '-'] {
        if let Some(pos) = lower.rfind(d) {
            let base = lower[..pos].trim_matches(|c| c == '/' || c == '_' || c == '-');
            let quote = lower[pos + 1..].trim_matches(|c| c == '/' || c == '_' || c == '-');
            if !base.is_empty() && !quote.is_empty()
                && let Some(q) = resolve_quote_token(quote)
            {
                return Some((q, base.to_string(), "delim".into()));
            }
        }
    }

    for &q in MAJOR_QUOTE_SYMBOLS_LC {
        // Quote at end
        if lower.ends_with(q) && lower.len() > q.len() {
            let remaining = lower[..lower.len() - q.len()].trim_end_matches(&['/', '_', '.'][..]);
            if !remaining.is_empty()
                && let Some(m) = RESOLVER.find(q, 0.95, None)
            {
                return Some((m.asset, remaining.to_string(), "end".into()));
            }
        }
        // Quote at start.
        //
        // For a fiat-fiat / major-major pair written WITHOUT a delimiter (e.g.
        // "USDJPY", "EURGBP"), the LEFT token is the BASE, not the quote — FX
        // convention. A naive start-match here inverts base/quote
        // (USDJPY → quote=USD, base=JPY, which is backwards). Guard: if the
        // REMAINING (right) token is itself a resolvable quote/fiat symbol,
        // treat both legs as written-order base/quote — the right token is the
        // quote, the start token is the base. This surfaces EURUSD / USDJPY /
        // XAUUSD / US500 correctly (RCA 2026-06-02 FX-surfacing gate).
        if lower.starts_with(q) && lower.len() > q.len() {
            let remaining = lower[q.len()..].trim_start_matches(&['/', '_', '.'][..]);
            if !remaining.is_empty()
                && let Some(start_asset) = RESOLVER.find(q, 0.95, None)
            {
                // Both legs are quote/fiat majors → written order wins:
                // base = start token (q), quote = remaining token.
                if let Some(right_quote) = resolve_quote_token(remaining) {
                    return Some((right_quote, q.to_string(), "start-fxorder".into()));
                }
                // Otherwise the start token is genuinely the quote
                // (e.g. a crypto-major prefix on a non-major base).
                return Some((start_asset.asset, remaining.to_string(), "start".into()));
            }
        }
    }
    None
}

// ---- Public API ----

/// Resolve a ticker symbol across all asset classes.
pub fn resolve_ticker(symbol: &str, instrument_type: InstrumentType) -> Result<TickerMatch, MitchError> {
    let mut steps = Vec::new();
    let original = symbol.to_string();

    // Step 1: strip suffixes
    let cleaned = strip_ticker_suffixes(symbol);
    if cleaned != symbol.to_lowercase() {
        steps.push(format!("Stripped suffixes: {} -> {}", symbol, cleaned));
    }

    // Step 2: detect quote currency
    if let Some((quote, remaining, pos)) = detect_quote_currency(&cleaned) {
        steps.push(format!("Detected quote {} at {}: remaining '{}'", quote.name, pos, remaining));

        if remaining.is_empty() {
            // Single currency - pair with USD
            let usd = RESOLVER.find("usd", 0.95, Some(AssetClass::FX))
                .ok_or_else(|| MitchError::InvalidData("Could not resolve USD".into()))?;
            let tid = TickerId::new(instrument_type, quote.class, quote.class_id, usd.asset.class, usd.asset.class_id, 0)?;
            steps.push("Used detected asset as base with USD quote".into());
            return Ok(TickerMatch {
                ticker: Ticker { id: tid.raw, name: format!("{}/USD", quote.name), instrument_type, base: quote, quote: usd.asset, sub_type: 0 },
                confidence: 0.9,
                processing_steps: steps,
            });
        }

        // When quote is a crypto major (usdt/usdc/btc/eth), force
        // class_filter=CR on the base lookup. Without it, fuzzy match landed
        // on EQ assets (e.g. SUI → "Sun Communities"), creating ghost MITCH
        // ids + ghost shard dirs on disk. Crypto-quote-pair ⇒ base must be CR.
        let cr_quote = matches!(
            quote.name.to_lowercase().as_str(),
            "tether" | "usd coin" | "bitcoin" | "ethereum"
        );
        let class_filter = if cr_quote { Some(AssetClass::CR) } else { None };

        // Base-lookup threshold. RCA ROOT1a (2026-06-01): Jaro-Winkler ≥0.9
        // let 4-char tickers absorb a same-prefix neighbour (SOL→SOLV .942,
        // HYPE→HYPER .96). Short pure-ticker bases get the strictest gate
        // (0.95) so only an exact-alias / exact-name hit (conf 1.0, from the
        // newly-added CSV rows) survives; longer queries keep 0.9.
        let base_threshold = if remaining.len() <= 4 { 0.95 } else { 0.9 };

        // 1:1 wrapped spot bases share the canonical index asset (CBBTC→BTC).
        let remaining_canon = canonical_price_base(&remaining);
        let base_lookup = if remaining_canon != remaining.to_uppercase() {
            steps.push(format!(
                "Price-canonical base: {} -> {}",
                remaining, remaining_canon
            ));
            remaining_canon.to_lowercase()
        } else {
            remaining.clone()
        };

        // Resolve remaining as base. Confident match or skip.
        if let Some(base) = RESOLVER.find(&base_lookup, base_threshold, class_filter) {
            let tid = TickerId::new(instrument_type, base.asset.class, base.asset.class_id, quote.class, quote.class_id, 0)?;
            let name = format!("{}/{}", base.asset.name, quote.name);
            steps.push(format!("Resolved base asset: {} (confidence: {:.2})", base.asset.name, base.confidence));
            return Ok(TickerMatch {
                ticker: Ticker { id: tid.raw, name, instrument_type, base: base.asset, quote, sub_type: 0 },
                confidence: base.confidence,
                processing_steps: steps,
            });
        }

        // RCA ROOT1b (2026-06-01): a quote WAS detected but the base failed to
        // resolve. DO NOT fall through to the whole-pair fuzz below — that path
        // re-fuzzed the entire pair and forced quote=USD, collapsing ALGO/USD ≡
        // ALGO/USDT, USDT/{THB,BRL,...}, H/USDT onto one id. A detected-quote
        // pair with an unresolvable base is an honest failure.
        return Err(MitchError::InvalidData(format!(
            "Unable to resolve base '{}' for quote '{}' (pair {})",
            remaining, quote.name, original
        )));
    }

    // Step 3: try as single asset with USD quote. Only reached when NO quote
    // was detected at all (bare single-asset symbol like "AAPL").
    // Threshold 0.9 (same RCA as base lookup above): suppress weak fuzzy hits.
    let cleaned_canon = canonical_price_base(&cleaned);
    let asset_lookup = if cleaned_canon != cleaned.to_uppercase() {
        steps.push(format!(
            "Price-canonical single asset: {} -> {}",
            cleaned, cleaned_canon
        ));
        cleaned_canon.to_lowercase()
    } else {
        cleaned.clone()
    };
    if let Some(asset) = RESOLVER.find(&asset_lookup, 0.9, None) {
        let usd = RESOLVER.find("usd", 0.95, Some(AssetClass::FX))
            .ok_or_else(|| MitchError::InvalidData("Could not resolve USD".into()))?;
        let tid = TickerId::new(instrument_type, asset.asset.class, asset.asset.class_id, usd.asset.class, usd.asset.class_id, 0)?;
        steps.push(format!("Resolved as single asset with USD quote: {} (confidence: {:.2})", asset.asset.name, asset.confidence));
        return Ok(TickerMatch {
            ticker: Ticker { id: tid.raw, name: format!("{}/USD", asset.asset.name), instrument_type, base: asset.asset, quote: usd.asset, sub_type: 0 },
            confidence: asset.confidence,
            processing_steps: steps,
        });
    }

    Err(MitchError::InvalidData(format!("Unable to resolve ticker: {}", original)))
}

/// Resolve asset within a specific asset class.
pub fn resolve_asset_in_class(name: &str, min_confidence: f64, asset_class: AssetClass) -> Option<AssetMatch> {
    RESOLVER.find(name, min_confidence, Some(asset_class))
}

/// Get asset by exact class and class_id.
pub fn get_asset_by_id(asset_class: AssetClass, class_id: u16) -> Option<Asset> {
    RESOLVER.by_id.get(&(asset_class, class_id)).cloned()
}

#[cfg(test)]
mod fx_surfacing_tests {
    use super::*;
    use mitch::common::InstrumentType;

    /// Delimiter-less fiat-fiat pairs must resolve in WRITTEN ORDER
    /// (left=base, right=quote). Regression for the start-branch inversion
    /// (USDJPY → quote=USD/base=JPY) that returned [] on /v1/last.
    #[test]
    fn fx_pairs_surface_in_written_order() {
        // (symbol, base-name-substr, quote-name-substr) — asserts written
        // order, NOT the inverted start-branch result.
        let cases = [
            ("EURUSD", "euro", "dollar"),
            ("USDJPY", "dollar", "yen"),
            ("EURGBP", "euro", "pound"),
            ("GBPUSD", "pound", "dollar"),
        ];
        for (sym, want_base, want_quote) in cases {
            let m = resolve_ticker(sym, InstrumentType::SPOT)
                .unwrap_or_else(|e| panic!("{sym} failed to resolve: {e:?}"));
            let base = m.ticker.base.name.to_lowercase();
            let quote = m.ticker.quote.name.to_lowercase();
            assert!(
                base.contains(want_base),
                "{sym}: base '{base}' should contain '{want_base}' (quote='{quote}')"
            );
            assert!(
                quote.contains(want_quote),
                "{sym}: quote '{quote}' should contain '{want_quote}' (base='{base}')"
            );
        }
    }
}
