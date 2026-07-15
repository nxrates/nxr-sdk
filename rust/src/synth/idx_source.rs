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
use std::sync::Mutex;

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
        let mut g = self.inner.lock().expect("ring lock poisoned");
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
        let mut g = self.inner.lock().expect("ring lock poisoned");
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
        self.inner.lock().expect("ring lock poisoned").len()
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
    /// consume promptly.
    pub fn iter_range(
        &self,
        from_ms: i64,
        to_ms: i64,
    ) -> Box<dyn Iterator<Item = IndexRecord> + Send + '_> {
        // Snapshot under the lock: clone matching records out so the lock
        // is released quickly. For today's hot range that's at most ~648k
        // × 56 B = 36 MB worst-case; in practice REST calls request a much
        // narrower [from, to] window.
        let g = self.inner.lock().expect("ring lock poisoned");
        let mut out: Vec<IndexRecord> = Vec::new();
        for rec in g.iter() {
            let header = rec.header;
            let ts_mts = header.get_timestamp();
            let ts_ms = mitch::timestamp::to_epoch_ms(ts_mts);
            if ts_ms < from_ms {
                continue;
            }
            if ts_ms > to_ms {
                break;
            }
            out.push(*rec);
        }
        Box::new(out.into_iter())
    }
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
        let v: Vec<IndexRecord> = s.iter_range(i64::MIN, i64::MAX).collect();
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
        let v: Vec<IndexRecord> = s.iter_range(t0 + 3 * step, t0 + 5 * step).collect();
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
        let v: Vec<IndexRecord> = s.iter_range(i64::MIN, i64::MAX).collect();
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
        let v: Vec<IndexRecord> = s.iter_range(0, 1_000).collect();
        assert!(v.is_empty());
    }
}
