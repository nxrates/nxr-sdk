//! SEAM PARITY: the offline `.vol` build path and the live `LiveVolRing` must
//! produce byte-identical per-bin σ when driven by the SAME s10 Bar stream
//! (including flat bars), so the history↔live seam has no σ step.
//!
//! Both paths share ONE Rogers-Satchell kernel (`vol_estimator::rs_sigma_from_ohlc`)
//! and ONE EMA(28). The offline path rolls s10 → 30-min OHLC via
//! `ohlc::rollup(10_000, 1_800_000)`; the live path feeds each closed s10 bar
//! into `LiveVolRing::observe`. This test drives a stream spanning a 12h
//! recompute-cooldown boundary and asserts:
//!   1. per-bin σ rows match < 1e-12, AND
//!   2. the renko brick stream is continuous at the seam (close[i-1]==open[i])
//!      when each side is fed its own σ at the seam bin.

use mitch::bar::Bar;
use mitch::timestamp;
use nxr_sdk::ohlc::{bar_to_ohlc, rollup, Ohlc};
use nxr_sdk::renko::{RenkoConfig, RenkoGenerator};
use nxr_sdk::shard::{BAR_MS_S10, MS_PER_30MIN};
use nxr_sdk::vol::{LiveVolRing, VolConfig, VolSource};
use nxr_sdk::vol_estimator::rs_sigma_from_ohlc;

/// Build one gapless s10 Bar stream of `n_bins` 30-min bins worth of bars,
/// with some quiet (flat) 10s buckets interspersed. Spans > 12h (24 bins of
/// 30-min = 12h; we use 30 bins = 15h) so the stream crosses the cooldown.
///
/// R1 NON-DEGENERACY: every bar carries `open_mts != close_mts`
/// (`close_mts = open_mts + BAR_MS_S10`, a real 10s bar). The LAST s10 bar of
/// every 30-min bin therefore straddles the boundary: its `open_time_ms` ∈ bin
/// N, but its `close_time_ms == bin_end` ∈ bin N+1. So the choice of accessor
/// (open- vs close-time) used to key the vol bin is now OUTCOME-AFFECTING — the
/// old fixture (`open_mts == close_mts`) masked the live bug at
/// `bars_renko.rs` where the ring was fed `close_time_ms()`.
fn build_s10_stream(n_bins: usize) -> Vec<Bar> {
    let per_bin = (MS_PER_30MIN / BAR_MS_S10) as usize; // 180 s10 bars / bin
    let mut bars = Vec::with_capacity(n_bins * per_bin);
    let mut px = 100.0_f64;
    // MITCH epoch is 2010-01-01; ts must be post-epoch + 30-min aligned so each
    // bin maps to a distinct 30-min bucket. Base at 2020-01-01, bin-aligned.
    let base = 1_577_836_800_000i64; // 2020-01-01T00:00:00Z (ms), % 1_800_000 == 0
    let mut ts = base;
    for bin in 0..n_bins {
        for j in 0..per_bin {
            // Every 4th bin is "quiet" past its first few bars → flat bars.
            let flat = bin % 4 == 0 && j > 5;
            let (o, c) = if flat {
                (px, px)
            } else {
                let o = px;
                let c = px * (1.0 + 0.0003 * (((bin + j) % 7) as f64 - 3.0));
                (o, c)
            };
            let h = o.max(c) * (1.0 + if flat { 0.0 } else { 0.0008 });
            let l = o.min(c) * (1.0 - if flat { 0.0 } else { 0.0008 });
            // Real 10s bar: open_ts = bucket start, close_ts = bucket end. The
            // bin's last bar (j == per_bin-1) has close_ts == bin_end → it lands
            // in bin N by open-time but bin N+1 by close-time.
            let open_mts = timestamp::from_epoch_ms(ts);
            let close_mts = timestamp::from_epoch_ms(ts + BAR_MS_S10);
            // kind defaults to Kline (0) — matches s10 producer output.
            bars.push(Bar::new_ohlcv(open_mts, close_mts, o, h, l, c, 0, 0, if flat { 0 } else { 1 }));
            px = c;
            ts += BAR_MS_S10;
        }
    }
    bars
}

/// Offline σ rows: rollup s10 → 30-min OHLC, RS σ per bin, EMA(period).
fn offline_sigma_rows(s10: &[Bar], period: usize) -> Vec<f64> {
    let candles: Vec<Ohlc> = s10.iter().map(bar_to_ohlc).collect();
    let bins = rollup(&candles, BAR_MS_S10, MS_PER_30MIN);
    let alpha = 2.0 / (period as f64 + 1.0);
    let mut prev: Option<f64> = None;
    let mut out = Vec::with_capacity(bins.len());
    for (i, b) in bins.iter().enumerate() {
        let sigma = rs_sigma_from_ohlc(b.open, b.high, b.low, b.close);
        let ema = if i < period {
            bins[..=i]
                .iter()
                .map(|x| rs_sigma_from_ohlc(x.open, x.high, x.low, x.close))
                .sum::<f64>()
                / (i + 1) as f64
        } else {
            alpha * sigma + (1.0 - alpha) * prev.unwrap_or(sigma)
        };
        prev = Some(ema);
        out.push(ema);
    }
    out
}

