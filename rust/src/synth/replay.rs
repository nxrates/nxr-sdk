//! Single-source synth-tick computation, shared by the live event-driven
//! kernel (`core::synth_kernel`) and the offline backfill driver
//! (`series-factory::synth_backfill_from_idx`).
//!
//! ## Conservative spread compounding (Tanaka rule)
//!
//! For `synth = base / quote`:
//!
//! ```text
//! synth.bid = base.bid / quote.ask    // sell base @ bid, buy quote @ ask
//! synth.ask = base.ask / quote.bid    // buy base @ ask, sell quote @ bid
//! ```
//!
//! This is the worst-case taker rule. Mid-mid would under-state the cross
//! spread by ~2× and feed a tighter σ-EMA into the renko producer than
//! reality warrants.
//!
//! ## TTL guard
//!
//! [`LEG_STALE_TTL_MS`] (5 s): either leg silent past TTL ⇒ suppress emit.
//! Reference clock: `max(now_ms, base_ts_ms, quote_ts_ms)` so a synthetic
//! replay clock can never trip the stale check on its own.
//!
//! ## CI propagation
//!
//! `σ_z/z = √((σ_x/x)² + (σ_y/y)²)` — relative-CI compounded across legs.
//!
//! ## Min-leg aggregation
//!
//! `vbid`, `vask`, `confidence`, `accepted` all inherit the worst-leg value.
//! `rejected` is reset to 0 on the synth (the synth tick itself was not
//! rejected — both inputs passed the sanity gate).

use crate::tdwap::{decode_ci_ubp, encode_ci_ubp};
use crate::IndexRecord;
use mitch::header::MitchHeader;
use mitch::index::Index;
use mitch::timestamp;

/// Either leg silent > TTL ⇒ suppress synth emit.
pub const LEG_STALE_TTL_MS: i64 = 5_000;

/// Provider id stamped on the synth `MitchHeader`. Distinct from the
/// aggregator (`0`) so consumers can tell a synth tick from a native one by
/// header inspection. Real MITCH providers are < 2000; 2010 is the synth
/// reservation.
pub const SYNTH_KERNEL_PROVIDER_ID: u16 = 2010;

/// Compute a synth `IndexRecord` from two leg snapshots, or return `None`
/// when any of the freshness / sanity / numeric gates fail.
///
/// Caller supplies:
/// - `base` / `quote`: the most recent `Index` body for each leg
/// - `base_ts_ms` / `quote_ts_ms`: epoch-ms timestamp of each snapshot
/// - `now_ms`: reference clock for the TTL gate (typically the inbound tick's
///   ts; replay drivers pass the tick's own ts so TTL is purely leg-to-leg).
/// - `synth_id`: MITCH ticker id of the output synth
/// - `seq`: monotonic sequence to stamp onto the synth header (caller wraps)
///
/// Returns the synth `IndexRecord` ready to be broadcast / sharded.
pub fn compute_synth_index(
    base: &Index,
    quote: &Index,
    base_ts_ms: i64,
    quote_ts_ms: i64,
    now_ms: i64,
    synth_id: u64,
    seq: u16,
) -> Option<IndexRecord> {
    // TTL gate.
    let ref_ts = now_ms.max(base_ts_ms).max(quote_ts_ms);
    if (ref_ts - base_ts_ms) > LEG_STALE_TTL_MS
        || (ref_ts - quote_ts_ms) > LEG_STALE_TTL_MS
    {
        return None;
    }

    // Sanity gates. Copy out of (potentially packed) Index body.
    let b_bid = base.bid;
    let b_ask = base.ask;
    let q_bid = quote.bid;
    let q_ask = quote.ask;
    let b_conf = base.confidence;
    let q_conf = quote.confidence;
    if b_conf == 0 || q_conf == 0 {
        return None;
    }
    // Magnitude cap on BOTH legs, not just finiteness: a poisoned quote leg
    // (finite but astronomical) divided into a normal base leg yields a
    // tiny-but-finite result that would sail past the `is_finite()` checks
    // below undetected (2026-07-10 incident - see mitch::MAX_PRICE doc).
    if !(b_bid > 0.0
        && b_ask >= b_bid
        && b_ask <= mitch::MAX_PRICE
        && q_bid > 0.0
        && q_ask >= q_bid
        && q_ask <= mitch::MAX_PRICE)
    {
        return None;
    }

    // Conservative spread compounding (Tanaka rule).
    let cross_bid = b_bid / q_ask;
    let cross_ask = b_ask / q_bid;
    if !(cross_bid > 0.0
        && cross_ask >= cross_bid
        && cross_bid.is_finite()
        && cross_ask.is_finite())
    {
        return None;
    }
    let cross_mid = 0.5 * (cross_bid + cross_ask);
    if !(cross_mid > 0.0 && cross_mid.is_finite()) {
        return None;
    }

    // CI propagation.
    let ci_b_ubp = decode_ci_ubp(base.ci);
    let ci_q_ubp = decode_ci_ubp(quote.ci);
    let rel_ci_ubp = crate::stats::rss(&[ci_b_ubp, ci_q_ubp]);

    // Min-leg aggregation.
    let vbid = base.vbid.min(quote.vbid);
    let vask = base.vask.min(quote.vask);
    let confidence = b_conf.min(q_conf);
    let accepted = base.accepted.min(quote.accepted);

    let synth_idx = Index {
        ticker: synth_id,
        bid: cross_bid,
        ask: cross_ask,
        vbid,
        vask,
        ci: encode_ci_ubp(rel_ci_ubp),
        tick_count: 1,
        confidence,
        accepted,
        rejected: 0,
        flags: 0,
    };

    // Stamp the newer of the two leg timestamps onto the synth header.
    let stamp_ts_ms = base_ts_ms.max(quote_ts_ms);
    let stamp_mts = timestamp::from_epoch_ms(stamp_ts_ms);
    let mut header = MitchHeader::new(
        mitch::common::message_type::INDEX,
        SYNTH_KERNEL_PROVIDER_ID,
        stamp_mts,
        1,
    );
    header.set_sequence(seq);
    Some(IndexRecord::new(header, synth_idx))
}

