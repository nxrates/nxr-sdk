//! Multi-timeframe volatility blend over Rogers-Satchell s10-OHLC bins.
//!
//! Canonical per-bin σ = Rogers-Satchell over s10-resampled 30-min OHLC
//! (see [`crate::vol_estimator`]). This module owns the EMA(28) smoothing +
//! the inverse-variance-weighted winsorized MTF blend across configurable
//! lookback windows. Source data (per-bin sigma_pct) is provided by anything
//! that implements [`VolSource`]: a memory-mapped .vol file (backtest), an
//! in-memory ring buffer (real-time), or a test fixture.
//!
//! The Renko engine itself does not depend on `VolSource`: callers resolve σ
//! via `MtfVolCalculator` then pass the number into
//! `RenkoGenerator::feed_tick_with_sigma`. This trait remains the input
//! contract for the calculator only.

use serde::{Deserialize, Serialize};

use crate::vol_estimator::rs_sigma_from_ohlc;

/// Abstract source of per-bin sigma values.
///
/// Bins are typically 30 minutes wide. Implementors provide O(1) length and
/// O(1) sigma lookup, plus binary-search-style timestamp-to-index mapping.
pub trait VolSource {
    /// Number of bins stored.
    fn len(&self) -> usize;

    /// `sigma_pct` for bin at index `i`. Returns 0.0 if out of range.
    fn sigma_pct(&self, i: usize) -> f64;

    /// Bin index for the given MITCH mts (u48 ticks since 2010). Clamps to
    /// `len().saturating_sub(1)` on overshoot.
    fn find_index_for_mts(&self, mts: u64) -> usize;
}

/// Volatility calculation config (typically from pipeline.yml `vol` section).
///
/// `sigma_blend_windows_days` controls the 2-layer MTF σ blend used at brick-
/// size compute time. The outer k-fit MTF lives on `CalibrationConfig` as
/// `k_fit_windows_days`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VolConfig {
    pub ema_period: usize,
    /// Per-window lookback (in days) used to blend the σ estimate. Inverse-
    /// variance weighted across windows.
    pub sigma_blend_windows_days: Vec<usize>,
    pub winsorize_pct: [f64; 2],
    pub winsorize_min_samples: usize,
    /// Minimum interval (ms) between brick-size recalibration recomputes. The
    /// canonical 12-hour cooldown: the offline `.vol` → `.k` recalibration is
    /// re-run at most every 12h; the 30-min vol bin is unchanged.
    #[serde(default = "default_recompute_cooldown_ms")]
    pub recompute_cooldown_ms: i64,
}

/// Canonical recalibration cooldown: 12 hours in ms.
pub const DEFAULT_RECOMPUTE_COOLDOWN_MS: i64 = 43_200_000;

fn default_recompute_cooldown_ms() -> i64 {
    DEFAULT_RECOMPUTE_COOLDOWN_MS
}

impl Default for VolConfig {
    fn default() -> Self {
        Self {
            ema_period: 28,
            sigma_blend_windows_days: vec![14, 60, 180],
            winsorize_pct: [0.05, 0.95],
            winsorize_min_samples: 5,
            recompute_cooldown_ms: DEFAULT_RECOMPUTE_COOLDOWN_MS,
        }
    }
}

/// Multi-timeframe sigma blender.
///
/// Reads 30-min Rogers-Satchell sigma from a `VolSource`, then blends across
/// configurable lookback windows using inverse-variance weighting and
/// winsorized mean for robustness.
pub struct MtfVolCalculator<'a, S: VolSource + ?Sized> {
    source: &'a S,
    config: VolConfig,
    buf: Vec<f64>,
}

impl<'a, S: VolSource + ?Sized> MtfVolCalculator<'a, S> {
    pub fn new(source: &'a S, config: VolConfig) -> Self {
        Self { source, config, buf: Vec::new() }
    }

    /// Precompute sigma for every bin into a flat cache for O(1) lookup.
    /// Use this once, then pass the result to hot loops (e.g. calibration).
    pub fn precompute_sigma_cache(&mut self) -> Vec<f64> {
        let n = self.source.len();
        let mut cache = Vec::with_capacity(n);
        for i in 0..n {
            cache.push(self.compute_sigma(i));
        }
        cache
    }

