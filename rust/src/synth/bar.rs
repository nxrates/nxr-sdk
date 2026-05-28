//! Synth `mitch::Bar` reconstruction with microstructure inheritance.
//!
//! Given aligned per-leg `Bar` series (by `close_mts` bucket), this module:
//! 1. Reconstructs synth OHLC via the quadratic-form variance estimator
//!    (see [`super::ohlc::reconstruct_synth_ohlc`]).
//! 2. Inherits microstructure fields from the **min-conf leg** for that bucket
//!    (per BTR collector convention: the weakest leg gates the synth quality).
//! 3. Computes `realized_var` as the quadratic-form sum
//!    `Σ e_i²·rv_i + 2·Σ_{i<j} e_i·e_j·ρ_ij·√(rv_i·rv_j)` (floored at 0).
//!
//! ## Inheritance rule (per BTR convention)
//!
//! For fields where a true synth value is not well-defined (e.g. order-flow
//! imbalance, reject_rate, drift), the **leg with minimum `avg_ci_ubp` confidence
//! signal** (i.e. min `avg_ci_ubp` ⇒ tightest CI ⇒ highest confidence) — wait,
//! actually `avg_ci_ubp` is *uncertainty*, so larger ⇒ less confident.
//! We pick the **leg with the LARGEST `avg_ci_ubp`** (= most uncertain) as the
//! gating leg. Microstructure inherits from that leg. This matches the BTR
//! "min-conf leg" rule when `conf = 10000 - ci_bps`.

use std::collections::HashMap;

use mitch::bar::Bar;

use super::ohlc::{OhlcLite, VarianceEstimator, reconstruct_synth_ohlc};
use super::paths::SynthPath;
use super::rolling::RollingCorrelation;

/// Default window for the rolling-ρ cache: 30 minutes of log-returns. At a
/// 10 s base TF that's 180 paired samples — the minimum that produces a
/// non-degenerate Pearson estimate while still being responsive to regime
/// shifts on the half-hour scale used elsewhere (Parkinson σ bins).
pub const DEFAULT_RHO_WINDOW_BUCKETS: usize = 180;

