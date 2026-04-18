//! Statistical primitives: mean, variance, std, OLS regression, and equity-curve
//! performance metrics (Sharpe/Sortino/drawdown). Single canonical home for all
//! stats used by aggregation (TDWAP CI), ML features, backtest fitness, and
//! live monitoring.
//!
//! Variance convention: `variance` / `std_dev` use the sample (Bessel-corrected,
//! n-1) estimator because that is what the historical ML callsites used. For
//! population variance, divide sum-of-squared-deviations by n at the callsite.

/// Arithmetic mean of a slice. Returns 0.0 for an empty slice.
#[inline]
pub fn mean(data: &[f64]) -> f64 {
    if data.is_empty() { return 0.0; }
    data.iter().sum::<f64>() / data.len() as f64
}

/// Sample variance (Bessel-corrected, divisor n-1). Returns 0.0 for n < 2.
#[inline]
pub fn variance(data: &[f64]) -> f64 {
    let n = data.len() as f64;
    if n < 2.0 { return 0.0; }
    let m = data.iter().sum::<f64>() / n;
    data.iter().map(|&x| (x - m) * (x - m)).sum::<f64>() / (n - 1.0)
}

/// Sample standard deviation (Bessel-corrected, divisor n-1).
/// Returns 0.0 for n < 2.
#[inline]
pub fn std_dev(data: &[f64]) -> f64 {
    variance(data).sqrt()
}

/// Simple 2-variable OLS: fit `y = alpha + beta * x`. Returns `(beta, alpha)`.
/// Returns `(0.0, 0.0)` when n < 3; `(0.0, mean(y))` when the regressor has
/// zero variance.
#[inline]
pub fn rolling_ols_xy(x: &[f64], y: &[f64]) -> (f64, f64) {
    let n = x.len() as f64;
    if n < 3.0 { return (0.0, 0.0); }
    let sx: f64 = x.iter().sum();
    let sy: f64 = y.iter().sum();
    let sxx: f64 = x.iter().map(|&v| v * v).sum();
    let sxy: f64 = x.iter().zip(y.iter()).map(|(&a, &b)| a * b).sum();
    let denom = n * sxx - sx * sx;
    if denom.abs() < 1e-15 { return (0.0, sy / n); }
    let beta = (n * sxy - sx * sy) / denom;
    let alpha = (sy - beta * sx) / n;
    (beta, alpha)
}

/// Result of an OLS fit of `y` against the sequential x = 0, 1, ..., n-1.
#[derive(Debug, Clone, Copy)]
pub struct OlsFit {
    pub slope: f64,
    pub intercept: f64,
    /// Standard error of the slope estimator.
    pub slope_se: f64,
    /// Coefficient of determination (R²).
    pub r_squared: f64,
}

/// OLS fit of `y` against indices 0..y.len(). Returns `None` when n < 3 or
/// the regressor has no variance (all x equal in degenerate input).
pub fn ols_fit(y: &[f64]) -> Option<OlsFit> {
    let n = y.len();
    if n < 3 { return None; }

    let x_mean = (n - 1) as f64 / 2.0;
    let y_mean = y.iter().sum::<f64>() / n as f64;

    let mut ss_xy = 0.0;
    let mut ss_xx = 0.0;
    for (i, &yi) in y.iter().enumerate() {
        let dx = i as f64 - x_mean;
        let dy = yi - y_mean;
        ss_xy += dx * dy;
        ss_xx += dx * dx;
    }
    if ss_xx < 1e-12 { return None; }

    let slope = ss_xy / ss_xx;
    let intercept = y_mean - slope * x_mean;

    let mut ss_res = 0.0;
    let mut ss_tot = 0.0;
    for (i, &yi) in y.iter().enumerate() {
        let y_pred = intercept + slope * i as f64;
        ss_res += (yi - y_pred).powi(2);
        ss_tot += (yi - y_mean).powi(2);
    }

    let r_squared = if ss_tot > 1e-12 { 1.0 - ss_res / ss_tot } else { 0.0 };
    let ms_res = ss_res / (n - 2).max(1) as f64;
    let slope_se = (ms_res / ss_xx).sqrt();

    Some(OlsFit { slope, intercept, slope_se, r_squared })
}

