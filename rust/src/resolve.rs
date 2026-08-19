//! Asset and ticker resolution engine.
//!
//! Moved from mitch (which should only define types + encoding) to the SDK
//! where business logic belongs. Provides:
//! - `resolve_ticker`: full symbol -> TickerMatch resolution with suffix stripping + quote detection
//! - `resolve_asset` / `resolve_asset_in_class`: fuzzy asset lookup with Jaro-Winkler scoring
//! - `get_asset_by_id` / `asset_by_id`: exact asset lookup by numeric ID
//! - `ticker_admissible`: the one resolvability + blacklist gate, shared by the
//!   UDP ingest path and the signing path

use mitch::common::{AssetClass, InstrumentType, MitchError};
use mitch::constants::{
    COMMODITIES_DATA, CRYPTO_ASSETS_DATA, DataEntry, EQUITIES_DATA, FOREX_DATA, INDICES_DATA,
    SOVEREIGN_DEBT_DATA,
};
use mitch::ticker::{Asset, AssetMatch, Ticker, TickerId, TickerMatch, pack_asset};
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
        " corporation",
        " company",
        " inc",
        " corp",
        " ltd",
        " llc",
        " limited",
        " group",
        " cie",
    ] {
        if s.ends_with(suffix) {
            s = s[..s.len() - suffix.len()].trim().to_string();
            break;
        }
    }

    s.chars()
        .filter(|c| c.is_alphanumeric() || *c == '+' || *c == '-')
        .collect()
}

// ---- Jaro-Winkler similarity (internal) ----

fn jaro_winkler_similarity(s1: &str, s2: &str) -> f64 {
    if s1 == s2 {
        return 1.0;
    }
    if s1.is_empty() || s2.is_empty() {
        return 0.0;
    }

    let jaro = jaro_similarity(s1, s2);
    let prefix_len = s1
        .chars()
        .zip(s2.chars())
        .take(4)
        .take_while(|(a, b)| a == b)
        .count() as f64;
    jaro + 0.1 * prefix_len * (1.0 - jaro)
}

