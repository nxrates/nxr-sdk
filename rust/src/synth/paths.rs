//! Synthetic symbol path registry — single source of truth.
//!
//! ## Symbol convention (NXR vs BTR)
//!
//! BTR canonical quote = `USDC` (collector source-of-truth, see
//! `~/Work/btr/sdk/src/types/paths.ts`). **NXR canonical quote = USDT** for crypto
//! (exchange-native) and `USD` for FX. Symbols use a slash separator: `BTC/USDT`,
//! `ETH/USDT`, `EUR/USD`, `EURC/USDT`, etc., to match the `/v1/synth/paths` REST
//! contract and `core/src/triangulator.rs` rule tables.
//!
//! ## Path semantics
//!
//! A `SynthPath` is `synth = Π leg_i^{exp_i}` with `exp_i ∈ {+1, -1}`:
//! - `exp = +1` → multiply by leg's price
//! - `exp = -1` → divide by leg's price (also swaps bid↔ask on tick composition)
//!
//! ## Categories (ported from BTR `SYNTH_PATHS`)
//!
//! - **Trivial identities** (`X/X`): consumers should short-circuit to `(1,1,1, conf=10000)`.
//! - **Pure inversions** (`USDT/BTC = 1 / (BTC/USDT)`): one signed leg with `exp=-1`.
//! - **2-leg crosses via USDT pivot** (`ETH/BTC = ETH/USDT × USDT/BTC`):
//!   `[(ETH/USDT, +1), (BTC/USDT, -1)]`.
//! - **Cross-quote (FX bridge)** (`BTC/EUR = BTC/USDT × USDT/EUR`):
//!   `[(BTC/USDT, +1), (EUR/USDT, -1)]`.
//! - **Gold crosses**.
//!
//! DAG ordering: synth-of-synth paths (if added later) must reference legs
//! that appear earlier in [`SYNTH_PATHS`].

use std::collections::HashMap;
use std::sync::LazyLock;

/// Signed leg: `(symbol, exponent)`. Exponent must be `+1` or `-1`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Leg {
    /// Leg symbol (slash format, e.g. `BTC/USDT`).
    pub sym: String,
    /// Signed exponent in the multiplicative composition (`+1` or `-1`).
    pub exp: i8,
}

impl Leg {
    /// Construct a leg from `(sym, exp)`.
    #[inline]
    pub fn new(sym: impl Into<String>, exp: i8) -> Self {
        debug_assert!(exp == 1 || exp == -1, "Leg exponent must be ±1, got {exp}");
        Self {
            sym: sym.into(),
            exp,
        }
    }
}

/// A synthetic symbol = ordered product of signed legs.
#[derive(Debug, Clone)]
pub struct SynthPath {
    /// Synth symbol name (e.g. `ETH/BTC`).
    pub sym: String,
    /// Ordered legs. Empty legs vector = trivial identity (1, 1, 1).
    pub legs: Vec<Leg>,
}

impl SynthPath {
    fn new(sym: &str, legs: &[(&str, i8)]) -> Self {
        Self {
            sym: sym.to_string(),
            legs: legs.iter().map(|(s, e)| Leg::new(*s, *e)).collect(),
        }
    }
}

