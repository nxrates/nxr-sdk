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
    // Trivial-identity path (e.g. EUR/EUR with 0 legs) → 1.0 quote, full conf.
    if path.legs.is_empty() {
        return Some(SynthTick { bid: 1.0, ask: 1.0, mid: 1.0, conf: 10_000 });
    }

    let mut mid = 1.0_f64;
    let mut bid = 1.0_f64;
    let mut ask = 1.0_f64;
    let mut conf: u16 = 10_000;

    for leg in &path.legs {
        let k = legs.get(leg.sym.as_str())?;
        if !(k.mid > 0.0) || !(k.bid > 0.0) || !(k.ask > 0.0) {
            return None;
        }
        if leg.exp == 1 {
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
