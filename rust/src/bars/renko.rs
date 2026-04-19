//! Adaptive Renko bar generation.
//!
//! Brick size formula:
//!   b_t = p_t * clamp(multiplier * sigma_blend(t), min_pct, max_pct)
//!
//! where sigma_blend comes from [`super::parkinson::MtfParkinsonCalculator`]
//! over any [`super::parkinson::VolSource`] (mmap for backtest, ring buffer
//! for real-time).
//!
//! Design:
//!   * Streaming, never holds all bars in RAM
//!   * Fixed lookbacks and auto-weighting, no over-fitting
//!   * Continuity invariants enforced (single-sided wick, open[i]=close[i-1])

use anyhow::Result;
use serde::{Deserialize, Serialize};

use super::grid::{grid_step_for_brick, snap_to_25_grid, snap_to_grid};
use super::parkinson::{MtfParkinsonCalculator, VolConfig, VolSource};

/// Adaptive Renko configuration.
///
/// `multiplier` controls bars/day via `brick_pct = multiplier * sigma_blend`
/// (auto-calibrated via target bars/day). `min_pct` is a safety floor.
#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
pub struct RenkoConfig {
    pub multiplier: f32,
    pub min_pct: f32,
    pub max_pct: f32,
}

impl Default for RenkoConfig {
    fn default() -> Self {
        Self { multiplier: 0.075, min_pct: 0.001, max_pct: 0.10 }
    }
}

impl RenkoConfig {
    /// Unique identifier for this config (used for output file naming).
    pub fn id(&self) -> String {
        format!(
            "m{:04}_mp{:04}",
            (self.multiplier * 10000.0) as u16,
            (self.min_pct * 1_000_000.0) as u16,
        )
    }

    pub fn validate(&self) -> Result<()> {
        if !(0.001..=1.0).contains(&self.multiplier) {
            anyhow::bail!("multiplier out of range: {}", self.multiplier);
        }
        if self.min_pct >= self.max_pct {
            anyhow::bail!("min_pct must be < max_pct");
        }
        Ok(())
    }
}

/// Renko bar with all required fields for downstream enrichment.
#[derive(Debug, Clone, Copy)]
pub struct RenkoBar {
    pub timestamp: i64,
    pub open: f64,
    pub high: f64,
    pub low: f64,
    pub close: f64,
    /// +1 up, -1 down
    pub direction: i8,
    /// Brick size in absolute price units.
    pub brick_size: f64,
    pub tick_count: u32,
    pub duration_ms: u32,
}

/// Streaming Renko bar generator with adaptive brick sizing.
///
/// Brick size = price * clamp(multiplier * sigma_blend, min_pct, max_pct).
/// Brick sizes are snapped to a 4-sigfig 2/5-multiple grid. Brick boundaries
/// snap to the implied grid step. No hysteresis: the MTF blending already
/// provides sufficient smoothing.
pub struct RenkoGenerator<'a, S: VolSource + ?Sized> {
    config: RenkoConfig,
    sigma_calc: MtfParkinsonCalculator<'a, S>,
    sigma_cache: Option<&'a [f64]>,
    current_brick_size: f64,
    last_recompute_period: i64,
    initialized: bool,
    last_close: f64,
    pending_high: f64,
    pending_low: f64,
    bar_start_ts: i64,
    tick_count: u32,
    n_bars: usize,
    total_duration_ms: u64,
}

impl<'a, S: VolSource + ?Sized> RenkoGenerator<'a, S> {
    pub fn new(config: RenkoConfig, source: &'a S, vol_config: VolConfig) -> Result<Self> {
        config.validate()?;
        Ok(Self {
            config,
            sigma_calc: MtfParkinsonCalculator::new(source, vol_config),
            sigma_cache: None,
            current_brick_size: 0.0,
            last_recompute_period: i64::MIN,
            initialized: false,
            last_close: 0.0,
            pending_high: 0.0,
            pending_low: 0.0,
            bar_start_ts: 0,
            tick_count: 0,
            n_bars: 0,
            total_duration_ms: 0,
        })
    }

    /// Use a precomputed sigma cache for O(1) lookups. Call
    /// [`MtfParkinsonCalculator::precompute_sigma_cache`] to produce one.
    pub fn set_sigma_cache(&mut self, cache: &'a [f64]) {
        self.sigma_cache = Some(cache);
    }

    /// Cumulative bar count and total duration (ms).
    pub fn stats(&self) -> (usize, u64) {
        (self.n_bars, self.total_duration_ms)
    }

    fn compute_brick_size(&mut self, price: f64, timestamp_ms: i64) -> f64 {
        let hour_idx = self.sigma_calc.find_index_for_ts(timestamp_ms);
        let sigma = if let Some(cache) = self.sigma_cache {
            cache.get(hour_idx).copied().unwrap_or(0.01)
        } else {
            self.sigma_calc.compute_sigma(hour_idx)
        };

        let raw_pct = self.config.multiplier as f64 * sigma;
        let clamped_pct = raw_pct.clamp(self.config.min_pct as f64, self.config.max_pct as f64);
        let raw_brick = price * clamped_pct;
        let brick_size = snap_to_25_grid(raw_brick);
        self.current_brick_size = brick_size;
        self.last_recompute_period = timestamp_ms / 1_800_000;
        brick_size
    }