    /// Delegate to the source.
    #[inline]
    pub fn find_index_for_mts(&self, mts: u64) -> usize {
        self.source.find_index_for_mts(mts)
    }

    /// Blended sigma at bin `hour_idx`.
    ///
    /// Returns the inverse-variance weighted blend of winsorized means
    /// across all configured lookback windows. Falls back to the bin's
    /// raw sigma if no window yields a valid sample.
    pub fn compute_sigma(&mut self, hour_idx: usize) -> f64 {
        let n = self.source.len();
        if hour_idx >= n {
            return 0.01;
        }

        let mut weighted_sum = 0.0;
        let mut weight_sum = 0.0;

        for &lookback_days in &self.config.sigma_blend_windows_days {
            let lookback_periods = lookback_days * 48;
            let start_idx = hour_idx.saturating_sub(lookback_periods);
            if start_idx >= hour_idx || hour_idx - start_idx < 48 {
                continue;
            }

            self.buf.clear();
            self.buf.reserve(hour_idx - start_idx + 1);
            for i in start_idx..=hour_idx {
                self.buf.push(self.source.sigma_pct(i));
            }

            let min_samples = self.config.winsorize_min_samples;
            let [lo_pct, hi_pct] = self.config.winsorize_pct;
            let (wmean, variance) =
                winsorized_mean_and_var_inplace(&mut self.buf, min_samples, lo_pct, hi_pct);
            if variance <= 0.0 || wmean <= 0.0 {
                continue;
            }

            let inv_var = 1.0 / variance;
            weighted_sum += inv_var * wmean;
            weight_sum += inv_var;
        }

        if weight_sum > 0.0 {
            weighted_sum / weight_sum
        } else {
            self.source.sigma_pct(hour_idx).max(0.01)
        }
    }
}

/// 30 minutes in milliseconds — the canonical vol bin width.
const VOL_BIN_MS: i64 = 1_800_000;

/// Online, in-process [`VolSource`] for the LIVE renko producer.
///
/// This is the real-time twin of the offline `.vol` file ([`VolMmap`] in
/// series-factory): it stores the SAME EMA-smoothed 30-min Rogers-Satchell
/// `sigma_pct` rows the offline `vol_builder` writes, so the LIVE renko brick
/// size — computed via [`MtfVolCalculator::compute_sigma`] over this
/// ring — is byte-identical to the offline brick size at the history↔live
/// seam (given identical [`VolConfig`] and trailing data).
///
/// ## Causality / no look-ahead
///
/// Bins are appended ONLY when a 30-min boundary closes (the bin's own window
/// is fully in the past). `compute_sigma(i)` reads only bins `≤ i`. No future
/// data ever enters a published sigma.
///
/// ## Construction
///
/// Feed CLOSED s10 bars' O/H/L/C via [`Self::observe`]; the ring rolls them up
/// into the current open 30-min OHLC bin (O = first s10.open, H = max s10.high,
/// L = min s10.low, C = last s10.close) and, when a bar crosses into a new
/// 30-min bucket, finalizes the closed bin into one EMA-smoothed
/// Rogers-Satchell row (matching `vol_builder::build_vol_from_s10` exactly).
/// Prime the trailing history on open via [`Self::prime_from_slice`] (e.g. from
/// the persistent `.vol` tail) so the first live bin's blended σ equals the
/// blend the backfill used for the boundary bin.
///
/// ## Bounded memory
///
/// Capacity is `cap` bins (caller sizes it to `max(blend window days)*48`).
/// Oldest bins are evicted on overflow — but `compute_sigma`'s longest window
/// only reaches back `max_window_days*48` bins, so eviction never truncates a
/// live blend.
#[derive(Debug, Clone)]
pub struct LiveVolRing {
    /// EMA-smoothed Rogers-Satchell sigma per closed 30-min bin (oldest..newest).
    sigmas: std::collections::VecDeque<f64>,
    /// 30-min-aligned epoch_ms of each stored bin (parallel to `sigmas`).
    bin_starts: std::collections::VecDeque<i64>,
    cap: usize,
    ema_period: usize,
    /// EMA state carried across bins (the smoothed value of the last bin).
    prev_ema: Option<f64>,
    /// Count of bins ever finalized (drives the expanding-mean EMA seed,
    /// identical to `vol_builder`'s `i < ema_period` branch).
    finalized_count: usize,
    /// Currently open bin: `(bin_start_ms, open, high, low, close)`. `None`
    /// until the first observed s10 bar. O = first s10.open, H/L rolling
    /// max/min, C = latest s10.close.
    open_bin: Option<(i64, f64, f64, f64, f64)>,
}

