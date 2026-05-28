//! Bucket-aligned OHLC resampler over `.idx` (`IndexRecord`) streams.
//!
//! ## What this is
//!
//! Time-bucket aggregation from per-tick `IndexRecord` rows to fixed-TF
//! OHLC candles. Companion to `bar_builder` (which builds the full canonical
//! `mitch::Bar` with microstructure features) — this module produces a
//! lighter [`Ohlc`] struct for ticks/charts/REST responses where only
//! OHLCV + tick count + average CI are needed.
//!
//! ## Bucket alignment
//!
//! All buckets are wall-clock UTC aligned. For a TF of `tf_ms`, the bucket
//! containing observation `ts_ms` starts at `(ts_ms / tf_ms) * tf_ms`
//! (integer floor). Since `ts_ms` is UTC epoch ms and `tf_ms` divides the
//! day for every whitelisted TF, boundaries land on UTC seconds 0, 10, …
//! and minute / hour / day boundaries as expected.
//!
//! ## OHLC semantics
//!
//! Mid per record = `(bid + ask) / 2`. Per bucket:
//!
//! - `open`  = first mid
//! - `high`  = max mid
//! - `low`   = min mid
//! - `close` = last mid
//! - `vbid`, `vask` = sum
//! - `tick_count` = number of records in the bucket
//! - `avg_ci_ubp` = sqrt-compressed mean of decoded `ci_ubp`
//!   (same encoding as `mitch::Bar::avg_ci_ubp`, scale [`mitch::common::CI_SCALE`])
//!
//! Empty buckets are NOT emitted. Consumers detect gaps by ts discontinuity.
//!
//! ## Rollup
//!
//! [`rollup`] folds a TF-N series into TF-M (M = k·N) via the OHLC monoid:
//! first/max/min/last + sum. Bucket alignment of TF-M is preserved because
//! the input TF-N bars are already wall-clock aligned.

use mitch::common::CI_SCALE;
use crate::tdwap::decode_ci_ubp;

use crate::ipc::record::IndexRecord;

/// Lightweight OHLC candle for chart / REST output.
///
/// Decode `avg_ci_ubp` via `(avg_ci_ubp / CI_SCALE)^2`.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Ohlc {
    /// Bucket-start epoch ms (UTC).
    pub ts: i64,
    /// Open price (first mid in bucket).
    pub open: f64,
    /// High price (max mid in bucket).
    pub high: f64,
    /// Low price (min mid in bucket).
    pub low: f64,
    /// Close price (last mid in bucket).
    pub close: f64,
    /// Summed bid volume across bucket.
    pub vbid: u64,
    /// Summed ask volume across bucket.
    pub vask: u64,
    /// Number of source records in bucket.
    pub tick_count: u32,
    /// Sqrt-compressed mean confidence interval (micro basis points of mid).
    pub avg_ci_ubp: u16,
}

// ── Internal accumulator ────────────────────────────────────────────────

/// Per-bucket scratch state. Held only while a bucket is open.
#[derive(Clone, Copy)]
struct BucketState {
    ts: i64,
    open: f64,
    high: f64,
    low: f64,
    close: f64,
    vbid: u64,
    vask: u64,
    tick_count: u32,
    sum_ci_ubp: f64,
}

impl BucketState {
    #[inline]
    fn new(ts: i64, mid: f64, vbid: u32, vask: u32, ci_ubp: f64) -> Self {
        Self {
            ts,
            open: mid,
            high: mid,
            low: mid,
            close: mid,
            vbid: vbid as u64,
            vask: vask as u64,
            tick_count: 1,
            sum_ci_ubp: ci_ubp,
        }
    }

    #[inline]
    fn ingest(&mut self, mid: f64, vbid: u32, vask: u32, ci_ubp: f64) {
        if mid > self.high {
            self.high = mid;
        }
        if mid < self.low {
            self.low = mid;
        }
        self.close = mid;
        self.vbid += vbid as u64;
        self.vask += vask as u64;
        self.tick_count += 1;
        self.sum_ci_ubp += ci_ubp;
    }

    #[inline]
    fn finalize(self) -> Ohlc {
        let avg_ci_ubp = encode_avg_ci_ubp(self.sum_ci_ubp, self.tick_count);
        Ohlc {
            ts: self.ts,
            open: self.open,
            high: self.high,
            low: self.low,
            close: self.close,
            vbid: self.vbid,
            vask: self.vask,
            tick_count: self.tick_count,
            avg_ci_ubp,
        }
    }
}

// ── Encoding helpers ────────────────────────────────────────────────────

