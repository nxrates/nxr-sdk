//! Synth library tests — ports `~/Work/btr/sdk/test/synth-tick.test.ts` and
//! `~/Work/btr/sdk/test/synth-ohlc.gbm.test.ts` verbatim, with leg symbols
//! rewritten in NXR slash format (e.g. `BTCUSDC` → `BTC/USDC`).
//!
//! Assertions on bid/ask/mid/conf composition, identity short-circuit, missing
//! leg → None, degeneracy → None, and a GBM Monte-Carlo bias test on the
//! quadratic-form RS estimator across M1/M5/H1 rollups.

use std::collections::HashMap;

use nxr_sdk::synth::{
    OhlcLite, RollingCorrelation, TimedOhlc, VarianceEstimator,
    compute_synth_tick, reconstruct_synth_ohlc, reconstruct_synth_series_at_base_tf_then_rollup,
};
use nxr_sdk::synth::paths::{SynthPath, Leg};
use nxr_sdk::synth::tick::LegTick;

// ─── Helpers ────────────────────────────────────────────────────────────

fn tick(bid: f64, ask: f64) -> LegTick {
    LegTick { bid, ask, mid: (bid + ask) / 2.0, conf: 10_000 }
}

fn tick_conf(bid: f64, ask: f64, conf: u16) -> LegTick {
    LegTick { bid, ask, mid: (bid + ask) / 2.0, conf }
}

fn close(a: f64, b: f64, abs_eps: f64) -> bool {
    (a - b).abs() <= abs_eps
}

fn mk_path(sym: &str, legs: &[(&str, i8)]) -> SynthPath {
    SynthPath {
        sym: sym.to_string(),
        legs: legs.iter().map(|(s, e)| Leg::new(*s, *e)).collect(),
    }
}

// ─── computeSynthTick (port of synth-tick.test.ts) ─────────────────────

/// Port of BTR test "2-leg product: BTCUSDT = BTCUSDC × USDCUSDT" — re-cast in
/// NXR symbols. Since NXR is USDT-canonical, the BTR back-compat path
/// `BTCUSDT = BTCUSDC × USDCUSDT` makes less sense here; we use the spiritually
/// equivalent **`BTC/USDC` via BTC/USDT × USDT/USDC** which exercises the same
/// 2-leg-product code path. Math identical: out.bid = leg1.bid × leg2.bid etc.
#[test]
fn two_leg_product() {
    // Synthesize "BTC/USDC" = (BTC/USDT) × (USDT/USDC) (which would be a -1 leg of USDC/USDT).
    // Use a fabricated path with both exp=+1 to exactly mirror the BTR test.
    let path = mk_path("BTC_X_USDT", &[("BTC/USDT", 1), ("USDT/USDC", 1)]);
    let mut legs: HashMap<&str, LegTick> = HashMap::new();
    legs.insert("BTC/USDT", tick(99_500.0, 100_500.0));
    legs.insert("USDT/USDC", tick(0.9999, 1.0001));
    let out = compute_synth_tick(&path, &legs).expect("composable");
    // mid = 100_000 × 1.0 = 100_000
    assert!(close(out.mid, 100_000.0, 1.0));
    // bid = 99_500 × 0.9999, ask = 100_500 × 1.0001
    assert!(close(out.bid, 99_500.0 * 0.9999, 1e-3));
    assert!(close(out.ask, 100_500.0 * 1.0001, 1e-3));
    assert!(out.bid <= out.mid);
    assert!(out.mid <= out.ask);
    assert_eq!(out.conf, 10_000);
}

/// Port of BTR test "1-leg inversion: USDCBTC = 1 / BTCUSDC (bid↔ask swap)".
/// NXR uses `USDT/BTC = 1 / (BTC/USDT)`.
#[test]
fn one_leg_inversion() {
    let path = &mk_path("USDT/BTC", &[("BTC/USDT", -1)]);
    let mut legs: HashMap<&str, LegTick> = HashMap::new();
    legs.insert("BTC/USDT", tick(99_500.0, 100_500.0));
    let out = compute_synth_tick(path, &legs).expect("composable");
    // inv.bid = 1/ask, inv.ask = 1/bid → bid < ask preserved.
    assert!(close(out.bid, 1.0 / 100_500.0, 1e-12));
    assert!(close(out.ask, 1.0 / 99_500.0, 1e-12));
    assert!(close(out.mid, 1.0 / 100_000.0, 1e-12));
    assert!(out.bid < out.ask);
}

