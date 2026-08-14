//! Canonical volatility kernels — Rogers-Satchell (OHLC) and Parkinson (HL).
//!
//! ONE home for both range estimators. Rogers-Satchell is the canonical per-bin
//! 30-min vol-bin σ, shared by the offline `.vol` builder, the live
//! [`crate::vol::LiveVolRing`], and the synth OHLC reconstruction. The ratified
//! decision (2026-06): the canonical 30-min vol-bin σ is the Rogers-Satchell
//! (1991) drift-robust range estimator computed over s10-resampled OHLC, with
//! `offline == live` byte-for-byte.
//!
//! Parkinson ([`parkinson_variance`] / [`parkinson_sigma`]) is NOT the vol-bin
//! basis. It survives for the two callers that genuinely need a high/low-only
//! estimator: the synth quadratic form (needs a per-leg VARIANCE it can invert
//! back to a log-range) and the offline audit's daily σ cross-check.
//!
//! ⚠ NOT every σ producer in the tree. `core/src/server/signed.rs` computes the
//! co-signed-quote σ with its OWN private 48-bar Parkinson estimator, so a
//! signed mark is gated on a σ that no `.vol` file or renko brick uses.
//!
//! That divergence is DELIBERATE, and swapping signed.rs to Rogers-Satchell is
//! NOT proven better (audit 2026-08-14, 21 d of live `.s10`, 30-min bins, the
//! signer's deployed 48- and 336-bar windows). RS/PK lands 0.94-1.05 on
//! BTC-USDT, EUR-USD, XAU-USD and EUR-USDC, inside the σ cosign tolerance, so
//! there is nothing to win; on a thin stable (USDC-USDT) it is 1.42-1.56,
//! because bid-ask bounce inflates the RS corner product on a wide book. Which
//! of the two is then CORRECT on that tape was not settled: every truth proxy
//! available on a bounce-dominated stable is itself bounce-contaminated. Do not
//! reopen this without a noise-robust proxy.
//!
//! RS stays HERE on two properties that survived review: per-bin drift
//! contamination orders RS < GK < PK on every tape measured, and EMA(28)
//! shrinks the variance of that contamination but not its bias, so it reaches
//! the renko brick. Its one failure mode is exact: RS is 0 on any monotone bar
//! (H=C,L=O or H=O,L=C). That is 0 % of non-degenerate 30-min bins on every
//! liquid tape, 6 % only on a stale session-equity tape, and the brick's
//! `min_pct` floor covers it.
//!
//! Per 30-min vol-bin OHLC (O = first s10.open, H = max s10.high,
//! L = min s10.low, C = last s10.close, on the TDWAP mid):
//!
//! ```text
//! v          = ln(H/C)·ln(H/O) + ln(L/C)·ln(L/O)
//! sigma_pct  = sqrt(v.max(0))
//! ```
//!
//! Emits the same per-bin std-of-log-price contract as the prior Parkinson
//! kernel → downstream EMA(28) → MTF inverse-variance winsorized blend →
//! `brick_pct = max(k·σ, MIN_BRICK_PCT)` stays byte-stable; ONLY the per-bin
//! kernel + its input source change.

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

/// `4 ln 2`, the Parkinson normalizer.
pub const FOUR_LN2: f64 = 4.0 * std::f64::consts::LN_2;
const INV_4LN2: f64 = 1.0 / FOUR_LN2;

/// Parkinson variance for one high/low bucket: `ln(H/L)^2 / (4 ln 2)`.
///
/// Caller validates `H >= L > 0`; garbage in yields a non-finite result rather
/// than a silent 0, because the two callers reject the bucket upstream and a
/// masked 0 would understate σ. Pairs with the [`FOUR_LN2`] inversion back to a
/// log-range in `synth::ohlc`.
#[inline]
pub fn parkinson_variance(h: f64, l: f64) -> f64 {
    let r = (h / l).ln();
    INV_4LN2 * r * r
}

/// Parkinson sigma (std-of-log-price) over aligned high/low slices.
///
/// `sqrt(mean(ln(H/L)^2) / (4 ln 2))`, valid for continuous GBM. Skips bars
/// where `low <= 0` or `high < low`. Returns 0.0 when no valid bar is found.
/// Same units as [`rs_sigma_from_ohlc`]: a per-bar fraction, unannualized.
pub fn parkinson_sigma(highs: &[f64], lows: &[f64]) -> f64 {
    let n = highs.len().min(lows.len());
    let mut sum = 0.0;
    let mut count = 0u32;
    for i in 0..n {
        if lows[i] > 0.0 && highs[i] >= lows[i] {
            let r = (highs[i] / lows[i]).ln();
            sum += r * r;
            count += 1;
        }
    }
    if count == 0 {
        return 0.0;
    }
    (sum / count as f64 / FOUR_LN2).sqrt()
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

    /// Pins the RS contract to a hand-computed value so a refactor cannot move
    /// σ silently: it is signed on-chain verbatim.
    #[test]
    fn rs_pinned_hand_computed() {
        // O=100 H=105 L=98 C=102:
        // ln(105/102)·ln(105/100) + ln(98/102)·ln(98/100) = 0.00222252274925343
        assert!((rs_variance(100.0, 105.0, 98.0, 102.0) - 0.002_222_522_749_253_43).abs() < 1e-15);
        assert!(
            (rs_sigma_from_ohlc(100.0, 105.0, 98.0, 102.0) - 0.047_143_639_541_866_4).abs()
                < 1e-15
        );
    }

    /// Pins the Parkinson contract the same way.
    #[test]
    fn parkinson_pinned_hand_computed() {
        // One bar, H=105 L=98: ln(105/98) = 0.0689928714869514; squared and
        // divided by 4 ln 2 = 2.7725887222397812.
        let v = parkinson_variance(105.0, 98.0);
        assert!((v - 0.001_716_812_983_416_35).abs() < 1e-15, "got {v}");
        // Single-bar sigma is sqrt of that variance.
        let s = parkinson_sigma(&[105.0], &[98.0]);
        assert!((s - v.sqrt()).abs() < 1e-15, "got {s}");
        assert!((s - 0.041_434_441_994_750_5).abs() < 1e-15, "got {s}");
        // Two bars average the SQUARED log-ranges, not the sigmas.
        let two = parkinson_sigma(&[105.0, 101.0], &[98.0, 100.0]);
        let manual = {
            let a = (105.0_f64 / 98.0).ln();
            let b = (101.0_f64 / 100.0).ln();
            ((a * a + b * b) / 2.0 / FOUR_LN2).sqrt()
        };
        assert!((two - manual).abs() < 1e-15);
    }

    #[test]
    fn parkinson_skips_invalid_bars() {
        assert_eq!(parkinson_sigma(&[105.0], &[0.0]), 0.0);
        assert_eq!(parkinson_sigma(&[98.0], &[105.0]), 0.0);
        assert_eq!(parkinson_sigma(&[], &[]), 0.0);
        // An invalid bar is dropped, not counted in the divisor.
        let mixed = parkinson_sigma(&[105.0, 98.0], &[98.0, 105.0]);
        assert!((mixed - parkinson_sigma(&[105.0], &[98.0])).abs() < 1e-15);
    }
}