impl LiveVolRing {
    /// New empty ring. `cap` = max stored bins; `ema_period` from `VolConfig`.
    pub fn new(cap: usize, ema_period: usize) -> Self {
        Self {
            sigmas: std::collections::VecDeque::with_capacity(cap.min(1 << 20)),
            bin_starts: std::collections::VecDeque::with_capacity(cap.min(1 << 20)),
            cap: cap.max(1),
            ema_period: ema_period.max(1),
            prev_ema: None,
            finalized_count: 0,
            open_bin: None,
        }
    }

    /// 30-min-aligned bin start for an epoch_ms timestamp.
    #[inline]
    fn bin_start(ts_ms: i64) -> i64 {
        (ts_ms / VOL_BIN_MS) * VOL_BIN_MS
    }

    /// Number of finalized (closed) bins currently retained.
    #[inline]
    pub fn len(&self) -> usize {
        self.sigmas.len()
    }

    #[inline]
    pub fn is_empty(&self) -> bool {
        self.sigmas.is_empty()
    }

    /// Prime the trailing ring from a slice of `(bin_start_ms, ema_sigma_pct)`
    /// rows (newest LAST), e.g. the tail of the persistent `.vol` file.
    ///
    /// Rows are assumed ALREADY EMA-smoothed (as written by `vol_builder`), so
    /// they are inserted verbatim and the live EMA carry (`prev_ema`) is set to
    /// the last row — the first live-finalized bin therefore continues the
    /// offline EMA exactly. Truncates to `cap` (keeps the newest).
    pub fn prime_from_slice(&mut self, rows: &[(i64, f64)]) {
        let take = rows.len().min(self.cap);
        let start = rows.len() - take;
        self.sigmas.clear();
        self.bin_starts.clear();
        for &(bs, sig) in &rows[start..] {
            self.sigmas.push_back(sig);
            self.bin_starts.push_back(bs);
        }
        // Treat primed history as already-finalized so the live EMA continues
        // (no expanding-mean re-seed) and the blend windows are full.
        self.finalized_count = self.sigmas.len();
        self.prev_ema = self.sigmas.back().copied();
        self.open_bin = None;
    }

    /// Finalize the currently open bin into one EMA-smoothed Rogers-Satchell row.
    ///
    /// Mirrors `vol_builder::build_vol_from_s10`: first `ema_period` bins use an
    /// expanding mean seed, then a standard EMA with `alpha = 2/(ema_period+1)`.
    fn finalize_open(&mut self) {
        let Some((bin_start, open, high, low, close)) = self.open_bin.take() else {
            return;
        };
        let sigma = rs_sigma_from_ohlc(open, high, low, close);
        let ema = if self.finalized_count < self.ema_period {
            // Expanding-mean seed. We track the running mean via prev_ema and
            // finalized_count to avoid retaining all raw sigmas.
            let n = self.finalized_count as f64;
            match self.prev_ema {
                Some(prev_mean) => (prev_mean * n + sigma) / (n + 1.0),
                None => sigma,
            }
        } else {
            let alpha = 2.0 / (self.ema_period as f64 + 1.0);
            alpha * sigma + (1.0 - alpha) * self.prev_ema.unwrap_or(sigma)
        };
        self.prev_ema = Some(ema);
        self.finalized_count += 1;
        self.sigmas.push_back(ema);
        self.bin_starts.push_back(bin_start);
        while self.sigmas.len() > self.cap {
            self.sigmas.pop_front();
            self.bin_starts.pop_front();
        }
    }

    /// Observe one CLOSED s10 bar's OHLC, keyed by its bucket-start `ts_ms`.
    ///
    /// Rolls the s10 bar into the current open 30-min vol bin via the OHLC
    /// monoid: O = first s10.open, H = rolling max(s10.high), L = rolling
    /// min(s10.low), C = latest s10.close. This is byte-identical to the
    /// offline `ohlc::rollup(10_000, 1_800_000)` aggregation when open and
    /// close fall in the same 30-min bucket.
    ///
    /// Prefer [`Self::observe_s10`] for production s10 bars whose
    /// `close_time_ms` may straddle the next 30-min boundary.
    pub fn observe(&mut self, ts_ms: i64, o: f64, h: f64, l: f64, c: f64) -> bool {
        self.observe_s10(ts_ms, ts_ms, o, h, l, c)
    }

