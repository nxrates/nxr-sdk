//! Synth OHLC reconstruction — verbatim port of
//! `reconstructSynthOhlc` + `reconstructSynthSeries[AtBaseTfThenRollup]`
//! from `~/Work/btr/sdk/src/types/synth-ohlc.ts` (lines 67-223).
//!
//! Per-leg variance (configurable):
//! - **Parkinson** (1980): `v_i = ln(H/L)² / (4·ln2)`
//! - **Rogers-Satchell** (1991, default — drift-robust):
//!   `v_i = ln(H/C)·ln(H/O) + ln(L/C)·ln(L/O)`
//!
//! Quadratic-form variance aggregator with optional leg-pair correlation `ρ_ij`:
//! ```text
//! V = Σ e_i²·v_i + 2·Σ_{i<j} e_i·e_j·ρ_ij·√(v_i·v_j)         (floor at 1e-12)
//! R_S = √(4·ln2 · V)                       (Parkinson inversion → synth log-range)
//! O_S = Π O_i^{e_i};  C_S = Π C_i^{e_i}
//! M_S = √(O_S · C_S)                       (geometric mid, log-symmetric)
//! H_S = M_S · exp(+R_S/2);  L_S = M_S · exp(-R_S/2)
//! ```

use std::collections::HashMap;

use super::paths::SynthPath;

/// Per-leg variance estimator selector.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum VarianceEstimator {
    /// Parkinson (1980) — range-only, drift-biased.
    Parkinson,
    /// Rogers-Satchell (1991) — drift-robust. **Default.**
    RogersSatchell,
}

impl Default for VarianceEstimator {
    fn default() -> Self {
        Self::RogersSatchell
    }
}

/// Minimal OHLC tuple (no timestamp).
#[derive(Clone, Copy, Debug)]
pub struct OhlcLite {
    pub o: f64,
    pub h: f64,
    pub l: f64,
    pub c: f64,
}

/// OHLC with the derived synth log-range (for vol diagnostics).
#[derive(Clone, Copy, Debug)]
pub struct OhlcWithRange {
    pub o: f64,
    pub h: f64,
    pub l: f64,
    pub c: f64,
    /// Synth log-range `R_S = √(4·ln2 · V)` (Parkinson inversion).
    pub log_range: f64,
}

/// OHLC with bucket-start timestamp (epoch ms).
#[derive(Clone, Copy, Debug)]
pub struct TimedOhlc {
    pub ts: i64,
    pub o: f64,
    pub h: f64,
    pub l: f64,
    pub c: f64,
}

/// OHLC with bucket-start timestamp and aggregate count (sum of base-TF buckets).
#[derive(Clone, Copy, Debug)]
pub struct TimedOhlcCount {
    pub ts: i64,
    pub o: f64,
    pub h: f64,
    pub l: f64,
    pub c: f64,
    pub count: u32,
}

const FOUR_LN2: f64 = 4.0 * std::f64::consts::LN_2;
const INV_4LN2: f64 = 1.0 / FOUR_LN2;
const V_FLOOR: f64 = 1e-12;

/// Per-leg variance under chosen estimator. Returns `NaN` on invalid input.
#[inline]
fn leg_variance(k: &OhlcLite, est: VarianceEstimator) -> f64 {
    match est {
        VarianceEstimator::Parkinson => {
            let r = (k.h / k.l).ln();
            INV_4LN2 * r * r
        }
        VarianceEstimator::RogersSatchell => {
            // RS is non-negative by construction when H≥max(O,C) and L≤min(O,C).
            let lhc = (k.h / k.c).ln();
            let lho = (k.h / k.o).ln();
            let llc = (k.l / k.c).ln();
            let llo = (k.l / k.o).ln();
            lhc * lho + llc * llo
        }
    }
}

/// Reconstruct synth OHLC for a single bucket from aligned per-leg OHLCs.
///
/// Returns `None` if any leg is missing or has non-positive / inverted prices.
///
/// `rho(i, j)` supplies the leg-pair correlation `ρ_ij ∈ [-1, 1]` by **leg index**
/// (position within `path.legs`). Symmetry is assumed; only `i < j` is queried.
/// Use `|_, _| 0.0` for the independent-legs default.
pub fn reconstruct_synth_ohlc<F: Fn(usize, usize) -> f64>(
    path: &SynthPath,
    leg_ohlc: &HashMap<&str, OhlcLite>,
    estimator: VarianceEstimator,
    rho: F,
) -> Option<OhlcWithRange> {
    let n = path.legs.len();
    if n == 0 {
        // Trivial identity → constant 1 across OHLC, zero range.
        return Some(OhlcWithRange { o: 1.0, h: 1.0, l: 1.0, c: 1.0, log_range: 0.0 });
    }

    let mut v = vec![0.0_f64; n];
    let mut e = vec![0_i32; n];
    let mut o_s = 1.0_f64;
    let mut c_s = 1.0_f64;

    for (i, leg) in path.legs.iter().enumerate() {
        let k = leg_ohlc.get(leg.sym.as_str())?;
        if k.o <= 0.0 || k.h <= 0.0 || k.l <= 0.0 || k.c <= 0.0 || k.h < k.l {
            return None;
        }
        if leg.exp == 1 {
            o_s *= k.o;
            c_s *= k.c;
        } else {
            o_s /= k.o;
            c_s /= k.c;
        }
        e[i] = leg.exp as i32;
        let vi = leg_variance(k, estimator);
        v[i] = if vi.is_finite() && vi >= 0.0 { vi } else { 0.0 };
    }

    // Quadratic-form variance: diag + 2·off-diag·ρ·√(v_i v_j).
    let mut var = 0.0_f64;
    for i in 0..n {
        var += (e[i] * e[i]) as f64 * v[i];
    }
    for i in 0..n {
        for j in (i + 1)..n {
            let rij = rho(i, j);
            if rij == 0.0 {
                continue;
            }
            var += 2.0 * (e[i] as f64) * (e[j] as f64) * rij * (v[i] * v[j]).sqrt();
        }
    }
    if !(var >= V_FLOOR) {
        var = V_FLOOR;
    }

    let r = (FOUR_LN2 * var).sqrt();          // Parkinson inversion → log-range
    let m = (o_s * c_s).sqrt();               // geometric mid
    let half_r = r * 0.5;
    let h_s = m * half_r.exp();
    let l_s = m * (-half_r).exp();

    Some(OhlcWithRange { o: o_s, h: h_s, l: l_s, c: c_s, log_range: r })
}

