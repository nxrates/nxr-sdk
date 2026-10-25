//! Pluggable source for synth `IndexRecord` streams (W7).
//!
//! Operator decision 2026-05-28: synth `.idx` is NOT persisted to disk
//! (cost: ~2.6 MB/min per pair @ 5 Hz × 8B × 9 fields, plus the daily
//! shard fsync churn). Today-only intra-day replay is served from an
//! in-RAM ring populated by the live kernel; prior days are served by
//! re-deriving from leg `.idx` shards on demand.
//!
//! ## Shape
//!
//! [`EphemeralIdxSource`]: in-RAM ring (today's records). Prior days are
//! served by re-deriving from leg `.idx` shards on demand (future work:
//! `ShardedIdxSource` / `UnifiedIdxSource`).
//!
//! ## Capacity sizing
//!
//! Default capacity = 24 h × 3600 s × 5 Hz × 1.5 headroom = 648 000
//! records per synth_id. Per-record size = 56 B ⇒ 36 MB / synth at full
//! capacity. With 10 configured pairs that's 360 MB worst-case — fits in
//! a NXR pod easily and is bounded.

use std::collections::VecDeque;
use std::sync::{Mutex, MutexGuard, PoisonError};

use crate::IndexRecord;

/// Default ring capacity per `EphemeralIdxSource` (≈ 24h × 5 Hz × 1.5).
pub const DEFAULT_EPHEMERAL_CAPACITY: usize = 648_000;

/// In-RAM today-ring for synth `IndexRecord`s.
///
/// Bounded by `capacity` (default [`DEFAULT_EPHEMERAL_CAPACITY`]). When full,
/// `push` evicts the oldest record (FIFO) — matches the operator's
/// "today-only" semantic.
///
/// ## Concurrency
///
/// Inner state is wrapped in a `Mutex<VecDeque<IndexRecord>>`. Hot-path
/// callers (the synth kernel pushing one record per tick) take the lock for
/// ~tens of ns; the REST handler reading a range takes the lock for the
/// duration of the iteration. For the launch pair count (≤ 10) at ≤ 5 Hz
/// per pair, contention is far below saturation. A lock-free ring is a
/// reasonable future optimisation but not required for correctness.
pub struct EphemeralIdxSource {
    inner: Mutex<VecDeque<IndexRecord>>,
    capacity: usize,
}

impl EphemeralIdxSource {
    /// Take the ring, recovering a poisoned lock.
    ///
    /// This ring is written by the emit path and read by REST. A panic while
    /// the lock is held poisons it, and the old `expect` then panicked on EVERY
    /// later access — turning one bad handler into a dead aggregator. A
    /// poisoned `VecDeque` is still structurally valid (push/pop cannot publish
    /// a half-updated deque), so reuse it.
    fn lock(&self) -> MutexGuard<'_, VecDeque<IndexRecord>> {
        self.inner.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Create a new ring with the given capacity. Panics if `capacity == 0`.
    pub fn with_capacity(capacity: usize) -> Self {
        assert!(capacity > 0, "EphemeralIdxSource capacity must be > 0");
        // Lazy allocation: rings are created per ticker (thousands of natives
        // + synths); an eager `with_capacity` reserved capacity×56B per ring
        // up front (36 MB/synth, 200 KB/native) before a single record landed.
        Self {
            inner: Mutex::new(VecDeque::new()),
            capacity,
        }
    }

    /// Create a new ring with the default capacity.
    pub fn new() -> Self {
        Self::with_capacity(DEFAULT_EPHEMERAL_CAPACITY)
    }

    /// Push one record. If the ring is at capacity, the oldest record is
    /// evicted FIFO. Records are pushed in arrival order; callers SHOULD
    /// push in monotonically non-decreasing `ts_ms` for the `iter_range`
    /// contract to hold (the live kernel does this by construction).
    pub fn push(&self, rec: IndexRecord) {
        let mut g = self.lock();
        if g.len() >= self.capacity {
            g.pop_front();
        }
        g.push_back(rec);
    }

    /// [`push`](Self::push) + evict-by-age: drops front records older than
    /// `window_ms` behind `rec`'s own timestamp (callers push in ts order),
    /// then applies the count cap. Bounds memory by true emit volume inside
    /// the window instead of the count capacity alone — quiet tickers hold
    /// a handful of heartbeats, not `capacity` records accumulated over days.
    pub fn push_windowed(&self, rec: IndexRecord, window_ms: i64) {
        let cutoff = mitch::timestamp::to_epoch_ms(rec.header.get_timestamp()) - window_ms;
        let mut g = self.lock();
        while let Some(front) = g.front() {
            if mitch::timestamp::to_epoch_ms(front.header.get_timestamp()) < cutoff {
                g.pop_front();
            } else {
                break;
            }
        }
        if g.len() >= self.capacity {
            g.pop_front();
        }
        g.push_back(rec);
    }

