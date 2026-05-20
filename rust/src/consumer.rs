//! Transport-agnostic consumer that dedupes CME-style A/B feeds and tracks
//! per-feed heartbeats for staleness detection.
//!
//! A [`Consumer`] reads framed MITCH messages from one or more
//! [`FrameSource`]s (typically two redundant UDP multicast groups) and emits
//! every unique frame exactly once. Uniqueness is keyed on the
//! `(mts, sequence)` tuple stored in the `MitchHeader` of every frame: the
//! publisher stamps the same pair on both A and B, so the consumer keeps
//! whichever arrives first and silently drops the second.
//!
//! A small ring of recently-seen keys backs the dedupe cache; the ring size
//! is the maximum tolerable A/B skew measured in frames, not a security
//! boundary. The default (`4096`) survives ~80 ms of skew at 50k frames/s.
//!
//! Heartbeat frames (`MSG_TYPE = 'h'`) are still delivered to the caller so
//! it can drive its own bookkeeping, but the consumer also updates a
//! per-feed [`FeedHealth`] counter so operators can surface stale feeds
//! before the upstream producer fails over.

use std::collections::{HashSet, VecDeque};
use std::sync::{Arc, Mutex};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

use anyhow::Result;
use tokio::sync::mpsc;
use tracing::{debug, instrument, warn};

use mitch::common::{message_sizes, message_type};
use mitch::header::MitchHeader;

use crate::transport::FrameSource;

// =============================================================================
// DEDUP RING
// =============================================================================

/// Default dedup window. One entry per frame, so 4096 handles bursty A/B
/// skew up to ~80 ms at 50k frames/s. Increase for higher-throughput feeds,
/// decrease to cap memory if frames arrive in strict order.
pub const DEFAULT_DEDUP_CAPACITY: usize = 4096;

/// Dedup cache for CME-style A/B feeds keyed on `(mts, sequence)` pairs
/// from the MITCH header. Backed by a `HashSet` for O(1) duplicate lookups
/// and a `VecDeque` tracking insertion order so the oldest key evicts when
/// the ring is full.
///
/// Keys are packed into a single `u64` (`mts << 16 | seq`) because the MITCH
/// timestamp is logically a u48 (16 µs ticks since 2010). Packing keeps the
/// hash bucket small and avoids hashing a tuple. An earlier version used a
/// linear scan on the theory that tuple comparison beat hashing; that only
/// holds for very small rings (<~32 entries). At capacity 4096 the hash
/// lookup is ~30x faster.
#[derive(Debug)]
pub struct DedupRing {
    seen: HashSet<u64>,
    order: VecDeque<u64>,
    capacity: usize,
}

impl DedupRing {
    /// Empty ring with the default capacity.
    pub fn new() -> Self {
        Self::with_capacity(DEFAULT_DEDUP_CAPACITY)
    }

    /// Empty ring with an explicit capacity.
    pub fn with_capacity(capacity: usize) -> Self {
        Self {
            seen: HashSet::with_capacity(capacity),
            order: VecDeque::with_capacity(capacity),
            capacity,
        }
    }

    /// Record the key and return `true` if this is the first sighting,
    /// `false` if the key is already in the ring.
    #[inline]
    pub fn observe(&mut self, mts: u64, sequence: u16) -> bool {
        let key = (mts << 16) | (sequence as u64);
        if !self.seen.insert(key) {
            return false;
        }
        self.order.push_back(key);
        if self.order.len() > self.capacity {
            if let Some(old) = self.order.pop_front() {
                self.seen.remove(&old);
            }
        }
        true
    }

    /// Current number of entries.
    pub fn len(&self) -> usize {
        self.order.len()
    }

    /// True when no frames have been observed yet.
    pub fn is_empty(&self) -> bool {
        self.order.is_empty()
    }
}

impl Default for DedupRing {
    fn default() -> Self {
        Self::new()
    }
}

// =============================================================================
// FEED HEALTH
// =============================================================================

/// Per-feed liveness state updated by the consumer on every inbound frame and
/// read by operators (or a metrics scraper) to decide when one leg of an A/B
/// pair has gone dark.
#[derive(Debug)]
pub struct FeedHealth {
    /// Wall-clock of the most recent frame (any type) seen on this feed.
    pub last_frame_at: std::sync::Mutex<Option<Instant>>,
    /// Wall-clock of the most recent heartbeat frame seen on this feed.
    pub last_heartbeat_at: std::sync::Mutex<Option<Instant>>,
    /// Total frames (data + heartbeat) observed, used as a liveness counter.
    pub frames_seen: AtomicU64,
    /// Duplicates dropped by the dedup ring; high counts here are a good
    /// thing, they prove A/B redundancy is doing its job.
    pub duplicates_dropped: AtomicU64,
    /// Feed label, e.g. `"A"` or `"B"`.
    pub label: &'static str,
}

impl FeedHealth {
    pub fn new(label: &'static str) -> Self {
        Self {
            last_frame_at: std::sync::Mutex::new(None),
            last_heartbeat_at: std::sync::Mutex::new(None),
            frames_seen: AtomicU64::new(0),
            duplicates_dropped: AtomicU64::new(0),
            label,
        }
    }

    /// Duration since the last frame, or `None` if the feed has never fired.
    pub fn age(&self) -> Option<Duration> {
        self.last_frame_at
            .lock()
            .ok()
            .and_then(|g| g.map(|t| t.elapsed()))
    }