/// Bucketize-and-reconstruct: group leg OHLC series into common time buckets,
/// reconstruct per bucket. Per-leg series must be sorted by ts ascending.
///
/// Buckets at which **any** leg is missing are skipped silently.
pub fn reconstruct_synth_series<F: Fn(usize, usize) -> f64 + Copy>(
    path: &SynthPath,
    leg_series: &HashMap<&str, &[TimedOhlc]>,
    tf_ms: i64,
    estimator: VarianceEstimator,
    rho: F,
) -> Vec<TimedOhlc> {
    assert!(tf_ms > 0, "tf_ms must be positive");

    // Per-leg series presence check: any missing → empty output.
    for leg in &path.legs {
        if !leg_series.contains_key(leg.sym.as_str()) {
            return Vec::new();
        }
    }

    // Group leg rows into common buckets keyed by bucket-start ts.
    // Inner map: leg-sym → OhlcLite (aggregated across rows in same bucket).
    let mut by_bucket: std::collections::BTreeMap<i64, HashMap<String, OhlcLite>> =
        std::collections::BTreeMap::new();

    for leg in &path.legs {
        let series = leg_series.get(leg.sym.as_str()).expect("checked above");
        for row in *series {
            let bucket = (row.ts.div_euclid(tf_ms)) * tf_ms;
            let entry = by_bucket.entry(bucket).or_default();
            match entry.get_mut(&leg.sym) {
                None => {
                    entry.insert(leg.sym.clone(), OhlcLite { o: row.o, h: row.h, l: row.l, c: row.c });
                }
                Some(prev) => {
                    // OHLC monoid for multiple base rows within a single target bucket.
                    if row.h > prev.h { prev.h = row.h; }
                    if row.l < prev.l { prev.l = row.l; }
                    prev.c = row.c;
                }
            }
        }
    }

    let mut out: Vec<TimedOhlc> = Vec::with_capacity(by_bucket.len());
    for (ts, leg_map) in by_bucket {
        // Convert HashMap<String, _> → HashMap<&str, _> for reconstruct call.
        let view: HashMap<&str, OhlcLite> = leg_map.iter().map(|(k, v)| (k.as_str(), *v)).collect();
        if let Some(s) = reconstruct_synth_ohlc(path, &view, estimator, rho) {
            out.push(TimedOhlc { ts, o: s.o, h: s.h, l: s.l, c: s.c });
        }
    }
    out
}

/// Canonical reconstruction path: compute synth at base TF (typically S10), then
/// roll up via the OHLC monoid (first/max/min/last) to target TF. Captures
/// intra-bucket leg correlation at base TF where it is weakest. Counts are summed.
///
/// `target_tf_ms` must be a multiple of `base_tf_ms`. When `target_tf_ms <= base_tf_ms`,
/// returns the base-TF synth series with `count=1` per row.
pub fn reconstruct_synth_series_at_base_tf_then_rollup<F: Fn(usize, usize) -> f64 + Copy>(
    path: &SynthPath,
    leg_series: &HashMap<&str, &[TimedOhlc]>,
    base_tf_ms: i64,
    target_tf_ms: i64,
    estimator: VarianceEstimator,
    rho: F,
) -> Vec<TimedOhlcCount> {
    let base = reconstruct_synth_series(path, leg_series, base_tf_ms, estimator, rho);
    if target_tf_ms <= base_tf_ms {
        return base
            .into_iter()
            .map(|r| TimedOhlcCount { ts: r.ts, o: r.o, h: r.h, l: r.l, c: r.c, count: 1 })
            .collect();
    }
    let tf = target_tf_ms;
    let mut out: Vec<TimedOhlcCount> = Vec::new();
    let mut cur: Option<TimedOhlcCount> = None;
    for r in base {
        let bucket = r.ts.div_euclid(tf) * tf;
        match cur.as_mut() {
            Some(c) if c.ts == bucket => {
                if r.h > c.h { c.h = r.h; }
                if r.l < c.l { c.l = r.l; }
                c.c = r.c;
                c.count += 1;
            }
            _ => {
                if let Some(c) = cur.take() {
                    out.push(c);
                }
                cur = Some(TimedOhlcCount { ts: bucket, o: r.o, h: r.h, l: r.l, c: r.c, count: 1 });
            }
        }
    }
    if let Some(c) = cur {
        out.push(c);
    }
    out
}