    /// Current ring length (for metrics + tests).
    pub fn len(&self) -> usize {
        self.lock().len()
    }

    /// True iff the ring is empty.
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Capacity (configured upper bound).
    pub fn capacity(&self) -> usize {
        self.capacity
    }

    /// Yield every record in `[from_ms, to_ms]` (inclusive) in ascending
    /// timestamp order. Allocates: clones matching records out of the
    /// mutex-guarded ring so the lock is released quickly; callers should
    /// consume promptly, or use [`iter_range_limited`](Self::iter_range_limited)
    /// when they only need the first `limit` records.
    pub fn iter_range(&self, from_ms: i64, to_ms: i64) -> Vec<IndexRecord> {
        self.iter_range_limited(from_ms, to_ms, usize::MAX)
    }

    /// [`iter_range`](Self::iter_range) with a hard cap on the records copied.
    ///
    /// `/v1/ohlc` and `/v1/idx` ask for a small `limit` but used to copy the
    /// whole window (a full day is ~648k × 56 B = 36 MB) out of the ring, while
    /// holding the same lock the emit path needs. Pass the caller's `limit` here
    /// and the copy, the allocation and the lock hold are all bounded by it.
    ///
    /// The lock is held for the copy only, and the bounds are FOUND by search
    /// rather than by walking the day: the ring is time-ordered because
    /// `push`/`push_windowed` append in arrival order and the live kernel emits
    /// monotonically non-decreasing `mts` (see [`push`](Self::push)). A caller
    /// that violates that contract gets the same answer the previous
    /// front-to-back scan gave — it also stopped at the first record past
    /// `to_ms` — never a wrong one silently.
    pub fn iter_range_limited(&self, from_ms: i64, to_ms: i64, limit: usize) -> Vec<IndexRecord> {
        if limit == 0 || from_ms > to_ms {
            return Vec::new();
        }
        let g = self.lock();
        let start = g.partition_point(|r| rec_ts_ms(r) < from_ms);
        let take = limit.min(g.len() - start);
        let mut out: Vec<IndexRecord> = Vec::with_capacity(take);
        for rec in g.range(start..) {
            if rec_ts_ms(rec) > to_ms || out.len() >= limit {
                break;
            }
            out.push(*rec);
        }
        out
    }
}

/// Record timestamp in epoch ms (the unit both bounds and the ring contract use).
#[inline]
fn rec_ts_ms(rec: &IndexRecord) -> i64 {
    mitch::timestamp::to_epoch_ms(rec.header.get_timestamp())
}

impl Default for EphemeralIdxSource {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use mitch::header::MitchHeader;
    use mitch::index::Index;

    fn mk_rec(ticker: u64, ts_ms: i64) -> IndexRecord {
        let mts = mitch::timestamp::from_epoch_ms(ts_ms);
        let header = MitchHeader::new(
            mitch::common::message_type::INDEX,
            crate::synth::SYNTH_KERNEL_PROVIDER_ID,
            mts,
            1,
        );
        let idx = Index {
            ticker,
            bid: 1.0,
            ask: 1.01,
            vbid: 100,
            vask: 100,
            ci: 0,
            tick_count: 1,
            confidence: 3,
            accepted: 3,
            rejected: 0,
            flags: 0,
        };
        IndexRecord::new(header, idx)
    }

    #[test]
    fn capacity_evicts_oldest() {
        let s = EphemeralIdxSource::with_capacity(3);
        // MITCH mts has 16 µs resolution after `from_epoch_ms`, so use ≥16ms
        // strides to keep ts roundtrip exact (assertions below depend on it).
        let t0 = 1_700_000_000_000_i64;
        let step = 32_i64;
        for i in 0..5_i64 {
            s.push(mk_rec(42, t0 + i * step));
        }
        assert_eq!(s.len(), 3);
        // First two evicted; remaining ts = t0+2step, t0+3step, t0+4step.
        let v: Vec<IndexRecord> = s.iter_range(i64::MIN, i64::MAX);
        assert_eq!(v.len(), 3);
        let first_ts = mitch::timestamp::to_epoch_ms(v[0].header.get_timestamp());
        let last_ts = mitch::timestamp::to_epoch_ms(v[2].header.get_timestamp());
        assert_eq!(first_ts, t0 + 2 * step);
        assert_eq!(last_ts, t0 + 4 * step);
    }