/// Reconstruct a series of synth bars from aligned per-leg bar series.
///
/// Per-leg series are assumed aligned by `close_mts` (caller ensures bucket alignment;
/// upstream callers typically resample to a common base TF first).
///
/// Buckets where any leg is missing are skipped silently.
///
/// `rho(i, j)` supplies leg-pair correlation by **leg index** within `path.legs`.
/// Use `|_, _| 0.0` for independent legs (under-estimates synth range when legs
/// are positively correlated; over-estimates when negatively correlated).
pub fn reconstruct_synth_bar_series<F: Fn(usize, usize) -> f64 + Copy>(
    path: &SynthPath,
    leg_bars: &HashMap<&str, &[Bar]>,
    estimator: VarianceEstimator,
    rho: F,
) -> Vec<Bar> {
    if path.legs.is_empty() {
        // Identity path — no canonical Bar interpretation; return empty.
        return Vec::new();
    }

    // Presence check.
    for leg in &path.legs {
        if !leg_bars.contains_key(leg.sym.as_str()) {
            return Vec::new();
        }
    }

    // Build per-bucket leg index. We key on close_mts and require every leg to
    // have a row at that bucket. The first leg drives the bucket set; subsequent
    // legs filter it.
    let first_sym = path.legs[0].sym.as_str();
    let first_series = *leg_bars.get(first_sym).unwrap();

    // Build sym -> close_mts -> Bar lookups for legs 1..n.
    let mut idx_by_leg: Vec<HashMap<u64, &Bar>> = Vec::with_capacity(path.legs.len());
    for leg in &path.legs {
        let series = *leg_bars.get(leg.sym.as_str()).unwrap();
        let map: HashMap<u64, &Bar> = series.iter().map(|b| (b.close_mts(), b)).collect();
        idx_by_leg.push(map);
    }

    let mut out: Vec<Bar> = Vec::with_capacity(first_series.len());

    for first_bar in first_series.iter() {
        let bucket = first_bar.close_mts();
        // Gather aligned bars for all legs; skip bucket if any miss.
        let mut leg_bars_at_bucket: Vec<&Bar> = Vec::with_capacity(path.legs.len());
        let mut all_present = true;
        for map in &idx_by_leg {
            match map.get(&bucket) {
                Some(b) => leg_bars_at_bucket.push(*b),
                None => {
                    all_present = false;
                    break;
                }
            }
        }
        if !all_present {
            continue;
        }

        // Build OhlcLite map for synth reconstruction.
        let mut ohlc_map: HashMap<&str, OhlcLite> = HashMap::with_capacity(path.legs.len());
        for (leg, b) in path.legs.iter().zip(leg_bars_at_bucket.iter()) {
            ohlc_map.insert(
                leg.sym.as_str(),
                OhlcLite { o: b.open, h: b.high, l: b.low, c: b.close },
            );
        }
        let Some(synth) = reconstruct_synth_ohlc(path, &ohlc_map, estimator, rho) else {
            continue;
        };

        // Quadratic-form sum of realized variance across signed legs.
        // rv_i ≥ 0, e_i ∈ {+1,-1} ⇒ e_i² = 1, so diagonal is Σ rv_i.
        // Off-diagonal: 2·e_i·e_j·ρ_ij·√(rv_i·rv_j).
        let n = path.legs.len();
        let rv: Vec<f64> = leg_bars_at_bucket.iter().map(|b| b.realized_var as f64).collect();
        let e: Vec<i32> = path.legs.iter().map(|l| l.exp as i32).collect();
        let mut rv_synth = 0.0_f64;
        for i in 0..n {
            rv_synth += (e[i] * e[i]) as f64 * rv[i].max(0.0);
        }
        for i in 0..n {
            for j in (i + 1)..n {
                let rij = rho(i, j);
                if rij == 0.0 {
                    continue;
                }
                rv_synth += 2.0 * (e[i] as f64) * (e[j] as f64) * rij * (rv[i].max(0.0) * rv[j].max(0.0)).sqrt();
            }
        }
        if !(rv_synth >= 0.0) {
            rv_synth = 0.0;
        }

        // Inheritance: pick the leg with the LARGEST `avg_ci_ubp` (most uncertainty
        // ⇒ min confidence). Ties broken by first-occurrence.
        let mut min_conf_idx = 0_usize;
        let mut max_ci = leg_bars_at_bucket[0].avg_ci_ubp;
        for (i, b) in leg_bars_at_bucket.iter().enumerate().skip(1) {
            if b.avg_ci_ubp > max_ci {
                max_ci = b.avg_ci_ubp;
                min_conf_idx = i;
            }
        }
        let gate = leg_bars_at_bucket[min_conf_idx];

        // Aggregate tick_count + volumes: sum over legs (proxy; alternative: max).
        // BTR collector keeps the gating-leg volume; we follow suit for parity.
        let vbid = gate.vbid;
        let vask = gate.vask;
        let tick_count = gate.tick_count;

        // Bar open_ts: use first_bar.open_ts (driver leg). Caller can override.
        let mut bar = Bar::new_ohlcv(
            first_bar.open_mts(),
            bucket,
            synth.o,
            synth.h,
            synth.l,
            synth.c,
            vbid,
            vask,
            tick_count,
        );
        bar.realized_var = rv_synth as f32;
        bar.bipower_var = gate.bipower_var;
        bar.drift = gate.drift;
        bar.vol_imbalance = gate.vol_imbalance;
        bar.avg_spread_bps = gate.avg_spread_bps;
        bar.max_abs_return = gate.max_abs_return;
        bar.avg_ci_ubp = gate.avg_ci_ubp;
        bar.reject_rate = gate.reject_rate;
        bar.kind = gate.kind;

        out.push(bar);
    }

    out
}

