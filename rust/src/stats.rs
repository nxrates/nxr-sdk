//! Statistical primitives: mean, dispersion, and robust centre. Single
//! canonical home for the stats used by aggregation (TDWAP CI), series
//! calibration, and live monitoring.
//!
//! Variance convention: `std_dev` uses the sample (Bessel-corrected, n-1)
//! estimator. For population variance, divide sum-of-squared-deviations by n
//! at the callsite.

/// Round a finite f64 to `sig` significant digits. Single source of truth for
/// the `round_sig` (rest.rs) and `round_to_6_sig_digits` (series-factory) dups.
/// Returns the input unchanged for `0.0`, NaN, or infinite values.
#[inline]
pub fn round_to_sig_digits(v: f64, sig: i32) -> f64 {
    if v == 0.0 || !v.is_finite() {
        return v;
    }
    let mag = v.abs().log10().floor() as i32;
    let factor = 10f64.powi(sig - 1 - mag);
    (v * factor).round() / factor
}

/// Arithmetic mean of a slice. Returns 0.0 for an empty slice.
#[inline]
pub fn mean(data: &[f64]) -> f64 {
    if data.is_empty() { return 0.0; }
    data.iter().sum::<f64>() / data.len() as f64
}

/// Sample standard deviation (Bessel-corrected, divisor n-1).
/// Returns 0.0 for n < 2.
#[inline]
pub fn std_dev(data: &[f64]) -> f64 {
    let n = data.len() as f64;
    if n < 2.0 { return 0.0; }
    let m = data.iter().sum::<f64>() / n;
    let var = data.iter().map(|&x| (x - m) * (x - m)).sum::<f64>() / (n - 1.0);
    var.sqrt()
}

/// Median of a sample. Allocates a sorted copy; O(n log n). Returns 0.0 for
/// an empty slice. NaN values sort as greater than non-NaN values.
pub fn median(data: &[f64]) -> f64 {
    if data.is_empty() { return 0.0; }
    let mut s = data.to_vec();
    s.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    let n = s.len();
    if n % 2 == 0 { (s[n / 2 - 1] + s[n / 2]) / 2.0 } else { s[n / 2] }
}

/// Median absolute deviation about the sample median. Single allocation; uses
/// `median` internally. Returns 0.0 for an empty slice. Single source of
/// truth for the inline MAD copies previously in `series-factory`
/// (`data_quality_audit.rs` + `renko_continuity_check.rs`). Phase
/// 59.R3.C5.C2/C3 (2026-05-30).
pub fn mad(data: &[f64]) -> f64 {
    if data.is_empty() { return 0.0; }
    let med = median(data);
    let devs: Vec<f64> = data.iter().map(|x| (x - med).abs()).collect();
    median(&devs)
}

/// Convenience variant of [`mad`] that returns `(median, mad)` in a single
/// pass through the sort step. Same defaults as `mad(&[])` ⇒ `(0.0, 0.0)`.
pub fn median_and_mad(data: &[f64]) -> (f64, f64) {
    if data.is_empty() { return (0.0, 0.0); }
    let med = median(data);
    let devs: Vec<f64> = data.iter().map(|x| (x - med).abs()).collect();
    (med, median(&devs))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mean_empty_is_zero() {
        assert_eq!(mean(&[]), 0.0);
    }

    #[test]
    fn mean_single_element() {
        assert_eq!(mean(&[7.5]), 7.5);
    }

    #[test]
    fn mean_multiple_elements() {
        assert_eq!(mean(&[1.0, 2.0, 3.0, 4.0]), 2.5);
    }

    #[test]
    fn mean_propagates_nan() {
        assert!(mean(&[1.0, f64::NAN, 3.0]).is_nan());
    }

    #[test]
    fn std_dev_empty_is_zero() {
        assert_eq!(std_dev(&[]), 0.0);
    }

    #[test]
    fn std_dev_single_element_is_zero() {
        assert_eq!(std_dev(&[42.0]), 0.0);
    }

    #[test]
    fn std_dev_sample_variance_bessel() {
        // Sample variance of [1,2,3,4,5] is 10/4 = 2.5; std = sqrt(2.5)
        let s = std_dev(&[1.0, 2.0, 3.0, 4.0, 5.0]);
        assert!((s - 2.5f64.sqrt()).abs() < 1e-12);
    }

    #[test]
    fn std_dev_matches_sample_variance() {
        let d = [1.0, 2.0, 3.0, 4.0, 5.0];
        let s = std_dev(&d);
        assert!((s * s - 2.5).abs() < 1e-12);
    }

    #[test]
    fn median_odd_and_even() {
        assert_eq!(median(&[3.0, 1.0, 2.0]), 2.0);
        assert_eq!(median(&[4.0, 1.0, 3.0, 2.0]), 2.5);
        assert_eq!(median(&[]), 0.0);
    }

}