/// Port of BTR test "2-leg ratio: ETHBTC = ETHUSDC / BTCUSDC".
/// NXR uses `ETH/BTC = (ETH/USDT) / (BTC/USDT)`.
#[test]
fn two_leg_ratio() {
    let path = &mk_path("ETH/BTC", &[("ETH/USDT", 1), ("BTC/USDT", -1)]);
    let mut legs: HashMap<&str, LegTick> = HashMap::new();
    legs.insert("ETH/USDT", tick(2_790.0, 2_810.0));    // mid 2800
    legs.insert("BTC/USDT", tick(99_500.0, 100_500.0)); // mid 100_000
    let out = compute_synth_tick(path, &legs).expect("composable");
    // mid ≈ 2800 / 100_000 = 0.028
    assert!(close(out.mid, 0.028, 1e-5));
    // bid = ethBid / btcAsk, ask = ethAsk / btcBid
    assert!(close(out.bid, 2_790.0 / 100_500.0, 1e-8));
    assert!(close(out.ask, 2_810.0 / 99_500.0, 1e-8));
    assert!(out.bid <= out.mid);
    assert!(out.mid <= out.ask);
}

/// Port of BTR test "conf composes via min; any leg conf=0 ⇒ synth conf=0".
#[test]
fn conf_composes_min() {
    let path = mk_path("X", &[("BTC/USDT", 1), ("USDT/USDC", 1)]);
    // Stale BTC leg → 0.
    let mut legs: HashMap<&str, LegTick> = HashMap::new();
    legs.insert("BTC/USDT", tick_conf(99_500.0, 100_500.0, 0));
    legs.insert("USDT/USDC", tick_conf(0.9999, 1.0001, 10_000));
    let stale = compute_synth_tick(&path, &legs).expect("composable");
    assert_eq!(stale.conf, 0);

    // Both fresh → 10000.
    legs.insert("BTC/USDT", tick_conf(99_500.0, 100_500.0, 10_000));
    legs.insert("USDT/USDC", tick_conf(0.9999, 1.0001, 10_000));
    let fresh = compute_synth_tick(&path, &legs).expect("composable");
    assert_eq!(fresh.conf, 10_000);

    // Min applies on intermediate values.
    legs.insert("BTC/USDT", tick_conf(99_500.0, 100_500.0, 5_000));
    legs.insert("USDT/USDC", tick_conf(0.9999, 1.0001, 8_000));
    let mid = compute_synth_tick(&path, &legs).expect("composable");
    assert_eq!(mid.conf, 5_000);
}

/// Port of BTR test "missing leg → null".
#[test]
fn missing_leg_returns_none() {
    let path = mk_path("X", &[("BTC/USDT", 1), ("USDT/USDC", 1)]);
    let mut legs: HashMap<&str, LegTick> = HashMap::new();
    legs.insert("BTC/USDT", tick(99_500.0, 100_500.0));
    // USDT/USDC missing.
    assert!(compute_synth_tick(&path, &legs).is_none());
}

/// Port of BTR test "non-positive leg quote → null".
#[test]
fn nonpositive_quote_returns_none() {
    let path = mk_path("X", &[("BTC/USDT", 1)]);
    let mut legs: HashMap<&str, LegTick> = HashMap::new();
    legs.insert("BTC/USDT", LegTick { bid: 0.0, ask: 100_500.0, mid: 50_250.0, conf: 10_000 });
    assert!(compute_synth_tick(&path, &legs).is_none());
}