/// Canonical synth path registry.
///
/// Adding a path = appending one entry. Engine + API auto-pick at next boot.
/// Order matters for synth-of-synth (DAG) — keep DAG invariant: a leg sym that
/// itself is a synth must appear *earlier* in this vec.
pub static SYNTH_PATHS: LazyLock<Vec<SynthPath>> = LazyLock::new(|| {
    vec![
        // ── Trivial identities (0-leg). Tick = (1,1,1, conf=10000). ──
        SynthPath::new("USDT/USDT", &[]),
        SynthPath::new("USDC/USDC", &[]),
        SynthPath::new("USD/USD", &[]),
        SynthPath::new("EUR/EUR", &[]),
        // ── Pure inversions (1 leg, exp=-1). ──
        // USDT/X = 1 / (X/USDT)
        SynthPath::new("USDT/BTC", &[("BTC/USDT", -1)]),
        SynthPath::new("USDT/ETH", &[("ETH/USDT", -1)]),
        SynthPath::new("USDT/SOL", &[("SOL/USDT", -1)]),
        SynthPath::new("USDT/PAXG", &[("PAXG/USDT", -1)]),
        // USDC/X = 1 / (X/USDC)
        SynthPath::new("USDC/BTC", &[("BTC/USDC", -1)]),
        SynthPath::new("USDC/ETH", &[("ETH/USDC", -1)]),
        // ── 2-leg crosses via USDT pivot (BTR ported, USDT-canonical). ──
        // ETH/BTC = (ETH/USDT) × (USDT/BTC) = (ETH/USDT) / (BTC/USDT)
        SynthPath::new("ETH/BTC", &[("ETH/USDT", 1), ("BTC/USDT", -1)]),
        SynthPath::new("SOL/BTC", &[("SOL/USDT", 1), ("BTC/USDT", -1)]),
        SynthPath::new("SOL/ETH", &[("SOL/USDT", 1), ("ETH/USDT", -1)]),
        SynthPath::new("ADA/BTC", &[("ADA/USDT", 1), ("BTC/USDT", -1)]),
        SynthPath::new("XRP/BTC", &[("XRP/USDT", 1), ("BTC/USDT", -1)]),
        // ── 2-leg crosses via USDT pivot — BNB-quoted (operator priority). ──
        // ETH/BNB = (ETH/USDT) × (USDT/BNB) = (ETH/USDT) / (BNB/USDT)
        SynthPath::new("ETH/BNB", &[("ETH/USDT", 1), ("BNB/USDT", -1)]),
        SynthPath::new("BTC/BNB", &[("BTC/USDT", 1), ("BNB/USDT", -1)]),
        // ── Cross-quote (FX bridge). EUR-quoted via EUR/USDT inverse. ──
        // BTC/EUR = BTC/USDT × USDT/EUR = (BTC/USDT) / (EUR/USDT)
        SynthPath::new("BTC/EUR", &[("BTC/USDT", 1), ("EUR/USDT", -1)]),
        SynthPath::new("ETH/EUR", &[("ETH/USDT", 1), ("EUR/USDT", -1)]),
        SynthPath::new("PAXG/EUR", &[("PAXG/USDT", 1), ("EUR/USDT", -1)]),
        // ── Gold crosses (USDT-denominated). ──
        SynthPath::new("BTC/PAXG", &[("BTC/USDT", 1), ("PAXG/USDT", -1)]),
        SynthPath::new("ETH/PAXG", &[("ETH/USDT", 1), ("PAXG/USDT", -1)]),
        // ── USDC-quoted FX and metals, pinned to the USD pivot. ──
        // `derive_legs` returns the FIRST pivot that merely RESOLVES, so the
        // moment a thin `<FX>/USDT` ticker is born it wins the USDT pivot and
        // caps the cross at that ticker's history. EUR/USDT gained its first
        // shard on 2026-07-31 and EUR/USDC collapsed from the full USD-pivot
        // depth to 3 days. These entries make the deep pivot explicit: both
        // legs are primaries, so the DAG invariant holds.
        // ponytail: per-symbol pinning, not general. The general fix is the
        // weighted cross graph in docs/internal/universal-cross-routing.md.
        SynthPath::new("EUR/USDC", &[("EUR/USD", 1), ("USDC/USD", -1)]),
        SynthPath::new("GBP/USDC", &[("GBP/USD", 1), ("USDC/USD", -1)]),
        SynthPath::new("AUD/USDC", &[("AUD/USD", 1), ("USDC/USD", -1)]),
        SynthPath::new("NZD/USDC", &[("NZD/USD", 1), ("USDC/USD", -1)]),
        SynthPath::new("XAU/USDC", &[("XAU/USD", 1), ("USDC/USD", -1)]),
        SynthPath::new("XAG/USDC", &[("XAG/USD", 1), ("USDC/USD", -1)]),
        // DAI is Pyth-only (feed 202, min 3 publishers), so no venue book backs
        // it. The USDT pivot resolved DAI/USDC off the dead kraken DAI/USDT
        // (24 shards, last 2026-08-02) and capped the cross at 84 H1 bars;
        // DAI/USD carries 757.
        SynthPath::new("DAI/USDC", &[("DAI/USD", 1), ("USDC/USD", -1)]),
    ]
});

