//! Centre, dispersion, and rounding.
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

/// Median of a sample under a projection to f64. Generic so integer samples
/// (`u64` bar counts, `i64` inter-record deltas) share this one definition.
/// Allocates a projected+sorted copy; O(n log n). Returns 0.0 for an empty
/// slice. NaN sorts as greater than non-NaN.
///
/// Even length averages the two middles. That is the whole-tree convention:
/// call sites taking the upper middle instead are drift, not policy.
pub fn median_by<T>(data: &[T], f: impl Fn(&T) -> f64) -> f64 {
    if data.is_empty() { return 0.0; }
    let mut s: Vec<f64> = data.iter().map(&f).collect();
    s.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    let n = s.len();
    if n % 2 == 0 { (s[n / 2 - 1] + s[n / 2]) / 2.0 } else { s[n / 2] }
}

/// Median of an f64 sample. See [`median_by`].
#[inline]
pub fn median(data: &[f64]) -> f64 {
    median_by(data, |&x| x)
}

/// Linear-interpolated quantile, `q` a fraction in [0, 1] (clamped). Matches
/// numpy's default `linear` interpolation, so `percentile(x, 0.5)` equals
/// [`median`] on both parities. Returns 0.0 for an empty slice.
pub fn percentile(data: &[f64], q: f64) -> f64 {
    if data.is_empty() { return 0.0; }
    let mut s = data.to_vec();
    s.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    let pos = q.clamp(0.0, 1.0) * (s.len() - 1) as f64;
    let (lo, hi) = (pos.floor() as usize, pos.ceil() as usize);
    if lo == hi { return s[lo]; }
    s[lo] + (s[hi] - s[lo]) * (pos - lo as f64)
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

    #[test]
    fn median_by_odd_and_even_on_integers() {
        assert_eq!(median_by(&[3u64, 1, 2], |&v| v as f64), 2.0);
        assert_eq!(median_by(&[4u64, 1, 3, 2], |&v| v as f64), 2.5);
        assert_eq!(median_by(&[10i64, 20, 30, 41], |&v| v as f64), 25.0);
        assert_eq!(median_by::<u64>(&[], |&v| v as f64), 0.0);
    }

    #[test]
    fn percentile_odd_and_even() {
        // numpy: percentile([1,2,3,4], q) -> 2.5 at q=0.5, 3.1 at q=0.7
        let even = [1.0, 2.0, 3.0, 4.0];
        assert!((percentile(&even, 0.5) - 2.5).abs() < 1e-12);
        assert!((percentile(&even, 0.7) - 3.1).abs() < 1e-12);
        let odd = [1.0, 2.0, 3.0];
        assert_eq!(percentile(&odd, 0.5), 2.0);
        assert_eq!(percentile(&odd, 0.0), 1.0);
        assert_eq!(percentile(&odd, 1.0), 3.0);
        assert_eq!(percentile(&[], 0.5), 0.0);
        // q=0.5 is the median on both parities.
        assert_eq!(percentile(&even, 0.5), median(&even));
        assert_eq!(percentile(&odd, 0.5), median(&odd));
    }
}
