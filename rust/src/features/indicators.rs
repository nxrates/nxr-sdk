//! Pure indicator functions over pre-computed arrays.
//!
//! All indicators are backward-looking over `[i+1-window..=i]`, no look-ahead.
//! Callers precompute returns / true-range / OBV / EMAs once and pass slices
//! into each indicator, so total work is O(n) per indicator.

use crate::stats::{mean as slice_mean, ols_slope, std_dev};

/// Mean of `data[i+1-window..=i]`.
#[inline]
pub fn mean_slice(data: &[f64], i: usize, window: usize) -> f64 {
    let start = (i + 1).saturating_sub(window);
    slice_mean(&data[start..=i])
}

/// SMA and sample standard deviation over `data[i+1-window..=i]`.
/// Falls back to `(data[i], 0.0)` when n < 2.
#[inline]
pub fn sma_std(data: &[f64], i: usize, window: usize) -> (f64, f64) {
    let start = (i + 1).saturating_sub(window);
    let w = &data[start..=i];
    if w.len() < 2 { return (data[i], 0.0); }
    (slice_mean(w), std_dev(w))
}

/// Parkinson volatility estimator using high-low range.
///
/// Formula: `sqrt(mean(ln(H/L)^2) / (4 ln 2))`, valid for continuous GBM.
#[inline]
pub fn parkinson_vol(highs: &[f64], lows: &[f64], i: usize, window: usize) -> f64 {
    let start = i.saturating_sub(window);
    let mut sum = 0.0;
    let mut count = 0u32;
    for j in start..=i {
        if lows[j] > 0.0 {
            let log_hl = (highs[j] / lows[j]).ln();
            sum += log_hl * log_hl;
            count += 1;
        }
    }
    if count > 0 {
        (sum / count as f64 / (4.0 * 2.0_f64.ln())).sqrt()
    } else {
        0.0
    }
}

/// Kaufman efficiency ratio: `|net move| / sum(|bar-to-bar moves|)`.
/// Range [0, 1]. 0 = fully choppy, 1 = straight line.
#[inline]
pub fn efficiency_ratio(closes: &[f64], i: usize, window: usize) -> f64 {
    let start = i.saturating_sub(window);
    let direction = (closes[i] - closes[start]).abs();
    let mut volatility = 0.0;
    for j in (start + 1)..=i {
        volatility += (closes[j] - closes[j - 1]).abs();
    }
    if volatility > 1e-12 { direction / volatility } else { 0.0 }
}

/// Average Directional Index (ADX): trend strength in [0, 1].
#[inline]
pub fn adx(highs: &[f64], lows: &[f64], closes: &[f64], i: usize, period: usize) -> f64 {
    let start = (i + 1).saturating_sub(period);
    if start == 0 { return 0.0; }
    let mut plus_dm = 0.0;
    let mut minus_dm = 0.0;
    let mut tr_sum = 0.0;
    for j in start..=i {
        if j == 0 { continue; }
        let up = highs[j] - highs[j - 1];
        let down = lows[j - 1] - lows[j];
        if up > down && up > 0.0 { plus_dm += up; }
        if down > up && down > 0.0 { minus_dm += down; }
        let hl = highs[j] - lows[j];
        let hc = (highs[j] - closes[j - 1]).abs();
        let lc = (lows[j] - closes[j - 1]).abs();
        tr_sum += hl.max(hc).max(lc);
    }
    if tr_sum < 1e-12 { return 0.0; }
    let di_plus = plus_dm / tr_sum;
    let di_minus = minus_dm / tr_sum;
    let di_sum = di_plus + di_minus;
    if di_sum < 1e-12 { return 0.0; }
    ((di_plus - di_minus).abs() / di_sum).clamp(0.0, 1.0)
}

/// Lag-1 autocorrelation of a precomputed return series over a window.
#[inline]
pub fn autocorrelation(rets: &[f64], i: usize, window: usize) -> f64 {
    let start = (i + 1).saturating_sub(window);
    let w = &rets[start..=i];
    if w.len() < 4 { return 0.0; }
    let n = w.len() as f64;
    let mean = w.iter().sum::<f64>() / n;
    let var: f64 = w.iter().map(|r| (r - mean).powi(2)).sum();
    if var < 1e-20 { return 0.0; }
    let mut cov = 0.0;
    for j in 1..w.len() {
        cov += (w[j] - mean) * (w[j - 1] - mean);
    }
    (cov / var).clamp(-1.0, 1.0)
}

/// Z-score of the current close against its SMA and std over `window`.
/// Clamped to [-4, 4] to keep GBM splits numerically stable.
#[inline]
pub fn zscore(closes: &[f64], i: usize, window: usize) -> f64 {
    let (mean, std) = sma_std(closes, i, window);
    if std > 1e-12 { ((closes[i] - mean) / std).clamp(-4.0, 4.0) } else { 0.0 }
}

/// RSI (Relative Strength Index), normalized to [0, 1] (not the usual 0-100).
#[inline]
pub fn rsi(rets: &[f64], i: usize, period: usize) -> f64 {
    let start = (i + 1).saturating_sub(period);
    let mut gain_sum = 0.0;
    let mut loss_sum = 0.0;
    for r in &rets[start..=i] {
        if *r > 0.0 { gain_sum += *r; }
        else { loss_sum += r.abs(); }
    }
    let n = (i + 1 - start) as f64;
    if n == 0.0 { return 0.5; }
    let avg_gain = gain_sum / n;
    let avg_loss = loss_sum / n;
    if avg_loss < 1e-15 { return 1.0; }
    let rs = avg_gain / avg_loss;
    1.0 - 1.0 / (1.0 + rs)
}

/// Hurst exponent via rescaled range (R/S) method on precomputed returns.
/// H > 0.5 trending, H < 0.5 mean-reverting, H ~ 0.5 random walk.
/// Clamped to [0, 1].
pub fn hurst_rs(rets: &[f64], i: usize, window: usize) -> f64 {
    let start = (i + 1).saturating_sub(window);
    let data = &rets[start..=i];
    if data.len() < 20 { return 0.5; }

    let mut log_ns = [0.0f64; 5];
    let mut log_rs = [0.0f64; 5];
    let mut count = 0usize;

    let mut sz = 10usize;
    while sz <= data.len() / 2 && count < 5 {
        let n_chunks = data.len() / sz;
        if n_chunks == 0 { break; }
        let mut rs_sum = 0.0;
        let mut rs_count = 0u32;
        for c in 0..n_chunks {
            let chunk = &data[c * sz..(c + 1) * sz];
            let mean = chunk.iter().sum::<f64>() / sz as f64;
            let mut cum = 0.0;
            let mut max_cum = f64::NEG_INFINITY;
            let mut min_cum = f64::INFINITY;
            let mut var_sum = 0.0;
            for &x in chunk {
                let d = x - mean;
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

/// Linear-regression slope over `data[i+1-window..=i]`. Thin window wrapper
/// over [`crate::stats::ols_slope`].
#[inline]
pub fn lin_reg_slope(data: &[f64], i: usize, window: usize) -> f64 {
    let start = (i + 1).saturating_sub(window);
    ols_slope(&data[start..=i])
}