/// O(1) lookup table built once from [`SYNTH_PATHS`].
static PATH_BY_SYM: LazyLock<HashMap<&'static str, &'static SynthPath>> =
    LazyLock::new(|| SYNTH_PATHS.iter().map(|p| (p.sym.as_str(), p)).collect());

/// Reverse map: leg sym → synth paths depending on that leg.
static SYNTH_DEPS: LazyLock<HashMap<&'static str, Vec<&'static SynthPath>>> = LazyLock::new(|| {
    let mut m: HashMap<&'static str, Vec<&'static SynthPath>> = HashMap::new();
    for p in SYNTH_PATHS.iter() {
        for leg in &p.legs {
            m.entry(leg.sym.as_str()).or_default().push(p);
        }
    }
    m
});

/// Lookup a synth path by symbol. `O(1)`.
#[inline]
pub fn path_for(sym: &str) -> Option<&'static SynthPath> {
    PATH_BY_SYM.get(sym).copied()
}

/// True iff `sym` is a synth (member of [`SYNTH_PATHS`]).
#[inline]
pub fn is_synth(sym: &str) -> bool {
    PATH_BY_SYM.contains_key(sym)
}

/// Reverse dependency lookup: returns the slice of synth paths that include
/// `leg` in their `legs`. Empty slice when no dependents.
#[inline]
pub fn synth_deps(leg: &str) -> &'static [&'static SynthPath] {
    SYNTH_DEPS.get(leg).map(Vec::as_slice).unwrap_or(&[])
}

/// Candidate pivots for compose-on-read, in tie-break order. USDT and USD come
/// first so a volume-blind caller keeps the historical route exactly; USDC is
/// reachable only here, and before it was added a PYUSD or RLUSD cross could not
/// route through its deepest book (Coinbase/Bullish quote those against USDC).
pub const PIVOTS: [&str; 3] = ["USDT", "USD", "USDC"];

/// Generic pivot derivation for compose-on-read: `A/B = (A/P) × (B/P)⁻¹`, over
/// [`PIVOTS`]. Each leg is accepted direct (`X/P`) or inverted (`P/X`, exponent
/// flipped); a leg equal to the pivot is the identity and is dropped. `resolve`
/// maps a candidate leg symbol to its ticker id iff that symbol is a REGISTERED
/// live feed (typically a `symbol_map` lookup) — mere resolvability is not
/// enough, the leg must actually have data behind it.
///
/// Volume-blind: takes the first pivot that resolves. Prefer
/// [`derive_legs_ranked`] anywhere 24h volumes are available, because
/// first-resolves-wins will happily route a cross through a near-dead book when
/// a far deeper one exists at the next pivot.
///
/// Used by `/v1/ohlc` to serve any cross with no persisted series and no
/// static [`SYNTH_PATHS`] entry. Returns `(leg_symbol, exponent, ticker_id)`.
pub fn derive_legs(
    base: &str,
    quote: &str,
    resolve: &dyn Fn(&str) -> Option<u64>,
) -> Option<Vec<(String, i8, u64)>> {
    derive_legs_ranked(base, quote, resolve, &|_| 0.0)
}