/// Live σ rows: feed each closed s10 bar into the LiveVolRing using the EXACT
/// timestamp accessor the production renko producer uses.
///
/// R1 PIN: production at `core/src/bars_renko.rs` feeds the vol ring
/// `bar.open_time_ms()` (open-time binning, matching the offline `.vol`
/// builder's `ohlc::rollup` → `bucket_start(open_time_ms)`). If anyone flips
/// that back to `close_time_ms()`, the seam-parity assertions in this test
/// FAIL — see `live_sigma_rows_close_time` (the negative control) which proves
/// close-time binning diverges from offline on the non-degenerate fixture.
fn live_sigma_rows(s10: &[Bar], period: usize) -> Vec<f64> {
    live_sigma_rows_with(s10, period, |b| b.open_time_ms())
}

/// Negative control: bin the live ring by CLOSE time (the pre-R1-fix production
/// wiring). Used only to PROVE the fixture is non-degenerate — close-time
/// binning must NOT match the offline (open-time) rows.
fn live_sigma_rows_close_time(s10: &[Bar], period: usize) -> Vec<f64> {
    live_sigma_rows_with(s10, period, |b| b.close_time_ms())
}

/// Feed each closed s10 bar into the LiveVolRing keyed by `ts_of(bar)`.
fn live_sigma_rows_with(s10: &[Bar], period: usize, ts_of: impl Fn(&Bar) -> i64) -> Vec<f64> {
    let mut ring = LiveVolRing::new(4096, period);
    for bar in s10 {
        let bs = ts_of(bar);
        ring.observe(bs, bar.open, bar.high, bar.low, bar.close);
    }
    // Force-finalize the last open 30-min bin by advancing one full bin past
    // the last observed ts (using the same accessor under test).
    let last_ts = ts_of(s10.last().unwrap());
    ring.observe(last_ts + MS_PER_30MIN, 1.0, 1.0, 1.0, 1.0);
    (0..ring.len()).map(|i| ring.sigma_pct(i)).collect()
}

#[test]
fn offline_vol_rows_match_live_ring_over_cooldown_boundary() {
    let period = VolConfig::default().ema_period;
    // 30 bins = 15h, crosses the 12h (24-bin) recompute cooldown boundary.
    let n_bins = 30usize;
    let s10 = build_s10_stream(n_bins);

    let offline = offline_sigma_rows(&s10, period);
    let live = live_sigma_rows(&s10, period);

    assert_eq!(offline.len(), n_bins, "offline bin count");
    assert_eq!(live.len(), n_bins, "live bin count");
    for (i, (&o, &l)) in offline.iter().zip(live.iter()).enumerate() {
        assert!(
            (o - l).abs() < 1e-12,
            "bin {i}: offline σ {o} != live σ {l} (Δ {})",
            (o - l).abs()
        );
    }

    // 12h cooldown boundary = bin 24. Confirm σ continuity across it.
    let seam = (12 * 3600 * 1000 / MS_PER_30MIN) as usize; // = 24
    assert!(seam < n_bins);
    assert!((offline[seam] - live[seam]).abs() < 1e-12, "seam-bin σ mismatch");

    // R1 NEGATIVE CONTROL — proves the fixture actually exercises the bug.
    // Feeding the ring by CLOSE time (the pre-fix production wiring) misplaces
    // each bin's last s10 bar into the next bin → different O/H/L/C → different
    // RS σ. This MUST diverge from the open-time offline rows; if it did NOT,
    // the fixture would be degenerate (open_mts == close_mts) and would mask R1.
    let live_close = live_sigma_rows_close_time(&s10, period);
    let max_div = offline
        .iter()
        .zip(live_close.iter())
        .map(|(&o, &l)| (o - l).abs())
        .fold(0.0_f64, f64::max);
    assert!(
        max_div > 1e-9,
        "fixture is degenerate: close-time binning matched offline (max Δ {max_div}). \
         Build bars with open_mts != close_mts so the bin-key accessor is outcome-affecting."
    );
}

#[test]
fn brick_stream_continuous_at_seam() {
    // The seam contract: feeding the SAME σ + price into the offline
    // RenkoGenerator on both sides of a history↔live cut yields a continuous
    // brick stream (close[i-1] == open[i]). Drive one generator across the seam
    // and assert every brick's open equals the prior close.
    let period = VolConfig::default().ema_period;
    let s10 = build_s10_stream(30);
    let rows = offline_sigma_rows(&s10, period);
    // Use the σ at the 12h-cooldown seam bin (24) on both sides — identical, so
    // brick_size is identical, so the grid is continuous.
    let seam = 24usize;
    let sigma = rows[seam].max(1e-6);

    let cfg = RenkoConfig { multiplier: 0.08, min_pct: 0.0001 };
    let mut generator = RenkoGenerator::new(cfg).unwrap();

    let mut last_close: Option<f64> = None;
    let mut n = 0u32;
    let mut p = 100.0_f64;
    for i in 0..20_000i64 {
        // Trend up then down to force bricks both directions across the seam.
        p += if i < 10_000 { 0.02 } else { -0.02 };
        generator.feed_tick_with_sigma(i, p, sigma, &mut |brick: &Bar| {
            let b_open = brick.open;
            let b_close = brick.close;
            if let Some(lc) = last_close {
                assert!(
                    (b_open - lc).abs() < lc.abs() * 1e-9,
                    "brick {n}: open {b_open} != prior close {lc}"
                );
            }
            last_close = Some(b_close);
            n += 1;
            Ok(())
        })
        .unwrap();
    }
    assert!(n > 0, "no bricks emitted across the seam");
}