    /// Observe one CLOSED s10 bar, mirroring `ohlc::rollup` straddler semantics:
    /// when `close_ts_ms` lands in the next 30-min bucket, the bar's H/L touch
    /// both bins; open updates only the open bucket, close only the close bucket.
    pub fn observe_s10(
        &mut self,
        open_ts_ms: i64,
        close_ts_ms: i64,
        o: f64,
        h: f64,
        l: f64,
        c: f64,
    ) -> bool {
        if !(o.is_finite() && h.is_finite() && l.is_finite() && c.is_finite()
            && o > 0.0 && h > 0.0 && l > 0.0 && c > 0.0)
        {
            return false;
        }
        let open_bs = Self::bin_start(open_ts_ms);
        let close_bs = Self::bin_start(close_ts_ms);
        if open_bs == close_bs {
            return self.touch_bin(open_bs, o, c, h, l, Some(o), Some(c));
        }
        let mut finalized = false;
        finalized |= self.touch_bin(open_bs, o, c, h, l, Some(o), None);
        finalized |= self.touch_bin(close_bs, o, c, h, l, None, Some(c));
        finalized
    }

    fn touch_bin(
        &mut self,
        bs: i64,
        bar_open: f64,
        bar_close: f64,
        h: f64,
        l: f64,
        set_open: Option<f64>,
        set_close: Option<f64>,
    ) -> bool {
        let mut finalized = false;
        match self.open_bin {
            None => {
                let o = set_open.unwrap_or(bar_open);
                let c = set_close.unwrap_or(bar_close);
                self.open_bin = Some((bs, o, h, l, c));
            }
            Some((cur_start, ..)) if bs > cur_start => {
                self.finalize_open();
                finalized = true;
                let o = set_open.unwrap_or(bar_open);
                let c = set_close.unwrap_or(bar_close);
                self.open_bin = Some((bs, o, h, l, c));
            }
            Some((cur_start, _o, ref mut bh, ref mut bl, ref mut bc)) => {
                debug_assert_eq!(cur_start, bs);
                if h > *bh {
                    *bh = h;
                }
                if l < *bl {
                    *bl = l;
                }
                if let Some(nc) = set_close {
                    *bc = nc;
                }
            }
        }
        finalized
    }

    /// Index of the newest finalized bin, or `None` if empty.
    #[inline]
    pub fn last_index(&self) -> Option<usize> {
        if self.sigmas.is_empty() {
            None
        } else {
            Some(self.sigmas.len() - 1)
        }
    }
}

/// On-disk `.vol` record size (u48 mts + f64 sigma, packed). Mirrors
/// `series_factory::vol_bin::VolRecord` — kept here so the live core (which
/// cannot depend on series-factory) can prime [`LiveVolRing`] from disk.
const VOL_RECORD_BYTES: usize = 14;

/// Read the trailing `max_rows` rows of a persistent `.vol` file as
/// `(bin_start_ms, sigma_pct)` pairs (oldest..newest), for priming
/// [`LiveVolRing::prime_from_slice`].
///
/// The `.vol` wire format is a dense sequence of 14-byte records:
/// `[u48 mts LE][f64 sigma_pct LE]`. Returns an empty vec if the file is
/// missing, empty, or not a record-size multiple (best-effort — the live ring
/// then warms from ticks with EWMA fallback). No look-ahead: rows are pure
/// historical σ.
pub fn read_vol_tail(path: &std::path::Path, max_rows: usize) -> Vec<(i64, f64)> {
    let Ok(bytes) = std::fs::read(path) else {
        return Vec::new();
    };
    if bytes.is_empty() || bytes.len() % VOL_RECORD_BYTES != 0 {
        return Vec::new();
    }
    let n = bytes.len() / VOL_RECORD_BYTES;
    let start = n.saturating_sub(max_rows);
    let mut out = Vec::with_capacity(n - start);
    for i in start..n {
        let off = i * VOL_RECORD_BYTES;
        let mut mts_bytes = [0u8; 6];
        mts_bytes.copy_from_slice(&bytes[off..off + 6]);
        let mts = mitch::timestamp::decode_u48(&mts_bytes);
        let bin_start_ms = mitch::timestamp::to_epoch_ms(mts);
        let mut sig_bytes = [0u8; 8];
        sig_bytes.copy_from_slice(&bytes[off + 6..off + 14]);
        let sigma = f64::from_le_bytes(sig_bytes);
        if sigma.is_finite() && sigma >= 0.0 {
            out.push((bin_start_ms, sigma));
        }
    }
    out
}

