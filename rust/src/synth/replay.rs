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
    if !(b_bid > 0.0 && b_ask >= b_bid && q_bid > 0.0 && q_ask >= q_bid) {
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
    let rel_ci_ubp = (ci_b_ubp.powi(2) + ci_q_ubp.powi(2)).sqrt();

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
