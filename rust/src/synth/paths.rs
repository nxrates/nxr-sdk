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
        Self { sym: sym.into(), exp }
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
        SynthPath::new("USD/USD",   &[]),
        SynthPath::new("EUR/EUR",   &[]),

        // ── Pure inversions (1 leg, exp=-1). ──
        // USDT/X = 1 / (X/USDT)
        SynthPath::new("USDT/BTC",  &[("BTC/USDT",  -1)]),
        SynthPath::new("USDT/ETH",  &[("ETH/USDT",  -1)]),
        SynthPath::new("USDT/SOL",  &[("SOL/USDT",  -1)]),
        SynthPath::new("USDT/PAXG", &[("PAXG/USDT", -1)]),
        // USDC/X = 1 / (X/USDC)
        SynthPath::new("USDC/BTC",  &[("BTC/USDC",  -1)]),
        SynthPath::new("USDC/ETH",  &[("ETH/USDC",  -1)]),

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
        SynthPath::new("BTC/EUR",  &[("BTC/USDT",  1), ("EUR/USDT", -1)]),
        SynthPath::new("ETH/EUR",  &[("ETH/USDT",  1), ("EUR/USDT", -1)]),
        SynthPath::new("PAXG/EUR", &[("PAXG/USDT", 1), ("EUR/USDT", -1)]),

        // ── Gold crosses (USDT-denominated). ──
        SynthPath::new("BTC/PAXG", &[("BTC/USDT", 1), ("PAXG/USDT", -1)]),
        SynthPath::new("ETH/PAXG", &[("ETH/USDT", 1), ("PAXG/USDT", -1)]),
    ]
});

/// O(1) lookup table built once from [`SYNTH_PATHS`].
static PATH_BY_SYM: LazyLock<HashMap<&'static str, &'static SynthPath>> = LazyLock::new(|| {
    SYNTH_PATHS.iter().map(|p| (p.sym.as_str(), p)).collect()
});

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

/// Generic pivot derivation for compose-on-read: `A/B = (A/P) × (B/P)⁻¹`,
/// trying pivots USDT then USD. Each leg is accepted direct (`X/P`) or
/// inverted (`P/X`, exponent flipped); a leg equal to the pivot is the
/// identity and is dropped. `resolve` maps a candidate leg symbol to its
/// ticker id iff that symbol is a REGISTERED live feed (typically a
/// `symbol_map` lookup) — mere resolvability is not enough, the leg must
/// actually have data behind it.
///
/// Used by `/v1/ohlc` to serve any cross with no persisted series and no
/// static [`SYNTH_PATHS`] entry. Returns `(leg_symbol, exponent, ticker_id)`.
pub fn derive_legs(
    base: &str,
    quote: &str,
    resolve: &dyn Fn(&str) -> Option<u64>,
) -> Option<Vec<(String, i8, u64)>> {
    if base.is_empty() || quote.is_empty() || base == quote {
        return None;
    }
    for pivot in ["USDT", "USD"] {
        let leg = |asset: &str, exp: i8| -> Option<Option<(String, i8, u64)>> {
            if asset == pivot {
                return Some(None); // identity leg
            }
            let direct = format!("{asset}/{pivot}");
            if let Some(id) = resolve(&direct) {
                return Some(Some((direct, exp, id)));
            }
            let inv = format!("{pivot}/{asset}");
            resolve(&inv).map(|id| Some((inv, -exp, id)))
        };
        if let (Some(a), Some(b)) = (leg(base, 1), leg(quote, -1)) {
            let legs: Vec<_> = [a, b].into_iter().flatten().collect();
            if !legs.is_empty() {
                return Some(legs);
            }
        }
    }
    None
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
                    assert!(j < i, "DAG violation: {} legs include {} at position ≥ {}", p.sym, leg.sym, i);
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
        assert_eq!(normalize_to_slash("0x060A8D644C100000"), "0x060A8D644C100000");
    }
}
