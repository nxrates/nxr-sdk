//! Transport-agnostic frame publisher with CME-style A/B fanout and optional
//! temporal replay.
//!
//! A [`Publisher`] reads frames from a [`tokio::sync::broadcast`] channel and
//! forwards each one to every configured [`FrameSink`]. Running two sinks (one
//! per redundant multicast group) gives consumers a CME MDP 3.0 / NASDAQ ITCH
//! style A/B feed they can dedupe to survive single-packet loss on one leg.
//! The publisher does not generate frames, it duplicates them; heartbeat
//! frames come from the upstream producer alongside data frames on the same
//! broadcast channel.
//!
//! Temporal redundancy is configured per publisher instance via
//! `ReplaySchedule`. Heartbeat publishers can enable triple-shot delivery
//! (`t=0, t+25 ms, t+75 ms`) while Index publishers stay single-shot because
//! a lost Index is replaced by the next 50 ms aggregation window. See
//! `docs/redundancy.md` for the research backing these defaults.
//!
//! The publisher is transport-agnostic (any [`FrameSink`] impl) and input-
//! format-agnostic (any [`AsFrameBytes`] impl). Core feeds already-packed
//! `IndexRecord` snapshots; external producers that hold pre-serialised bytes
//! can feed `Arc<Vec<u8>>` or `Vec<u8>` through the same loop without a
//! second conversion hop.

use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

use anyhow::Result;
use tokio::sync::broadcast;
use tracing::{instrument, warn};

use crate::ipc::AsFrameBytes;
use crate::transport::FrameSink;

/// Temporal replay schedule for a publisher instance.
///
/// The publisher always sends each frame to every sink at `t=0`. When
/// `offsets` is non-empty, the same frame is re-sent at each offset measured
/// from the initial send (not cumulative). `[25ms, 75ms]` produces 3 total
/// shots per sink at `t=0, t+25ms, t+75ms`.
///
/// Pick offsets that exceed the expected loss-burst duration on the target
/// path. On a healthy LAN this is dominated by kernel socket-buffer drain
/// (~100-500 µs under load) and NIC / switch microbursts (<1 ms cut-through,
/// 5-10 ms store-forward under pressure). RFC 2198 (RTP Redundant Audio)
/// uses 20 ms spacing for a similar UDP loss-tolerance problem; our default
/// is a slightly wider 25 ms + 75 ms triple-shot for heartbeats. Full
/// rationale: `docs/redundancy.md`.
#[derive(Debug, Clone, Default)]
pub(crate) struct ReplaySchedule {
    offsets: Vec<Duration>,
}

impl ReplaySchedule {
    /// No replay: single-shot per sink. Right for high-rate aggregated data
    /// where the next frame supersedes a lost one (e.g. Index at 20 Hz).
    pub(crate) fn none() -> Self {
        Self::default()
    }

    /// Arbitrary offsets from `t=0`. Empty vec behaves like [`Self::none`].
    #[allow(dead_code)]
    pub(crate) fn at_offsets(offsets: Vec<Duration>) -> Self {
        Self { offsets }
    }

    /// Preset for low-rate critical frames (heartbeats, control messages):
    /// triple-shot at `t=0, t+25 ms, t+75 ms`. Sized to decorrelate from
    /// sub-ms kernel queue events and typical switch microbursts at
    /// negligible wire cost. See `docs/redundancy.md`.
    #[allow(dead_code)]
    pub(crate) fn heartbeat() -> Self {
        Self::at_offsets(vec![
            Duration::from_millis(25),
            Duration::from_millis(75),
        ])
    }

    /// Configured offsets, in send order.
    pub(crate) fn offsets(&self) -> &[Duration] {
        &self.offsets
    }

    /// True when no replay shots are configured.
    #[allow(dead_code)]
    pub(crate) fn is_empty(&self) -> bool {
        self.offsets.is_empty()
    }
}

