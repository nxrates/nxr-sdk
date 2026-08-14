//! Herfindahl concentration over a weight vector.
//!
//! One definition for both readings of the same number: `hhi` in [1/N, 1]
//! (1 = a single holder) and `n_eff = 1/hhi` in [1, N], the effective count.
//! `core/src/weights.rs` derives the per-ticker weight ceiling from `sqrt(hhi)`
//! and `tdwap` publishes `n_eff` as the breadth axis of a composite.

/// Herfindahl index: Σ (w_i / Σw)². Takes UNNORMALISED weights; normalising
/// first is a no-op (Σw = 1).
///
/// Returns 1.0 for an empty vector or a non-positive sum: with nothing to
/// spread across, maximal concentration is the conservative reading.
pub fn hhi(weights: &[f64]) -> f64 {
    let sum: f64 = weights.iter().sum();
    if sum <= 0.0 {
        return 1.0;
    }
    weights
        .iter()
        .map(|w| {
            let s = w / sum;
            s * s
        })
        .sum()
}

/// Effective holder count from running sums, so a streaming accumulator does
/// not need the vector. `(Σw)² / Σw²`, the reciprocal of [`hhi`].
/// Returns 0.0 when `Σw² <= 0` (nothing accumulated).
#[inline]
pub fn n_eff_from_sums(w_sum: f64, w_sq_sum: f64) -> f64 {
    if w_sq_sum > 0.0 { (w_sum * w_sum) / w_sq_sum } else { 0.0 }
}

/// Effective holder count over a weight vector: `1 / hhi(weights)`.
#[inline]
pub fn n_eff(weights: &[f64]) -> f64 {
    1.0 / hhi(weights)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn uniform_is_one_over_n() {
        let w = [1.0, 1.0, 1.0, 1.0];
        assert!((hhi(&w) - 0.25).abs() < 1e-12);
        assert!((n_eff(&w) - 4.0).abs() < 1e-12);
    }

    #[test]
    fn single_holder_is_one() {
        assert!((hhi(&[7.0]) - 1.0).abs() < 1e-12);
        assert!((n_eff(&[7.0]) - 1.0).abs() < 1e-12);
    }

    #[test]
    fn degenerate_input_is_maximal_concentration() {
        assert_eq!(hhi(&[]), 1.0);
        assert_eq!(hhi(&[0.0, 0.0]), 1.0);
    }

    #[test]
    fn scale_invariant() {
        let a = [3.0, 1.0, 6.0];
        let b = [300.0, 100.0, 600.0];
        assert!((hhi(&a) - hhi(&b)).abs() < 1e-12);
    }

    #[test]
    fn from_sums_matches_the_slice_form() {
        let w = [4.0, 2.0, 1.0, 1.0];
        let s: f64 = w.iter().sum();
        let sq: f64 = w.iter().map(|x| x * x).sum();
        assert!((n_eff_from_sums(s, sq) - n_eff(&w)).abs() < 1e-12);
        assert_eq!(n_eff_from_sums(0.0, 0.0), 0.0);
    }
}