impl VolSource for LiveVolRing {
    #[inline]
    fn len(&self) -> usize {
        self.sigmas.len()
    }

    #[inline]
    fn sigma_pct(&self, i: usize) -> f64 {
        self.sigmas.get(i).copied().unwrap_or(0.0)
    }

    /// Map an mts (16 µs ticks since 2010) to the nearest stored bin index.
    /// Live producers normally call `compute_sigma(last_index())`; this exists
    /// for `VolSource` completeness.
    #[inline]
    fn find_index_for_mts(&self, mts: u64) -> usize {
        // Convert mts → epoch_ms via the standard MITCH epoch (2010-01-01).
        // 1 tick = 16 µs ⇒ ms = mts * 16 / 1000 + EPOCH_MS_2010. We avoid a
        // hard dep on `timestamp` here: callers that need precise mapping use
        // the offline VolMmap. Fall back to the newest bin on any ambiguity.
        let target_ms = (mts as i128 * 16 / 1000) as i64;
        match self.bin_starts.binary_search(&Self::bin_start(target_ms)) {
            Ok(idx) => idx,
            Err(idx) => idx.min(self.sigmas.len().saturating_sub(1)),
        }
    }
}

/// Winsorized mean and variance, in-place.
///
/// Sorts the buffer, clips to `[lo_pct, hi_pct]` percentile boundaries, then
/// returns (mean, variance). Reuses the buffer to avoid allocation.
fn winsorized_mean_and_var_inplace(
    values: &mut [f64],
    min_samples: usize,
    lo_pct: f64,
    hi_pct: f64,
) -> (f64, f64) {
    let n = values.len();
    if n < min_samples {
        let mean = values.iter().sum::<f64>() / n.max(1) as f64;
        let var = if n > 1 {
            values.iter().map(|v| { let d = v - mean; d * d }).sum::<f64>() / (n - 1) as f64
        } else {
            0.0
        };
        return (mean, var);
    }

    values.sort_unstable_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));

    let lo = (n as f64 * lo_pct) as usize;
    let hi = ((n as f64 * hi_pct) as usize).saturating_sub(1).min(n - 1);
    let lo_val = values[lo];
    let hi_val = values[hi];

    let mut sum = 0.0;
    for v in values.iter_mut() {
        *v = v.clamp(lo_val, hi_val);
        sum += *v;
    }
    let mean = sum / n as f64;
    let var = values.iter().map(|v| { let d = v - mean; d * d }).sum::<f64>() / (n - 1) as f64;
    (mean, var)
}

#[cfg(test)]
mod tests {
    use super::*;

    struct StaticSource(Vec<f64>);

    impl VolSource for StaticSource {
        fn len(&self) -> usize { self.0.len() }
        fn sigma_pct(&self, i: usize) -> f64 { self.0.get(i).copied().unwrap_or(0.0) }
        fn find_index_for_mts(&self, _mts: u64) -> usize { self.0.len().saturating_sub(1) }
    }

    #[test]
    fn winsorized_mean_suppresses_outlier() {
        let mut values: Vec<f64> = (1..=20).map(|i| i as f64).collect();
        values[19] = 1000.0;
        let (mean, _var) = winsorized_mean_and_var_inplace(&mut values, 5, 0.05, 0.95);
        assert!(mean < 12.0, "winsorized mean should suppress outlier: got {}", mean);
    }

    #[test]
    fn calc_returns_fallback_on_small_source() {
        let src = StaticSource(vec![0.02]);
        let mut calc = MtfVolCalculator::new(&src, VolConfig::default());
        assert!((calc.compute_sigma(0) - 0.02).abs() < 1e-9);
    }