/// Convenience: base-TF then OHLC-monoid rollup of synth bars.
///
/// Currently a thin wrapper that returns the base-TF synth bars as-is when
/// `target_tf_ms == base_tf_ms`. For target > base, rollup uses the OHLC monoid
/// on (o, h, l, c) and **sums** microstructure-like aggregates (`realized_var`,
/// `vbid`, `vask`, `tick_count`). Confidence-gated fields (`drift`, `vol_imbalance`,
/// `avg_spread_bps`, `avg_ci_ubp`, `reject_rate`, `kind`) inherit from the bucket
/// containing the worst confidence (max `avg_ci_ubp`).
pub fn reconstruct_synth_bar_series_at_base_tf_then_rollup<F: Fn(usize, usize) -> f64 + Copy>(
    path: &SynthPath,
    leg_bars: &HashMap<&str, &[Bar]>,
    base_tf_ms: i64,
    target_tf_ms: i64,
    estimator: VarianceEstimator,
    rho: F,
) -> Vec<Bar> {
    let base = reconstruct_synth_bar_series(path, leg_bars, estimator, rho);
    if target_tf_ms <= base_tf_ms {
        return base;
    }
    let tf = target_tf_ms as u64;
    let mut out: Vec<Bar> = Vec::new();
    let mut cur: Option<Bar> = None;
    let mut cur_bucket: u64 = 0;

    for b in base {
        let bucket = (b.close_mts() / tf) * tf;
        match cur.as_mut() {
            Some(c) if cur_bucket == bucket => {
                if b.high > c.high { c.high = b.high; }
                if b.low < c.low { c.low = b.low; }
                c.close = b.close;
                c.set_close_mts(b.close_mts());
                c.vbid = c.vbid.saturating_add(b.vbid);
                c.vask = c.vask.saturating_add(b.vask);
                c.tick_count = c.tick_count.saturating_add(b.tick_count);
                c.realized_var += b.realized_var;
                c.bipower_var += b.bipower_var;
                // Gate inheritance: replace gate-driven fields if this sub-bar is worse-conf.
                if b.avg_ci_ubp > c.avg_ci_ubp {
                    c.drift = b.drift;
                    c.vol_imbalance = b.vol_imbalance;
                    c.avg_spread_bps = b.avg_spread_bps;
                    c.max_abs_return = c.max_abs_return.max(b.max_abs_return);
                    c.avg_ci_ubp = b.avg_ci_ubp;
                    c.reject_rate = b.reject_rate;
                } else {
                    c.max_abs_return = c.max_abs_return.max(b.max_abs_return);
                }
            }
            _ => {
                if let Some(c) = cur.take() {
                    out.push(c);
                }
                let mut nb = b;
                nb.set_open_mts(bucket);
                cur = Some(nb);
                cur_bucket = bucket;
            }
        }
    }
    if let Some(c) = cur {
        out.push(c);
    }
    out
}

/// Pre-computed rolling Pearson-ρ cache keyed by `(leg_i, leg_j, close_mts)`.
///
/// Built by [`build_rolling_rho_cache`]; queried via the closure returned
/// from [`rho_cache_callback`]. Entries are populated for the FIRST bucket
/// at which both legs have ≥ 2 observations; buckets where the window has
/// insufficient samples return `0.0` from the callback (safe fallback —
/// degenerates to independent-leg variance summation).
pub type RhoCache = HashMap<(usize, usize, u64), f64>;

