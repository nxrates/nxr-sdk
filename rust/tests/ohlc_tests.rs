//! Tests for `ohlc.rs`: bucket-aligned resample, streaming variant, rollup.

use mitch::common::message_type;
use mitch::header::MitchHeader;
use mitch::index::Index;
use mitch::timestamp;

use nxr_sdk::ipc::record::IndexRecord;
use nxr_sdk::ohlc::{idx_to_ohlc, idx_to_ohlc_stream, ohlc_ci_ubp, rollup, Ohlc};
use nxr_sdk::tdwap::encode_ci_ubp;

// ── Test helpers ────────────────────────────────────────────────────────

/// Build a synthetic IndexRecord with the given epoch ms timestamp,
/// bid/ask, volumes, and ci_ubp (decoded, micro basis points of mid).
fn make_rec(ts_ms: i64, bid: f64, ask: f64, vbid: u32, vask: u32, ci_ubp: f64) -> IndexRecord {
    let mts = timestamp::from_epoch_ms(ts_ms);
    let header = MitchHeader::new(message_type::INDEX, 0, mts, 1);
    let ci = encode_ci_ubp(ci_ubp);
    let index = Index::new(
        0xDEADBEEFu64, // ticker (non-zero, validate() not called here)
        bid,
        ask,
        ci,
        vbid,
        vask,
        1,         // tick_count
        1,         // confidence
        1,         // accepted
        0,         // rejected
    );
    IndexRecord::new(header, index)
}

const TF_10S: i64 = 10_000;
const TF_60S: i64 = 60_000;

// ── Tests ───────────────────────────────────────────────────────────────

#[test]
fn empty_input_yields_empty_output() {
    let recs: Vec<IndexRecord> = Vec::new();
    let out = idx_to_ohlc(&recs, TF_60S);
    assert!(out.is_empty());
}

#[test]
fn single_record_yields_one_bar() {
    let base_ms = 1_700_000_000_000i64; // aligned to second
    let recs = vec![make_rec(base_ms, 100.0, 101.0, 5, 7, 0.0)];
    let out = idx_to_ohlc(&recs, TF_60S);
    assert_eq!(out.len(), 1);
    let b = out[0];
    let mid = (100.0 + 101.0) * 0.5;
    assert_eq!(b.open, mid);
    assert_eq!(b.high, mid);
    assert_eq!(b.low, mid);
    assert_eq!(b.close, mid);
    assert_eq!(b.vbid, 5);
    assert_eq!(b.vask, 7);
    assert_eq!(b.tick_count, 1);
    // Bucket-aligned ts must equal floor(ts/tf)*tf.
    assert_eq!(b.ts, (base_ms / TF_60S) * TF_60S);
}

#[test]
fn aligned_60s_six_records_one_bar() {
    // Bucket start chosen at an exact minute boundary in epoch ms.
    let base_ms = 1_700_000_000_000i64 - 1_700_000_000_000i64 % TF_60S;
    let prices: [(f64, f64); 6] = [
        (100.0, 101.0),
        (100.5, 101.5),
        (102.0, 103.0), // high lives here
        (99.0, 100.0),  // low lives here
        (101.0, 102.0),
        (100.0, 101.0), // close = (100+101)/2 = 100.5
    ];
    let recs: Vec<IndexRecord> = prices
        .iter()
        .enumerate()
        .map(|(i, (b, a))| make_rec(base_ms + (i as i64) * 10_000, *b, *a, 10, 20, 0.0))
        .collect();

    let out = idx_to_ohlc(&recs, TF_60S);
    assert_eq!(out.len(), 1);
    let b = out[0];
    assert_eq!(b.ts, base_ms);
    assert_eq!(b.open, (100.0 + 101.0) * 0.5);
    assert_eq!(b.high, (102.0 + 103.0) * 0.5);
    assert_eq!(b.low, (99.0 + 100.0) * 0.5);
    assert_eq!(b.close, (100.0 + 101.0) * 0.5);
    assert_eq!(b.vbid, 60);
    assert_eq!(b.vask, 120);
    assert_eq!(b.tick_count, 6);
}

#[test]
fn multi_bucket_60s_records_split() {
    // 12 records: 6 in bucket A, 6 in bucket B (60s apart).
    let base_ms = 1_700_000_000_000i64 - 1_700_000_000_000i64 % TF_60S;
    let mut recs = Vec::with_capacity(12);
    for i in 0..6 {
        recs.push(make_rec(base_ms + (i as i64) * 10_000, 100.0 + i as f64, 101.0 + i as f64, 1, 1, 0.0));
    }
    for i in 0..6 {
        recs.push(make_rec(base_ms + 60_000 + (i as i64) * 10_000, 200.0 + i as f64, 201.0 + i as f64, 2, 2, 0.0));
    }
    let out = idx_to_ohlc(&recs, TF_60S);
    assert_eq!(out.len(), 2);
    assert_eq!(out[0].ts, base_ms);
    assert_eq!(out[1].ts, base_ms + 60_000);
    assert_eq!(out[0].tick_count, 6);
    assert_eq!(out[1].tick_count, 6);
    assert_eq!(out[0].vbid, 6);
    assert_eq!(out[1].vbid, 12);
    // Spot-check open/close of second bucket.
    assert_eq!(out[1].open, (200.0 + 201.0) * 0.5);
    assert_eq!(out[1].close, (205.0 + 206.0) * 0.5);
}