/// Two-leg merge state machine that turns an interleaved, ts-ascending stream
/// of leg `IndexRecord`s into the gated synth tick stream.
///
/// Single source of truth for the *gated* reconstruction, shared by:
/// - the offline backfill driver (`series-factory::synth_backfill_from_idx`),
/// - the offline calibrator (`series-factory::nxr_calibrate`), and
/// - (semantically) the live kernel (`core::synth_kernel`).
///
/// Methodology §5 (one reconstruction path, hist==live): the calibrator MUST
/// fit `k` on the SAME tick density the live/backfill renko producer sees. The
/// gate ([`compute_synth_index`]: TTL + confidence + sanity) drops synth ticks
/// during stale-leg / low-confidence windows; an ungated merge over-counts
/// crossings in those windows → too-small `k` → live over-emit. Routing both
/// calibrate and backfill through this state machine makes the reconstruction
/// byte-identical.
pub struct SynthReplayState {
    pub synth_id: u64,
    pub base_id: u64,
    pub quote_id: u64,
    last_base: Option<(Index, i64)>,
    last_quote: Option<(Index, i64)>,
    /// Wrapping monotonic sequence stamped on emitted synth headers.
    seq: u16,
    /// Counters surfaced by callers (e.g. backfill side-car JSON).
    pub emit_count: u64,
    pub stale_drop_count: u64,
}

impl SynthReplayState {
    pub fn new(synth_id: u64, base_id: u64, quote_id: u64) -> Self {
        Self {
            synth_id,
            base_id,
            quote_id,
            last_base: None,
            last_quote: None,
            seq: 0,
            emit_count: 0,
            stale_drop_count: 0,
        }
    }

    /// Feed one leg tick. Returns the synth `IndexRecord` if a synth emit is
    /// warranted (both legs live, both within TTL, sanity gates pass).
    ///
    /// `now_ms` is the wall-clock the live kernel would have seen — for replay
    /// we pass the tick's own `ts_ms` (so the TTL gate is purely a function of
    /// leg-to-leg staleness, never of replay-clock drift). A record whose
    /// ticker is neither leg is ignored (returns `None` without mutating
    /// counters).
    pub fn feed_leg_tick(&mut self, rec: &IndexRecord, now_ms: i64) -> Option<IndexRecord> {
        // Copy ticker out of the packed body before comparing.
        let ticker = rec.index.ticker;
        let is_base = ticker == self.base_id;
        let is_quote = ticker == self.quote_id;
        if !is_base && !is_quote {
            return None;
        }
        let ts_ms = {
            let header = rec.header;
            timestamp::to_epoch_ms(header.get_timestamp())
        };

        if is_base {
            self.last_base = Some((rec.index, ts_ms));
        } else {
            self.last_quote = Some((rec.index, ts_ms));
        }

        let (base, base_ts) = self.last_base?;
        let (quote, quote_ts) = self.last_quote?;

        // All math + gates live in `compute_synth_index` (single source for
        // live kernel + offline replay). The compute helper folds TTL + sanity
        // drops together; any `None` is counted as a stale-drop here (matches
        // the live kernel's semantics).
        let synth_rec =
            match compute_synth_index(&base, &quote, base_ts, quote_ts, now_ms, self.synth_id, self.seq) {
                Some(r) => r,
                None => {
                    self.stale_drop_count += 1;
                    return None;
                }
            };
        self.seq = self.seq.wrapping_add(1);
        self.emit_count += 1;
        Some(synth_rec)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn mk_index(ticker: u64, bid: f64, ask: f64) -> Index {
        Index {
            ticker,
            bid,
            ask,
            vbid: 100,
            vask: 100,
            ci: 0,
            tick_count: 1,
            confidence: 3,
            accepted: 3,
            rejected: 0,
            flags: 0,
        }
    }

    #[test]
    fn conservative_spread_rule() {
        let base = mk_index(1, 3000.0, 3001.0);
        let quote = mk_index(2, 60_000.0, 60_010.0);
        let t = 1_700_000_000_000_i64;
        let out = compute_synth_index(&base, &quote, t, t, t, 42, 0).expect("synth");
        let bid = out.index.bid;
        let ask = out.index.ask;
        assert!((bid - 3000.0 / 60_010.0).abs() < 1e-12);
        assert!((ask - 3001.0 / 60_000.0).abs() < 1e-12);
        assert!(ask > bid);
    }

    #[test]
    fn ttl_drops_stale_leg() {
        let base = mk_index(1, 3000.0, 3001.0);
        let quote = mk_index(2, 60_000.0, 60_010.0);
        let t = 1_700_000_000_000_i64;
        // Quote 6 s older than base: TTL=5s ⇒ drop.
        let res = compute_synth_index(&base, &quote, t, t - 6_000, t, 42, 0);
        assert!(res.is_none());
    }

    #[test]
    fn accepted_is_min_leg() {
        let mut base = mk_index(1, 3000.0, 3001.0);
        let mut quote = mk_index(2, 60_000.0, 60_010.0);
        base.accepted = 5;
        quote.accepted = 2;
        let t = 1_700_000_000_000_i64;
        let out = compute_synth_index(&base, &quote, t, t, t, 42, 0).expect("synth");
        let accepted = out.index.accepted;
        assert_eq!(accepted, 2);
    }
}