/// Build a rolling Pearson-ρ cache for every unique leg pair `(i, j)` with
/// `i < j` over the aligned per-leg bar buckets.
///
/// `window_buckets` is the sliding window in BARS (not ms). At 10 s base TF,
/// `window_buckets = 180` ≈ 30 min — the same horizon used elsewhere for σ.
///
/// Implementation: a single forward sweep over the shared bucket axis (the
/// first leg's series, intersected with all others); per-leg log-returns
/// `ln(close_t / close_{t-1})` feed N `RollingCorrelation` accumulators (one
/// per ordered pair). After each bucket update, snapshot `value()` into the
/// cache at that bucket's `close_mts`.
pub fn build_rolling_rho_cache(
    path: &SynthPath,
    leg_bars: &HashMap<&str, &[Bar]>,
    window_buckets: usize,
) -> RhoCache {
    let mut cache: RhoCache = HashMap::new();
    let n_legs = path.legs.len();
    if n_legs < 2 {
        return cache;
    }
    for leg in &path.legs {
        if !leg_bars.contains_key(leg.sym.as_str()) {
            return cache;
        }
    }
    let first_sym = path.legs[0].sym.as_str();
    let first_series = *leg_bars.get(first_sym).unwrap();
    // Per-leg bucket → close lookup.
    let mut per_leg_close: Vec<HashMap<u64, f64>> = Vec::with_capacity(n_legs);
    for leg in &path.legs {
        let series = *leg_bars.get(leg.sym.as_str()).unwrap();
        per_leg_close.push(series.iter().map(|b| (b.close_mts(), b.close as f64)).collect());
    }
    // Ordered-pair accumulators (i, j) with i < j.
    let win = window_buckets.max(2);
    let mut accs: HashMap<(usize, usize), RollingCorrelation> = HashMap::new();
    for i in 0..n_legs {
        for j in (i + 1)..n_legs {
            accs.insert((i, j), RollingCorrelation::new(win));
        }
    }
    // Previous-bucket per-leg close (for log-returns).
    let mut prev_close: Vec<Option<f64>> = vec![None; n_legs];

    for first_bar in first_series.iter() {
        let bucket = first_bar.close_mts();
        // Require every leg present at this bucket.
        let mut closes: Vec<f64> = Vec::with_capacity(n_legs);
        let mut all_present = true;
        for leg_map in &per_leg_close {
            match leg_map.get(&bucket) {
                Some(c) => closes.push(*c),
                None => {
                    all_present = false;
                    break;
                }
            }
        }
        if !all_present {
            continue;
        }
        // Compute log-returns vs prior bucket.
        let mut rets: Vec<Option<f64>> = vec![None; n_legs];
        for k in 0..n_legs {
            if let Some(pc) = prev_close[k] {
                if pc > 0.0 && closes[k] > 0.0 {
                    rets[k] = Some((closes[k] / pc).ln());
                }
            }
        }
        // Update each ordered-pair accumulator and snapshot ρ.
        for i in 0..n_legs {
            for j in (i + 1)..n_legs {
                if let (Some(ri), Some(rj)) = (rets[i], rets[j]) {
                    let acc = accs.get_mut(&(i, j)).unwrap();
                    acc.add(ri, rj);
                    cache.insert((i, j, bucket), acc.value());
                }
            }
        }
        // Slide prev_close.
        for k in 0..n_legs {
            prev_close[k] = Some(closes[k]);
        }
    }
    cache
}

/// Build a rho-callback closure suitable for [`reconstruct_synth_bar_series`]
/// from a pre-built [`RhoCache`] and the current bucket's `close_mts`. Returns
/// `0.0` for cache misses (safe fallback to independent-leg variance summation).
///
/// Note: ρ is symmetric (`ρ(i, j) == ρ(j, i)`); the closure normalises to the
/// canonical `(min(i, j), max(i, j))` cache key.
pub fn rho_cache_callback<'a>(
    cache: &'a RhoCache,
    bucket: u64,
) -> impl Fn(usize, usize) -> f64 + Copy + 'a {
    move |i: usize, j: usize| -> f64 {
        if i == j {
            return 1.0;
        }
        let (a, b) = if i < j { (i, j) } else { (j, i) };
        cache.get(&(a, b, bucket)).copied().unwrap_or(0.0)
    }
}

