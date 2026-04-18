//! Time/price-based bar accumulator producing canonical `mitch::Bar`.
//!
//! Accumulates raw ticks in a single pass computing OHLCV, Welford dispersion,
//! OLS drift, volume imbalance, tick efficiency, and log volume. Flushes to the
//! 128-byte `mitch::Bar` wire format directly - no intermediate structs.
//!
//! This is the canonical bar builder for the NX Rates stack. Series-factory and
//! third-party integrators should use this instead of rolling their own.

use mitch::bar::Bar;
use mitch::timestamp;

/// Single-pass accumulator for building a `mitch::Bar` from raw ticks.
///
/// Usage: call `ingest()` for each valid tick in the bar's time window,
/// then `flush()` to produce the bar and reset state.
pub struct BarAccumulator {
    // OHLCV
    open: f64,
    high: f64,
    low: f64,
    close: f64,
    vbid: u64,
    vask: u64,
    tick_count: u32,
    open_mts: u64,
    close_mts: u64,

    // Welford online variance (for dispersion = CV% = sigma/mean)
    w_mean: f64,
    w_m2: f64,

    // OLS linear regression accumulators (for drift = normalized slope)
    first_ms: f64,
    sum_x: f64,
    sum_y: f64,
    sum_xy: f64,
    sum_xx: f64,
    last_x: f64,
}

impl BarAccumulator {
    pub fn new() -> Self {
        Self {
            open: 0.0, high: f64::NEG_INFINITY, low: f64::INFINITY, close: 0.0,
            vbid: 0, vask: 0, tick_count: 0, open_mts: 0, close_mts: 0,
            w_mean: 0.0, w_m2: 0.0,
            first_ms: 0.0, sum_x: 0.0, sum_y: 0.0, sum_xy: 0.0, sum_xx: 0.0, last_x: 0.0,
        }
    }

    /// Ingest a single tick into the bar.
    ///
    /// `epoch_ms` is the tick's timestamp in milliseconds since Unix epoch.
    /// Caller is responsible for outlier filtering before calling this.
    #[inline]
    pub fn ingest(&mut self, bid: f64, ask: f64, vbid: u32, vask: u32, epoch_ms: i64) {
        let mid = (bid + ask) * 0.5;
        let mts = timestamp::from_epoch_ms(epoch_ms);
        self.tick_count += 1;

        // OHLCV
        if self.tick_count == 1 {
            self.open = mid;
            self.open_mts = mts;
            self.first_ms = epoch_ms as f64;
        }
        if mid > self.high { self.high = mid; }
        if mid < self.low { self.low = mid; }
        self.close = mid;
        self.close_mts = mts;
        self.vbid += vbid as u64;
        self.vask += vask as u64;

        // Welford
        let n = self.tick_count as f64;
        let delta = mid - self.w_mean;
        self.w_mean += delta / n;
        self.w_m2 += delta * (mid - self.w_mean);

        // OLS regression accumulators (x = seconds since first tick)
        let x = (epoch_ms as f64 - self.first_ms) / 1000.0;
        self.sum_x += x;
        self.sum_y += mid;
        self.sum_xy += x * mid;
        self.sum_xx += x * x;
        self.last_x = x;
    }

    /// Flush accumulated ticks to a `mitch::Bar` and reset state.
    /// Returns `None` if no ticks were ingested.
    pub fn flush(&mut self) -> Option<Bar> {
        if self.tick_count == 0 {
            return None;
        }

        let n = self.tick_count as f64;
        let vbid = self.vbid.min(u32::MAX as u64) as u32;
        let vask = self.vask.min(u32::MAX as u64) as u32;
        let total_vol = vbid as f64 + vask as f64;

        // Dispersion: CV% = (sigma / mean) * 100
        let dispersion = if self.tick_count >= 2 && self.w_mean.abs() > 1e-10 {
            let variance = self.w_m2 / n;
            ((variance.sqrt() / self.w_mean) * 100.0) as f32
        } else {
            0.0
        };

        // Drift: normalized OLS slope = (slope * duration / close) * 100
        let drift = if self.tick_count >= 2 && self.last_x > 0.0 && self.close.abs() > 1e-10 {
            let x_mean = self.sum_x / n;
            let y_mean = self.sum_y / n;
            let denom = self.sum_xx - n * x_mean * x_mean;
            if denom.abs() > 1e-12 {
                let slope = (self.sum_xy - n * x_mean * y_mean) / denom;
                ((slope * self.last_x / self.close) * 100.0) as f32
            } else {
                0.0
            }
        } else {
            0.0
        };

        // Volume imbalance: (vask - vbid) / (vask + vbid)
        let vol_imbalance = if total_vol > 0.0 {
            ((vask as f64 - vbid as f64) / total_vol) as f32
        } else {
            0.0
        };

        // Tick efficiency: |close - open| / (price * tick_count)
        let tick_efficiency = if self.tick_count > 0 && self.close.abs() > 1e-10 {
            ((self.close - self.open).abs() / (self.close * n)) as f32
        } else {
            0.0
        };

        // Log volume
        let log_volume = (total_vol + 1.0).ln() as f32;

        let mut bar = Bar::new_ohlcv(
            self.open_mts, self.close_mts,
            self.open, self.high, self.low, self.close,
            vbid, vask, self.tick_count,
        );
        bar.dispersion = dispersion;
        bar.drift = drift;
        bar.vol_imbalance = vol_imbalance;
        bar.tick_efficiency = tick_efficiency;
        bar.log_volume = log_volume;

        self.reset();
        Some(bar)
    }

    /// Number of ticks ingested since last flush.
    #[inline]
    pub fn count(&self) -> u32 { self.tick_count }

    fn reset(&mut self) {
        *self = Self::new();
    }
}

impl Default for BarAccumulator {
    fn default() -> Self { Self::new() }
}

/// Create a flat (zero-tick) bar at the given timestamp for gap filling.
/// Uses the reference price for all OHLC fields.
pub fn flat_bar(epoch_ms: i64, ref_price: f64) -> Bar {
    let mts = timestamp::from_epoch_ms(epoch_ms);
    Bar::new_ohlcv(mts, mts, ref_price, ref_price, ref_price, ref_price, 0, 0, 0)
}