    /// LiveVolRing must produce EMA-smoothed Rogers-Satchell rows byte-identical
    /// to the offline `build_vol_from_s10` path for the same s10-OHLC bin
    /// stream. This is the seam-parity guarantee: same input bins ⇒ same `.vol`
    /// rows ⇒ same σ. Each 30-min bin here is fed as ONE rolled-up OHLC bar
    /// (the rollup monoid is exercised separately in the offline path).
    #[test]
    fn live_vol_ring_matches_offline_ema_rs() {
        let ema_period = 28usize;
        // Synthetic 30-min OHLC bins (bin_start, o, h, l, c).
        let bin = 1_800_000i64;
        let mut bins: Vec<(i64, f64, f64, f64, f64)> = Vec::new();
        let mut px = 100.0;
        for i in 0..200i64 {
            let o = px;
            let c = px * (1.0 + 0.0005 * (((i % 5) as f64) - 2.0));
            let h = o.max(c) * (1.0 + 0.002 + 0.0005 * ((i % 3) as f64));
            let l = o.min(c) * (1.0 - 0.002 - 0.0005 * ((i % 4) as f64));
            bins.push((i * bin, o, h, l, c));
            px = c;
        }

        // Reference: replicate vol_builder's expanding-mean + EMA over RS σ.
        let alpha = 2.0 / (ema_period as f64 + 1.0);
        let rs = |o: f64, h: f64, l: f64, c: f64| rs_sigma_from_ohlc(o, h, l, c);
        let mut ref_rows: Vec<f64> = Vec::new();
        let mut prev: Option<f64> = None;
        for (i, &(_, o, h, l, c)) in bins.iter().enumerate() {
            let sigma = rs(o, h, l, c);
            let ema = if i < ema_period {
                bins[..=i].iter().map(|&(_, a, b, d, e)| rs(a, b, d, e)).sum::<f64>() / (i + 1) as f64
            } else {
                alpha * sigma + (1.0 - alpha) * prev.unwrap_or(sigma)
            };
            prev = Some(ema);
            ref_rows.push(ema);
        }

        // LiveVolRing: feed one closed s10-equivalent OHLC bar per 30-min bin,
        // then advance one extra bin to force-finalize the last open bin.
        let mut ring = LiveVolRing::new(400, ema_period);
        for &(bs, o, h, l, c) in &bins {
            ring.observe(bs + 1, o, h, l, c);
        }
        ring.observe(bins.last().unwrap().0 + bin + 1, 1.0, 1.0, 1.0, 1.0);

        assert_eq!(ring.len(), bins.len(), "ring bin count mismatch");
        for (i, &expected) in ref_rows.iter().enumerate() {
            let got = ring.sigma_pct(i);
            assert!(
                (got - expected).abs() < 1e-12,
                "bin {i}: ring σ {got} != offline σ {expected}"
            );
        }
    }

    /// OHLC monoid inside the ring: multiple s10 bars within ONE 30-min bin must
    /// roll up to O=first.open, H=max, L=min, C=last.close before RS σ.
    #[test]
    fn live_vol_ring_rolls_up_intrabin_ohlc() {
        let bin = 1_800_000i64;
        let mut ring = LiveVolRing::new(8, 28);
        // 3 s10 bars in bin 0.
        ring.observe(0, 100.0, 101.0, 99.5, 100.5);
        ring.observe(10_000, 100.5, 103.0, 100.0, 102.0); // new high
        ring.observe(20_000, 102.0, 102.5, 98.0, 99.0);   // new low + last close
        // Advance to bin 1 to finalize bin 0.
        ring.observe(bin + 1, 99.0, 99.0, 99.0, 99.0);
        // Expected RS over rolled-up O=100, H=103, L=98, C=99.
        let expected = rs_sigma_from_ohlc(100.0, 103.0, 98.0, 99.0);
        assert!((ring.sigma_pct(0) - expected).abs() < 1e-12);
    }

    /// Priming from a `.vol` tail must place rows verbatim and continue the EMA.
    #[test]
    fn live_vol_ring_prime_continues_ema() {
        let mut ring = LiveVolRing::new(100, 28);
        let rows: Vec<(i64, f64)> = (0..50).map(|i| (i as i64 * 1_800_000, 0.01 + 0.0001 * i as f64)).collect();
        ring.prime_from_slice(&rows);
        assert_eq!(ring.len(), 50);
        // last primed row becomes the EMA carry
        let last = rows.last().unwrap().1;
        assert!((ring.sigma_pct(49) - last).abs() < 1e-12);
    }
}