/// Convenience: reconstruct synth bar series with a rolling Pearson-ρ cache
/// computed on-the-fly from the per-leg close series. Identical to
/// [`reconstruct_synth_bar_series`] but the ρ callback is bucket-aware and
/// driven by [`build_rolling_rho_cache`].
///
/// Use this everywhere the previous code passed `|_, _| 0.0` — it strictly
/// dominates: when legs are independent, the rolling ρ converges to 0 and
/// the output matches the identity-ρ path; when legs are correlated, the
/// quadratic-form variance sum is accurate.
pub fn reconstruct_synth_bar_series_rolling_rho(
    path: &SynthPath,
    leg_bars: &HashMap<&str, &[Bar]>,
    estimator: VarianceEstimator,
    window_buckets: usize,
) -> Vec<Bar> {
    if path.legs.is_empty() {
        return Vec::new();
    }
    let cache = build_rolling_rho_cache(path, leg_bars, window_buckets);
    // We can't just hand the cache into the existing reconstructor — that
    // closure shape is `Fn(i, j) -> f64` with no bucket context. So we
    // re-implement the outer loop here (small + lean) calling
    // `reconstruct_synth_ohlc` per bucket with a bucket-keyed rho closure.
    let first_sym = path.legs[0].sym.as_str();
    let Some(first_series) = leg_bars.get(first_sym).copied() else {
        return Vec::new();
    };
    let mut idx_by_leg: Vec<HashMap<u64, &Bar>> = Vec::with_capacity(path.legs.len());
    for leg in &path.legs {
        let Some(series) = leg_bars.get(leg.sym.as_str()).copied() else {
            return Vec::new();
        };
        idx_by_leg.push(series.iter().map(|b| (b.close_mts(), b)).collect());
    }
    let mut out: Vec<Bar> = Vec::with_capacity(first_series.len());
    for first_bar in first_series.iter() {
        let bucket = first_bar.close_mts();
        let mut leg_bars_at_bucket: Vec<&Bar> = Vec::with_capacity(path.legs.len());
        let mut all_present = true;
        for map in &idx_by_leg {
            match map.get(&bucket) {
                Some(b) => leg_bars_at_bucket.push(*b),
                None => {
                    all_present = false;
                    break;
                }
            }
        }
        if !all_present {
            continue;
        }
        let mut ohlc_map: HashMap<&str, OhlcLite> = HashMap::with_capacity(path.legs.len());
        for (leg, b) in path.legs.iter().zip(leg_bars_at_bucket.iter()) {
            ohlc_map.insert(
                leg.sym.as_str(),
                OhlcLite { o: b.open, h: b.high, l: b.low, c: b.close },
            );
        }
        let rho_cb = rho_cache_callback(&cache, bucket);
        let Some(synth) = reconstruct_synth_ohlc(path, &ohlc_map, estimator, rho_cb) else {
            continue;
        };
        // realized_var via same quadratic form (use rolling ρ).
        let n = path.legs.len();
        let rv: Vec<f64> = leg_bars_at_bucket.iter().map(|b| b.realized_var as f64).collect();
        let e: Vec<i32> = path.legs.iter().map(|l| l.exp as i32).collect();
        let mut rv_synth = 0.0_f64;
        for i in 0..n {
            rv_synth += (e[i] * e[i]) as f64 * rv[i].max(0.0);
        }
        for i in 0..n {
            for j in (i + 1)..n {
                let rij = rho_cb(i, j);
                if rij == 0.0 {
                    continue;
                }
                rv_synth += 2.0
                    * (e[i] as f64)
                    * (e[j] as f64)
                    * rij
                    * (rv[i].max(0.0) * rv[j].max(0.0)).sqrt();
            }
        }
        if !(rv_synth >= 0.0) {
            rv_synth = 0.0;
        }
        // Microstructure inheritance — same rule as the identity-ρ path.
        let mut min_conf_idx = 0_usize;
        let mut max_ci = leg_bars_at_bucket[0].avg_ci_ubp;
        for (i, b) in leg_bars_at_bucket.iter().enumerate().skip(1) {
            if b.avg_ci_ubp > max_ci {
                max_ci = b.avg_ci_ubp;
                min_conf_idx = i;
            }
        }
        let gate = leg_bars_at_bucket[min_conf_idx];
        let vbid = gate.vbid;
        let vask = gate.vask;
        let tick_count = gate.tick_count;
        let mut bar = Bar::new_ohlcv(
            first_bar.open_mts(),
            bucket,
            synth.o,
            synth.h,
            synth.l,
            synth.c,
            vbid,
            vask,
            tick_count,
        );
        bar.realized_var = rv_synth as f32;
        bar.bipower_var = gate.bipower_var;
        bar.drift = gate.drift;
        bar.vol_imbalance = gate.vol_imbalance;
        bar.avg_spread_bps = gate.avg_spread_bps;
        bar.max_abs_return = gate.max_abs_return;
        bar.avg_ci_ubp = gate.avg_ci_ubp;
        bar.reject_rate = gate.reject_rate;
        bar.kind = gate.kind;
        out.push(bar);
    }
    out
}

