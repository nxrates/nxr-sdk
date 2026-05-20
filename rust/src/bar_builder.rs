//! Time/price-based bar accumulator producing canonical `mitch::Bar`.
//!
//! Single-pass streaming accumulator for the canonical microstructure feature
//! set stored in `mitch::Bar` (cache line 1 + 32B microstructure section):
//!
//! - `realized_var`   : Σ r² (HF canonical variance)
//! - `bipower_var`    : (π/2) · Σ |r_t|·|r_{t-1}| (Barndorff-Nielsen & Shephard
//!                      jump-robust variance; RV - BV ≈ jump variation)
//! - `drift`          : OLS slope · duration / close (dimensionless)
//! - `vol_imbalance`  : Σ sign(r_t) · (vbid+vask)_t / total_vol (signed OFI)
//! - `avg_spread_bps` : mean((ask - bid) / mid) × 1e4
//! - `max_abs_return` : max |r_t| (tail / single-tick jump indicator)
//! - `avg_ci_ubp`     : mean Index.ci_ubp, sqrt-compressed to u16
//! - `reject_rate`    : rejected / (accepted + rejected) × 65535
//!
//! `ci_ubp`, `accepted` and `rejected` parameters are forwarded from the
//! `Index` input; callers that only have raw tick data pass `ci_ubp = 0.0`,
//! `accepted = 1` (one observation), `rejected = 0` and the quality fields
//! remain at their zero defaults.

use mitch::bar::Bar;
use mitch::timestamp;

use mitch::common::CI_SCALE;

/// Single-pass accumulator for building a `mitch::Bar`.
///
/// Usage: call `ingest()` for each valid ingestion inside the bar window,
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

    // OLS regression accumulators (drift = normalized slope)
    first_ms: f64,
    sum_x: f64,
    sum_y: f64,
    sum_xy: f64,
    sum_xx: f64,
    last_x: f64,

    // Return-based accumulators
    prev_mid: f64,
    rv_sum: f64,           // Σ r² (realized variance)
    prev_abs_r: f64,       // sentinel < 0 until first return computed
    bipower_sum: f64,      // Σ |r_t|·|r_{t-1}| for t ≥ 2
    max_abs_r: f64,        // max |r_t|

    // Signed order-flow imbalance: Σ sign(r_t) · (vbid+vask)_t.
    // OFI numerator + denominator share scope (tick 2 onward).
    ofi_sum: f64,
    ofi_total_vol: f64,

    // Spread accumulator (basis points of mid). Numerator skips mid<=0 ticks;
    // denominator (`n_spread_samples`) mirrors that scope.
    sum_spread_bps: f64,
    n_spread_samples: u32,

    // Quality accumulators (from Index-level inputs; zero for tick-only)
    sum_ci_ubp: f64,
    sum_accepted: u64,
    sum_rejected: u64,
}

impl BarAccumulator {
    pub fn new() -> Self {
        Self {
            open: 0.0, high: f64::NEG_INFINITY, low: f64::INFINITY, close: 0.0,
            vbid: 0, vask: 0, tick_count: 0, open_mts: 0, close_mts: 0,
            first_ms: 0.0, sum_x: 0.0, sum_y: 0.0, sum_xy: 0.0, sum_xx: 0.0, last_x: 0.0,
            prev_mid: 0.0, rv_sum: 0.0,
            prev_abs_r: -1.0, bipower_sum: 0.0, max_abs_r: 0.0,
            ofi_sum: 0.0, ofi_total_vol: 0.0,
            sum_spread_bps: 0.0, n_spread_samples: 0,
            sum_ci_ubp: 0.0, sum_accepted: 0, sum_rejected: 0,
        }
    }

