//! Adaptive Renko bar generation — unified engine shared by live + offline.
//!
//! Brick size formula:
//!   b_t = p_t * max(multiplier * sigma_pct, min_pct)
//!
//! where `sigma_pct` is provided by the caller per tick (or per 30-min bin)
//! and corresponds to the calibrator-aligned Parkinson σ at 30-min horizon.
//!
//! The σ source is the caller's concern — offline callers can resolve it
//! from a memory-mapped `.vol` file via [`crate::parkinson::MtfParkinsonCalculator`];
//! live callers maintain a Δt-weighted EWMA. The engine itself does not
//! consult any [`crate::parkinson::VolSource`] — it takes a number.
//!
//! NO upper ceiling on brick % — operator directive 2026-05-24 ("markets be
//! markets"): an adaptive Renko that caps brick % on high-σ days biases
//! calibration's binary search downward (the search assumes the clamp does
//! not fire). The only remaining safety is `min_pct` (floor against
//! div-by-zero / sigma=0).
//!
//! ## Multi-brick tick semantics
//!
//! When a single tick crosses `N >= 2` brick boundaries, brick #1 carries
//! the full microstructure aggregate (BarAccumulator flush) and bricks 2..N
//! are emitted with `tick_count = 0` AND the [`crate::shard::FLAG_RENKO_SYNTHETIC_BRICK`]
//! flag set on `Bar.flags`. Consumers that only care about price-grid
//! geometry can ignore the flag; consumers training models on tick density
//! / spread / OFI should filter `(flags & FLAG_RENKO_SYNTHETIC_BRICK) == 0`.
//!
//! Design:
//!   * Streaming, never holds all bars in RAM
//!   * Continuity invariants enforced (single-sided wick, open[i]=close[i-1])
//!   * Emits `mitch::Bar` with `kind = BarKind::Renko as u8`.

use anyhow::Result;
use mitch::bar::{Bar, BarKind};
use mitch::timestamp;
use serde::{Deserialize, Serialize};

use crate::bar_builder::BarAccumulator;
use crate::grid::{grid_step_for_brick, snap_to_25_grid, snap_to_grid};
use crate::shard::FLAG_RENKO_SYNTHETIC_BRICK;

/// Adaptive Renko configuration.
///
/// `multiplier` controls bars/day via `brick_pct = multiplier * sigma_pct`
/// (auto-calibrated via target bars/day). `min_pct` is a safety floor.
/// No upper ceiling — see module docstring.
#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
pub struct RenkoConfig {
    pub multiplier: f32,
    pub min_pct: f32,
}

impl Default for RenkoConfig {
    fn default() -> Self {
        Self { multiplier: 0.075, min_pct: 0.0001 }
    }
}

impl RenkoConfig {
    pub fn validate(&self) -> Result<()> {
        // Upper bound mirrors `CalibrationConfig.mult_bounds[1]` (default 4.0):
        // binary search may legitimately converge above 1.0 on high-vol regimes
        // (e.g. SOL/USDT 2026-05-20: geo_mean=1.18 → emitted bricks=298 bpd,
        // err=0.4%). Capping at 1.0 here aborted the run entirely. Operator
        // policy: "markets be markets" — let k float to whatever the data
        // requires. The floor stays at 0.001 (guards against k=0 degenerate).
        if !(0.001..=4.0).contains(&self.multiplier) {
            anyhow::bail!("multiplier out of range: {}", self.multiplier);
        }
        if self.min_pct < 0.0 {
            anyhow::bail!("min_pct must be >= 0");
        }
        Ok(())
    }
}

/// Lower floor on effective k. A k below this is treated as a calibration
/// failure (boundary-clamp from degenerate σ — see audit 2026-05-26).
/// Single tripwire shared by live producer and offline calibrator.
pub const K_FLOOR: f64 = 0.05;

/// Renko brick-pct safety floor. Guards against σ=0 → div-by-zero / zero
/// brick degenerate. Mirrors `RenkoConfig::default().min_pct` so live +
/// offline + calibration share the same floor. Canonical home — promote
/// any local copies to this constant.
pub const MIN_BRICK_PCT: f64 = 0.0001;