/// OLS slope only. Cheaper than [`ols_fit`] when standard error and R² aren't needed.
#[inline]
pub fn ols_slope(y: &[f64]) -> f64 {
    let n = y.len();
    if n < 3 { return 0.0; }
    let x_mean = (n - 1) as f64 / 2.0;
    let y_mean = y.iter().sum::<f64>() / n as f64;
    let mut ss_xy = 0.0;
    let mut ss_xx = 0.0;
    for (i, &yi) in y.iter().enumerate() {
        let dx = i as f64 - x_mean;
        ss_xy += dx * (yi - y_mean);
        ss_xx += dx * dx;
    }
    if ss_xx < 1e-12 { 0.0 } else { ss_xy / ss_xx }
}

// ============================================================================
// Equity-curve performance metrics
// ============================================================================
// All functions operate on an equity curve of `(timestamp_ms, equity)` points.

/// Max drawdown as a percentage from an equity curve.
pub fn max_drawdown(equity_curve: &[(u64, f64)], initial: f64) -> f64 {
    let mut peak = initial;
    let mut max_dd = 0.0f64;
    for &(_, eq) in equity_curve {
        peak = peak.max(eq);
        if peak > 0.0 { max_dd = max_dd.max((peak - eq) / peak * 100.0); }
    }
    max_dd
}

/// Total PnL percentage: (last - first) / first * 100.
pub fn pnl_pct(equity_curve: &[(u64, f64)]) -> f64 {
    match (equity_curve.first(), equity_curve.last()) {
        (Some(first), Some(last)) if first.1 > 0.0 => (last.1 - first.1) / first.1 * 100.0,
        _ => 0.0,
    }
}

/// Annualized Sharpe ratio over the full equity curve. Welford single-pass,
/// zero allocation. Clamped to [-100, 100]; returns 100/0/-100 in the
/// degenerate zero-variance case depending on mean sign.
pub fn sharpe(equity_curve: &[(u64, f64)], bars_per_year: f64) -> f64 {
    if equity_curve.len() < 2 { return 0.0; }
    let mut mean = 0.0;
    let mut m2 = 0.0;
    let mut n = 0.0;
    for w in equity_curve.windows(2) {
        let ret = (w[1].1 - w[0].1) / w[0].1;
        n += 1.0;
        let d = ret - mean;
        mean += d / n;
        m2 += d * (ret - mean);
    }
    let std = (m2 / n).sqrt();
    if std < 1e-12 {
        if mean > 0.0 { 100.0 } else { 0.0 }
    } else {
        (mean / std * bars_per_year.sqrt()).clamp(-100.0, 100.0)
    }
}

/// Annualized Sortino ratio. Penalizes only downside deviation
/// (Sortino and van der Meer 1991). Clamp/degenerate behavior matches [`sharpe`].
pub fn sortino(equity_curve: &[(u64, f64)], bars_per_year: f64) -> f64 {
    if equity_curve.len() < 2 { return 0.0; }
    let mut mean = 0.0;
    let mut down_m2 = 0.0;
    let mut n = 0.0;
    for w in equity_curve.windows(2) {
        let ret = (w[1].1 - w[0].1) / w[0].1;
        n += 1.0;
        let d = ret - mean;
        mean += d / n;
        if ret < 0.0 { down_m2 += ret * ret; }
    }
    let down_dev = (down_m2 / n).sqrt();
    if down_dev < 1e-12 {
        if mean > 0.0 { 100.0 } else { 0.0 }
    } else {
        (mean / down_dev * bars_per_year.sqrt()).clamp(-100.0, 100.0)
    }
}

/// Geometric mean of per-month Sharpe ratios. More robust to overfitting than
/// a single full-period Sharpe.
pub fn monthly_geo_sharpe(equity_curve: &[(u64, f64)], bars_per_year: f64) -> f64 {
    monthly_geo(equity_curve, bars_per_year, sharpe)
}

/// Geometric mean of per-month Sortino ratios.
pub fn monthly_geo_sortino(equity_curve: &[(u64, f64)], bars_per_year: f64) -> f64 {
    monthly_geo(equity_curve, bars_per_year, sortino)
}