/// Port of BTR test "identity (0-leg) → 1.0 w/ full conf".
#[test]
fn identity_zero_leg() {
    let path = &mk_path("EUR/EUR", &[]);
    let legs: HashMap<&str, LegTick> = HashMap::new();
    let out = compute_synth_tick(path, &legs).expect("identity composable");
    assert_eq!(out.mid, 1.0);
    assert_eq!(out.bid, 1.0);
    assert_eq!(out.ask, 1.0);
    assert_eq!(out.conf, 10_000);
}

/// Port of BTR test "bid ≤ mid ≤ ask preserved across random 2-leg compositions".
#[test]
fn bid_mid_ask_invariant() {
    let path = &mk_path("ETH/BTC", &[("ETH/USDT", 1), ("BTC/USDT", -1)]);
    for s in 1..=50_u32 {
        let eth = 1000.0 + s as f64 * 50.0;
        let btc = 50_000.0 + s as f64 * 1000.0;
        let mut legs: HashMap<&str, LegTick> = HashMap::new();
        legs.insert("ETH/USDT", tick(eth * 0.999, eth * 1.001));
        legs.insert("BTC/USDT", tick(btc * 0.999, btc * 1.001));
        let out = compute_synth_tick(path, &legs).expect("composable");
        assert!(out.bid <= out.mid + 1e-12, "bid > mid at s={s}");
        assert!(out.mid <= out.ask + 1e-12, "mid > ask at s={s}");
    }
}

// ─── reconstruct_synth_ohlc sanity ─────────────────────────────────────

/// Single-bucket synth OHLC for a 2-leg ratio path. Verifies:
/// - O_S = O_eth / O_btc, C_S = C_eth / C_btc (positional)
/// - H_S > max(O_S, C_S) and L_S < min(O_S, C_S) (variance-blended range)
/// - log_range > 0 when legs have nonzero range
#[test]
fn ohlc_single_bucket_two_leg_ratio() {
    let path = &mk_path("ETH/BTC", &[("ETH/USDT", 1), ("BTC/USDT", -1)]);
    let mut legs: HashMap<&str, OhlcLite> = HashMap::new();
    legs.insert("ETH/USDT", OhlcLite { o: 2800.0, h: 2820.0, l: 2780.0, c: 2810.0 });
    legs.insert("BTC/USDT", OhlcLite { o: 100_000.0, h: 100_500.0, l: 99_500.0, c: 100_200.0 });
    let synth = reconstruct_synth_ohlc(path, &legs, VarianceEstimator::RogersSatchell, |_, _| 0.0)
        .expect("composable");
    assert!(close(synth.o, 2800.0 / 100_000.0, 1e-9));
    assert!(close(synth.c, 2810.0 / 100_200.0, 1e-9));
    // Geometric mid sits inside leg quotient range.
    assert!(synth.h >= synth.o.max(synth.c) - 1e-12);
    assert!(synth.l <= synth.o.min(synth.c) + 1e-12);
    assert!(synth.log_range > 0.0);
}

/// Parkinson vs Rogers-Satchell produce different (but same-order) ranges.
#[test]
fn parkinson_vs_rs_estimators_diverge() {
    let path = &mk_path("ETH/BTC", &[("ETH/USDT", 1), ("BTC/USDT", -1)]);
    let mut legs: HashMap<&str, OhlcLite> = HashMap::new();
    legs.insert("ETH/USDT", OhlcLite { o: 2800.0, h: 2820.0, l: 2780.0, c: 2810.0 });
    legs.insert("BTC/USDT", OhlcLite { o: 100_000.0, h: 100_500.0, l: 99_500.0, c: 100_200.0 });
    let park = reconstruct_synth_ohlc(path, &legs, VarianceEstimator::Parkinson, |_, _| 0.0).unwrap();
    let rs   = reconstruct_synth_ohlc(path, &legs, VarianceEstimator::RogersSatchell, |_, _| 0.0).unwrap();
    assert!(park.log_range > 0.0 && rs.log_range > 0.0);
    // They should not be identical for a non-trivial path; both within a factor of ~3.
    let ratio = park.log_range / rs.log_range;
    assert!(ratio > 0.3 && ratio < 3.0, "park/rs ratio out of band: {ratio}");
}

