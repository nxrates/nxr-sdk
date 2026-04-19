//! Multi-timeframe (MTF) feature engineering from OHLCV bars.
//!
//! 7 orthogonal indicators x 3 scales (short / medium / long) = 21 MTF features,
//! plus 2 cross-scale regime ratios, 5 derived single-scale features, and 1
//! vol-of-vol feature = 29 core features (31 with optional time features).
//!
//! The GBM acts as feature selector: each indicator is provided at multiple
//! scales and importance-based selection discovers which carry alpha. This
//! avoids lookback parameter optimization (which overfits to historical regime
//! durations).
//!
//! Categories:
//!   MOMENTUM:   returns, RSI
//!   VOLATILITY: Parkinson
//!   TREND:      efficiency ratio, ADX, autocorrelation
//!   MEAN REV:   z-score
//!
//! All features are backward-looking from the current bar, no look-ahead bias.

use super::candle::Candle;
use super::indicators::{
    adx, autocorrelation, efficiency_ratio, hurst_rs, lin_reg_slope, mean_slice,
    parkinson_vol, rsi, zscore,
};

/// Default short-scale lookback (bars) when `horizon_bars == 0`.
pub const DEFAULT_S: usize = 20;
/// Default medium-scale lookback (bars) when `horizon_bars == 0`.
pub const DEFAULT_M: usize = 60;
/// Default long-scale lookback (bars) when `horizon_bars == 0`.
pub const DEFAULT_L: usize = 200;

/// Number of core features (without optional time features).
pub const N_CORE_FEATURES: usize = 29;
/// Number of features including optional time encoding (sin/cos of TOD).
pub const N_FEATURES_WITH_TIME: usize = 31;
/// Minimum bars required before features are valid. Matches longest lookback
/// plus margin.
pub const MIN_BARS: usize = 500;

/// Derive feature lookbacks `(S, M, L)` from a prediction horizon in bars.
///
/// Ratio 1:3:10, literature-aligned (Lopez de Prado, Kakushadze):
///   S = 1x horizon  (immediate momentum matching prediction window)
///   M = 3x horizon  (intraday trend context)
///   L = 10x horizon (regime detection; Hurst needs >= 100 samples)
#[inline]
pub fn compute_lookbacks(horizon_bars: usize) -> (usize, usize, usize) {
    let h = horizon_bars.max(5);
    (h, h * 3, h * 10)
}

/// Ordered names for the 29 (or 31) features emitted by [`compute_all_features`].
pub fn feature_names(with_time_features: bool) -> Vec<String> {
    let mut names = vec![
        // MTF MOMENTUM: returns (3)
        "ret_s".into(), "ret_m".into(), "ret_l".into(),
        // MTF MOMENTUM: RSI (3)
        "rsi_s".into(), "rsi_m".into(), "rsi_l".into(),
        // MTF VOLATILITY: Parkinson (3)
        "pvol_s".into(), "pvol_m".into(), "pvol_l".into(),
        // MTF TREND: efficiency ratio (3)
        "er_s".into(), "er_m".into(), "er_l".into(),
        // MTF TREND: ADX (3)
        "adx_s".into(), "adx_m".into(), "adx_l".into(),
        // MTF TREND: autocorrelation (3)
        "autocorr_s".into(), "autocorr_m".into(), "autocorr_l".into(),
        // MTF MEAN REVERSION: z-score (3)
        "zscore_s".into(), "zscore_m".into(), "zscore_l".into(),
        // CROSS-SCALE REGIME (2)
        "vol_regime".into(),
        "er_regime".into(),
        // DERIVED SINGLE-SCALE (5)
        "hurst".into(),
        "ema_cross".into(),
        "price_accel".into(),
        "obv_slope".into(),
        "clv".into(),
        // VOL-OF-VOL REGIME (1)
        "vol_of_vol".into(),
    ];
    if with_time_features {
        names.push("time_sin".into());
        names.push("time_cos".into());
    }
    names
}