#[test]
fn rollup_10s_to_60s() {
    // Build 6 synthetic 10s Ohlc bars; roll up to one 60s bar.
    let base_ms = 1_700_000_000_000i64 - 1_700_000_000_000i64 % TF_60S;
    let bars: Vec<Ohlc> = (0..6)
        .map(|i| {
            let o = 100.0 + i as f64;
            let h = o + 0.5;
            let l = o - 0.5;
            let c = o + 0.25;
            Ohlc {
                ts: base_ms + (i as i64) * TF_10S,
                close_ts: base_ms + (i as i64) * TF_10S + TF_10S - 1,
                open: o,
                high: h,
                low: l,
                close: c,
                vbid: 10,
                vask: 20,
                tick_count: 5,
                avg_ci_ubp: 0,
            }
        })
        .collect();

    let out = rollup(&bars, TF_10S, TF_60S);
    assert_eq!(out.len(), 1);
    let b = out[0];
    assert_eq!(b.ts, base_ms);
    assert_eq!(b.open, bars[0].open);
    assert_eq!(b.close, bars[5].close);
    let expected_high = bars.iter().map(|x| x.high).fold(f64::NEG_INFINITY, f64::max);
    let expected_low = bars.iter().map(|x| x.low).fold(f64::INFINITY, f64::min);
    assert_eq!(b.high, expected_high);
    assert_eq!(b.low, expected_low);
    assert_eq!(b.vbid, 60);
    assert_eq!(b.vask, 120);
    assert_eq!(b.tick_count, 30);
}

#[test]
fn streaming_matches_batch() {
    // Build a mixed multi-bucket input and verify the streaming variant
    // collects to exactly the same Vec<Ohlc> as the batch fn.
    let base_ms = 1_700_000_000_000i64 - 1_700_000_000_000i64 % TF_60S;
    let mut recs = Vec::new();
    // 3 buckets: 4 records, 3 records, 1 record.
    for i in 0..4 {
        recs.push(make_rec(base_ms + (i as i64) * 5_000, 100.0 + i as f64, 101.0 + i as f64, 1, 1, 100.0));
    }
    for i in 0..3 {
        recs.push(make_rec(base_ms + 60_000 + (i as i64) * 10_000, 110.0 + i as f64, 111.0 + i as f64, 2, 2, 200.0));
    }
    recs.push(make_rec(base_ms + 120_000, 130.0, 131.0, 3, 3, 300.0));

    let batch = idx_to_ohlc(&recs, TF_60S);
    let streamed: Vec<Ohlc> = idx_to_ohlc_stream(recs.iter(), TF_60S).collect();
    assert_eq!(batch, streamed);
    assert_eq!(batch.len(), 3);
}

#[test]
fn avg_ci_ubp_encodes_mean() {
    // 5 records with known ci_ubp values. The decoded mean from the
    // sqrt-compressed avg_ci_ubp should match the arithmetic mean within
    // round-trip tolerance of the sqrt-compression.
    let base_ms = 1_700_000_000_000i64 - 1_700_000_000_000i64 % TF_60S;
    let cis = [100.0, 200.0, 400.0, 800.0, 1600.0]; // ci_ubp values
    let recs: Vec<IndexRecord> = cis
        .iter()
        .enumerate()
        .map(|(i, ci)| make_rec(base_ms + (i as i64) * 10_000, 100.0, 101.0, 1, 1, *ci))
        .collect();
    let out = idx_to_ohlc(&recs, TF_60S);
    assert_eq!(out.len(), 1);
    // Reconstruct mean of *encoded* round-tripped ci_ubp values, since the
    // wire encoding loses sub-sqrt precision before we ingest.
    let round_tripped: Vec<f64> = cis
        .iter()
        .map(|c| nxr_sdk::tdwap::decode_ci_ubp(encode_ci_ubp(*c)))
        .collect();
    let want_mean = round_tripped.iter().sum::<f64>() / round_tripped.len() as f64;
    let got_mean = ohlc_ci_ubp(out[0].avg_ci_ubp);
    // Allow ~1% relative slack: the sqrt-then-square round-trip plus the
    // final mean re-encode introduces small quantization error.
    let rel = (got_mean - want_mean).abs() / want_mean.max(1.0);
    assert!(rel < 0.01, "decoded mean {} vs want {} (rel {})", got_mean, want_mean, rel);
}

#[test]
fn gap_between_records_no_synthetic_bars() {
    // Two records with a 120s gap, TF=10s. Must produce exactly 2 bars,
    // with discontiguous ts (no synthetic zero-volume fillers in between).
    let base_ms = 1_700_000_000_000i64 - 1_700_000_000_000i64 % TF_10S;
    let recs = vec![
        make_rec(base_ms, 100.0, 101.0, 1, 1, 0.0),
        make_rec(base_ms + 120_000, 200.0, 201.0, 1, 1, 0.0),
    ];
    let out = idx_to_ohlc(&recs, TF_10S);
    assert_eq!(out.len(), 2, "must NOT emit synthetic gap bars");
    assert_eq!(out[0].ts, base_ms);
    assert_eq!(out[1].ts, base_ms + 120_000);
    // ts gap (120000) is larger than TF (10000) by 12x -> consumer detects.
    assert!(out[1].ts - out[0].ts == 120_000);
}
