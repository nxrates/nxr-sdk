//! End-to-end integration tests for the A/B consumer.
//!
//! Each test stands up two in-memory [`FrameSource`]s, feeds canned MITCH
//! frames through a real [`Consumer`], and asserts the downstream mpsc sees
//! exactly the expected payloads. This covers the transport-agnostic surface
//! without touching the network.

use std::collections::VecDeque;

use anyhow::Result;
use nxr_sdk::consumer::{Consumer, DedupRing};
use nxr_sdk::transport::FrameSource;
use tokio::sync::mpsc;

use mitch::common::message_type;
use mitch::header::MitchHeader;
use mitch::heartbeat::Heartbeat;

/// 16B MITCH heartbeat frame with a specific `(mts, sequence)` key. Tests use
/// heartbeat bodies for every frame because the transport layer is opaque to
/// payload shape and heartbeats are the smallest legal MITCH frame.
fn make_frame(mts: u64, sequence: u16) -> Vec<u8> {
    let mut header = MitchHeader::new(message_type::HEARTBEAT, 0, mts, 1);
    header.sequence = sequence;
    let mut buf = Vec::with_capacity(32);
    buf.extend_from_slice(&header.pack());
    let body = Heartbeat::feed(0);
    buf.extend_from_slice(bytemuck::bytes_of(&body));
    buf
}

/// Deterministic in-memory source: yields a fixed queue of frames, then
/// signals EOF. Used to drive consumer integration tests without UDP.
struct MockSource {
    queue: VecDeque<Vec<u8>>,
    label: &'static str,
}

impl MockSource {
    fn new(label: &'static str, frames: Vec<Vec<u8>>) -> Self {
        Self {
            queue: frames.into(),
            label,
        }
    }
}

impl FrameSource for MockSource {
    fn recv(&mut self) -> impl std::future::Future<Output = Result<Option<Vec<u8>>>> + Send {
        let next = self.queue.pop_front();
        async move { Ok(next) }
    }

    fn label(&self) -> &'static str {
        self.label
    }
}

async fn drain(mut rx: mpsc::Receiver<Vec<u8>>) -> Vec<Vec<u8>> {
    let mut out = Vec::new();
    while let Some(f) = rx.recv().await {
        out.push(f);
    }
    out
}

#[tokio::test]
async fn a_only_delivers_every_frame() {
    // Only feed A produces; B is silent. Consumer must deliver all frames
    // without dedupe ambiguity.
    let frames: Vec<Vec<u8>> = (0..10u16).map(|s| make_frame(1_000 + s as u64, s)).collect();
    let a = MockSource::new("A", frames.clone());
    let b = MockSource::new("B", vec![]);
    let (tx, rx) = mpsc::channel(64);
    let (consumer, _health) = Consumer::new(vec![a, b], tx);
    let task = tokio::spawn(consumer.run());

    let out = drain(rx).await;
    task.await.unwrap().unwrap();

    assert_eq!(out.len(), 10);
    assert_eq!(out, frames);
}

#[tokio::test]
async fn b_only_delivers_every_frame() {
    let frames: Vec<Vec<u8>> = (0..10u16).map(|s| make_frame(2_000 + s as u64, s)).collect();
    let a = MockSource::new("A", vec![]);
    let b = MockSource::new("B", frames.clone());
    let (tx, rx) = mpsc::channel(64);
    let (consumer, _health) = Consumer::new(vec![a, b], tx);
    let task = tokio::spawn(consumer.run());

    let out = drain(rx).await;
    task.await.unwrap().unwrap();

    assert_eq!(out.len(), 10);
}