/// Publisher that fans out every inbound frame to every registered sink.
///
/// The publisher is generic over the sink type `S`: construct with
/// `Publisher::new(vec![sink_a, sink_b])` and call [`Publisher::run`] inside
/// a tokio task. It runs until the broadcast channel closes. Sinks are kept
/// behind `Arc` so replay tasks can share handles without cloning the
/// underlying socket.
pub struct Publisher<S: FrameSink> {
    sinks: Vec<Arc<S>>,
    replay: ReplaySchedule,
}

impl<S: FrameSink + 'static> Publisher<S> {
    /// Construct a publisher over the given fanout sinks. Typical usage is
    /// `vec![sink_a, sink_b]` for CME-style A/B redundancy, but the publisher
    /// works with any number of sinks (one for a single-feed setup, N for
    /// fan-out).
    pub fn new(sinks: Vec<S>) -> Self {
        Self {
            sinks: sinks.into_iter().map(Arc::new).collect(),
            replay: ReplaySchedule::none(),
        }
    }

    /// Configure temporal redundancy: each frame is replayed at the given
    /// offsets on every sink, in addition to the initial `t=0` send.
    #[allow(dead_code)]
    pub(crate) fn with_replay(mut self, replay: ReplaySchedule) -> Self {
        self.replay = replay;
        self
    }

    /// Consume `rx` and forward every frame to every sink until the broadcast
    /// channel closes. Send errors and broadcast-lag events are aggregated
    /// and logged at most once per second PER SINK so a transient failure on
    /// one leg of an A/B pair does not mask concurrent failures on the other,
    /// and so a persistent transport failure stays visible without flooding.
    #[instrument(name = "publisher", skip_all, fields(sinks = self.sinks.len()))]
    pub async fn run<M>(&self, mut rx: broadcast::Receiver<M>) -> Result<()>
    where
        M: AsFrameBytes + Clone + Send + 'static,
    {
        let errors: Vec<AtomicU64> = (0..self.sinks.len()).map(|_| AtomicU64::new(0)).collect();
        let mut last_sink_report: Vec<Instant> =
            (0..self.sinks.len()).map(|_| Instant::now()).collect();
        let lagged = AtomicU64::new(0);
        let mut last_lag_report = Instant::now();

        loop {
            match rx.recv().await {
                Ok(frame) => {
                    let bytes = frame.as_frame_bytes();
                    for (idx, (sink, err_ctr)) in
                        self.sinks.iter().zip(errors.iter()).enumerate()
                    {
                        if let Err(e) = sink.send(bytes).await {
                            let n = err_ctr.fetch_add(1, Ordering::Relaxed) + 1;
                            if last_sink_report[idx].elapsed() >= Duration::from_secs(1) {
                                warn!(
                                    sink = sink.label(),
                                    err = %e,
                                    dropped = n,
                                    "sink send failed"
                                );
                                err_ctr.store(0, Ordering::Relaxed);
                                last_sink_report[idx] = Instant::now();
                            }
                        }
                    }

                    // Replay: one spawned task per offset sleeps and re-fans
                    // the same frame on every sink. Sink handles are Arc so
                    // this is refcount bumps, not socket clones; M: Clone is
                    // an Arc bump for Arc<Vec<u8>>, a copy for Vec<u8>.
                    // Replay send errors are swallowed: the t=0 send already
                    // surfaces sink-level failures through the throttled
                    // warn path above, and logging the same error up to N
                    // more times per frame would drown the signal.
                    for &offset in self.replay.offsets() {
                        let sinks = self.sinks.clone();
                        let frame = frame.clone();
                        tokio::spawn(async move {
                            tokio::time::sleep(offset).await;
                            let bytes = frame.as_frame_bytes();
                            for sink in &sinks {
                                let _ = sink.send(bytes).await;
                            }
                        });
                    }
                }
                Err(broadcast::error::RecvError::Lagged(n)) => {
                    let total = lagged.fetch_add(n, Ordering::Relaxed) + n;
                    if last_lag_report.elapsed() >= Duration::from_secs(1) {
                        warn!(skipped = total, "publisher lagged: broadcast overflow");
                        lagged.store(0, Ordering::Relaxed);
                        last_lag_report = Instant::now();
                    }
                }
                Err(broadcast::error::RecvError::Closed) => break,
            }
        }
        Ok(())
    }
}