/// Compute all features for the full bar series in a single pass.
///
/// Arguments:
///   * `candles`: bar series (ordered ascending by ts).
///   * `with_time_features`: append `time_sin` and `time_cos` (seconds-of-day).
///   * `horizon_bars`: sets MTF scales `S = 1x, M = 3x, L = 10x`. Pass 0 for
///     defaults `(20, 60, 200)`.
///
/// Returns `(timestamps, features, n_cols)` where `features` is row-major
/// `features[row * n_cols + col]`. Rows before `L` (the long lookback) are
/// filled with NaN.
pub fn compute_all_features(
    candles: &[Candle],
    with_time_features: bool,
    horizon_bars: usize,
) -> (Vec<u64>, Vec<f64>, usize) {
    let (s, m, l) = if horizon_bars > 0 {
        compute_lookbacks(horizon_bars)
    } else {
        (DEFAULT_S, DEFAULT_M, DEFAULT_L)
    };
    let n = candles.len();

    let mut closes = Vec::with_capacity(n);
    let mut highs = Vec::with_capacity(n);
    let mut lows = Vec::with_capacity(n);
    let mut volumes = Vec::with_capacity(n);
    let mut timestamps = Vec::with_capacity(n);
    for c in candles {
        closes.push(c.c);
        highs.push(c.h);
        lows.push(c.l);
        volumes.push(c.v);
        timestamps.push(c.ts);
    }

    let mut rets = vec![0.0f64; n];
    let mut obv = vec![0.0f64; n];
    let mut ema_s = vec![0.0f64; n];
    let mut ema_m = vec![0.0f64; n];

    let alpha_s = 2.0 / (s as f64 + 1.0);
    let alpha_m = 2.0 / (m as f64 + 1.0);

    if n > 0 {
        ema_s[0] = closes[0];
        ema_m[0] = closes[0];
    }
    for i in 1..n {
        if closes[i - 1] > 0.0 { rets[i] = closes[i] / closes[i - 1] - 1.0; }
        obv[i] = obv[i - 1]
            + if rets[i] > 0.0 { volumes[i] }
              else if rets[i] < 0.0 { -volumes[i] }
              else { 0.0 };
        ema_s[i] = alpha_s * closes[i] + (1.0 - alpha_s) * ema_s[i - 1];
        ema_m[i] = alpha_m * closes[i] + (1.0 - alpha_m) * ema_m[i - 1];
    }

    let nf = if with_time_features { N_FEATURES_WITH_TIME } else { N_CORE_FEATURES };
    let mut all_features: Vec<f64> = vec![f64::NAN; n * nf];

    for i in l..n {
        let base = i * nf;
        let mut col = 0;

        macro_rules! f {
            ($val:expr) => { all_features[base + col] = $val; col += 1; }
        }

        // MTF Returns (3)
        f!(closes[i] / closes[i - s] - 1.0);
        f!(closes[i] / closes[i - m] - 1.0);
        f!(closes[i] / closes[i - l] - 1.0);

        // MTF RSI (3)
        f!(rsi(&rets, i, s));
        f!(rsi(&rets, i, m));
        f!(rsi(&rets, i, l));

        // MTF Parkinson Volatility (3)
        let pv_s = parkinson_vol(&highs, &lows, i, s);
        let pv_m = parkinson_vol(&highs, &lows, i, m);
        let pv_l = parkinson_vol(&highs, &lows, i, l);
        f!(pv_s); f!(pv_m); f!(pv_l);

        // MTF Efficiency Ratio (3)
        let er_s = efficiency_ratio(&closes, i, s);
        let er_m = efficiency_ratio(&closes, i, m);
        let er_l = efficiency_ratio(&closes, i, l);
        f!(er_s); f!(er_m); f!(er_l);

        // MTF ADX (3)
        f!(adx(&highs, &lows, &closes, i, s));
        f!(adx(&highs, &lows, &closes, i, m));
        f!(adx(&highs, &lows, &closes, i, l));

        // MTF Autocorrelation (3)
        f!(autocorrelation(&rets, i, s));
        f!(autocorrelation(&rets, i, m));
        f!(autocorrelation(&rets, i, l));

        // MTF Z-score (3)
        f!(zscore(&closes, i, s));
        f!(zscore(&closes, i, m));
        f!(zscore(&closes, i, l));

        // Cross-scale regime ratios (2)
        f!(if pv_l > 1e-12 { pv_s / pv_l } else { 1.0 });
        f!(if er_l > 1e-6 { er_s / er_l } else { 1.0 });

        // Derived single-scale (5)
        f!(hurst_rs(&rets, i, l));
        f!(if ema_m[i] > 1e-10 { ema_s[i] / ema_m[i] - 1.0 } else { 0.0 });
        let ret_now = closes[i] / closes[i - s] - 1.0;
        let ret_prev = closes[i - s] / closes[i - 2 * s] - 1.0;
        f!(ret_now - ret_prev);
        let obv_sl = lin_reg_slope(&obv, i, s);
        let avg_vol = mean_slice(&volumes, i, s);
        f!(if avg_vol > 1e-10 { obv_sl / avg_vol } else { 0.0 });
        let bar_range = highs[i] - lows[i];
        f!(if bar_range > 1e-10 {
            (2.0 * closes[i] - highs[i] - lows[i]) / bar_range
        } else {
            0.0
        });

        // Vol-of-vol (1)
        f!({
            let start = (i + 1).saturating_sub(m);
            let mut sum = 0.0;
            let mut sum2 = 0.0;
            let mut cnt = 0.0;
            for j in start..=i {
                let hl = (highs[j] / lows[j].max(1e-18)).ln().abs() / 1.6651;
                sum += hl;
                sum2 += hl * hl;
                cnt += 1.0;
            }
            if cnt > 1.0 {
                let mean_v = sum / cnt;
                let var_v = (sum2 / cnt - mean_v * mean_v).max(0.0);
                if mean_v > 1e-12 { (var_v.sqrt() / mean_v).clamp(0.0, 4.0) } else { 0.0 }
            } else {
                0.0
            }
        });

        // Optional time features
        if with_time_features {
            let secs = (timestamps[i] / 1000) % 86400;
            let frac = secs as f64 / 86400.0;
            f!((2.0 * std::f64::consts::PI * frac).sin());
            f!((2.0 * std::f64::consts::PI * frac).cos());
        }

        debug_assert_eq!(col, nf);
    }

    (timestamps, all_features, nf)
}