    /// Duration since the last heartbeat, or `None` if no heartbeat has
    /// arrived yet.
    pub fn heartbeat_age(&self) -> Option<Duration> {
        self.last_heartbeat_at
            .lock()
            .ok()
            .and_then(|g| g.map(|t| t.elapsed()))
    }
}

// =============================================================================
// CONSUMER
// =============================================================================

/// CME-style A/B consumer: reads from N homogeneous sources, dedupes by
/// `(mts, sequence)`, and forwards unique frames to an mpsc channel.
///
/// The consumer is generic over a single source type so the async methods
/// stay dyn-free and inline cleanly. A typical UDP A/B deployment uses
/// `Consumer<UdpMulticastSource>` with two sources; mixed-transport setups
/// (UDP + TCP) should run two independent consumers rather than one
/// polymorphic consumer.
pub struct Consumer<Src: FrameSource + 'static> {
    sources: Vec<Src>,
    health: Vec<Arc<FeedHealth>>,
    // std::sync::Mutex: the ring is held for ~100 ns with no `.await` inside,
    // so the async-aware `tokio::sync::Mutex` just adds scheduling overhead.
    dedup: Arc<Mutex<DedupRing>>,
    out_tx: mpsc::Sender<Vec<u8>>,
}

impl<Src: FrameSource + 'static> Consumer<Src> {
    /// Build a consumer over the given sources. `out_tx` is the downstream
    /// sink the caller drains; it closes when every source task exits.
    ///
    /// Returns the built consumer plus a vec of per-feed [`FeedHealth`]
    /// handles so the caller can read staleness metrics concurrently with
    /// the consumer running.
    pub fn new(
        sources: Vec<Src>,
        out_tx: mpsc::Sender<Vec<u8>>,
    ) -> (Self, Vec<Arc<FeedHealth>>) {
        let health: Vec<Arc<FeedHealth>> = sources
            .iter()
            .map(|s| Arc::new(FeedHealth::new(s.label())))
            .collect();
        let handles = health.clone();
        (
            Self {
                sources,
                health,
                dedup: Arc::new(Mutex::new(DedupRing::new())),
                out_tx,
            },
            handles,
        )
    }

    /// Spawn one task per source and run until every source closes.
    #[instrument(name = "consumer", skip_all, fields(sources = self.sources.len()))]
    pub async fn run(self) -> Result<()> {
        let mut tasks = Vec::with_capacity(self.sources.len());
        for (source, health) in self.sources.into_iter().zip(self.health.into_iter()) {
            let dedup = Arc::clone(&self.dedup);
            let out_tx = self.out_tx.clone();
            tasks.push(tokio::spawn(run_source(source, health, dedup, out_tx)));
        }
        for t in tasks {
            let _ = t.await;
        }
        Ok(())
    }
}

async fn run_source<Src: FrameSource>(
    mut source: Src,
    health: Arc<FeedHealth>,
    dedup: Arc<Mutex<DedupRing>>,
    out_tx: mpsc::Sender<Vec<u8>>,
) {
    let label = source.label();
    loop {
        let frame = match source.recv().await {
            Ok(Some(buf)) => buf,
            Ok(None) => break,
            Err(e) => {
                warn!(feed = label, err = %e, "source recv error");
                continue;
            }
        };

        if frame.len() < message_sizes::HEADER {
            debug!(feed = label, got = frame.len(), "short frame dropped");
            continue;
        }

        let header = match MitchHeader::unpack(&frame[..message_sizes::HEADER]) {
            Ok(h) => h,
            Err(e) => {
                debug!(feed = label, err = %e, "header unpack failed");
                continue;
            }
        };
        let mts = header.get_timestamp();
        let seq = header.sequence;
        let mt = header.message_type();

        health.frames_seen.fetch_add(1, Ordering::Relaxed);
        if let Ok(mut g) = health.last_frame_at.lock() {
            *g = Some(Instant::now());
        }
        if mt == message_type::HEARTBEAT
            && let Ok(mut g) = health.last_heartbeat_at.lock()
        {
            *g = Some(Instant::now());
        }

        let fresh = {
            // Poisoned lock recovery: a panicking observer can't corrupt the
            // ring since observe() is infallible, so treat the guard as live.
            let mut ring = dedup.lock().unwrap_or_else(|p| p.into_inner());
            ring.observe(mts, seq)
        };
        if !fresh {
            health.duplicates_dropped.fetch_add(1, Ordering::Relaxed);
            continue;
        }

        if out_tx.send(frame).await.is_err() {
            break;
        }
    }
}

// =============================================================================
// TESTS
// =============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dedup_ring_drops_exact_duplicates() {
        let mut ring = DedupRing::with_capacity(8);
        assert!(ring.observe(100, 0));
        assert!(!ring.observe(100, 0));
        assert!(ring.observe(100, 1));
        assert!(ring.observe(101, 0));
        assert!(!ring.observe(101, 0));
        assert_eq!(ring.len(), 3);
    }

    #[test]
    fn dedup_ring_evicts_oldest_when_full() {
        let mut ring = DedupRing::with_capacity(2);
        assert!(ring.observe(1, 0));
        assert!(ring.observe(2, 0));
        assert!(ring.observe(3, 0));
        assert!(ring.observe(1, 0), "key 1 evicted and is fresh again");
        assert_eq!(ring.len(), 2);
    }
}