#[test]
fn ohlc_missing_leg_returns_none() {
    let path = &mk_path("ETH/BTC", &[("ETH/USDT", 1), ("BTC/USDT", -1)]);
    let mut legs: HashMap<&str, OhlcLite> = HashMap::new();
    legs.insert("ETH/USDT", OhlcLite { o: 2800.0, h: 2820.0, l: 2780.0, c: 2810.0 });
    // BTC/USDT missing.
    assert!(reconstruct_synth_ohlc(path, &legs, VarianceEstimator::RogersSatchell, |_, _| 0.0).is_none());
}

#[test]
fn ohlc_identity_path_returns_unit() {
    let path = &mk_path("EUR/EUR", &[]);
    let legs: HashMap<&str, OhlcLite> = HashMap::new();
    let synth = reconstruct_synth_ohlc(path, &legs, VarianceEstimator::RogersSatchell, |_, _| 0.0)
        .expect("identity composable");
    assert_eq!(synth.o, 1.0);
    assert_eq!(synth.h, 1.0);
    assert_eq!(synth.l, 1.0);
    assert_eq!(synth.c, 1.0);
    assert_eq!(synth.log_range, 0.0);
}

// ─── RollingCorrelation (port of GBM test) ─────────────────────────────

/// Deterministic PRNG (mulberry32) — verbatim from BTR test.
fn mulberry32(seed: u32) -> impl FnMut() -> f64 {
    let mut t = seed;
    move || {
        t = t.wrapping_add(0x6D2B79F5);
        let mut r = t;
        r = (r ^ (r >> 15)).wrapping_mul(r | 1);
        r ^= r.wrapping_add((r ^ (r >> 7)).wrapping_mul(r | 61));
        ((r ^ (r >> 14)) as f64) / 4_294_967_296.0
    }
}

/// Box-Muller w/ stored spare — verbatim from BTR test.
fn gauss(mut rng: impl FnMut() -> f64 + 'static) -> impl FnMut() -> f64 {
    let mut spare: Option<f64> = None;
    move || {
        if let Some(v) = spare.take() {
            return v;
        }
        loop {
            let u = 2.0 * rng() - 1.0;
            let v = 2.0 * rng() - 1.0;
            let s = u * u + v * v;
            if s < 1.0 && s != 0.0 {
                let f = (-2.0 * s.ln() / s).sqrt();
                spare = Some(v * f);
                return u * f;
            }
        }
    }
}

/// Port of BTR test "RollingCorrelation tracks Pearson ρ on synthetic correlated data".
#[test]
fn rolling_correlation_tracks_pearson() {
    let rng = mulberry32(42);
    let mut g = gauss(rng);
    let mut rc = RollingCorrelation::new(500);
    let rho_true = 0.6_f64;
    let c2 = (1.0 - rho_true * rho_true).sqrt();
    for _ in 0..500 {
        let z1 = g();
        let z2 = g();
        rc.add(z1, rho_true * z1 + c2 * z2);
    }
    let est = rc.value();
    assert!((est - rho_true).abs() < 0.1, "estimate {} differs from true {}", est, rho_true);
}

#[test]
fn rolling_correlation_clamps() {
    let mut rc = RollingCorrelation::new(10);
    // Perfectly correlated → ρ = 1, clamped to 0.99.
    for i in 0..10 {
        let x = i as f64;
        rc.add(x, 2.0 * x + 5.0);
    }
    let v = rc.value();
    assert!(v <= 0.99 + 1e-12, "perfect-corr clamp failed: {v}");
    assert!(v >= 0.99 - 1e-9, "perfect-corr should be near +0.99: {v}");
}

#[test]
fn rolling_correlation_handles_constant_input() {
    let mut rc = RollingCorrelation::new(5);
    for _ in 0..5 {
        rc.add(1.0, 2.0);
    }
    // Zero variance → ρ = 0.
    assert_eq!(rc.value(), 0.0);
}