    #[test]
    fn iter_range_clips_inclusive() {
        let s = EphemeralIdxSource::with_capacity(10);
        let t0 = 1_700_000_000_000_i64;
        let step = 32_i64;
        for i in 0..10_i64 {
            s.push(mk_rec(42, t0 + i * step));
        }
        // [t0+3step, t0+5step] should yield 3 records (i=3, 4, 5).
        let v: Vec<IndexRecord> = s.iter_range(t0 + 3 * step, t0 + 5 * step);
        assert_eq!(v.len(), 3, "expected 3 records in [3step, 5step]");
        let ts0 = mitch::timestamp::to_epoch_ms(v[0].header.get_timestamp());
        let ts2 = mitch::timestamp::to_epoch_ms(v[2].header.get_timestamp());
        assert_eq!(ts0, t0 + 3 * step);
        assert_eq!(ts2, t0 + 5 * step);
    }

    #[test]
    fn push_windowed_evicts_by_age_and_count() {
        let s = EphemeralIdxSource::with_capacity(4);
        let t0 = 1_700_000_000_000_i64;
        let step = 32_i64;
        // Window = 2 steps: each push drops records older than ts-2step.
        for i in 0..6_i64 {
            s.push_windowed(mk_rec(42, t0 + i * step), 2 * step);
        }
        // Age window keeps i=3..5 (ts >= t5 - 2step); count cap (4) not hit.
        let v: Vec<IndexRecord> = s.iter_range(i64::MIN, i64::MAX);
        assert_eq!(v.len(), 3);
        let first_ts = mitch::timestamp::to_epoch_ms(v[0].header.get_timestamp());
        assert_eq!(first_ts, t0 + 3 * step);
        // Count cap still binds with a wide window.
        let s2 = EphemeralIdxSource::with_capacity(2);
        for i in 0..5_i64 {
            s2.push_windowed(mk_rec(42, t0 + i * step), i64::MAX / 2);
        }
        assert_eq!(s2.len(), 2);
    }

    #[test]
    fn empty_range_returns_zero() {
        let s = EphemeralIdxSource::with_capacity(10);
        let v: Vec<IndexRecord> = s.iter_range(0, 1_000);
        assert!(v.is_empty());
    }

    /// The bounded form must copy only what was asked for: `/v1/idx` passes the
    /// caller's `limit`, and a full-day ring is ~36 MB of records.
    #[test]
    fn iter_range_limited_stops_at_limit() {
        let s = EphemeralIdxSource::with_capacity(100);
        let t0 = 1_700_000_000_000_i64;
        let step = 32_i64;
        for i in 0..100_i64 {
            s.push(mk_rec(42, t0 + i * step));
        }
        let v = s.iter_range_limited(i64::MIN, i64::MAX, 7);
        assert_eq!(v.len(), 7, "must not copy the window it was not asked for");
        assert_eq!(
            mitch::timestamp::to_epoch_ms(v[6].header.get_timestamp()),
            t0 + 6 * step,
            "and it must be the OLDEST 7, in order"
        );
        assert!(s.iter_range_limited(t0, t0 + step, 0).is_empty());
    }

    /// Bounds are found by search, so a window in the middle and a window with
    /// no records must both land exactly where the old front-to-back scan did.
    #[test]
    fn iter_range_bounds_are_exact_for_mid_windows() {
        let s = EphemeralIdxSource::with_capacity(50);
        let t0 = 1_700_000_000_000_i64;
        let step = 32_i64;
        for i in 0..50_i64 {
            s.push(mk_rec(42, t0 + i * step));
        }
        // Window inside the ring, both ends inclusive, one record wide.
        let one = s.iter_range(t0 + 20 * step, t0 + 20 * step);
        assert_eq!(one.len(), 1);
        // Window entirely past the newest record.
        assert!(s.iter_range(t0 + 1_000 * step, t0 + 2_000 * step).is_empty());
        // Window entirely before the oldest record.
        assert!(s.iter_range(t0 - 1_000 * step, t0 - step).is_empty());
        // Open-ended from a mid bound to the newest.
        let tail = s.iter_range(t0 + 47 * step, i64::MAX);
        assert_eq!(tail.len(), 3);
    }

    /// A panic while the lock is held must not wedge the ring: the emit path
    /// would panic on every later push (the old `expect("ring lock poisoned")`).
    #[test]
    fn a_poisoned_ring_still_serves_and_accepts() {
        let s = std::sync::Arc::new(EphemeralIdxSource::with_capacity(8));
        let t0 = 1_700_000_000_000_i64;
        s.push(mk_rec(42, t0));
        let poisoner = std::sync::Arc::clone(&s);
        // Panic while holding the guard, which is what poisons the mutex.
        let _ = std::thread::spawn(move || {
            let _g = poisoner.inner.lock().expect("fresh lock");
            panic!("poison the ring");
        })
        .join();
        assert!(s.inner.is_poisoned(), "precondition: the test poisoned it");
        s.push(mk_rec(42, t0 + 32));
        assert_eq!(s.len(), 2);
        assert_eq!(s.iter_range(i64::MIN, i64::MAX).len(), 2);
    }
}