    /// Ingest a single observation into the bar.
    ///
    /// `epoch_ms` is the observation timestamp in milliseconds since Unix epoch.
    /// `ci_ubp` is the decoded confidence interval in micro basis points of mid
    /// (pass `0.0` for tick-only inputs). `accepted` / `rejected` are the
    /// provider-level counts from the originating Index (pass `1` / `0` for
    /// tick-only inputs, so each tick is treated as one accepted observation).
    /// The caller is responsible for outlier filtering before this call.
    #[inline]
    pub fn ingest(
        &mut self,
        bid: f64,
        ask: f64,
        vbid: u32,
        vask: u32,
        epoch_ms: i64,
        ci_ubp: f64,
        accepted: u32,
        rejected: u32,
    ) {
        let mid = (bid + ask) * 0.5;
        let mts = timestamp::from_epoch_ms(epoch_ms);
        self.tick_count += 1;

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

        // OLS regression: x = seconds since first tick, y = mid.
        let x = (epoch_ms as f64 - self.first_ms) / 1000.0;
        self.sum_x += x;
        self.sum_y += mid;
        self.sum_xy += x * mid;
        self.sum_xx += x * x;
        self.last_x = x;

        // Spread (bps of mid). Guard against zero mid; track sample count
        // separately so the divisor in flush() mirrors the numerator's scope.
        if mid > 0.0 {
            self.sum_spread_bps += ((ask - bid) / mid) * 10_000.0;
            self.n_spread_samples += 1;
        }

        // Return-based statistics (defined from the second tick onward).
        if self.prev_mid > 0.0 && mid > 0.0 {
            let r = (mid / self.prev_mid).ln();
            let abs_r = r.abs();
            self.rv_sum += r * r;
            if abs_r > self.max_abs_r {
                self.max_abs_r = abs_r;
            }
            if self.prev_abs_r >= 0.0 {
                self.bipower_sum += abs_r * self.prev_abs_r;
            }
            self.prev_abs_r = abs_r;
            // Signed OFI: attribute this observation's volume to the sign of r_t.
            // OFI numerator + denominator share scope (tick 2 onward).
            let sign = if r > 0.0 { 1.0 } else if r < 0.0 { -1.0 } else { 0.0 };
            let tick_vol = vbid as f64 + vask as f64;
            self.ofi_sum += sign * tick_vol;
            self.ofi_total_vol += tick_vol;
        }
        self.prev_mid = mid;

        // Quality accumulators (weighted by accepted-count, so Index-level inputs
        // with multi-provider consensus are weighted more than tick-only inputs).
        self.sum_ci_ubp += ci_ubp * accepted as f64;
        self.sum_accepted += accepted as u64;
        self.sum_rejected += rejected as u64;
    }

    /// Flush accumulated observations to a `mitch::Bar` and reset state.
    /// Returns `None` if nothing was ingested.
    pub fn flush(&mut self) -> Option<Bar> {
        if self.tick_count == 0 {
            return None;
        }

        let n = self.tick_count as f64;
        let vbid = self.vbid.min(u32::MAX as u64) as u32;
        let vask = self.vask.min(u32::MAX as u64) as u32;

        let realized_var = self.rv_sum as f32;
        // Barndorff-Nielsen & Shephard (2004) jump-robust variance.
        let bipower_var = (self.bipower_sum * core::f64::consts::FRAC_PI_2) as f32;
        let max_abs_return = self.max_abs_r as f32;

        let drift = if self.tick_count >= 2 && self.last_x > 0.0 && self.close.abs() > 1e-10 {
            let x_mean = self.sum_x / n;
            let y_mean = self.sum_y / n;
            let denom = self.sum_xx - n * x_mean * x_mean;
            if denom.abs() > 1e-12 {
                let slope = (self.sum_xy - n * x_mean * y_mean) / denom;
                (slope * self.last_x / self.close) as f32
            } else {
                0.0
            }
        } else {
            0.0
        };

        // mid<=0 guard skipped from numerator; mirror in denominator.
        let avg_spread_bps = if self.n_spread_samples > 0 {
            (self.sum_spread_bps / self.n_spread_samples as f64) as f32
        } else {
            0.0
        };

        let vol_imbalance = if self.ofi_total_vol > 0.0 {
            (self.ofi_sum / self.ofi_total_vol) as f32
        } else {
            0.0
        };

        let avg_ci_ubp = if self.sum_accepted > 0 {
            let avg = self.sum_ci_ubp / self.sum_accepted as f64;
            let enc = (avg.max(0.0).sqrt() * CI_SCALE).round();
            enc.clamp(0.0, u16::MAX as f64) as u16
        } else {
            0
        };

        let total_obs = self.sum_accepted + self.sum_rejected;
        let reject_rate = if total_obs > 0 {
            let rate = self.sum_rejected as f64 / total_obs as f64;
            (rate * u16::MAX as f64).round() as u16
        } else {
            0
        };

        let mut bar = Bar::new_ohlcv(
            self.open_mts, self.close_mts,
            self.open, self.high, self.low, self.close,
            vbid, vask, self.tick_count,
        );
        bar.realized_var = realized_var;
        bar.bipower_var = bipower_var;
        bar.drift = drift;
        bar.vol_imbalance = vol_imbalance;
        bar.avg_spread_bps = avg_spread_bps;
        bar.max_abs_return = max_abs_return;
        bar.avg_ci_ubp = avg_ci_ubp;
        bar.reject_rate = reject_rate;
        // Time-bucketed bar: stamp the kind explicitly (don't rely on zero-init).
        bar.kind = mitch::bar::BarKind::Kline as u8;

        self.reset();
        Some(bar)
    }

    /// Number of observations ingested since last flush.
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