#[tokio::test]
async fn both_alive_dedupes_to_single_stream() {
    // A and B carry the same 10-frame sequence. Consumer should emit exactly
    // 10 unique frames; the other 10 are duplicates dropped by the dedup ring.
    let frames: Vec<Vec<u8>> = (0..10u16).map(|s| make_frame(3_000 + s as u64, s)).collect();
    let a = MockSource::new("A", frames.clone());
    let b = MockSource::new("B", frames.clone());
    let (tx, rx) = mpsc::channel(64);
    let (consumer, health) = Consumer::new(vec![a, b], tx);
    let task = tokio::spawn(consumer.run());

    let out = drain(rx).await;
    task.await.unwrap().unwrap();

    assert_eq!(out.len(), 10, "expected exactly 10 unique frames");

    let total_dupes: u64 = health
        .iter()
        .map(|h| h.duplicates_dropped.load(std::sync::atomic::Ordering::Relaxed))
        .sum();
    assert_eq!(total_dupes, 10, "expected exactly 10 duplicate drops");

    let total_seen: u64 = health
        .iter()
        .map(|h| h.frames_seen.load(std::sync::atomic::Ordering::Relaxed))
        .sum();
    assert_eq!(total_seen, 20, "every frame must increment frames_seen");
}

#[tokio::test]
async fn reorder_between_feeds_still_dedupes() {
    // B arrives before A on some keys and after A on others. Dedup must not
    // depend on which feed "wins" first.
    let f = |mts: u64, seq: u16| make_frame(mts, seq);
    let a_frames = vec![f(100, 0), f(100, 1), f(100, 2), f(100, 3)];
    let b_frames = vec![f(100, 2), f(100, 0), f(100, 3), f(100, 1)];

    let a = MockSource::new("A", a_frames);
    let b = MockSource::new("B", b_frames);
    let (tx, rx) = mpsc::channel(64);
    let (consumer, _health) = Consumer::new(vec![a, b], tx);
    let task = tokio::spawn(consumer.run());

    let out = drain(rx).await;
    task.await.unwrap().unwrap();

    assert_eq!(out.len(), 4, "four unique (mts, seq) pairs must surface once each");
}

#[tokio::test]
async fn heartbeat_updates_liveness_clock() {
    let a_frames = vec![make_frame(500, 0)];
    let a = MockSource::new("A", a_frames);
    let (tx, rx) = mpsc::channel(64);
    let (consumer, health) = Consumer::new(vec![a], tx);
    let task = tokio::spawn(consumer.run());
    let _ = drain(rx).await;
    task.await.unwrap().unwrap();

    assert!(
        health[0].age().is_some(),
        "last_frame_at must be populated after any frame"
    );
    assert!(
        health[0].heartbeat_age().is_some(),
        "last_heartbeat_at must be populated after a heartbeat frame"
    );
}

#[tokio::test]
async fn heartbeat_absent_stays_none() {
    // Feed goes silent forever: every liveness clock stays None. This is the
    // signal operators watch to page when a leg dies.
    let a = MockSource::new("A", vec![]);
    let (tx, rx) = mpsc::channel::<Vec<u8>>(64);
    let (consumer, health) = Consumer::new(vec![a], tx);
    let task = tokio::spawn(consumer.run());
    let _ = drain(rx).await;
    task.await.unwrap().unwrap();

    assert!(health[0].age().is_none());
    assert!(health[0].heartbeat_age().is_none());
}

#[test]
fn dedup_ring_handles_sequence_wraparound() {
    // Sequence is u16 so it wraps at 65535 -> 0. Within a single mts that
    // would collide, but mts advances every 16us so (mts, 0) after wrap is a
    // new key. Prove the ring treats the post-wrap key as fresh.
    let mut ring = DedupRing::with_capacity(8);
    assert!(ring.observe(1, u16::MAX));
    assert!(!ring.observe(1, u16::MAX), "same key repeats are dropped");
    assert!(ring.observe(2, 0), "post-wrap key under a new mts is fresh");
    assert!(ring.observe(2, 1));
}

#[test]
fn dedup_ring_handles_mts_monotonic_reset() {
    // If the producer restarts, mts resets to a smaller value. The ring must
    // still treat the new (mts, seq) pair as fresh rather than latching to
    // "already seen" based on magnitude.
    let mut ring = DedupRing::with_capacity(8);
    assert!(ring.observe(1_000_000, 0));
    assert!(ring.observe(1_000_001, 0));
    assert!(ring.observe(42, 0), "lower mts after reset is still fresh");
    assert!(!ring.observe(42, 0));
}