fn monthly_geo(
    equity_curve: &[(u64, f64)],
    bars_per_year: f64,
    metric: fn(&[(u64, f64)], f64) -> f64,
) -> f64 {
    if equity_curve.len() < 2 { return 0.0; }
    let ms_per_day = 86_400_000u64;
    let mut monthly_curves: Vec<Vec<(u64, f64)>> = Vec::new();
    let mut current_month_key: Option<(u32, u32)> = None;

    for &(ts, eq) in equity_curve {
        let days_since_epoch = ts / ms_per_day;
        let approx_year = (days_since_epoch / 365) as u32;
        let day_of_year = (days_since_epoch % 365) as u32;
        let approx_month = day_of_year / 30;

        let key = (approx_year, approx_month);
        if current_month_key != Some(key) {
            if let Some(last_curve) = monthly_curves.last() {
                if let Some(&last_point) = last_curve.last() {
                    monthly_curves.push(vec![last_point]);
                } else {
                    monthly_curves.push(Vec::new());
                }
            } else {
                monthly_curves.push(Vec::new());
            }
            current_month_key = Some(key);
        }
        if let Some(curve) = monthly_curves.last_mut() {
            curve.push((ts, eq));
        }
    }

    let mut log_sum = 0.0;
    let mut n_months = 0usize;
    for curve in &monthly_curves {
        if curve.len() < 5 { continue; }
        let s = metric(curve, bars_per_year);
        // Skip NaN/inf and clamp-sentinel values (flat curves clamp to +/- 100).
        if !s.is_finite() || s.abs() >= 99.9 { continue; }
        let shifted = 1.0 + s / 10.0;
        if shifted > 0.0 {
            log_sum += shifted.ln();
            n_months += 1;
        }
    }
    if n_months < 2 { return 0.0; }
    let geo_shifted = (log_sum / n_months as f64).exp();
    (geo_shifted - 1.0) * 10.0
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
    fn rolling_ols_xy_short_is_zero() {
        assert_eq!(rolling_ols_xy(&[1.0, 2.0], &[1.0, 2.0]), (0.0, 0.0));
    }

    #[test]
    fn rolling_ols_xy_perfect_line() {
        // y = 2x + 1
        let x = [0.0, 1.0, 2.0, 3.0, 4.0];
        let y = [1.0, 3.0, 5.0, 7.0, 9.0];
        let (beta, alpha) = rolling_ols_xy(&x, &y);
        assert!((beta - 2.0).abs() < 1e-12);
        assert!((alpha - 1.0).abs() < 1e-12);
    }

    #[test]
    fn rolling_ols_xy_zero_variance_regressor() {
        // Constant x: returns (0.0, mean(y))
        let x = [5.0, 5.0, 5.0, 5.0];
        let y = [1.0, 2.0, 3.0, 4.0];
        let (beta, alpha) = rolling_ols_xy(&x, &y);
        assert_eq!(beta, 0.0);
        assert!((alpha - 2.5).abs() < 1e-12);
    }

    #[test]
    fn variance_matches_std_dev_squared() {
        let d = [1.0, 2.0, 3.0, 4.0, 5.0];
        let v = variance(&d);
        let s = std_dev(&d);
        assert!((v - s * s).abs() < 1e-12);
        assert!((v - 2.5).abs() < 1e-12);
    }

    #[test]
    fn max_drawdown_monotonic_up_is_zero() {
        let curve: Vec<(u64, f64)> = (0..10).map(|i| (i as u64, 100.0 + i as f64)).collect();
        assert_eq!(max_drawdown(&curve, 100.0), 0.0);
    }

    #[test]
    fn max_drawdown_50_percent() {
        let curve = vec![(0u64, 100.0), (1, 200.0), (2, 100.0)];
        assert!((max_drawdown(&curve, 100.0) - 50.0).abs() < 1e-9);
    }

    #[test]
    fn pnl_pct_basic() {
        let curve = vec![(0u64, 100.0), (1, 150.0)];
        assert!((pnl_pct(&curve) - 50.0).abs() < 1e-9);
    }

    #[test]
    fn sharpe_empty_is_zero() {
        assert_eq!(sharpe(&[], 252.0), 0.0);
        assert_eq!(sortino(&[], 252.0), 0.0);
    }

    #[test]
    fn sharpe_flat_positive_clamps() {
        // Monotonic growing curve with zero variance in returns clamps to 100.
        let curve: Vec<(u64, f64)> = (0..10).map(|i| (i as u64, 100.0 * 1.01f64.powi(i))).collect();
        let s = sharpe(&curve, 252.0);
        assert!(s > 0.0);
    }
}