#[test]
fn rolling_correlation_ignores_non_finite() {
    let mut rc = RollingCorrelation::new(3);
    rc.add(f64::NAN, 1.0);
    rc.add(1.0, f64::INFINITY);
    assert_eq!(rc.count(), 0);
}

// ─── GBM Monte-Carlo (port of synth-ohlc.gbm.test.ts) ──────────────────

fn gbm_paths(
    mut rng: impl FnMut() -> f64 + 'static,
    n_ticks: usize,
    dt_sec: f64,
    sig_a: f64,
    sig_b: f64,
    rho_true: f64,
    s0a: f64,
    s0b: f64,
) -> (Vec<f64>, Vec<f64>) {
    // We need `gauss` over `rng`, but `gauss` takes ownership. Use a local closure
    // that holds rng via mutable capture.
    let mut spare: Option<f64> = None;
    let mut g = move || -> f64 {
        if let Some(v) = spare.take() {
            return v;
        }
        loop {
            let u = 2.0 * rng() - 1.0;
            let v = 2.0 * rng() - 1.0;
            let s = u * u + v * v;
            if s < 1.0 && s != 0.0 {
                let f = (-2.0 * s.ln() / s).sqrt();
                spare = Some(v * f);
                return u * f;
            }
        }
    };

    let mut a = Vec::with_capacity(n_ticks);
    let mut b = Vec::with_capacity(n_ticks);
    let dt = dt_sec / (365.0 * 24.0 * 3600.0);
    let sqdt = dt.sqrt();
    let mut sa = s0a;
    let mut sb = s0b;
    let c = rho_true;
    let c2 = (1.0_f64 - c * c).max(0.0).sqrt();
    for _ in 0..n_ticks {
        let z1 = g();
        let z2 = g();
        let wa = z1;
        let wb = c * z1 + c2 * z2;
        sa *= ((-0.5 * sig_a * sig_a) * dt + sig_a * sqdt * wa).exp();
        sb *= ((-0.5 * sig_b * sig_b) * dt + sig_b * sqdt * wb).exp();
        a.push(sa);
        b.push(sb);
    }
    (a, b)
}

fn to_ohlc(prices: &[f64], dt_sec: f64, base_tf_ms: i64) -> Vec<TimedOhlc> {
    let base_sec = base_tf_ms as f64 / 1000.0;
    let ticks_per_bucket = ((base_sec / dt_sec).round().max(1.0)) as usize;
    let mut out: Vec<TimedOhlc> = Vec::new();
    let mut i = 0;
    while i < prices.len() {
        let end = (i + ticks_per_bucket).min(prices.len());
        let slice = &prices[i..end];
        if slice.is_empty() {
            break;
        }
        let mut h = slice[0];
        let mut l = slice[0];
        for &p in slice {
            if p > h { h = p; }
            if p < l { l = p; }
        }
        out.push(TimedOhlc {
            ts: (i as f64 * dt_sec * 1000.0) as i64,
            o: slice[0],
            h,
            l,
            c: slice[slice.len() - 1],
        });
        i += ticks_per_bucket;
    }
    out
}

fn true_synth_ohlc(a: &[f64], b: &[f64], dt_sec: f64, target_tf_ms: i64) -> Vec<TimedOhlc> {
    let ratio: Vec<f64> = a.iter().zip(b.iter()).map(|(x, y)| x / y).collect();
    to_ohlc(&ratio, dt_sec, target_tf_ms)
}

fn mean_log_range_bias(estimated: &[nxr_sdk::synth::TimedOhlcCount], truth: &[TimedOhlc]) -> f64 {
    let mut t_map: HashMap<i64, &TimedOhlc> = HashMap::new();
    for t in truth {
        t_map.insert(t.ts, t);
    }
    let mut sum = 0.0;
    let mut n = 0;
    for e in estimated {
        let Some(t) = t_map.get(&e.ts) else { continue };
        let rt = (t.h / t.l).ln();
        let re = (e.h / e.l).ln();
        if !(rt > 0.0) || !re.is_finite() {
            continue;
        }
        sum += (re - rt) / rt;
        n += 1;
    }
    if n > 0 { sum / n as f64 } else { f64::NAN }
}

