//! Uncertainty propagation.
//!
//! One rule, applied wherever independent uncertainties combine: add in
//! quadrature. Everything else in the tree that ends in `.sqrt()` is a
//! different question and stays where it is; see the notes on the call sites
//! listed under [`rss`].

/// Root-sum-square: `sqrt(Σ x²)`. The combination law for INDEPENDENT
/// uncertainty contributions, absolute or relative (both legs must be the same
/// kind). Returns 0.0 for an empty slice.
///
/// Relative form: for a product `z = x·y`, `ci_z = |z| · rss([ci_x/|x|,
/// ci_y/|y|])`. That is the whole of `core/src/triangulator.rs` cross CI.
///
/// NOT this rule, deliberately:
/// - `idx_heal::heal_bin` pools the CI of repeated observations of ONE quantity
///   inside a time bin: a volume-weighted RMS `sqrt(Σ w·ci² / Σ w)`, which
///   shrinks with sample count where quadrature grows.
/// - `tdwap` combines two VARIANCE components of one estimate that arrive
///   already squared (`sqrt(σ²_disagree + σ²_stale)`); routing them through
///   `rss` would square roots it just took.
/// - `synth::compose::compose_cross_s10` is this rule at n legs, but it
///   accumulates `Σ ci²` inside the loop that also folds OHLC, volume and
///   spread. Collecting a leg vector just to call `rss` would allocate per bar.
pub fn rss(terms: &[f64]) -> f64 {
    terms.iter().map(|x| x * x).sum::<f64>().sqrt()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pythagorean() {
        assert!((rss(&[3.0, 4.0]) - 5.0).abs() < 1e-12);
        assert!((rss(&[1.0, 2.0, 2.0]) - 3.0).abs() < 1e-12);
    }

    #[test]
    fn degenerate() {
        assert_eq!(rss(&[]), 0.0);
        assert_eq!(rss(&[0.0, 0.0]), 0.0);
        assert_eq!(rss(&[7.0]), 7.0);
    }

    #[test]
    fn matches_the_open_coded_binary_form() {
        let (a, b) = (0.0037_f64, 0.0121_f64);
        assert_eq!(rss(&[a, b]), (a.powi(2) + b.powi(2)).sqrt());
    }
}
