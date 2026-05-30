//! Statistical primitives: mean, variance, std, OLS regression, and equity-curve
//! performance metrics (Sharpe/Sortino/drawdown). Single canonical home for all
//! stats used by aggregation (TDWAP CI), ML features, backtest fitness, and
//! live monitoring.
//!
//! Variance convention: `variance` / `std_dev` use the sample (Bessel-corrected,
//! n-1) estimator because that is what the historical ML callsites used. For
//! population variance, divide sum-of-squared-deviations by n at the callsite.

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

/// Coefficient of variation: `std(data) / |mean(data)|`. Population variance
/// (divisor n, not n-1) to match the cross-fold stability convention used in
/// Renko stats HOMO/ROBUST components.
///
/// Returns 0.0 for n < 2, and 1.0 when `|mean| < 1e-10` (degenerate signal).
pub fn cv(data: &[f64]) -> f64 {
    if data.len() < 2 { return 0.0; }
    let n = data.len() as f64;
    let m = data.iter().sum::<f64>() / n;
    if m.abs() < 1e-10 { return 1.0; }
    let std = (data.iter().map(|&v| (v - m).powi(2)).sum::<f64>() / n).sqrt();
    std / m.abs()
}

/// Lag-k sample autocorrelation function (ACF). Population variance
/// normalisation (divisor n, not n-1). Returns 0.0 when `lag >= series.len()`,
/// length < 2, or zero-variance input. Callers needing strict [-1, 1] should
/// clamp the output.
pub fn acf(series: &[f64], lag: usize) -> f64 {
    if lag >= series.len() || series.len() < 2 { return 0.0; }
    let n = series.len() - lag;
    let m = series[..n].iter().sum::<f64>() / n as f64;
    let mut num = 0.0;
    let mut den = 0.0;
    for i in 0..n {
        let d = series[i] - m;
        num += d * (series[i + lag] - m);
        den += d * d;
    }
    if den > 1e-10 { num / den } else { 0.0 }
}

/// Rolling Parkinson volatility estimator over aligned high/low slices.
///
/// Formula: `sqrt(mean(ln(H/L)^2) / (4 ln 2))`, valid for continuous GBM.
/// Skips bars where `low <= 0` (treated as invalid). Returns 0.0 when no
/// valid bars are found. Arrays must be the same length.
///
/// For a single-bar estimator see [`crate::parkinson_sigma`].
pub fn parkinson_vol(highs: &[f64], lows: &[f64]) -> f64 {
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
    if count == 0 { return 0.0; }
    (sum / count as f64 / (4.0 * std::f64::consts::LN_2)).sqrt()
}

/// Hurst exponent via rescaled range (R/S) analysis on a return series.
///
/// H > 0.5 → trending; H < 0.5 → mean-reverting; H ~ 0.5 → random walk.
/// Returns 0.5 (random walk fallback) when the input is shorter than 20 or
/// R/S cannot be computed across at least 2 window sizes. Clamped to [0, 1].
pub fn hurst_rs(rets: &[f64]) -> f64 {
    if rets.len() < 20 { return 0.5; }

    let mut log_ns = [0.0f64; 5];
    let mut log_rs = [0.0f64; 5];
    let mut count = 0usize;

    let mut sz = 10usize;
    while sz <= rets.len() / 2 && count < 5 {
        let n_chunks = rets.len() / sz;
        if n_chunks == 0 { break; }
        let mut rs_sum = 0.0;
        let mut rs_count = 0u32;
        for c in 0..n_chunks {
            let chunk = &rets[c * sz..(c + 1) * sz];
            let m = chunk.iter().sum::<f64>() / sz as f64;
            let mut cum = 0.0;
            let mut max_cum = f64::NEG_INFINITY;
            let mut min_cum = f64::INFINITY;
            let mut var_sum = 0.0;
            for &x in chunk {
                let d = x - m;
                cum += d;
                max_cum = max_cum.max(cum);
                min_cum = min_cum.min(cum);
                var_sum += d * d;
            }
            let s = (var_sum / sz as f64).sqrt();
            if s > 1e-15 {
                rs_sum += (max_cum - min_cum) / s;
                rs_count += 1;
            }
        }
        if rs_count > 0 {
            log_ns[count] = (sz as f64).ln();
            log_rs[count] = (rs_sum / rs_count as f64).ln();
            count += 1;
        }
        sz *= 2;
    }

    if count < 2 { return 0.5; }
    let n = count as f64;
    let sx: f64 = log_ns[..count].iter().sum();
    let sy: f64 = log_rs[..count].iter().sum();
    let sxy: f64 = log_ns[..count].iter().zip(&log_rs[..count]).map(|(x, y)| x * y).sum();
    let sxx: f64 = log_ns[..count].iter().map(|x| x * x).sum();
    let denom = n * sxx - sx * sx;
    if denom.abs() < 1e-15 { return 0.5; }
    ((n * sxy - sx * sy) / denom).clamp(0.0, 1.0)
}