    /// Feed one tick, emitting any produced bars via the callback.
    ///
    /// State persists across calls so this can run across multiple input
    /// files without reset.
    pub fn feed_tick<F>(&mut self, ts: i64, price: f64, write_bar: &mut F) -> Result<()>
    where
        F: FnMut(&RenkoBar) -> Result<()>,
    {
        if !self.initialized {
            self.compute_brick_size(price, ts);
            let grid = grid_step_for_brick(self.current_brick_size);
            self.last_close = snap_to_grid(price, grid);
            self.pending_high = price;
            self.pending_low = price;
            self.bar_start_ts = ts;
            self.tick_count = 1;
            self.initialized = true;
            return Ok(());
        }

        self.tick_count += 1;

        let current_half_hour = ts / 1_800_000;
        if current_half_hour > self.last_recompute_period {
            self.compute_brick_size(price, ts);
            let new_grid = grid_step_for_brick(self.current_brick_size);
            self.last_close = snap_to_grid(self.last_close, new_grid);
        }

        let sz = self.current_brick_size;
        if sz <= 0.0 || !sz.is_finite() || !price.is_finite() || price <= 0.0 {
            return Ok(());
        }

        self.pending_high = self.pending_high.max(price);
        self.pending_low = self.pending_low.min(price);

        let grid = grid_step_for_brick(sz);

        const MAX_BRICKS_PER_TICK: usize = 10_000;
        let mut bricks_this_tick = 0usize;

        let mut first_in_seq = true;
        while price - self.last_close >= sz {
            let close = snap_to_grid(self.last_close + sz, grid);
            if close <= self.last_close || bricks_this_tick >= MAX_BRICKS_PER_TICK {
                break;
            }
            bricks_this_tick += 1;
            let duration = (ts - self.bar_start_ts) as u32;
            self.total_duration_ms += duration as u64;

            let l = if first_in_seq { self.pending_low.min(self.last_close) } else { self.last_close };
            let bar = RenkoBar {
                timestamp: ts,
                open: self.last_close,
                high: close,
                low: l,
                close,
                direction: 1,
                brick_size: sz,
                tick_count: if first_in_seq { self.tick_count } else { 0 },
                duration_ms: duration,
            };

            first_in_seq = false;
            write_bar(&bar)?;
            self.n_bars += 1;

            self.last_close = close;
            self.pending_high = close;
            self.pending_low = close;
            self.bar_start_ts = ts;
            self.tick_count = 0;
        }

        first_in_seq = true;
        while self.last_close - price >= sz {
            let close = snap_to_grid(self.last_close - sz, grid);
            if close >= self.last_close || bricks_this_tick >= MAX_BRICKS_PER_TICK {
                break;
            }
            bricks_this_tick += 1;
            let duration = (ts - self.bar_start_ts) as u32;
            self.total_duration_ms += duration as u64;

            let h = if first_in_seq { self.pending_high.max(self.last_close) } else { self.last_close };
            let bar = RenkoBar {
                timestamp: ts,
                open: self.last_close,
                high: h,
                low: close,
                close,
                direction: -1,
                brick_size: sz,
                tick_count: if first_in_seq { self.tick_count } else { 0 },
                duration_ms: duration,
            };

            first_in_seq = false;
            write_bar(&bar)?;
            self.n_bars += 1;

            self.last_close = close;
            self.pending_high = close;
            self.pending_low = close;
            self.bar_start_ts = ts;
            self.tick_count = 0;
        }

        Ok(())
    }

    /// Feed many ticks from an iterator. Returns (bars_emitted, total_duration_ms)
    /// for this call only (cumulative stats live in [`Self::stats`]).
    pub fn generate<F>(
        &mut self,
        price_iter: impl Iterator<Item = (i64, f64)>,
        mut write_bar: F,
    ) -> Result<(usize, u64)>
    where
        F: FnMut(&RenkoBar) -> Result<()>,
    {
        let bars_before = self.n_bars;
        let dur_before = self.total_duration_ms;

        for (ts, price) in price_iter {
            self.feed_tick(ts, price, &mut write_bar)?;
        }

        Ok((self.n_bars - bars_before, self.total_duration_ms - dur_before))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn config_validation() {
        assert!(RenkoConfig::default().validate().is_ok());
        let bad = RenkoConfig { multiplier: 0.0, ..Default::default() };
        assert!(bad.validate().is_err());
    }

    #[test]
    fn config_id() {
        let config = RenkoConfig { multiplier: 0.075, min_pct: 0.000830, ..Default::default() };
        assert_eq!(config.id(), "m0750_mp0830");
    }
}
