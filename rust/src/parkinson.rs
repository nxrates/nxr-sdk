//! Multi-timeframe Parkinson volatility.
//!
//! Inverse-variance weighted blend of winsorized means across configurable
//! lookback windows. Source data (per-bin sigma_pct) is provided by anything
//! that implements [`VolSource`]: a memory-mapped .vol file (backtest), an
//! in-memory ring buffer (real-time), or a test fixture.
//!
//! The Renko engine itself does not depend on `VolSource`: callers resolve σ
//! via `MtfParkinsonCalculator` then pass the number into
//! `RenkoGenerator::feed_tick_with_sigma`. This trait remains the input
//! contract for the calculator only.

use serde::{Deserialize, Serialize};

/// Abstract source of per-bin Parkinson sigma values.
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
}

impl Default for VolConfig {
    fn default() -> Self {
        Self {
            ema_period: 28,
            sigma_blend_windows_days: vec![14, 60, 180],
            winsorize_pct: [0.05, 0.95],
            winsorize_min_samples: 5,
        }
    }
}

/// Multi-timeframe Parkinson sigma blender.
///
/// Reads 30-min Parkinson sigma from a `VolSource`, then blends across
/// configurable lookback windows using inverse-variance weighting and
/// winsorized mean for robustness.
pub struct MtfParkinsonCalculator<'a, S: VolSource + ?Sized> {
    source: &'a S,
    config: VolConfig,
    buf: Vec<f64>,
}

impl<'a, S: VolSource + ?Sized> MtfParkinsonCalculator<'a, S> {
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

    /// Blended Parkinson sigma at bin `hour_idx`.
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

/// 30 minutes in milliseconds — the canonical Parkinson bin width.
const VOL_BIN_MS: i64 = 1_800_000;

/// Parkinson sigma from a 30-min bin's high/low.
///
/// `sigma = |ln(high/low)| / (2 * sqrt(ln 2))`. Mirrors
/// `nxr_sdk::agg::parkinson_sigma` (kept inline here so `parkinson` has no
/// `agg` dependency). Returns 0.0 for degenerate input.
#[inline]
fn parkinson_sigma_hl(high: f64, low: f64) -> f64 {
    if high <= 0.0 || low <= 0.0 || high < low {
        return 0.0;
    }
    (high / low).ln().abs() / (2.0 * std::f64::consts::LN_2.sqrt())
}

/// Online, in-process [`VolSource`] for the LIVE renko producer.
///
/// This is the real-time twin of the offline `.vol` file ([`VolMmap`] in
/// series-factory): it stores the SAME EMA-smoothed 30-min Parkinson
/// `sigma_pct` rows the offline `vol_builder` writes, so the LIVE renko brick
/// size — computed via [`MtfParkinsonCalculator::compute_sigma`] over this
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
/// Feed live mids via [`Self::observe`]; the ring buckets them into the
/// current open 30-min HLC and, when a tick crosses into a new bucket,
/// finalizes the closed bucket into one EMA-smoothed Parkinson row (matching
/// `vol_builder::write_vol_records_from_hlc` exactly). Prime the trailing
/// history on open via [`Self::prime_from_slice`] (e.g. from the persistent
/// `.vol` tail) so the first live bin's blended σ equals the blend the
/// backfill used for the boundary bin.
///
/// ## Bounded memory
///
/// Capacity is `cap` bins (caller sizes it to `max(blend window days)*48`).
/// Oldest bins are evicted on overflow — but `compute_sigma`'s longest window
/// only reaches back `max_window_days*48` bins, so eviction never truncates a
/// live blend.
#[derive(Debug, Clone)]
pub struct LiveVolRing {
    /// EMA-smoothed Parkinson sigma per closed 30-min bin (oldest..newest).
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
    /// Currently open bucket: (bin_start_ms, high, low). `None` until first obs.
    open_bin: Option<(i64, f64, f64)>,
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