/// Sum of squared residuals for simple OLS: `y = a + b*x`. Cheap diagnostic
/// for comparing candidate regressors (e.g. F-style nested-model tests).
pub fn ols_ssr(x: &[f64], y: &[f64]) -> f64 {
    let (b, a) = ols_xy(x, y);
    x.iter().zip(y.iter()).map(|(&xi, &yi)| {
        let r = yi - a - b * xi;
        r * r
    }).sum()
}

/// SSR for 2-regressor OLS: `y = a + b1*x1 + b2*x2`. Returns `f64::MAX` when
/// n < 4 and falls back to [`ols_ssr`] on `(x1, y)` when the 2x2 normal
/// equations are singular (perfectly collinear regressors).
pub fn ols_ssr_2(x1: &[f64], x2: &[f64], y: &[f64]) -> f64 {
    let n = y.len() as f64;
    if n < 4.0 { return f64::MAX; }

    let m_y = y.iter().sum::<f64>() / n;
    let m_1 = x1.iter().sum::<f64>() / n;
    let m_2 = x2.iter().sum::<f64>() / n;

    let mut s11 = 0.0; let mut s12 = 0.0; let mut s22 = 0.0;
    let mut s1y = 0.0; let mut s2y = 0.0;
    for i in 0..y.len() {
        let d1 = x1[i] - m_1;
        let d2 = x2[i] - m_2;
        let dy = y[i] - m_y;
        s11 += d1 * d1;
        s12 += d1 * d2;
        s22 += d2 * d2;
        s1y += d1 * dy;
        s2y += d2 * dy;
    }

    let det = s11 * s22 - s12 * s12;
    if det.abs() < 1e-15 { return ols_ssr(x1, y); }

    let b1 = (s22 * s1y - s12 * s2y) / det;
    let b2 = (s11 * s2y - s12 * s1y) / det;
    let a = m_y - b1 * m_1 - b2 * m_2;

    x1.iter().zip(x2.iter()).zip(y.iter()).map(|((&x1i, &x2i), &yi)| {
        let r = yi - a - b1 * x1i - b2 * x2i;
        r * r
    }).sum()
}

/// Conditional Value at Risk (Expected Shortfall) at the given confidence level
/// plus the population standard deviation of the same returns slice.
///
/// `level` is a confidence level in (0, 1); the tail consists of the worst
/// `(1 - level) * n` returns. Returns `None` for an empty input. CVaR is
/// returned as the mean of the tail (negative for losses); vol is population
/// std.
pub fn cvar_and_vol(returns: &[f64], level: f64) -> Option<(f64, f64)> {
    if returns.is_empty() { return None; }

    let mut sorted = returns.to_vec();
    sorted.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));

    let n = sorted.len();
    let var_idx = ((1.0 - level) * n as f64).ceil().min(n as f64 - 1.0) as usize;

    let cvar = if var_idx < n {
        let tail = &sorted[..=var_idx];
        tail.iter().sum::<f64>() / tail.len() as f64
    } else { 0.0 };

    let m = returns.iter().sum::<f64>() / n as f64;
    let vol = (returns.iter().map(|r| (r - m).powi(2)).sum::<f64>() / n as f64).sqrt();

    Some((cvar, vol))
}

