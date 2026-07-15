//! Shared aggregation primitives used by all forwarders and the sink.
//!
//! - `TickAccumulator`: buffers raw ticks, flushes to `Index` every aggregation cycle
//! - `RunningStats`: EMA-based z-score for outlier rejection
//! - `is_valid_tick`: sanity check on bid/ask
//! - Timestamp helpers: `now_ns`, `now_mts`, `now_ms`, `now_sec`

use mitch::Index;

// ---- Timestamp helpers ----

#[inline]
fn since_epoch() -> std::time::Duration {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
}

/// Current epoch time in nanoseconds.
#[inline]
pub fn now_ns() -> i64 {
    since_epoch().as_nanos() as i64
}

/// Current epoch time in milliseconds.
#[inline]
pub fn now_ms() -> u64 {
    since_epoch().as_millis() as u64
}

/// Current epoch time in seconds.
#[inline]
pub fn now_sec() -> u64 {
    since_epoch().as_secs()
}

/// Current time as mts (MITCH timestamp: 16 us intervals since 2010).
#[inline]
pub fn now_mts() -> u64 {
    mitch::timestamp::from_epoch_ns(now_ns())
}

// ---- Quality gate ----

/// Basic tick sanity check: positive prices, ask >= bid.
#[inline]
pub fn is_valid_tick(bid: f64, ask: f64) -> bool {
    bid > 0.0 && ask > 0.0 && ask >= bid
}

// ---- Outlier detection ----

/// EMA-based running statistics for z-score outlier rejection.
///
/// Uses exponential moving average (alpha = 0.01, ~100-tick effective window) instead
/// of all-time Welford accumulation. This prevents variance from becoming pathologically
/// tight after days of uptime, which would reject legitimate price moves during
/// news events as outliers.
pub struct RunningStats {
    ema_mean: f64,
    ema_var: f64,
    count: u64,
}

const EMA_ALPHA: f64 = 0.01;

impl Default for RunningStats {
    fn default() -> Self {
        Self { ema_mean: 0.0, ema_var: 0.0, count: 0 }
    }
}

impl RunningStats {
    /// Update with a new mid price. Returns z-score (0.0 during warmup, count < 10).
    #[inline]
    pub fn update(&mut self, mid: f64) -> f64 {
        if self.count == 0 {
            self.ema_mean = mid;
            self.ema_var = 0.0;
            self.count = 1;
            return 0.0;
        }

        let delta = mid - self.ema_mean;
        let z = if self.count < 10 {
            0.0
        } else {
            let stddev = self.ema_var.sqrt();
            if stddev < 1e-12 { 0.0 } else { delta.abs() / stddev }
        };

        self.ema_mean += EMA_ALPHA * delta;
        self.ema_var = (1.0 - EMA_ALPHA) * (self.ema_var + EMA_ALPHA * delta * delta);
        self.count += 1;
        z
    }
}

// ---- Tick accumulator ----

/// Buffers raw ticks and flushes to an `Index` every aggregation cycle.
///
/// Used by forwarders (nxr-crypto, nxr-oracle) for per-(provider, ticker) local
/// aggregation. Each cycle: N raw ticks are accumulated, then `flush()`
/// produces a single `Index` carrying the LATEST bid/ask and summed volumes.
///
/// Latest, not window mean (audit 2026-07-15): the mean put every published
/// mark at the window centroid — ~half an aggregation interval stale AT
/// emission (~100ms at 200ms cadence), the single largest structural latency
/// term vs consuming a venue's book stream directly. The window still
/// contributes tick_count/volumes/rejected for audit; cross-venue smoothing
/// happens downstream in the sink's TDWAP, which is its job.
pub struct TickAccumulator {
    ticker: u64,
    last_bid: f64,
    last_ask: f64,
    acc_bid_vol: u64,
    acc_ask_vol: u64,
    acc_count: u32,
    /// Ticks rejected by the caller's pre-filter (e.g. z-score gate) during
    /// this window. Reported in `Index.rejected` on flush, clamped to u8.
    acc_rejected: u32,
}

impl TickAccumulator {
    pub fn new(ticker: u64) -> Self {
        Self {
            ticker,
            last_bid: 0.0,
            last_ask: 0.0,
            acc_bid_vol: 0,
            acc_ask_vol: 0,
            acc_count: 0,
            acc_rejected: 0,
        }
    }

    /// Buffer a single raw tick's values (price = last-write-wins).
    #[inline]
    pub fn ingest(&mut self, bid: f64, ask: f64, vbid: u32, vask: u32) {
        self.last_bid = bid;
        self.last_ask = ask;
        self.acc_bid_vol += vbid as u64;
        self.acc_ask_vol += vask as u64;
        self.acc_count += 1;
    }

    /// Record that a raw tick was dropped by the caller's pre-filter
    /// (e.g. z-score outlier gate). Flushed into `Index.rejected` (u8, saturating).
    /// Conservative by design: only counts upstream rejections that the caller
    /// chose to route through this method.
    #[inline]
    pub fn reject(&mut self) {
        self.acc_rejected = self.acc_rejected.saturating_add(1);
    }

    /// Emit the window's LATEST quote as an `Index` and reset the window
    /// counters. Returns `None` if no ticks arrived since the last flush.
    /// Rejected-count also resets each cycle, so `Index.rejected` reflects
    /// outliers rejected in the window ending at this flush.
    pub fn flush(&mut self) -> Option<Index> {
        if self.acc_count == 0 {
            // Also reset rejected so a silent window does not carry stale counts.
            self.acc_rejected = 0;
            return None;
        }
        let rejected = self.acc_rejected.min(u8::MAX as u32) as u8;
        let index = Index {
            ticker: self.ticker,
            bid: self.last_bid,
            ask: self.last_ask,
            vbid: self.acc_bid_vol.min(u32::MAX as u64) as u32,
            vask: self.acc_ask_vol.min(u32::MAX as u64) as u32,
            ci: 0,
            tick_count: self.acc_count.min(u16::MAX as u32) as u16,
            confidence: 1,
            accepted: 1,
            rejected,
            flags: 0,
        };
        self.acc_bid_vol = 0;
        self.acc_ask_vol = 0;
        self.acc_count = 0;
        self.acc_rejected = 0;
        Some(index)
    }
}