/// Encode the mean of accumulated `ci_ubp` values using the same
/// sqrt compression as `mitch::Bar::avg_ci_ubp` (and `BarAccumulator`).
#[inline]
fn encode_avg_ci_ubp(sum_ci_ubp: f64, tick_count: u32) -> u16 {
    if tick_count == 0 {
        return 0;
    }
    let avg = sum_ci_ubp / tick_count as f64;
    let enc = (avg.max(0.0).sqrt() * CI_SCALE).round();
    enc.clamp(0.0, u16::MAX as f64) as u16
}

/// Extract `(ts_ms, mid, vbid, vask, ci_ubp)` from a record.
/// Reads packed fields into locals to avoid unaligned references.
#[inline]
fn decode_record(rec: &IndexRecord) -> (i64, f64, u32, u32, f64) {
    let ts_ms = mitch::timestamp::to_epoch_ms(rec.header.get_timestamp());
    // Copy out of the packed struct first to satisfy alignment rules.
    let bid = rec.index.bid;
    let ask = rec.index.ask;
    let vbid = rec.index.vbid;
    let vask = rec.index.vask;
    let ci = rec.index.ci;
    let mid = (bid + ask) * 0.5;
    let ci_ubp = decode_ci_ubp(ci);
    (ts_ms, mid, vbid, vask, ci_ubp)
}

/// Compute bucket-start epoch ms for `ts_ms` at TF `tf_ms` (floor align).
#[inline]
fn bucket_start(ts_ms: i64, tf_ms: i64) -> i64 {
    ts_ms.div_euclid(tf_ms) * tf_ms
}

// ── Public API ──────────────────────────────────────────────────────────

/// Bucket-aligned resample of an `IndexRecord` slice to a TF in milliseconds.
///
/// Returns bars in chronological order. The first bar opens at
/// `floor(ts[0] / tf_ms) * tf_ms`. Gaps (no-data buckets) are skipped —
/// no synthetic zero-volume bars are emitted.
///
/// Records are assumed to be in non-decreasing timestamp order. Out-of-order
/// records older than the current bucket are silently ignored (callers should
/// pre-sort if needed); records that fall into the current bucket are merged.
pub fn idx_to_ohlc(records: &[IndexRecord], tf_ms: i64) -> Vec<Ohlc> {
    assert!(tf_ms > 0, "tf_ms must be positive");
    if records.is_empty() {
        return Vec::new();
    }

    let mut out: Vec<Ohlc> = Vec::with_capacity(records.len() / 8 + 1);
    let mut cur: Option<BucketState> = None;

    for rec in records {
        let (ts_ms, mid, vbid, vask, ci_ubp) = decode_record(rec);
        let bs = bucket_start(ts_ms, tf_ms);
        match cur.as_mut() {
            Some(state) if state.ts == bs => {
                state.ingest(mid, vbid, vask, ci_ubp);
            }
            Some(state) if bs > state.ts => {
                out.push(state.finalize());
                cur = Some(BucketState::new(bs, mid, vbid, vask, ci_ubp));
            }
            Some(_) => {
                // bs < state.ts: out-of-order record, skip.
            }
            None => {
                cur = Some(BucketState::new(bs, mid, vbid, vask, ci_ubp));
            }
        }
    }

    if let Some(state) = cur {
        out.push(state.finalize());
    }

    out
}

/// Streaming variant — yields bars as they close.
///
/// A bar closes when a subsequent record falls into a strictly later bucket.
/// The final bar is emitted when the input iterator ends.
pub fn idx_to_ohlc_stream<'a, I>(it: I, tf_ms: i64) -> impl Iterator<Item = Ohlc> + 'a
where
    I: Iterator<Item = &'a IndexRecord> + 'a,
{
    assert!(tf_ms > 0, "tf_ms must be positive");
    OhlcStream {
        inner: it,
        tf_ms,
        cur: None,
        done: false,
    }
}

struct OhlcStream<I> {
    inner: I,
    tf_ms: i64,
    cur: Option<BucketState>,
    done: bool,
}