/// [`derive_legs`], but picks the DEEPEST viable pivot instead of the first.
///
/// `vol` returns a leg's 24h USD volume (0.0 when unknown). A route is scored by
/// its THINNEST leg, not the sum: a composition is only as trustworthy as its
/// weakest hop, and summing lets one deep leg mask a leg with no book behind it.
/// Highest score wins; ties (notably the all-zero case, i.e. no volume data at
/// all) fall back to [`PIVOTS`] order, so a caller with no weights file behaves
/// exactly as the volume-blind path did.
pub fn derive_legs_ranked(
    base: &str,
    quote: &str,
    resolve: &dyn Fn(&str) -> Option<u64>,
    vol: &dyn Fn(&str) -> f64,
) -> Option<Vec<(String, i8, u64)>> {
    if base.is_empty() || quote.is_empty() || base == quote {
        return None;
    }
    // Guard against self-reference: when pivot == base (or == quote), the
    // inverted candidate `{pivot}/{asset}` can reconstruct `{base}/{quote}`
    // itself (e.g. deriving USDT/JPY at the USDT pivot resolves the leg
    // "USDT/JPY" straight back to the output's own registered id). Reject
    // any candidate equal to the symbol being derived so the loop falls
    // through to the next pivot instead of self-composing.
    let self_sym = format!("{base}/{quote}");
    let mut best: Option<(f64, Vec<(String, i8, u64)>)> = None;
    for pivot in PIVOTS {
        let leg = |asset: &str, exp: i8| -> Option<Option<(String, i8, u64)>> {
            if asset == pivot {
                return Some(None); // identity leg
            }
            let direct = format!("{asset}/{pivot}");
            if direct != self_sym
                && let Some(id) = resolve(&direct)
            {
                return Some(Some((direct, exp, id)));
            }
            let inv = format!("{pivot}/{asset}");
            if inv == self_sym {
                return None;
            }
            resolve(&inv).map(|id| Some((inv, -exp, id)))
        };
        let (Some(a), Some(b)) = (leg(base, 1), leg(quote, -1)) else {
            continue;
        };
        let legs: Vec<_> = [a, b].into_iter().flatten().collect();
        if legs.is_empty() {
            continue;
        }
        // Bottleneck depth. An identity leg (pivot == base or quote) is dropped
        // above and so cannot pull the score down: a one-leg route is scored on
        // the only book it actually reads.
        let score = legs
            .iter()
            .map(|(sym, _, _)| vol(sym))
            .fold(f64::INFINITY, f64::min);
        if best.as_ref().is_none_or(|(s, _)| score > *s) {
            best = Some((score, legs));
        }
    }
    best.map(|(_, legs)| legs)
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
    fn registry_nonempty_and_lookup_works() {
        assert!(SYNTH_PATHS.len() >= 12, "registry should have ≥12 entries");
        assert!(path_for("ETH/BTC").is_some());
        assert!(path_for("DOES/NOT/EXIST").is_none());
        assert!(is_synth("ETH/BTC"));
        assert!(!is_synth("BTC/USDT")); // raw symbol, not synth
    }

    #[test]
    fn trivial_identity_has_zero_legs() {
        let p = path_for("EUR/EUR").expect("EUR/EUR present");
        assert!(p.legs.is_empty());
    }

    #[test]
    fn dag_invariant_legs_reference_only_earlier_paths_or_raw() {
        // For each synth path, every leg sym must be either a raw symbol (no entry in
        // PATH_BY_SYM) OR refer to a synth path that appears strictly earlier in the vec.
        let order: HashMap<&str, usize> = SYNTH_PATHS
            .iter()
            .enumerate()
            .map(|(i, p)| (p.sym.as_str(), i))
            .collect();
        for (i, p) in SYNTH_PATHS.iter().enumerate() {
            for leg in &p.legs {
                if let Some(&j) = order.get(leg.sym.as_str()) {
                    assert!(
                        j < i,
                        "DAG violation: {} legs include {} at position ≥ {}",
                        p.sym,
                        leg.sym,
                        i
                    );
                }
            }
        }
    }

    #[test]
    fn synth_deps_built_correctly() {
        let deps = synth_deps("BTC/USDT");
        assert!(!deps.is_empty(), "BTC/USDT must back several synths");
        // ETH/BTC depends on BTC/USDT.
        assert!(deps.iter().any(|p| p.sym == "ETH/BTC"));
    }

    #[test]
    fn dash_normalizes_to_slash() {
        assert_eq!(normalize_to_slash("BTC-USDT"), "BTC/USDT");
        assert_eq!(normalize_to_slash("BTC/USDT"), "BTC/USDT");
        assert_eq!(
            normalize_to_slash("0x060A8D644C100000"),
            "0x060A8D644C100000"
        );
    }

    #[test]
    fn usdc_quoted_fx_pins_the_usd_pivot() {
        // `derive_legs` returns the first pivot that merely RESOLVES, so a
        // newborn thin EUR/USDT would win the USDT pivot and cap EUR/USDC at
        // that ticker's history (measured: 3 days, 2026-07-31). The explicit
        // path must win, and it must route through USD.
        for sym in [
            "EUR/USDC", "GBP/USDC", "AUD/USDC", "NZD/USDC", "XAU/USDC", "XAG/USDC", "DAI/USDC",
        ] {
            let p = path_for(sym).unwrap_or_else(|| panic!("{sym} must have an explicit path"));
            let legs: Vec<&str> = p.legs.iter().map(|l| l.sym.as_str()).collect();
            assert_eq!(
                legs[1], "USDC/USD",
                "{sym} must be quoted off the USD pivot"
            );
            assert!(
                legs[0].ends_with("/USD"),
                "{sym} base leg must be USD-quoted"
            );
            assert!(
                !legs.iter().any(|l| l.contains("USDT")),
                "{sym} must not route through USDT"
            );
        }
    }

    #[test]
    fn derive_legs_rejects_usdt_pivot_self_reference() {
        // USDT/JPY at the USDT pivot: base="USDT" == pivot, so the quote leg's
        // inverse candidate is literally "USDT/JPY" (the symbol being derived).
        // A registry where "USDT/JPY" itself resolves (as it would once the
        // triangulator has registered its own synthesis output) must not
        // short-circuit to that self-referential leg; it must fall through to
        // the USD pivot instead.
        let resolve = |sym: &str| -> Option<u64> {
            match sym {
                "USDT/USD" => Some(1),
                "USD/JPY" => Some(2),
                "USDT/JPY" => Some(999), // self id; must be rejected as a leg
                _ => None,
            }
        };
        let legs = derive_legs("USDT", "JPY", &resolve).expect("should resolve via USD pivot");
        assert!(
            legs.iter()
                .all(|(sym, _, id)| sym != "USDT/JPY" && *id != 999),
            "derive_legs must not self-reference: got {legs:?}"
        );
        assert_eq!(
            legs.len(),
            2,
            "expected USDT/USD + USD/JPY legs, got {legs:?}"
        );
    }

    #[test]
    fn derive_legs_no_resolvable_pivot_returns_none() {
        let resolve = |_: &str| -> Option<u64> { None };
        assert!(derive_legs("FOO", "BAR", &resolve).is_none());
    }

    /// Registry where a stable is quoted on BOTH the USDT and USDC pivots, so
    /// the choice is decided purely by depth, never by pivot order.
    fn two_pivot_resolve(sym: &str) -> Option<u64> {
        match sym {
            "PYUSD/USDT" => Some(1),
            "PYUSD/USDC" => Some(2),
            "USDC/USD" => Some(3),
            "USDT/USD" => Some(4),
            _ => None,
        }
    }

    #[test]
    fn ranked_picks_the_deeper_pivot_not_the_first() {
        // PYUSD's real book is on Coinbase/Bullish against USDC; the USDT quote
        // is an order of magnitude thinner. Volume-blind derivation takes USDT
        // purely because it is first in PIVOTS, which is the bug this fixes.
        let vol = |sym: &str| -> f64 {
            match sym {
                "PYUSD/USDT" => 1_000_000.0,
                "PYUSD/USDC" => 90_000_000.0,
                "USDC/USD" => 500_000_000.0,
                "USDT/USD" => 500_000_000.0,
                _ => 0.0,
            }
        };
        let blind = derive_legs("PYUSD", "USD", &two_pivot_resolve).unwrap();
        assert_eq!(blind[0].0, "PYUSD/USDT", "volume-blind keeps the old route");

        let ranked = derive_legs_ranked("PYUSD", "USD", &two_pivot_resolve, &vol).unwrap();
        assert_eq!(
            ranked[0].0, "PYUSD/USDC",
            "ranked must route through the deeper book, got {ranked:?}"
        );
    }

    #[test]
    fn ranked_scores_the_bottleneck_leg_not_the_sum() {
        // USDC route: one enormous leg beside a dead one. USDT route: both legs
        // moderate. Summing picks the route with the dead leg (500.001M vs 4M);
        // bottleneck picks USDT, which is the only one that can actually fill.
        let vol = |sym: &str| -> f64 {
            match sym {
                "PYUSD/USDC" => 1_000.0,
                "USDC/USD" => 500_000_000.0,
                "PYUSD/USDT" => 2_000_000.0,
                "USDT/USD" => 2_000_000.0,
                _ => 0.0,
            }
        };
        let legs = derive_legs_ranked("PYUSD", "USD", &two_pivot_resolve, &vol).unwrap();
        assert_eq!(
            legs[0].0, "PYUSD/USDT",
            "a route is only as good as its thinnest leg, got {legs:?}"
        );
    }

    #[test]
    fn ranked_falls_back_to_pivot_order_without_volume_data() {
        // No weights file yet (fresh pod, or nxr-calibrate): every score is 0,
        // so the ranker must degrade to exactly the volume-blind route rather
        // than to whichever pivot happens to sort last.
        let legs = derive_legs_ranked("PYUSD", "USD", &two_pivot_resolve, &|_| 0.0).unwrap();
        assert_eq!(legs[0].0, "PYUSD/USDT");
    }

    #[test]
    fn ranked_keeps_the_self_reference_guard() {
        // Same trap as the volume-blind case, but now the ranker scores every
        // pivot before choosing: a self-referential candidate must stay rejected
        // rather than win on a high score.
        let resolve = |sym: &str| -> Option<u64> {
            match sym {
                "USDT/USD" => Some(1),
                "USD/JPY" => Some(2),
                "USDT/JPY" => Some(999),
                _ => None,
            }
        };
        let vol = |sym: &str| -> f64 { if sym == "USDT/JPY" { 1e12 } else { 1.0 } };
        let legs = derive_legs_ranked("USDT", "JPY", &resolve, &vol).unwrap();
        assert!(
            legs.iter()
                .all(|(sym, _, id)| sym != "USDT/JPY" && *id != 999),
            "ranked must not self-reference even when the self leg scores highest: {legs:?}"
        );
    }

    #[test]
    fn ranked_reaches_usdc_only_books() {
        // Before USDC joined PIVOTS this cross was underivable: neither asset
        // has a USDT or USD quote, so both pivots failed and the pair went dark.
        let resolve = |sym: &str| -> Option<u64> {
            match sym {
                "RLUSD/USDC" => Some(1),
                "PYUSD/USDC" => Some(2),
                _ => None,
            }
        };
        let legs = derive_legs_ranked("RLUSD", "PYUSD", &resolve, &|_| 1.0).unwrap();
        assert_eq!(legs.len(), 2, "expected both USDC legs, got {legs:?}");
        assert!(legs.iter().all(|(sym, _, _)| sym.ends_with("/USDC")));
    }

    #[test]
    fn ranked_drops_identity_leg_and_scores_the_real_book() {
        // base == pivot: the USDC leg is the identity and is dropped, so the
        // score must come from the surviving leg alone. If the identity were
        // scored as 0 it would sink every pivot that touches it.
        let resolve = |sym: &str| -> Option<u64> {
            match sym {
                "PYUSD/USDC" => Some(1),
                "PYUSD/USDT" => Some(2),
                _ => None,
            }
        };
        let vol = |sym: &str| -> f64 {
            match sym {
                "PYUSD/USDC" => 90_000_000.0,
                "PYUSD/USDT" => 1_000.0,
                _ => 0.0,
            }
        };
        let legs = derive_legs_ranked("USDC", "PYUSD", &resolve, &vol).unwrap();
        assert_eq!(legs.len(), 1, "identity leg must be dropped: {legs:?}");
        assert_eq!(legs[0].0, "PYUSD/USDC");
    }
}
