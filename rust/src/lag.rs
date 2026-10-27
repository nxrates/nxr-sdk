//! Lock-free log2 latency histogram for hot paths: one relaxed RMW per
//! sample, no allocation, no lock. Either one owner drains it per window
//! ([`LagHist::take`]), or readers snapshot the cumulative counts
//! ([`LagHist::snap`]) and diff them ([`LagSnap::since`]), which any number
//! of readers can do without splitting a window.
//!
//! Quantiles are bucket UPPER edges (bucket k = [2^(k-1), 2^k - 1] ms),
//! capped at the max: an upper bound, at most 2x the true value.

use std::sync::atomic::{AtomicU32, Ordering::Relaxed};

pub const BUCKETS: usize = 16;

/// Bucket 0 = 0 ms, bucket k = [2^(k-1), 2^k - 1] ms; the last is open (>= 16384 ms).
#[inline]
pub fn bucket(ms: u32) -> usize {
    ((u32::BITS - ms.leading_zeros()) as usize).min(BUCKETS - 1)
}

/// Upper edge of bucket `k`; the open last bucket has none.
fn edge(k: usize) -> u32 {
    if k + 1 == BUCKETS { u32::MAX } else { (1u32 << k) - 1 }
}

#[derive(Debug, Default)]
pub struct LagHist {
    b: [AtomicU32; BUCKETS],
    max: AtomicU32,
}

impl LagHist {
    pub const fn new() -> Self {
        Self {
            b: [const { AtomicU32::new(0) }; BUCKETS],
            max: AtomicU32::new(0),
        }
    }

    #[inline]
    pub fn record(&self, ms: u32) {
        self.b[bucket(ms)].fetch_add(1, Relaxed);
        // A plain load first: the max RMW runs only on a new high.
        if ms > self.max.load(Relaxed) {
            self.max.fetch_max(ms, Relaxed);
        }
    }

    /// Cumulative counts and max since start; nothing reset.
    pub fn snap(&self) -> LagSnap {
        LagSnap {
            b: std::array::from_fn(|i| self.b[i].load(Relaxed)),
            max: self.max.load(Relaxed),
        }
    }

    /// Read and zero: one window.
    pub fn take(&self) -> LagSnap {
        LagSnap {
            b: std::array::from_fn(|i| self.b[i].swap(0, Relaxed)),
            max: self.max.swap(0, Relaxed),
        }
    }
}

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct LagSnap {
    pub b: [u32; BUCKETS],
    pub max: u32,
}

impl LagSnap {
    pub fn n(&self) -> u64 {
        self.b.iter().map(|&c| u64::from(c)).sum()
    }

    pub fn merge(&mut self, o: &LagSnap) {
        for (a, b) in self.b.iter_mut().zip(o.b) {
            *a = a.saturating_add(b);
        }
        self.max = self.max.max(o.max);
    }

    /// The window between an earlier cumulative `prev` and this one. A count
    /// that went down means the source restarted: the whole of `self` is the
    /// window. The window max is the top non-empty bucket's upper edge, capped
    /// at the cumulative max (the exact window max is not kept).
    pub fn since(&self, prev: &LagSnap) -> LagSnap {
        if self.b.iter().zip(prev.b).any(|(n, p)| *n < p) {
            return *self;
        }
        let b: [u32; BUCKETS] = std::array::from_fn(|i| self.b[i] - prev.b[i]);
        let max = b
            .iter()
            .rposition(|&c| c > 0)
            .map_or(0, |k| edge(k).min(self.max));
        LagSnap { b, max }
    }

    /// Upper edge of the bucket holding quantile `q`, capped at `max`; `None` when empty.
    pub fn quantile(&self, q: f64) -> Option<u32> {
        let n = self.n();
        if n == 0 {
            return None;
        }
        let rank = ((q * n as f64).ceil() as u64).clamp(1, n);
        let mut acc = 0;
        for (k, &c) in self.b.iter().enumerate() {
            acc += u64::from(c);
            if acc >= rank {
                return Some(edge(k).min(self.max));
            }
        }
        Some(self.max)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn buckets_are_log2_and_saturate() {
        let cases = [(0, 0), (1, 1), (2, 2), (3, 2), (4, 3), (7, 3), (8, 4), (1023, 10), (1024, 11)];
        for (ms, b) in cases {
            assert_eq!(bucket(ms), b, "{ms}");
        }
        assert_eq!(bucket(16_383), 14);
        assert_eq!(bucket(16_384), 15);
        assert_eq!(bucket(u32::MAX), 15);
    }

    #[test]
    fn quantiles_report_bucket_upper_edges_capped_at_max() {
        let h = LagHist::new();
        assert_eq!(h.take().quantile(0.5), None);
        for ms in [1, 2, 3, 5, 5, 6, 9, 40, 100, 900] {
            h.record(ms);
        }
        let s = h.take();
        assert_eq!((s.n(), s.max), (10, 900));
        assert_eq!(s.quantile(0.5), Some(7), "5th sample sits in [4, 7]");
        assert_eq!(s.quantile(0.99), Some(900), "edge 1023 capped at max");
        assert_eq!(h.take(), LagSnap::default(), "take zeroes the window");
        h.record(60_000);
        assert_eq!(h.take().quantile(0.5), Some(60_000), "open bucket reports max");
    }

    #[test]
    fn snap_is_non_destructive_and_since_diffs_windows() {
        let h = LagHist::new();
        h.record(3);
        h.record(900);
        let a = h.snap();
        assert_eq!(h.snap(), a, "reading resets nothing");
        h.record(5);
        h.record(6);
        let w = h.snap().since(&a);
        assert_eq!((w.n(), w.b[3]), (2, 2));
        assert_eq!(w.max, 7, "top bucket edge, not the 900 of an older window");
        assert_eq!(w.quantile(0.5), Some(7));
        assert_eq!(h.snap().since(&h.snap()), LagSnap::default(), "empty window");
        let restarted = LagHist::new();
        restarted.record(2);
        assert_eq!(restarted.snap().since(&a), restarted.snap(), "count went down: all of it");
        h.record(40_000);
        assert_eq!(h.snap().since(&a).max, 40_000, "open bucket: capped at the cumulative max");
    }

    #[test]
    fn merge_adds_counts_and_keeps_the_max() {
        let (a, b) = (LagHist::new(), LagHist::new());
        a.record(3);
        b.record(3);
        b.record(500);
        let mut s = a.take();
        s.merge(&b.take());
        assert_eq!((s.n(), s.max, s.b[2]), (3, 500, 2));
    }
}