impl<'a, I> Iterator for OhlcStream<I>
where
    I: Iterator<Item = &'a IndexRecord>,
{
    type Item = Ohlc;

    fn next(&mut self) -> Option<Self::Item> {
        if self.done {
            return None;
        }
        loop {
            match self.inner.next() {
                Some(rec) => {
                    let (ts_ms, mid, vbid, vask, ci_ubp) = decode_record(rec);
                    let bs = bucket_start(ts_ms, self.tf_ms);
                    match self.cur.as_mut() {
                        Some(state) if state.ts == bs => {
                            state.ingest(mid, vbid, vask, ci_ubp);
                        }
                        Some(state) if bs > state.ts => {
                            let closed = state.clone();
                            self.cur = Some(BucketState::new(bs, mid, vbid, vask, ci_ubp));
                            return Some(closed.finalize());
                        }
                        Some(_) => {
                            // Out-of-order; drop.
                        }
                        None => {
                            self.cur = Some(BucketState::new(bs, mid, vbid, vask, ci_ubp));
                        }
                    }
                }
                None => {
                    self.done = true;
                    return self.cur.take().map(BucketState::finalize);
                }
            }
        }
    }
}

/// Roll a TF-N series up to TF-M where M = k·N (k integer ≥ 2).
///
/// OHLC monoid: open=first.open, close=last.close, high=max, low=min,
/// vbid/vask/tick_count summed. `avg_ci_ubp` is rebuilt by un-compressing
/// each input bar's stored value, weighting by `tick_count`, and re-encoding.
pub fn rollup(series: &[Ohlc], src_tf_ms: i64, dst_tf_ms: i64) -> Vec<Ohlc> {
    assert!(src_tf_ms > 0 && dst_tf_ms > 0, "TFs must be positive");
    assert!(
        dst_tf_ms >= src_tf_ms && dst_tf_ms % src_tf_ms == 0,
        "dst_tf_ms must be an integer multiple of src_tf_ms"
    );
    if series.is_empty() {
        return Vec::new();
    }

    let mut out: Vec<Ohlc> = Vec::with_capacity(series.len() / 4 + 1);
    let mut cur: Option<BucketState> = None;

    for bar in series {
        let bs = bucket_start(bar.ts, dst_tf_ms);
        let ci_ubp_bar = decode_ci_ubp(bar.avg_ci_ubp);
        // Weight ci by tick_count so the rolled-up mean is a true tick-weighted
        // mean (matches what idx_to_ohlc would compute on the raw records).
        let ci_contrib = ci_ubp_bar * bar.tick_count as f64;
        match cur.as_mut() {
            Some(state) if state.ts == bs => {
                if bar.high > state.high {
                    state.high = bar.high;
                }
                if bar.low < state.low {
                    state.low = bar.low;
                }
                state.close = bar.close;
                state.vbid += bar.vbid;
                state.vask += bar.vask;
                state.tick_count = state.tick_count.saturating_add(bar.tick_count);
                state.sum_ci_ubp += ci_contrib;
            }
            Some(state) if bs > state.ts => {
                out.push(state.finalize());
                cur = Some(BucketState {
                    ts: bs,
                    open: bar.open,
                    high: bar.high,
                    low: bar.low,
                    close: bar.close,
                    vbid: bar.vbid,
                    vask: bar.vask,
                    tick_count: bar.tick_count,
                    sum_ci_ubp: ci_contrib,
                });
            }
            Some(_) => {
                // Out-of-order input bar; skip.
            }
            None => {
                cur = Some(BucketState {
                    ts: bs,
                    open: bar.open,
                    high: bar.high,
                    low: bar.low,
                    close: bar.close,
                    vbid: bar.vbid,
                    vask: bar.vask,
                    tick_count: bar.tick_count,
                    sum_ci_ubp: ci_contrib,
                });
            }
        }
    }

    if let Some(state) = cur {
        out.push(state.finalize());
    }

    out
}

// ── Re-exports for test convenience ─────────────────────────────────────

/// Decode an encoded `avg_ci_ubp` value back to `ci_ubp` (micro basis points).
/// Provided for tests / consumers that want to verify the mean directly.
#[inline]
pub fn ohlc_ci_ubp(avg_ci_ubp: u16) -> f64 {
    decode_ci_ubp(avg_ci_ubp)
}

/// Convert a `mitch::Bar` (96B kline) into the lightweight `Ohlc` row.
///
/// Used by `/v1/ohlc` to read `.s10` (uniform 10s bars) as the base layer
/// then `rollup()` to higher TFs. The Bar.kind field is ignored — caller
/// is expected to pre-filter by kind=Kline (0).
#[inline]
pub fn bar_to_ohlc(b: &mitch::bar::Bar) -> Ohlc {
    Ohlc {
        ts: b.open_time_ms(),
        open: b.open,
        high: b.high,
        low: b.low,
        close: b.close,
        vbid: b.vbid as u64,
        vask: b.vask as u64,
        tick_count: b.tick_count,
        avg_ci_ubp: b.avg_ci_ubp,
    }
}
