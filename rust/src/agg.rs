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

/// THE spread ceiling of the pipeline, in basis points: a CORRUPTION guard,
/// not a market filter. Mirrors `mitch::Index::reject_reason`'s `spread_cap`,
/// so a tick, a converted quote and a composite are all held to one number.
/// Anything wider is a parse fault (wrong field read, two different books'
/// sides, a price and a quantity swapped), never a quote: `1e-12 / 1e6` used to
/// pass and become a provider's mid. Market width is bounded by the class band
/// (`aggregator::quote_bands`) and priced by the kernel, never cut here.
pub const MAX_SPREAD_BPS: f64 = 2_000.0;

/// Basic tick sanity check: finite, positive prices, uncrossed, and not
/// absurdly wide.
///
/// `bid == ask` is ACCEPTED and must stay accepted: trades-only venues and the
/// oracle relays publish `bid == ask == px` and are marked with
/// `FLAG_NO_BOOK` downstream (see `nxr_sdk::shard::FLAG_NO_BOOK`). Rejecting a
/// locked book here would dark every one of them.
#[inline]
pub fn is_valid_tick(bid: f64, ask: f64) -> bool {
    if !(bid.is_finite() && ask.is_finite() && bid > 0.0 && ask > 0.0 && ask >= bid) {
        return false;
    }
    let mid = (bid + ask) * 0.5;
    (ask - bid) / mid * 10_000.0 <= MAX_SPREAD_BPS
}

/// [`is_valid_tick`] for a venue ORDER BOOK: additionally refuses a locked
/// book (`bid == ask`). Only trades-only and oracle feeds legitimately publish
/// `bid == ask`; a CEX top of book that locks is a mid-update or stale-cross
/// frame, and admitting it hands the kernel a zero spread (floored, but still
/// the tightest leg in the blend).
#[inline]
pub fn is_valid_book(bid: f64, ask: f64) -> bool {
    is_valid_tick(bid, ask) && ask > bid
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
    /// Every tick this window was a [`Self::reaffirm`]: flushed as
    /// [`crate::shard::FLAG_REAFFIRM`].
    acc_reaffirm_only: bool,
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
            acc_reaffirm_only: true,
        }
    }

    /// Buffer a single raw tick's values (price = last-write-wins).
    ///
    /// ## `vbid` / `vask` unit contract — QUOTE NOTIONAL (changed 2026-07-26)
    ///
    /// Callers MUST pass top-of-book size as `round(price * quantity)`, not the
    /// base-asset quantity. `Index.vbid/vask` are `u32` and the 40 B wire record
    /// is frozen, so a base-quantity cast TRUNCATED toward zero: coinbase's
    /// `0.28772224` BTC became `0` while a sub-cent memecoin's `4.1e6` tokens
    /// stayed `4_100_000`. The field was therefore not comparable across assets
    /// and not summable — structurally broken, not merely scaled.
    ///
    /// Prices are unaffected either way: TDWAP never weights a provider by
    /// size (`crate::tdwap`), so no mark,
    /// `ci`, or signed quote moves with this. Producer of record is
    /// `crypto/src/client.rs::ingest_tick`; client-facing semantics are in
    /// `docs/api-v1.md#volume-units-vbid--vask`, which also states that history
    /// spanning the deploy carries both conventions with no per-row marker.
    #[inline]
    pub fn ingest(&mut self, bid: f64, ask: f64, vbid: u32, vask: u32) {
        self.last_bid = bid;
        self.last_ask = ask;
        self.acc_bid_vol += vbid as u64;
        self.acc_ask_vol += vask as u64;
        self.acc_count += 1;
        self.acc_reaffirm_only = false;
    }

    /// Buffer an unchanged quote the caller has PROVEN live (venue sequence
    /// advanced, size moved, or venue-wide activity). Same as [`Self::ingest`]
    /// except that a window holding only these flushes as a re-affirm.
    #[inline]
    pub fn reaffirm(&mut self, bid: f64, ask: f64, vbid: u32, vask: u32) {
        let only = self.acc_reaffirm_only;
        self.ingest(bid, ask, vbid, vask);
        self.acc_reaffirm_only = only;
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
        let flags = if self.acc_reaffirm_only {
            crate::shard::FLAG_REAFFIRM
        } else {
            0
        };
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
            flags,
        };
        self.acc_bid_vol = 0;
        self.acc_ask_vol = 0;
        self.acc_count = 0;
        self.acc_rejected = 0;
        self.acc_reaffirm_only = true;
        Some(index)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::shard::FLAG_REAFFIRM;

    #[test]
    fn locked_book_is_not_a_book() {
        assert!(is_valid_book(0.9992, 0.9993));
        assert!(!is_valid_book(1.0, 1.0), "locked");
        assert!(!is_valid_book(1.0, 0.9), "crossed");
        assert!(is_valid_tick(1.0, 1.0), "a trades-only tick stays valid");
    }

    #[test]
    fn a_window_of_only_reaffirms_flushes_flagged() {
        let mut acc = TickAccumulator::new(7);
        acc.reaffirm(1.0, 1.0001, 5, 5);
        assert_eq!(acc.flush().unwrap().flags, FLAG_REAFFIRM);
        acc.reaffirm(1.0, 1.0001, 5, 5);
        acc.ingest(1.0, 1.0002, 5, 5);
        assert_eq!(
            acc.flush().unwrap().flags,
            0,
            "a real tick in the window clears it"
        );
        acc.ingest(1.0, 1.0002, 5, 5);
        acc.reaffirm(1.0, 1.0002, 5, 5);
        assert_eq!(acc.flush().unwrap().flags, 0);
    }
}