/// Simple 2-variable OLS: fit `y = alpha + beta * x`. Returns `(beta, alpha)`.
/// Returns `(0.0, 0.0)` when n < 3; `(0.0, mean(y))` when the regressor has
/// zero variance.
#[inline]
pub fn ols_xy(x: &[f64], y: &[f64]) -> (f64, f64) {
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

/// Annualized Sharpe ratio over the full equity curve. Welford single-pass,
/// zero allocation. Clamped to [-100, 100]; returns 100/0/-100 in the
/// degenerate zero-variance case depending on mean sign. Only exposed to
/// power the per-month geometric aggregator below.
fn sharpe(equity_curve: &[(u64, f64)], bars_per_year: f64) -> f64 {
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

/// Annualized Sortino ratio, downside-deviation variant of [`sharpe`].
fn sortino(equity_curve: &[(u64, f64)], bars_per_year: f64) -> f64 {
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
    let ms_per_day = crate::shard::MS_PER_DAY as u64;
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
    fn ols_xy_short_is_zero() {
        assert_eq!(ols_xy(&[1.0, 2.0], &[1.0, 2.0]), (0.0, 0.0));
    }

    #[test]
    fn ols_xy_perfect_line() {
        // y = 2x + 1
        let x = [0.0, 1.0, 2.0, 3.0, 4.0];
        let y = [1.0, 3.0, 5.0, 7.0, 9.0];
        let (beta, alpha) = ols_xy(&x, &y);
        assert!((beta - 2.0).abs() < 1e-12);
        assert!((alpha - 1.0).abs() < 1e-12);
    }

    #[test]
    fn ols_xy_zero_variance_regressor() {
        // Constant x: returns (0.0, mean(y))
        let x = [5.0, 5.0, 5.0, 5.0];
        let y = [1.0, 2.0, 3.0, 4.0];
        let (beta, alpha) = ols_xy(&x, &y);
        assert_eq!(beta, 0.0);
        assert!((alpha - 2.5).abs() < 1e-12);
    }

    #[test]
    fn std_dev_matches_sample_variance() {
        let d = [1.0, 2.0, 3.0, 4.0, 5.0];
        let s = std_dev(&d);
        assert!((s * s - 2.5).abs() < 1e-12);
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
    fn monthly_geo_empty_is_zero() {
        assert_eq!(monthly_geo_sharpe(&[], 252.0), 0.0);
        assert_eq!(monthly_geo_sortino(&[], 252.0), 0.0);
    }

    #[test]
    fn median_odd_and_even() {
        assert_eq!(median(&[3.0, 1.0, 2.0]), 2.0);
        assert_eq!(median(&[4.0, 1.0, 3.0, 2.0]), 2.5);
        assert_eq!(median(&[]), 0.0);
    }

    #[test]
    fn cv_zero_mean_returns_one() {
        assert_eq!(cv(&[0.0, 0.0, 0.0]), 1.0);
        assert_eq!(cv(&[1.0]), 0.0);
    }

    #[test]
    fn acf_perfect_repeat() {
        let s = [1.0, -1.0, 1.0, -1.0, 1.0, -1.0, 1.0, -1.0];
        assert!((acf(&s, 2) - 1.0).abs() < 1e-9);
        assert!((acf(&s, 1) + 1.0).abs() < 1e-9);
    }

    #[test]
    fn ols_ssr_perfect_fit_is_zero() {
        let x = [0.0, 1.0, 2.0, 3.0];
        let y = [1.0, 3.0, 5.0, 7.0];
        assert!(ols_ssr(&x, &y) < 1e-20);
    }

    #[test]
    fn cvar_basic() {
        let returns = vec![-0.10, -0.08, -0.05, -0.02, 0.01, 0.03, 0.05, 0.07, 0.09, 0.11];
        let (c, v) = cvar_and_vol(&returns, 0.95).unwrap();
        assert!(c < 0.0);
        assert!(v > 0.0);
    }
}