/// Cap on bricks per single tick. Post Phase 58.L.1: σ scale is bounded by
/// calibration; 1 000 bricks/tick implies a 100 000% move relative to the
/// brick floor — impossible in any real market regime. Defensive guard.
/// Canonical home — `core::bars_renko*` and `series-factory::synth_backfill`
/// import this; no local copies allowed.
pub const MAX_BRICKS_PER_TICK: usize = 1_000;

/// Streaming Renko bar generator with adaptive brick sizing.
///
/// The engine is σ-agnostic: callers pass `sigma_pct` per tick (or per
/// 30-min bin, refreshed on the same cadence the calibrator uses). The
/// engine recomputes `brick_size = ref_price * max(multiplier * sigma_pct,
/// min_pct)` lazily whenever `sigma_pct` changes meaningfully — typically
/// on every call, since the math is cheap and the comparison overhead is
/// not worth optimising further.
///
/// Emits `mitch::Bar` with `kind = BarKind::Renko as u8`. Microstructure
/// fields (realized_var, bipower_var, drift, vol_imbalance, avg_spread_bps,
/// max_abs_return, avg_ci_ubp, reject_rate) are populated when callers feed
/// full IndexRecord context via `feed_index_record(...)`. The lighter
/// `feed_tick_with_sigma(ts, mid, sigma_pct, ...)` path (used by the offline
/// calibrator for fast brick counting) leaves microstructure at zero —
/// calibration only cares about brick count, not enrichment.
pub struct RenkoGenerator {
    config: RenkoConfig,
    current_brick_size: f64,
    /// Grid step derived from `current_brick_size`; recomputed only when the
    /// brick size changes (every 30 min or on init), so the hot path reads it
    /// with no arithmetic.
    current_grid_step: f64,
    last_recompute_period: i64,
    initialized: bool,
    last_close: f64,
    pending_high: f64,
    pending_low: f64,
    bar_start_ts: i64,
    tick_count: u32,
    n_bars: usize,
    /// Microstructure accumulator. Populated only via `feed_index_record`;
    /// flushed at every brick emit. The legacy `feed_tick_with_sigma`
    /// path (offline calibration) leaves this empty — emit_bar then writes
    /// zero micros, which is acceptable since calibration discards Bar bodies.
    acc: BarAccumulator,
}

impl RenkoGenerator {
    pub fn new(config: RenkoConfig) -> Result<Self> {
        config.validate()?;
        Ok(Self {
            config,
            current_brick_size: 0.0,
            current_grid_step: 0.0,
            last_recompute_period: i64::MIN,
            initialized: false,
            last_close: 0.0,
            pending_high: 0.0,
            pending_low: 0.0,
            bar_start_ts: 0,
            tick_count: 0,
            n_bars: 0,
            acc: BarAccumulator::new(),
        })
    }

    /// Cumulative bar count.
    pub fn n_bars(&self) -> usize {
        self.n_bars
    }

    /// Current brick size in absolute price units (post snap_to_25_grid).
    /// Used by the live wrapper to seed continuity after a restart.
    #[inline]
    pub fn current_brick_size(&self) -> f64 {
        self.current_brick_size
    }

    /// Current anchor close (price at the last emitted brick close, or the
    /// initialisation seed for the very first brick). Used by the live
    /// wrapper to surface `last_close` to its broadcast/multicast layer.
    #[inline]
    pub fn last_close(&self) -> f64 {
        self.last_close
    }

    /// Force-seed `last_close` and `initialized=true`. Used on warm restart:
    /// the wrapper reads the previous brick close off disk and primes the
    /// engine so the first post-restart tick can immediately decide whether
    /// it crosses a brick boundary. `brick_size` is recomputed lazily on the
    /// next tick (when σ is consulted).
    pub fn seed_last_close(&mut self, last_close: f64) {
        self.last_close = last_close;
        self.pending_high = last_close;
        self.pending_low = last_close;
        self.initialized = last_close > 0.0;
    }