#[cfg(test)]
mod rho_cache_tests {
    use super::*;
    use mitch::bar::Bar;
    use super::super::paths::{Leg, SynthPath};

    fn mk_bar(close_mts_ms: i64, close: f64) -> Bar {
        let mts = mitch::timestamp::from_epoch_ms(close_mts_ms);
        Bar::new_ohlcv(mts, mts, close, close, close, close, 0, 0, 1)
    }

    /// Build a 2-leg synth path with synthetic but correlated leg close series
    /// (leg B = α·leg A + noise) and verify the rolling ρ cache resolves a
    /// strictly positive value once the window fills, falling back to 0
    /// in early buckets.
    #[test]
    fn rolling_rho_nonzero_for_correlated_legs() {
        let path = SynthPath {
            sym: "A/B".to_string(),
            legs: vec![
                Leg { sym: "X/USDT".to_string(), exp: 1 },
                Leg { sym: "Y/USDT".to_string(), exp: 1 },
            ],
        };
        let mut a_bars: Vec<Bar> = Vec::new();
        let mut b_bars: Vec<Bar> = Vec::new();
        // 600 buckets at 10 s cadence — exceeds 180-window so cache fills.
        let t0 = 1_700_000_000_000_i64;
        let mut pa = 100.0_f64;
        let mut pb = 50.0_f64;
        // Deterministic pseudo-random walk; leg B follows A scaled.
        let mut s: u64 = 0xDEADBEEFCAFEBABE;
        let mut nx = || -> f64 {
            s = s.wrapping_add(0x9E3779B97F4A7C15);
            let mut z = s;
            z = (z ^ (z >> 30)).wrapping_mul(0xBF58476D1CE4E5B9);
            z = (z ^ (z >> 27)).wrapping_mul(0x94D049BB133111EB);
            z ^= z >> 31;
            (((z >> 11) as f64) + 1.0) / ((1u64 << 53) as f64 + 1.0) - 0.5
        };
        for i in 0..600_i64 {
            let r = nx() * 0.002; // ±0.1% step
            pa *= (1.0 + r).max(1e-9);
            pb *= (1.0 + 0.8 * r + 0.2 * nx() * 0.002).max(1e-9); // correlated + small idio
            a_bars.push(mk_bar(t0 + i * 10_000, pa));
            b_bars.push(mk_bar(t0 + i * 10_000, pb));
        }
        let mut lb: HashMap<&str, &[Bar]> = HashMap::new();
        lb.insert("X/USDT", a_bars.as_slice());
        lb.insert("Y/USDT", b_bars.as_slice());
        let cache = build_rolling_rho_cache(&path, &lb, 180);
        // Final bucket should have a non-zero ρ (legs are correlated by construction).
        // Cache is keyed by encoded mts (matches `Bar::close_mts()`), not raw epoch ms.
        let final_bucket = mitch::timestamp::from_epoch_ms(t0 + 599 * 10_000);
        let rho_final = rho_cache_callback(&cache, final_bucket)(0, 1);
        assert!(
            rho_final.abs() > 0.1,
            "rolling ρ on correlated legs must be > 0.1, got {}",
            rho_final
        );
        // Symmetry.
        let rho_swap = rho_cache_callback(&cache, final_bucket)(1, 0);
        assert!((rho_swap - rho_final).abs() < 1e-12, "ρ must be symmetric");
        // Cache-miss falls back to 0.
        let missing_bucket = mitch::timestamp::from_epoch_ms(t0 - 10_000);
        let rho_miss = rho_cache_callback(&cache, missing_bucket)(0, 1);
        assert_eq!(rho_miss, 0.0, "cache miss falls back to 0");
    }
}
