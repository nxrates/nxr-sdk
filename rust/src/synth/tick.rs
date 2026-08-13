//! Instantaneous synth tick composition — verbatim port of `computeSynthTick`
//! from `~/Work/btr/sdk/src/types/synth-ohlc.ts` (lines 326-360).
//!
//! Multiplicative composition over signed legs:
//! ```text
//! mid = Π (k_i.mid)^{e_i}
//! bid = Π ( e=+1 ? k_i.bid : 1/k_i.ask )^|e|
//! ask = Π ( e=+1 ? k_i.ask : 1/k_i.bid )^|e|
//! conf = min_i k_i.conf
//! ```
//!
//! Inversion swaps bid↔ask so that `bid ≤ ask` is preserved post-composition.
//! Degenerate inputs (missing leg, non-positive quote) return `None`.
//! A crossed quote after composition (`ask < bid`) collapses to `conf=0`.

use std::collections::HashMap;

use super::paths::SynthPath;

/// Per-leg scalar quote at a single instant.
#[derive(Clone, Copy, Debug)]
pub struct LegTick {
    pub bid: f64,
    pub ask: f64,
    pub mid: f64,
    /// Confidence in bps ∈ [0, 10000]. 0 = stale / one-sided.
    pub conf: u16,
}

/// Composed synth tick (output of [`compute_synth_tick`]).
#[derive(Clone, Copy, Debug)]
pub struct SynthTick {
    pub bid: f64,
    pub ask: f64,
    pub mid: f64,
    pub conf: u16,
}

/// Compose a synthetic instantaneous tick from signed legs.
///
/// Returns `None` if any leg is missing or has any non-positive bid/ask/mid.
/// Identity path (`legs.is_empty()`) returns `(mid=1, bid=1, ask=1, conf=10000)`.
///
/// A crossed quote in the output (`ask < bid`) collapses `conf` to `0` — caller
/// can decide whether to treat as stale or surface anyway. This mirrors BTR.
pub fn compute_synth_tick(
    path: &SynthPath,
    legs: &HashMap<&str, LegTick>,
) -> Option<SynthTick> {
    let signed: Vec<(i8, LegTick)> = path
        .legs
        .iter()
        .map(|l| legs.get(l.sym.as_str()).map(|k| (l.exp, *k)))
        .collect::<Option<_>>()?;
    compose_legs(&signed)
}

/// The composition itself, over `(exponent, quote)` pairs. Keyed on nothing: the
/// caller has already matched legs to quotes, by symbol ([`compute_synth_tick`])
/// or by MITCH ticker id ([`super::cross::Route::compose`]).
pub fn compose_legs(legs: &[(i8, LegTick)]) -> Option<SynthTick> {
    // Trivial-identity path (e.g. EUR/EUR with 0 legs) → 1.0 quote, full conf.
    if legs.is_empty() {
        return Some(SynthTick { bid: 1.0, ask: 1.0, mid: 1.0, conf: 10_000 });
    }

    let mut mid = 1.0_f64;
    let mut bid = 1.0_f64;
    let mut ask = 1.0_f64;
    let mut conf: u16 = 10_000;

    for (exp, k) in legs {
        // is_finite() + magnitude cap (not just >0.0): a poisoned leg (finite
        // but astronomical) multiplied straight into `mid`/`bid`/`ask` below
        // would otherwise silently corrupt the whole cross (2026-07-10
        // incident class - see mitch::MAX_PRICE doc).
        if !(k.mid.is_finite() && k.mid > 0.0 && k.mid <= mitch::MAX_PRICE)
            || !(k.bid.is_finite() && k.bid > 0.0 && k.bid <= mitch::MAX_PRICE)
            || !(k.ask.is_finite() && k.ask > 0.0 && k.ask <= mitch::MAX_PRICE)
        {
            return None;
        }
        if *exp == 1 {
            mid *= k.mid;
            bid *= k.bid;
            ask *= k.ask;
        } else {
            // exp == -1: inversion swaps bid↔ask to preserve bid ≤ ask post-composition.
            mid /= k.mid;
            bid /= k.ask;
            ask /= k.bid;
        }
        if k.conf < conf {
            conf = k.conf;
        }
    }

    // Degeneracy guard: crossed / non-positive quote → conf=0 (signal stale).
    if !(ask >= bid) || bid <= 0.0 || ask <= 0.0 {
        conf = 0;
    }

    Some(SynthTick { bid, ask, mid, conf })
}