    /// Finalize the currently open bucket into one EMA-smoothed Parkinson row.
    ///
    /// Mirrors `vol_builder::write_vol_records_from_hlc`: first `ema_period`
    /// bins use an expanding mean seed, then a standard EMA with
    /// `alpha = 2/(ema_period+1)`.
    fn finalize_open(&mut self) {
        let Some((bin_start, high, low)) = self.open_bin.take() else {
            return;
        };
        let sigma = parkinson_sigma_hl(high, low);
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

    /// Observe one live mid quote. `high`/`low` should be the tick's ask/bid
    /// (matching `vol_builder`'s H=ask, L=bid bucketing); pass `mid` for both
    /// if only a composite price is available.
    ///
    /// Closes and finalizes any bins the timestamp has advanced past, then
    /// opens/extends the bucket for `ts_ms`. Returns `true` if at least one
    /// bin was finalized (caller may recompute σ on a boundary).
    pub fn observe(&mut self, ts_ms: i64, high: f64, low: f64) -> bool {
        if !(high.is_finite() && low.is_finite() && high > 0.0 && low > 0.0) {
            return false;
        }
        let bs = Self::bin_start(ts_ms);
        let mut finalized = false;
        match self.open_bin {
            None => {
                self.open_bin = Some((bs, high.max(low), low.min(high)));
            }
            Some((cur_start, _, _)) if bs > cur_start => {
                // Crossed into a new bucket — finalize the closed one. Only the
                // immediately-prior bucket carries data; any fully-empty
                // intervening 30-min slots are skipped (no synthetic rows, same
                // as the offline BTreeMap which only stores observed buckets).
                self.finalize_open();
                finalized = true;
                self.open_bin = Some((bs, high.max(low), low.min(high)));
            }
            Some((cur_start, ref mut h, ref mut l)) => {
                debug_assert_eq!(cur_start, bs);
                if high > *h {
                    *h = high;
                }
                if low < *l {
                    *l = low;
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
        let mut calc = MtfParkinsonCalculator::new(&src, VolConfig::default());
        assert!((calc.compute_sigma(0) - 0.02).abs() < 1e-9);
    }

    /// LiveVolRing must produce EMA-smoothed Parkinson rows byte-identical to
    /// the offline `vol_builder` path for the same HLC bucket stream. This is
    /// the seam-parity guarantee: same input bins ⇒ same `.vol` rows ⇒ same σ.
    #[test]
    fn live_vol_ring_matches_offline_ema_parkinson() {
        let ema_period = 28usize;
        // Synthetic 30-min HLC buckets (bin_start, high, low).
        let bin = 1_800_000i64;
        let mut buckets: Vec<(i64, f64, f64)> = Vec::new();
        let mut h = 100.0;
        for i in 0..200i64 {
            let lo = h * (1.0 - 0.003 - 0.001 * ((i % 7) as f64));
            buckets.push((i * bin, h, lo));
            h *= 1.0 + 0.0005 * (((i % 5) as f64) - 2.0);
        }

        // Reference: replicate vol_builder's expanding-mean + EMA exactly.
        let alpha = 2.0 / (ema_period as f64 + 1.0);
        let parkinson = |high: f64, low: f64| (high / low).ln().abs() / (2.0 * std::f64::consts::LN_2.sqrt());
        let mut ref_rows: Vec<f64> = Vec::new();
        let mut prev: Option<f64> = None;
        for (i, &(_, hh, ll)) in buckets.iter().enumerate() {
            let sigma = parkinson(hh, ll);
            let ema = if i < ema_period {
                buckets[..=i].iter().map(|&(_, a, b)| parkinson(a, b)).sum::<f64>() / (i + 1) as f64
            } else {
                alpha * sigma + (1.0 - alpha) * prev.unwrap_or(sigma)
            };
            prev = Some(ema);
            ref_rows.push(ema);
        }

        // LiveVolRing: feed one mid-tick at the END of each bucket, plus a tick
        // in the NEXT bucket to force finalization. We feed (ts, ask=high,
        // bid=low) twice per bucket so H/L are captured.
        let mut ring = LiveVolRing::new(400, ema_period);
        for &(bs, hh, ll) in &buckets {
            ring.observe(bs + 1, hh, ll);            // open + set H
            ring.observe(bs + bin - 1, ll, ll);      // same bucket, no-op on H/L
        }
        // Force-finalize the last open bucket by advancing into a new bin.
        ring.observe(buckets.last().unwrap().0 + bin + 1, 1.0, 1.0);

        // Ring should have one finalized row per input bucket.
        assert_eq!(ring.len(), buckets.len(), "ring bin count mismatch");
        for (i, &expected) in ref_rows.iter().enumerate() {
            let got = ring.sigma_pct(i);
            assert!(
                (got - expected).abs() < 1e-12,
                "bin {i}: ring σ {got} != offline σ {expected}"
            );
        }
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