/// Port of BTR test "GBM Monte Carlo: RS-ρ mean range bias < 3% across M1/M5/H1".
/// Uses fabricated path `[A, +1; B, -1]` (synth = A/B) since registry uses real symbols.
#[test]
fn gbm_rs_rho_bias_under_3pct_across_tfs() {
    let path = mk_path("A_OVER_B", &[("A", 1), ("B", -1)]);
    let base_tf_ms: i64 = 10_000;
    let dt_sec = 1.0;
    let n_trials = 200;
    let ticks = 4000;
    let rho_true = 0.5;
    let sig_a = 0.6;
    let sig_b = 0.5;

    for &(name, tf_ms) in &[("M1", 60_000_i64), ("M5", 300_000), ("H1", 3_600_000)] {
        let mut sum_bias = 0.0;
        let mut n = 0;
        for trial in 0..n_trials {
            let rng = mulberry32(0xBEEF + trial);
            let (a, b) = gbm_paths(rng, ticks, dt_sec, sig_a, sig_b, rho_true, 100.0, 50.0);
            let a_ohlc = to_ohlc(&a, dt_sec, base_tf_ms);
            let b_ohlc = to_ohlc(&b, dt_sec, base_tf_ms);
            let mut leg_series: HashMap<&str, &[TimedOhlc]> = HashMap::new();
            leg_series.insert("A", &a_ohlc);
            leg_series.insert("B", &b_ohlc);
            let est = reconstruct_synth_series_at_base_tf_then_rollup(
                &path, &leg_series, base_tf_ms, tf_ms,
                VarianceEstimator::RogersSatchell, |_, _| rho_true,
            );
            let truth = true_synth_ohlc(&a, &b, dt_sec, tf_ms);
            let bias = mean_log_range_bias(&est, &truth);
            if bias.is_finite() {
                sum_bias += bias;
                n += 1;
            }
        }
        let mean_bias = sum_bias / (n.max(1) as f64);
        assert!(
            mean_bias.abs() < 0.03,
            "{name} mean bias {} exceeds 3% threshold (n={})",
            mean_bias,
            n
        );
    }
}

/// Port of BTR test "GBM Monte Carlo: ρ=0 fallback bias bounded < 20%".
#[test]
fn gbm_rho_zero_bias_under_20pct() {
    let path = mk_path("A_OVER_B", &[("A", 1), ("B", -1)]);
    let base_tf_ms: i64 = 10_000;
    let dt_sec = 1.0;
    let n_trials = 50;
    let ticks = 2000;
    let rho_true = 0.5;
    let tf_ms: i64 = 60_000;
    let mut sum_abs = 0.0;
    let mut n = 0;
    for trial in 0..n_trials {
        let rng = mulberry32(0x1234 + trial);
        let (a, b) = gbm_paths(rng, ticks, dt_sec, 0.6, 0.5, rho_true, 100.0, 50.0);
        let a_ohlc = to_ohlc(&a, dt_sec, base_tf_ms);
        let b_ohlc = to_ohlc(&b, dt_sec, base_tf_ms);
        let mut leg_series: HashMap<&str, &[TimedOhlc]> = HashMap::new();
        leg_series.insert("A", &a_ohlc);
        leg_series.insert("B", &b_ohlc);
        let est = reconstruct_synth_series_at_base_tf_then_rollup(
            &path, &leg_series, base_tf_ms, tf_ms,
            VarianceEstimator::RogersSatchell, |_, _| 0.0,
        );
        let truth = true_synth_ohlc(&a, &b, dt_sec, tf_ms);
        let bias = mean_log_range_bias(&est, &truth);
        if bias.is_finite() {
            sum_abs += bias.abs();
            n += 1;
        }
    }
    assert!(sum_abs / (n.max(1) as f64) < 0.2);
}