fn jaro_similarity(s1: &str, s2: &str) -> f64 {
    let c1: Vec<char> = s1.chars().collect();
    let c2: Vec<char> = s2.chars().collect();
    let (l1, l2) = (c1.len(), c2.len());

    if l1 == 0 && l2 == 0 {
        return 1.0;
    }
    if l1 == 0 || l2 == 0 {
        return 0.0;
    }

    let window = if l1.max(l2) <= 2 {
        0
    } else {
        l1.max(l2) / 2 - 1
    };
    let mut m1 = vec![false; l1];
    let mut m2 = vec![false; l2];
    let mut matches = 0usize;

    for i in 0..l1 {
        let lo = i.saturating_sub(window);
        let hi = (i + window + 1).min(l2);
        for j in lo..hi {
            if m2[j] || c1[i] != c2[j] {
                continue;
            }
            m1[i] = true;
            m2[j] = true;
            matches += 1;
            break;
        }
    }
    if matches == 0 {
        return 0.0;
    }

    let mut transpositions = 0usize;
    let mut k = 0;
    for i in 0..l1 {
        if !m1[i] {
            continue;
        }
        while !m2[k] {
            k += 1;
        }
        if c1[i] != c2[k] {
            transpositions += 1;
        }
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
        if s.starts_with(p) {
            s = s[1..].to_string();
            break;
        }
    }

    // Two-pass suffix stripping for compound suffixes
    for _ in 0..2 {
        let mut changed = false;

        // Delimiter-based single-char suffixes
        for d in &["-", "_", ".", "$", "^", "#"] {
            if let Some(pos) = s.rfind(d)
                && matches!(
                    &s[pos + 1..],
                    "us" | "m" | "c" | "z" | "b" | "r" | "d" | "i"
                )
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

        if !changed {
            break;
        }
    }
    s
}

// ---- Asset resolver ----

/// Strength of an EXACT hit of `key` on `asset`: 0 = canonical name or PRIMARY
/// (first) alias, 1 = secondary alias, `None` = no exact hit.
///
/// Load order used to decide which row owned a shared key (last write wins), so
/// an index's 11th alias outranked an equity's own ticker: `ES` → S&P 500 not
/// Eversource, `TW` → Taiwan Weighted not Tradeweb, `BSX` → Sensex not Boston
/// Scientific. Rank first, then lowest (class, class_id), makes the winner
/// independent of CSV order.
fn exact_rank(asset: &Asset, key: &str) -> Option<u8> {
    if normalize_asset_name(&asset.name) == key {
        return Some(0);
    }
    asset
        .aliases
        .split('|')
        .filter(|s| !s.is_empty())
        .position(|a| normalize_asset_name(a) == key)
        .map(|i| u8::from(i > 0))
}

/// Deterministic exact-match precedence key: primary before secondary, then
/// lowest class then lowest class_id.
fn exact_key(asset: &Asset, rank: u8) -> (u8, u8, u16) {
    (rank, asset.class as u8, asset.class_id)
}

struct AssetResolver {
    by_id: HashMap<(AssetClass, u16), Asset>,
    by_normalized: HashMap<String, (Asset, u8)>,
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
        // IP (10) = Indices & Index Products. IN (9) is Infrastructure: the
        // two-letter alias reads like "INdices" and was wrong until 2026-08-14.
        r.load_class(AssetClass::IP, INDICES_DATA);
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
                let rank = exact_rank(asset, &norm).unwrap_or(1);
                if let Some((existing, _)) = self.by_normalized.get(&norm)
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
                            existing.class,
                            existing.class_id,
                            existing.name,
                            asset.class,
                            asset.class_id,
                            asset.name,
                        );
                    } else {
                        // EXPECTED, and resolved: exact_rank picks the winner and
                        // class_filter disambiguates at lookup (see the block comment
                        // above). It fired 316 times on every single boot, which is
                        // noise describing the table working, not a collision to fix.
                        tracing::debug!(
                            key = %norm,
                            existing = %existing.name,
                            incoming = %asset.name,
                            "cross-class / non-crypto asset normalized-key collision (resolved by exact_rank; disambiguated by class_filter at lookup)"
                        );
                    }
                }
                if self
                    .by_normalized
                    .get(&norm)
                    .is_none_or(|(ex, er)| exact_key(asset, rank) < exact_key(ex, *er))
                {
                    self.by_normalized.insert(norm, (asset.clone(), rank));
                }
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

    fn find(
        &self,
        query: &str,
        min_confidence: f64,
        class_filter: Option<AssetClass>,
    ) -> Option<AssetMatch> {
        if query.trim().is_empty() {
            return None;
        }

        let norm = normalize_asset_name(&strip_ticker_suffixes(query));

        let candidates: Vec<&Asset> = match class_filter {
            Some(c) => self.by_class.get(&c)?.iter().collect(),
            None => self.all.iter().collect(),
        };

        // Exact name/alias match ALWAYS beats any fuzzy hit, in any class.
        //
        // The RAW normalized query is tried BEFORE the suffix-stripped one:
        // `strip_ticker_suffixes` eats a trailing `-b`/`-c`/`-d`/... which is a
        // share-class marker, not a broker suffix, so "BRK-B" collapsed to
        // "brk" and lost the exact alias to a 0.91 fuzz on commodity "BR"
        // (Brent) — signing crude oil under Berkshire's id (2026-08-14).
        // Aliases are compared NORMALIZED: the CSV column is uppercase, so the
        // raw `a == norm` compare here never fired for any asset.
        for key in [normalize_asset_name(query), norm.clone()] {
            if key.is_empty() {
                continue;
            }
            if let Some((asset, _)) = self.by_normalized.get(&key)
                && class_filter.is_none_or(|c| c == asset.class)
            {
                return Some(AssetMatch {
                    asset: asset.clone(),
                    confidence: 1.0,
                    matched_field: "exact".into(),
                });
            }
            // Class-filtered fallback: the global winner sits in another class.
            if let Some(asset) = candidates
                .iter()
                .filter_map(|a| exact_rank(a, &key).map(|r| (exact_key(a, r), *a)))
                .min_by_key(|(k, _)| *k)
                .map(|(_, a)| a)
            {
                return Some(AssetMatch {
                    asset: asset.clone(),
                    confidence: 1.0,
                    matched_field: format!("Exact alias match on '{}'", key),
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
                // Ties break on shortest name, then lowest (class, class_id),
                // so the winner never depends on CSV load order.
                let is_better = best.as_ref().is_none_or(|cur| {
                    (
                        best_sim,
                        cur.asset.name.len(),
                        cur.asset.class as u8,
                        cur.asset.class_id,
                    ) > (
                        cur.confidence,
                        asset.name.len(),
                        asset.class as u8,
                        asset.class_id,
                    )
                });
                if is_better {
                    best = Some(AssetMatch {
                        asset: asset.clone(),
                        confidence: best_sim,
                        matched_field: field,
                    });
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
    // FX fiat first for non-major tokens (THB, BRL, TRY, ...).
    let is_major = MAJOR_QUOTE_SYMBOLS_LC.contains(&token);
    if !is_major
        && let Some(m) = RESOLVER.find(token, 1.0, Some(AssetClass::FX))
        && m.confidence >= 1.0
    {
        return Some(m.asset);
    }
    // Major quotes resolve class-pinned: crypto majors → CR, fiat majors → FX.
    // An unfiltered lookup lets an exact alias collision in another class win:
    // indices.csv "Ethereum Index" carries alias ETH, so quote "eth" resolved
    // to Indices:2101 — misclassing every ETH-quoted pair (BNB/ETH, SOL/ETH)
    // and breaking their target_bpd class bucket (RCA 2026-06-09). Mirrors the
    // base-side cr_quote class filter.
    if is_major {
        let class = match token {
            "usdt" | "usdc" | "btc" | "eth" => AssetClass::CR,
            _ => AssetClass::FX,
        };
        if let Some(m) = RESOLVER.find(token, 0.95, Some(class)) {
            return Some(m.asset);
        }
    }
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
            if !base.is_empty()
                && !quote.is_empty()
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
                && let Some(asset) = resolve_quote_token(q)
            {
                return Some((asset, remaining.to_string(), "end".into()));
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
                && let Some(start_asset) = resolve_quote_token(q)
            {
                // Both legs are quote/fiat majors → written order wins:
                // base = start token (q), quote = remaining token.
                if let Some(right_quote) = resolve_quote_token(remaining) {
                    return Some((right_quote, q.to_string(), "start-fxorder".into()));
                }
                // Otherwise the start token is genuinely the quote
                // (e.g. a crypto-major prefix on a non-major base).
                return Some((start_asset, remaining.to_string(), "start".into()));
            }
        }
    }
    None
}

// ---- Public API ----

/// Resolve a ticker symbol across all asset classes.
pub fn resolve_ticker(
    symbol: &str,
    instrument_type: InstrumentType,
) -> Result<TickerMatch, MitchError> {
    let mut steps = Vec::new();
    let original = symbol.to_string();

    // Step 1: strip suffixes
    let cleaned = strip_ticker_suffixes(symbol);
    if cleaned != symbol.to_lowercase() {
        steps.push(format!("Stripped suffixes: {} -> {}", symbol, cleaned));
    }

    // Step 2: detect quote currency
    if let Some((quote, remaining, pos)) = detect_quote_currency(&cleaned) {
        steps.push(format!(
            "Detected quote {} at {}: remaining '{}'",
            quote.name, pos, remaining
        ));

        if remaining.is_empty() {
            // Single currency - pair with USD
            let usd = RESOLVER
                .find("usd", 0.95, Some(AssetClass::FX))
                .ok_or_else(|| MitchError::InvalidData("Could not resolve USD".into()))?;
            let tid = TickerId::new(
                instrument_type,
                quote.class,
                quote.class_id,
                usd.asset.class,
                usd.asset.class_id,
                0,
            )?;
            steps.push("Used detected asset as base with USD quote".into());
            return Ok(TickerMatch {
                ticker: Ticker {
                    id: tid.raw,
                    name: format!("{}/USD", quote.name),
                    instrument_type,
                    base: quote,
                    quote: usd.asset,
                    sub_type: 0,
                },
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

        // CAT-1 fungible aliases (MATIC→POL, WETH→ETH, XBT→BTC, …) are folded
        // into the canonical asset's CSV alias column, so the resolver collapses
        // them to a single ticker_id here automatically (no code path needed).
        // CAT-2 custodial BTC wraps (WBTC, cbBTC, …) intentionally do NOT
        // collapse — they keep distinct ids and share BTC's series only at the
        // shard chokepoint (see `series_alias::series_canonical_ticker_id`).

        // Resolve remaining as base. Confident match or skip.
        //
        // CR-then-FX (2026-07-25): the CR-only filter above is load-bearing
        // against EQ mis-hits (`SUI` → "Sun Communities", `PEPE` → "pepsico" at
        // 0.94) and must NOT be widened to `None`. But CR-ONLY made every
        // FX-base × crypto-quote pair unresolvable — `EUR/USD` resolved while
        // `EUR/USDT` did not — so 25 configured `cross_pairs` (AED, AUD, BRL,
        // CAD, EUR, GBP, KZT, PLN, SGD, TRY, UAH against USDT/USDC/BTC/ETH) fell
        // through to `resolve_ticker_id`'s FNV fallback and were sharded under
        // PHANTOM ids whose decoded class/instrument-type is garbage
        // (`EUR/USDT` → base_class=CB, quote_class=PM, itype=DIGI — published
        // verbatim by `/v1/tickers/detail`). Fiat IS a legitimate base against a
        // crypto quote, so fall back to FX — never to EQ, which is the mis-hit
        // class the filter exists to exclude.
        //
        // CM added 2026-08-13: the FX-only fallback left every commodity base ×
        // crypto quote phantom in exactly the same way (52 live rows: `WTI/USDT`
        // → base_class=SP quote_class=CL itype=FUT, `CATTLE/USDT` → itype=WAR).
        // Metals, energy and softs are as legitimate a base against a stablecoin
        // as fiat is, so the fallback walks FX then CM.
        //
        // IP added 2026-08-14 so an index CFD can be crossed into crypto
        // (`GER40/BTC`), EQ last so an equity can too (`AAPL/BTC`).
        // EQ IS ONLY SAFE WHILE EVERY TRADED TOKEN HAS ITS OWN crypto-assets.csv
        // ROW: with no CR row, the same-ticker equity is the only EXACT alias
        // holder and wins outright regardless of order (CFG → Citizens
        // Financial, MET → MetLife, FF → F&F, INF → Informa, until those rows
        // were added). Registering a new traded token is therefore mandatory,
        // not cosmetic. Guarded by `crypto_quoted_bases_never_lose_to_an_equity`.
        let base_match = RESOLVER
            .find(&remaining, base_threshold, class_filter)
            .or_else(|| {
                class_filter.filter(|_| cr_quote).and_then(|_| {
                    [
                        AssetClass::FX,
                        AssetClass::CM,
                        AssetClass::IP,
                        AssetClass::EQ,
                    ]
                        .into_iter()
                        .find_map(|c| RESOLVER.find(&remaining, base_threshold, Some(c)))
                })
            });
        if let Some(base) = base_match {
            let tid = TickerId::new(
                instrument_type,
                base.asset.class,
                base.asset.class_id,
                quote.class,
                quote.class_id,
                0,
            )?;
            let name = format!("{}/{}", base.asset.name, quote.name);
            steps.push(format!(
                "Resolved base asset: {} (confidence: {:.2})",
                base.asset.name, base.confidence
            ));
            return Ok(TickerMatch {
                ticker: Ticker {
                    id: tid.raw,
                    name,
                    instrument_type,
                    base: base.asset,
                    quote,
                    sub_type: 0,
                },
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
    // CAT-1 aliases resolve via CSV; CAT-2 stays distinct (see Step 2 note).
    if let Some(asset) = RESOLVER.find(&cleaned, 0.9, None) {
        let usd = RESOLVER
            .find("usd", 0.95, Some(AssetClass::FX))
            .ok_or_else(|| MitchError::InvalidData("Could not resolve USD".into()))?;
        let tid = TickerId::new(
            instrument_type,
            asset.asset.class,
            asset.asset.class_id,
            usd.asset.class,
            usd.asset.class_id,
            0,
        )?;
        steps.push(format!(
            "Resolved as single asset with USD quote: {} (confidence: {:.2})",
            asset.asset.name, asset.confidence
        ));
        return Ok(TickerMatch {
            ticker: Ticker {
                id: tid.raw,
                name: format!("{}/USD", asset.asset.name),
                instrument_type,
                base: asset.asset,
                quote: usd.asset,
                sub_type: 0,
            },
            confidence: asset.confidence,
            processing_steps: steps,
        });
    }

    Err(MitchError::InvalidData(format!(
        "Unable to resolve ticker: {}",
        original
    )))
}

/// Resolve asset within a specific asset class.
pub fn resolve_asset_in_class(
    name: &str,
    min_confidence: f64,
    asset_class: AssetClass,
) -> Option<AssetMatch> {
    RESOLVER.find(name, min_confidence, Some(asset_class))
}

/// Get asset by exact class and class_id.
pub fn get_asset_by_id(asset_class: AssetClass, class_id: u16) -> Option<Asset> {
    asset_by_id(asset_class, class_id).cloned()
}

/// Borrowed form of [`get_asset_by_id`]. The tables are `'static`, so the hot
/// paths (two lookups per UDP frame) never pay the two `String` clones.
pub fn asset_by_id(asset_class: AssetClass, class_id: u16) -> Option<&'static Asset> {
    RESOLVER.by_id.get(&(asset_class, class_id))
}

/// The `(base, quote)` assets a MITCH ticker id names. `None` on a side means
/// that side names no registered asset. The ONE decode, so identity, price
/// class and admissibility can never disagree about what a ticker is.
pub fn ticker_assets(ticker_id: u64) -> (Option<&'static Asset>, Option<&'static Asset>) {
    let t = TickerId::from_raw(ticker_id);
    (
        asset_by_id(t.base_asset_class(), t.base_asset_id()),
        asset_by_id(t.quote_asset_class(), t.quote_asset_id()),
    )
}

/// Case-insensitive hit on ANY identifier form of `asset`: every alias
/// (including the canonical symbol), the long human `name`, or the decimal
/// 32-bit MITCH global asset id. An operator writing the list in whichever form
/// they have to hand must reach the same asset.
///
/// ⚠ ALIASES COLLIDE ACROSS CLASSES, by design and in numbers (`load_class`
/// logs ~316 such collisions on every boot: `DASH` is both crypto Dash and the
/// equity DoorDash, `ES` both the S&P 500 index and Eversource, `SOLV` both
/// Solv and Solventum). A short alias therefore blacklists MORE than one asset,
/// and since this predicate also gates UDP ingest, that gaps the collided
/// asset's history rather than merely refusing to sign it. Prefer the MITCH
/// asset id form for anything under 5 characters.
pub fn asset_blacklisted(asset: &Asset, blacklisted: &std::collections::HashSet<String>) -> bool {
    if blacklisted.is_empty() {
        return false;
    }
    let hit = |t: &str| blacklisted.iter().any(|b| b.eq_ignore_ascii_case(t));
    // `Asset::id` IS `pack_asset(class, class_id)` (see `AssetResolver::new`), so
    // the id form costs one `itoa`-free format into a stack buffer, not a heap
    // `String` per leg per frame.
    let mut gid = [0u8; 10];
    let gid = fmt_u32(asset.id, &mut gid);
    hit(&asset.name) || hit(gid) || asset.aliases.split('|').any(hit)
}

/// Decimal `n` written into `buf`, returned as a borrowed `str`. u32 max is 10
/// digits, so a `[u8; 10]` never overflows.
fn fmt_u32(mut n: u32, buf: &mut [u8; 10]) -> &str {
    let mut i = buf.len();
    loop {
        i -= 1;
        buf[i] = b'0' + (n % 10) as u8;
        n /= 10;
        if n == 0 {
            break;
        }
    }
    // Digits only: always valid UTF-8.
    std::str::from_utf8(&buf[i..]).unwrap_or("")
}

/// Why [`ticker_admissible`] refused a ticker id.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TickerRefusal {
    /// A shape the resolver cannot mint: non-SPOT, or a non-zero sub-type.
    Shape,
    /// Base names no registered asset, or a blacklisted one.
    Base,
    /// Quote names no registered asset, or a blacklisted one.
    Quote,
}

impl std::fmt::Display for TickerRefusal {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::Shape => "not a SPOT ticker with sub-type 0",
            Self::Base => "base asset is unregistered or blacklisted",
            Self::Quote => "quote asset is unregistered or blacklisted",
        })
    }
}

/// THE admissibility predicate: RESOLVABILITY, not membership.
///
/// A ticker id is admissible when it is a shape [`crate::try_resolve_ticker_id`]
/// could have minted (SPOT, sub-type 0) and both of its assets are in the
/// canonical tables and un-blacklisted. No configured list takes part. There is
/// no ticker declaration and no whitelist anywhere, and the asset blacklist is
/// the ONLY exclusion, which is why it is enforced here rather than per call
/// site: a blacklisted asset must be unreachable through composition too.
///
/// Shared by the UDP ingest gate and the signing path so a node's ingested,
/// served and signable universes cannot drift. A light node therefore ingests,
/// prices and signs anything a full node does.
///
/// This is also the ingest bound that replaced the registry whitelist, the root
/// cause fix for the 2026-07-10 runtime OOM: `Index::validate` only rejects
/// ticker==0, the price-sanity gate fails OPEN for a never-before-seen id, and
/// no per-ticker map evicts, so an unbounded id space grows `state`/`dirty`/
/// `msg_counter` for the life of the process. The admissible set here is finite
/// and fixed at build time: 2,278 assets across 6 class tables, so at most
/// 2,278^2 (about 5.2M) ids instead of 2^64. Pinning SPOT and sub-type 0 is
/// load-bearing for that bound, not cosmetic: the 20 free sub-type bits alone
/// would multiply it by 2^20, and the instrument-type nibble by another 16.
/// Random wire corruption clears all four conditions with probability about
/// 3e-13 per frame (asset ids are sparse: 2,278 live points in a 2^20
/// class-and-id space), i.e. never within a process lifetime.
pub fn ticker_admissible(
    ticker_id: u64,
    blacklisted: &std::collections::HashSet<String>,
) -> Result<(), TickerRefusal> {
    let t = TickerId::from_raw(ticker_id);
    // RAW nibbles, not the decoded enums: `InstrumentType::from_id` and
    // `AssetClass::from_id` fall back to variant 0 for the reserved codes, so
    // `t.is_spot()` is true for instrument types 14 and 15 and class 14/15 read
    // as EQ. Comparing each nibble against what it decoded to rejects exactly
    // the undefined codes, which is what keeps the admissible set at one raw id
    // per (base, quote) pair instead of nine.
    if (ticker_id >> 60) & 0xF != InstrumentType::SPOT as u64
        || t.sub_type() != 0
        || (ticker_id >> 56) & 0xF != t.base_asset_class() as u64
        || (ticker_id >> 36) & 0xF != t.quote_asset_class() as u64
    {
        return Err(TickerRefusal::Shape);
    }
    let (base, quote) = ticker_assets(ticker_id);
    match base {
        Some(a) if !asset_blacklisted(a, blacklisted) => {}
        _ => return Err(TickerRefusal::Base),
    }
    match quote {
        Some(a) if !asset_blacklisted(a, blacklisted) => {}
        _ => return Err(TickerRefusal::Quote),
    }
    Ok(())
}

#[cfg(test)]
mod admissibility_tests {
    use super::*;
    use std::collections::HashSet;

    const SOL_USDC: u64 = 448_509_916_384_067_584;
    const BTC_USDC: u64 = 435_315_776_850_755_584;

    /// Resolvability, not membership: any id whose two legs name registered
    /// assets is admissible, whether or not anything declared it.
    #[test]
    fn a_registered_pair_is_admissible_without_being_declared() {
        let none = HashSet::new();
        assert_eq!(ticker_admissible(SOL_USDC, &none), Ok(()));
        assert_eq!(ticker_admissible(BTC_USDC, &none), Ok(()));
    }

    /// The bound: an id must decode to assets the canonical tables actually
    /// hold. `class_id` is u16 and the tables are sparse, so almost none do.
    #[test]
    fn an_unregistered_asset_is_refused_on_either_leg() {
        let none = HashSet::new();
        let t = TickerId::from_raw(SOL_USDC);
        let bad_base = TickerId::new(
            InstrumentType::SPOT,
            t.base_asset_class(),
            0xFFFF,
            t.quote_asset_class(),
            t.quote_asset_id(),
            0,
        )
        .unwrap()
        .raw;
        let bad_quote = TickerId::new(
            InstrumentType::SPOT,
            t.base_asset_class(),
            t.base_asset_id(),
            t.quote_asset_class(),
            0xFFFF,
            0,
        )
        .unwrap()
        .raw;
        assert_eq!(ticker_admissible(bad_base, &none), Err(TickerRefusal::Base));
        assert_eq!(ticker_admissible(bad_quote, &none), Err(TickerRefusal::Quote));
        // The FNV phantom an unresolvable symbol used to mint decodes to noise.
        assert!(ticker_admissible(crate::phantom_ticker_id("EUR/USDT"), &none).is_err());
    }

    /// SPOT and sub-type 0 are load-bearing for the bound, not cosmetic: the 20
    /// free sub-type bits alone would multiply the admissible id space by 2^20
    /// and the instrument-type nibble by another 16. `try_resolve_ticker_id`
    /// can mint neither, so neither is reachable from a symbol.
    #[test]
    fn only_the_shape_the_resolver_mints_is_admissible() {
        let none = HashSet::new();
        assert_eq!(
            ticker_admissible(SOL_USDC | 1, &none),
            Err(TickerRefusal::Shape)
        );
        assert_eq!(
            ticker_admissible(SOL_USDC | 0xFFFFF, &none),
            Err(TickerRefusal::Shape)
        );
        for itype in 1u64..16 {
            assert_eq!(
                ticker_admissible(SOL_USDC | (itype << 60), &none),
                Err(TickerRefusal::Shape),
                "instrument type {itype}"
            );
        }
        // The RESERVED codes are the trap: `from_id` maps 14 and 15 back to
        // variant 0, so a decoded-enum check would read them as SPOT and as EQ
        // and admit eight extra raw ids for every real pair.
        for reserved in [14u64, 15] {
            assert_eq!(
                ticker_admissible(SOL_USDC | (reserved << 56), &none),
                Err(TickerRefusal::Shape),
                "reserved base class {reserved}"
            );
            assert_eq!(
                ticker_admissible(SOL_USDC | (reserved << 36), &none),
                Err(TickerRefusal::Shape),
                "reserved quote class {reserved}"
            );
        }
    }

    /// The blacklist is the SOLE exclusion, binds on the ASSET (either leg), and
    /// accepts every identifier form an operator might have to hand.
    #[test]
    fn the_blacklist_binds_either_leg_in_any_identifier_form() {
        for banned in ["SOL", "sol", "WSOL", "Solana", "407917"] {
            let set: HashSet<String> = [banned.to_string()].into_iter().collect();
            assert_eq!(
                ticker_admissible(SOL_USDC, &set),
                Err(TickerRefusal::Base),
                "{banned} as base"
            );
            assert_eq!(ticker_admissible(BTC_USDC, &set), Ok(()), "{banned} unrelated");
        }
        let quote_banned: HashSet<String> = ["USDC".to_string()].into_iter().collect();
        assert_eq!(
            ticker_admissible(SOL_USDC, &quote_banned),
            Err(TickerRefusal::Quote)
        );
    }

    /// The MITCH asset id form the test above spells literally must be the one
    /// `pack_asset` produces, or that case passes vacuously. `Asset::id` must
    /// BE that value: `asset_blacklisted` reads the field instead of repacking.
    #[test]
    fn the_mitch_asset_id_form_is_the_packed_global_id() {
        let (base, _) = ticker_assets(SOL_USDC);
        let base = base.expect("SOL registered");
        assert_eq!(base.id, pack_asset(base.class, base.class_id));
        assert_eq!(base.id.to_string(), "407917");
        let mut buf = [0u8; 10];
        assert_eq!(fmt_u32(base.id, &mut buf), "407917");
        assert_eq!(fmt_u32(0, &mut [0u8; 10]), "0");
        assert_eq!(fmt_u32(u32::MAX, &mut [0u8; 10]), "4294967295");
    }
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

    /// Crypto-major quote tokens must resolve class-pinned to CR. Regression
    /// for indices.csv "Ethereum Index" (alias ETH) winning the unfiltered
    /// quote lookup — every ETH-quoted pair (BNB/ETH, SOL/ETH) got quote
    /// class Indices:2101 instead of CR:5801, misrouting its target_bpd
    /// class bucket (RCA 2026-06-09).
    #[test]
    fn crypto_major_quotes_resolve_to_cr() {
        use mitch::common::AssetClass;
        for sym in ["BNB/ETH", "SOL/ETH", "ETH/BTC", "BNBETH", "ETHBTC"] {
            let m = resolve_ticker(sym, InstrumentType::SPOT)
                .unwrap_or_else(|e| panic!("{sym} failed to resolve: {e:?}"));
            let tid = mitch::ticker::TickerId::from_raw(m.ticker.id);
            assert_eq!(
                tid.quote_asset_class(),
                AssetClass::CR,
                "{sym}: quote class must be CR, got {:?} (quote='{}')",
                tid.quote_asset_class(),
                m.ticker.quote.name
            );
            assert_eq!(
                tid.base_asset_class(),
                AssetClass::CR,
                "{sym}: base class must be CR"
            );
        }
    }
}