    fn compute_brick_size(&mut self, price: f64, timestamp_ms: i64, sigma_pct: f64) -> f64 {
        // K_FLOOR defends against boundary-clamped k from a degenerate
        // calibration (see docs/internal/renko-synth-audit-2026-05-26.md).
        let k_eff = (self.config.multiplier as f64).max(K_FLOOR);
        let raw_pct = k_eff * sigma_pct;
        // Floor only — no ceiling (markets be markets, see module doc).
        let clamped_pct = raw_pct.max(self.config.min_pct as f64);
        let raw_brick = price * clamped_pct;
        let brick_size = snap_to_25_grid(raw_brick);
        self.current_brick_size = brick_size;
        self.current_grid_step = grid_step_for_brick(brick_size);
        self.last_recompute_period = timestamp_ms / 1_800_000;
        brick_size
    }

    #[inline]
    fn emit_bar<F>(
        &mut self,
        open_ts: i64,
        close_ts: i64,
        open: f64,
        high: f64,
        low: f64,
        close: f64,
        tick_count: u32,
        first_in_seq: bool,
        write_bar: &mut F,
    ) -> Result<()>
    where
        F: FnMut(&Bar) -> Result<()>,
    {
        let open_mts = timestamp::from_epoch_ms(open_ts);
        let close_mts = timestamp::from_epoch_ms(close_ts);
        // Pull microstructure from the accumulator if it has been fed via
        // `feed_index_record`. First-brick-of-sequence carries the full
        // accumulator snapshot (many ticks); subsequent bricks in the same
        // multi-brick tick fire with `acc.count() == 0` and emit zero micros
        // (the tick block already collapsed into brick #1's stats).
        let micros = if self.acc.count() > 0 {
            self.acc.flush()
        } else {
            None
        };
        let (vbid, vask) = if let Some(b) = micros.as_ref() {
            (b.vbid, b.vask)
        } else {
            (0u32, 0u32)
        };
        let mut bar = Bar::new_ohlcv(open_mts, close_mts, open, high, low, close, vbid, vask, tick_count);
        if let Some(b) = micros.as_ref() {
            bar.realized_var = b.realized_var;
            bar.bipower_var = b.bipower_var;
            bar.drift = b.drift;
            bar.vol_imbalance = b.vol_imbalance;
            bar.avg_spread_bps = b.avg_spread_bps;
            bar.max_abs_return = b.max_abs_return;
            bar.avg_ci_ubp = b.avg_ci_ubp;
            bar.reject_rate = b.reject_rate;
        }
        bar.kind = BarKind::Renko as u8;
        if !first_in_seq {
            // Synthetic brick 2..N within one multi-brick tick — tag so
            // consumers can filter from microstructure-sensitive analyses.
            bar.flags |= FLAG_RENKO_SYNTHETIC_BRICK;
        }
        write_bar(&bar)?;
        self.n_bars += 1;
        Ok(())
    }

    /// Feed one IndexRecord-derived observation. Drives brick detection AND
    /// accumulates microstructure (RV/BV/drift/OFI/spread/quality) so the
    /// emitted Bar carries the enrichment block.
    pub fn feed_index_record<F>(
        &mut self,
        ts: i64,
        bid: f64,
        ask: f64,
        vbid: u32,
        vask: u32,
        ci_ubp: f64,
        accepted: u32,
        rejected: u32,
        sigma_pct: f64,
        write_bar: &mut F,
    ) -> Result<()>
    where
        F: FnMut(&Bar) -> Result<()>,
    {
        let mid = (bid + ask) * 0.5;
        if !(mid.is_finite() && mid > 0.0) {
            return Ok(());
        }
        // Ingest into the accumulator BEFORE brick detection so the closing
        // tick is included in the closing brick's microstructure stats.
        self.acc.ingest(bid, ask, vbid, vask, ts, ci_ubp, accepted, rejected);
        self.feed_tick_with_sigma(ts, mid, sigma_pct, write_bar)
    }

