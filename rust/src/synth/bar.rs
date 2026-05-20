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
