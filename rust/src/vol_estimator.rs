//! Canonical per-bin volatility estimator — Rogers-Satchell over OHLC.
//!
//! ONE kernel, shared by every σ producer (offline `.vol` builder, the live
//! [`crate::vol::LiveVolRing`], and the synth OHLC reconstruction). The ratified
//! decision (2026-06): the canonical 30-min vol-bin σ is the Rogers-Satchell
//! (1991) drift-robust range estimator computed over s10-resampled OHLC, with
//! `offline == live` byte-for-byte.
//!
//! Per 30-min vol-bin OHLC (O = first s10.open, H = max s10.high,
//! L = min s10.low, C = last s10.close, on the TDWAP mid):
//!
//! ```text
//! v          = ln(H/C)·ln(H/O) + ln(L/C)·ln(L/O)
//! sigma_pct  = sqrt(v.max(0))
//! ```
//!
//! This emits the SAME per-bin std-of-log-price contract the old
//! `parkinson_sigma()` emitted → downstream EMA(28) → MTF inverse-variance
//! winsorized blend → `brick_pct = max(k·σ, MIN_BRICK_PCT)` stays byte-stable;
//! ONLY the per-bin kernel + its input source change.

/// Rogers-Satchell variance for one OHLC bucket.
///
/// `v = ln(H/C)·ln(H/O) + ln(L/C)·ln(L/O)`. Non-negative by construction when
/// `H ≥ max(O,C)` and `L ≤ min(O,C)`. Returns `0.0` on degenerate / non-finite
/// input (any non-positive price).
#[inline]
pub fn rs_variance(o: f64, h: f64, l: f64, c: f64) -> f64 {
    if !(o > 0.0 && h > 0.0 && l > 0.0 && c > 0.0) {
        return 0.0;
    }
    let lhc = (h / c).ln();
    let lho = (h / o).ln();
    let llc = (l / c).ln();
    let llo = (l / o).ln();
    let v = lhc * lho + llc * llo;
    if v.is_finite() { v } else { 0.0 }
}

/// Per-bin Rogers-Satchell sigma (std-of-log-price) for one OHLC bucket.
///
/// `sigma_pct = sqrt(v.max(0))`. This is the canonical per-bin σ contract — the
/// drop-in replacement for the old Parkinson HL kernel. Same units, same
/// downstream EMA/blend.
#[inline]
pub fn rs_sigma_from_ohlc(o: f64, h: f64, l: f64, c: f64) -> f64 {
    rs_variance(o, h, l, c).max(0.0).sqrt()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rs_zero_range_is_zero() {
        assert_eq!(rs_sigma_from_ohlc(100.0, 100.0, 100.0, 100.0), 0.0);
    }

    #[test]
    fn rs_nonneg_on_well_formed_ohlc() {
        let s = rs_sigma_from_ohlc(100.0, 102.0, 99.0, 101.0);
        assert!(s > 0.0 && s.is_finite(), "got {s}");
    }

    #[test]
    fn rs_degenerate_inputs_are_zero() {
        assert_eq!(rs_sigma_from_ohlc(0.0, 1.0, 1.0, 1.0), 0.0);
        assert_eq!(rs_sigma_from_ohlc(1.0, -1.0, 1.0, 1.0), 0.0);
    }

    #[test]
    fn rs_matches_manual_formula() {
        let (o, h, l, c) = (100.0_f64, 105.0_f64, 98.0_f64, 102.0_f64);
        let v = (h / c).ln() * (h / o).ln() + (l / c).ln() * (l / o).ln();
        assert!((rs_sigma_from_ohlc(o, h, l, c) - v.max(0.0).sqrt()).abs() < 1e-15);
    }
}