    /// Feed one tick with an explicit σ_pct, emitting any produced bars via
    /// the callback. `sigma_pct` is the calibrator-aligned 30-min Parkinson
    /// σ (or its live EWMA equivalent) as a fraction of price.
    #[inline]
    pub fn feed_tick_with_sigma<F>(
        &mut self,
        ts: i64,
        price: f64,
        sigma_pct: f64,
        write_bar: &mut F,
    ) -> Result<()>
    where
        F: FnMut(&Bar) -> Result<()>,
    {
        if !self.initialized {
            self.compute_brick_size(price, ts, sigma_pct);
            self.last_close = snap_to_grid(price, self.current_grid_step);
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
            self.compute_brick_size(price, ts, sigma_pct);
            self.last_close = snap_to_grid(self.last_close, self.current_grid_step);
        }

        let sz = self.current_brick_size;
        if sz <= 0.0 || !sz.is_finite() || !price.is_finite() || price <= 0.0 {
            return Ok(());
        }

        self.pending_high = self.pending_high.max(price);
        self.pending_low = self.pending_low.min(price);

        let grid = self.current_grid_step;

        let mut bricks_this_tick = 0usize;

        let mut first_in_seq = true;
        while price - self.last_close >= sz {
            let close = snap_to_grid(self.last_close + sz, grid);
            if close <= self.last_close || bricks_this_tick >= MAX_BRICKS_PER_TICK {
                break;
            }
            bricks_this_tick += 1;

            let low = if first_in_seq { self.pending_low.min(self.last_close) } else { self.last_close };
            let tick_count_for_bar = if first_in_seq { self.tick_count } else { 0 };
            self.emit_bar(self.bar_start_ts, ts, self.last_close, close, low, close, tick_count_for_bar, first_in_seq, write_bar)?;

            first_in_seq = false;
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

            let high = if first_in_seq { self.pending_high.max(self.last_close) } else { self.last_close };
            let tick_count_for_bar = if first_in_seq { self.tick_count } else { 0 };
            self.emit_bar(self.bar_start_ts, ts, self.last_close, high, close, close, tick_count_for_bar, first_in_seq, write_bar)?;

            first_in_seq = false;
            self.last_close = close;
            self.pending_high = close;
            self.pending_low = close;
            self.bar_start_ts = ts;
            self.tick_count = 0;
        }

        Ok(())
    }

    /// Feed many `(ts, mid, sigma_pct)` observations from an iterator.
    pub fn generate<F>(
        &mut self,
        iter: impl Iterator<Item = (i64, f64, f64)>,
        mut write_bar: F,
    ) -> Result<usize>
    where
        F: FnMut(&Bar) -> Result<()>,
    {
        let bars_before = self.n_bars;
        for (ts, price, sigma) in iter {
            self.feed_tick_with_sigma(ts, price, sigma, &mut write_bar)?;
        }
        Ok(self.n_bars - bars_before)
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
        // min_pct=0 is now allowed (no floor); negative is still rejected.
        let zero_floor = RenkoConfig { multiplier: 0.075, min_pct: 0.0 };
        assert!(zero_floor.validate().is_ok());
        let neg = RenkoConfig { multiplier: 0.075, min_pct: -1e-6 };
        assert!(neg.validate().is_err());
    }

    #[test]
    fn synthetic_brick_flag_on_multi_brick_tick() {
        // Construct a generator that will fire multiple bricks on a single
        // tick (brick_size set very small relative to the price jump).
        let cfg = RenkoConfig { multiplier: 0.075, min_pct: 0.0001 };
        let mut r = RenkoGenerator::new(cfg).expect("generator new");
        let sigma_pct = 0.001; // 0.1% σ

        let mut bars: Vec<Bar> = Vec::new();
        // Seed at p=100. First tick initialises.
        r.feed_tick_with_sigma(0, 100.0, sigma_pct, &mut |b| { bars.push(*b); Ok(()) }).unwrap();
        // Jump to p=110: ~10% move, brick_size ≈ price * 0.075 * 0.001 ≈ 0.0075
        // (floored at min_pct=0.0001 → brick ≈ 0.01). Either way produces many bricks.
        r.feed_tick_with_sigma(1_000, 110.0, sigma_pct, &mut |b| { bars.push(*b); Ok(()) }).unwrap();

        assert!(bars.len() >= 2, "expected multi-brick emission, got {}", bars.len());
        // Brick #1 should not have the synthetic flag set; subsequent bricks
        // emitted within the same tick should.
        assert_eq!(bars[0].flags & FLAG_RENKO_SYNTHETIC_BRICK, 0,
            "first brick must not carry synthetic flag");
        for (i, b) in bars.iter().enumerate().skip(1) {
            assert_ne!(b.flags & FLAG_RENKO_SYNTHETIC_BRICK, 0,
                "brick #{} (synthetic in multi-brick tick) must carry flag", i);
        }
    }
}
