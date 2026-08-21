//! Multi-timeframe window resolution and blending.
//!
//! ONE definition of "which lookbacks, how are they weighted, what happens when
//! a leg cannot fill" shared by every sigma consumer. Each consumer keeps its
//! OWN estimator (`vol`: winsorized per-bin mean of Rogers-Satchell sigma;
//! `core::server::signed`: Parkinson over bar highs/lows) and shares only this.
//!
//! ## Why minutes
//!
//! Lookbacks are stored in MINUTES, never days and never bars. Days cannot
//! express a sub-day leg (6 h) at all, and bars silently rebase whenever the bar
//! width changes. [`MtfWindows::bars`] is the ONLY minutes-to-bars conversion
//! site in the tree.
//!
//! ## Dimensional note
//!
//! Legs may differ in bar WIDTH (a 5 m fast leg beside 30 m slow legs). Each
//! leg's kernel prices a PER-BAR sigma at its own width; the consumer rescales
//! via [`SigmaLeg::to_per_30m_scale`] BEFORE blending, so the blend stays a
//! per-30 m-bar sigma and needs no annualisation and no horizon rescale, and
//! none is applied here. This preserves the downstream per-30 m-bar contract.

use serde::{Deserialize, Serialize};

/// Signed-quote sigma legs, in minutes: 1 h / 24 h / 7 d.
///
/// The short leg tracks a live regime change at a 5 m bar width, the long legs
/// put a floor under a quiet weekend that a single 24 h window collapses
/// through.
pub const DEFAULT_SIGMA_WINDOWS_MIN: [u32; 3] = [60, 1_440, 10_080];

/// Bar width per signed-quote sigma leg, in minutes: the fast leg rolls up at
/// 5 m to capture intra-hour bursts, the mid/slow legs stay on the canonical
/// 30 m bar.
pub const DEFAULT_SIGMA_BAR_MIN: [u32; 3] = [5, 30, 30];

/// Blend weight per signed-quote sigma leg, INVERSE-VARIANCE: a range
/// estimator's sampling variance falls like 1/n, so quality scales with the
/// leg's bar count (12 / 48 / 336). `MtfWindows::blend` multiplies these by
/// the runtime taper quality, so a partially filled long leg does not get its
/// full prior.
pub const DEFAULT_SIGMA_WEIGHTS: [f64; 3] = [12.0, 48.0, 336.0];

/// One resolved sigma leg: lookback in MINUTES, rollup bar width in minutes,
/// and the kernel that prices it.
///
/// A leg's own estimate is a PER-BAR sigma at `bar_min` width; the consumer
/// rescales to the contract scale (per 30 m bar) by `sqrt(bar_min/30)` before
/// blending, so legs of different widths stay dimensionally comparable.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct SigmaLeg {
    /// Lookback in minutes. Converted to bars at [`SigmaLeg::bar_min`] by
    /// [`SigmaLeg::bars`].
    pub window_min: u32,
    /// Rollup bar width in minutes (> 0).
    pub bar_min: u32,
    /// Kernel that prices this leg.
    pub estimator: crate::vol_estimator::SigmaEstimator,
}

impl Default for SigmaLeg {
    fn default() -> Self {
        Self {
            window_min: DEFAULT_SIGMA_WINDOWS_MIN[0],
            bar_min: DEFAULT_SIGMA_BAR_MIN[0],
            estimator: crate::vol_estimator::SigmaEstimator::Yz,
        }
    }
}

impl SigmaLeg {
    /// Lookback in whole bars of this leg's own width. Rounds down, never
    /// below 1: the caller's arming floor decides whether that is enough.
    pub fn bars(&self) -> usize {
        (self.window_min.max(1) / self.bar_min.max(1)).max(1) as usize
    }

    /// Span of the lookback in WHOLE 30 m bars: the history depth this leg
    /// needs from storage that keeps only the canonical 30 m series. THE
    /// retention/scan conversion for mixed-width leg sets.
    pub fn span_bars_30m(&self) -> usize {
        (self.window_min.max(1) / 30).max(1) as usize
    }

    /// Rescale a per-bar sigma at this leg's width to the per-30 m-bar
    /// contract scale. Variance accumulates linearly in time, so the factor
    /// is `sqrt(30/bar_min)`: a per-5 m sigma UNDERSTATES the per-30 m move.
    pub fn to_per_30m_scale(&self, per_bar_sigma: f64) -> f64 {
        per_bar_sigma * (30.0 / self.bar_min as f64).sqrt()
    }

    /// Per-bar sigma of this leg over OHLC slices AT ITS OWN WIDTH.
    pub fn sigma(&self, opens: &[f64], highs: &[f64], lows: &[f64], closes: &[f64]) -> f64 {
        self.estimator.sigma(opens, highs, lows, closes)
    }
}

/// The default signed-quote sigma leg set: 1 h @ 5 m Yang-Zhang, 24 h @ 30 m
/// Garman-Klass, 7 d @ 30 m Garman-Klass, blended by inverse-variance weights.
pub const DEFAULT_SIGMA_LEGS: [SigmaLeg; 3] = [
    SigmaLeg {
        window_min: 60,
        bar_min: 5,
        estimator: crate::vol_estimator::SigmaEstimator::Yz,
    },
    SigmaLeg {
        window_min: 1_440,
        bar_min: 30,
        estimator: crate::vol_estimator::SigmaEstimator::Gk,
    },
    SigmaLeg {
        window_min: 10_080,
        bar_min: 30,
        estimator: crate::vol_estimator::SigmaEstimator::Gk,
    },
];

/// Renko brick-sizing legs: 14 d / 60 d / 180 d, in minutes.
pub const DEFAULT_BRICK_WINDOWS_MIN: [u32; 3] = [20_160, 86_400, 259_200];

/// A set of lookback windows (minutes) with their blend weights.
///
/// Weights are relative: they are renormalised over the legs that actually
/// filled, so a dropped leg never shrinks the result. An empty `weights` means
/// equal weighting.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct MtfWindows {
    /// Lookback per leg, in minutes. Ascending by convention, not enforced.
    pub windows_min: Vec<u32>,
    /// Relative weight per leg. Empty = equal. Short vectors are padded with
    /// the equal weight; extra entries are ignored.
    #[serde(default)]
    pub weights: Vec<f64>,
}

impl MtfWindows {
    pub fn new(windows_min: Vec<u32>, weights: Vec<f64>) -> Self {
        Self {
            windows_min,
            weights,
        }
    }

    /// Equal-weighted set.
    pub fn equal(windows_min: impl Into<Vec<u32>>) -> Self {
        Self {
            windows_min: windows_min.into(),
            weights: Vec::new(),
        }
    }

    #[inline]
    pub fn len(&self) -> usize {
        self.windows_min.len()
    }

    #[inline]
    pub fn is_empty(&self) -> bool {
        self.windows_min.is_empty()
    }

    /// Configured weight of leg `i` (1.0 when unspecified). Non-finite or
    /// non-positive entries are treated as 1.0 rather than silently zeroing a
    /// leg out of the blend.
    #[inline]
    pub fn weight(&self, i: usize) -> f64 {
        match self.weights.get(i) {
            Some(&w) if w.is_finite() && w > 0.0 => w,
            _ => 1.0,
        }
    }

    /// Window lengths in whole bars of `bar_min` minutes. THE conversion site.
    ///
    /// Rounds down but never below 1: a leg shorter than one bar is still one
    /// bar, and the caller's own arming floor (not this function) decides
    /// whether that is enough to trust.
    pub fn bars(&self, bar_min: u32) -> Vec<usize> {
        let w = bar_min.max(1);
        self.windows_min
            .iter()
            .map(|&m| (m / w).max(1) as usize)
            .collect()
    }

    /// Bars needed by the LONGEST leg: the history depth a consumer must retain
    /// for every configured leg to be able to fill. Drives retention derivation.
    pub fn max_bars(&self, bar_min: u32) -> usize {
        self.bars(bar_min).into_iter().max().unwrap_or(0)
    }

    /// Whole days of history the longest leg needs, rounded UP. `bar_min` is
    /// the bar width the consumer stores.
    pub fn max_days(&self, bar_min: u32) -> u16 {
        let mins = self.max_bars(bar_min) as u64 * u64::from(bar_min.max(1));
        mins.div_ceil(1_440).max(1) as u16
    }

    /// Weighted blend over the legs that filled.
    ///
    /// `legs[i]` corresponds to `windows_min[i]`: `None` = that leg could not
    /// fill (too few real bars, degenerate estimate) and is DROPPED, with the
    /// weights renormalised across the survivors. `Some((value, quality))`
    /// contributes `value` at weight `configured_weight(i) * quality`; pass
    /// `1.0` for `quality` when the estimator has no per-leg precision measure
    /// (inverse variance is the usual one).
    ///
    /// Returns `None` when no leg filled: emitting a blend from an empty sample
    /// is exactly the failure this guards. Never extrapolates a short leg.
    pub fn blend(&self, legs: &[Option<(f64, f64)>]) -> Option<f64> {
        let mut num = 0.0;
        let mut den = 0.0;
        for (i, leg) in legs.iter().enumerate().take(self.windows_min.len()) {
            let Some((value, quality)) = *leg else {
                continue;
            };
            if !value.is_finite() || !quality.is_finite() || quality <= 0.0 {
                continue;
            }
            let w = self.weight(i) * quality;
            num += w * value;
            den += w;
        }
        (den > 0.0).then(|| num / den)
    }
}

impl Default for MtfWindows {
    fn default() -> Self {
        Self::new(
            DEFAULT_SIGMA_WINDOWS_MIN.to_vec(),
            DEFAULT_SIGMA_WEIGHTS.to_vec(),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn minutes_to_bars_at_several_bar_sizes() {
        // Default legs: 1 h / 24 h / 1 w with INVERSE-VARIANCE weights.
        let w = MtfWindows::default();
        assert_eq!(w.windows_min, DEFAULT_SIGMA_WINDOWS_MIN.to_vec());
        assert_eq!(w.weights, DEFAULT_SIGMA_WEIGHTS.to_vec());
        assert_eq!(w.bars(30), vec![2, 48, 336]);
        assert_eq!(w.bars(5), vec![12, 288, 2_016]);
        // Legacy 6 h / 2 d / 1 w set, kept as an explicit case.
        let legacy = MtfWindows::equal([360u32, 2_880, 10_080]);
        assert_eq!(
            legacy.bars(30),
            vec![12, 96, 336],
            "6 h / 2 d / 1 w in 30 m bars"
        );
        assert_eq!(legacy.bars(1), vec![360, 2_880, 10_080], "1 m bars");
        assert_eq!(legacy.bars(60), vec![6, 48, 168], "1 h bars");
        assert_eq!(
            legacy.bars(1_440),
            vec![1, 2, 7],
            "1 d bars, 6 h floors to 1"
        );
        assert_eq!(legacy.max_bars(30), 336);
        // Brick-sizing legs keep their 14/60/180-day meaning in minutes.
        let b = MtfWindows::equal(DEFAULT_BRICK_WINDOWS_MIN);
        assert_eq!(b.bars(30), vec![14 * 48, 60 * 48, 180 * 48]);
    }

    #[test]
    fn sigma_leg_resolution_and_scaling() {
        let fast = &DEFAULT_SIGMA_LEGS[0];
        assert_eq!(fast.bars(), 12, "1 h at 5 m bars");
        assert_eq!(fast.span_bars_30m(), 2);
        assert_eq!(DEFAULT_SIGMA_LEGS[1].bars(), 48, "24 h at 30 m bars");
        assert_eq!(DEFAULT_SIGMA_LEGS[2].bars(), 336, "1 w at 30 m bars");
        assert_eq!(DEFAULT_SIGMA_LEGS[2].span_bars_30m(), 336);
        // A 5 m-bar sigma is a per-5-minute number: the per-30 m-bar contract
        // scale is sqrt(6) larger.
        let s = fast.to_per_30m_scale(0.001);
        assert!((s - 0.001 * (30.0f64 / 5.0).sqrt()).abs() < 1e-15);
        // A 30 m leg is already at contract scale.
        assert_eq!(DEFAULT_SIGMA_LEGS[1].to_per_30m_scale(0.001), 0.001);
        assert_eq!(
            DEFAULT_SIGMA_LEGS[0].estimator,
            crate::vol_estimator::SigmaEstimator::Yz
        );
        assert_eq!(
            DEFAULT_SIGMA_LEGS[1].estimator,
            crate::vol_estimator::SigmaEstimator::Gk
        );
        assert_eq!(
            DEFAULT_SIGMA_LEGS[2].estimator,
            crate::vol_estimator::SigmaEstimator::Gk
        );
    }

    #[test]
    fn max_days_covers_the_longest_window() {
        assert_eq!(MtfWindows::equal(DEFAULT_SIGMA_WINDOWS_MIN).max_days(30), 7);
        assert_eq!(MtfWindows::equal([360u32]).max_days(30), 1, "6 h rounds up");
        assert_eq!(MtfWindows::equal([10_081u32]).max_days(30), 7, "336 bars");
        assert_eq!(MtfWindows::equal([20_160u32]).max_days(30), 14, "2 w");
    }

    #[test]
    fn dropped_leg_renormalises_to_unit_weight() {
        let w = MtfWindows::equal([360u32, 2_880, 10_080]);
        // All three fill: plain mean.
        let all = w.blend(&[Some((0.01, 1.0)), Some((0.02, 1.0)), Some((0.03, 1.0))]);
        assert!((all.unwrap() - 0.02).abs() < 1e-12);
        // Short leg cannot fill: the survivors must renormalise to 1.0, i.e.
        // the answer is their mean, NOT a third of it.
        let dropped = w.blend(&[None, Some((0.02, 1.0)), Some((0.03, 1.0))]);
        assert!((dropped.unwrap() - 0.025).abs() < 1e-12);
        // One survivor is that survivor exactly.
        let one = w.blend(&[None, None, Some((0.03, 1.0))]);
        assert!((one.unwrap() - 0.03).abs() < 1e-12);
        // No survivor is a refusal, never a fabricated number.
        assert!(w.blend(&[None, None, None]).is_none());
    }

    #[test]
    fn equal_windows_reproduce_the_single_window_answer() {
        // Three legs of the SAME length see the same sample, so the blend must
        // equal the single-window value under any weighting.
        for weights in [vec![], vec![1.0, 1.0, 1.0], vec![0.2, 0.5, 0.3]] {
            let w = MtfWindows::new(vec![2_880; 3], weights);
            let single = 0.0137_f64;
            let got = w
                .blend(&[
                    Some((single, 1.0)),
                    Some((single, 3.7)),
                    Some((single, 0.4)),
                ])
                .unwrap();
            assert!((got - single).abs() < 1e-12, "got {got}");
        }
    }

    #[test]
    fn explicit_weights_are_honored_and_quality_multiplies() {
        let w = MtfWindows::new(vec![360, 2_880], vec![3.0, 1.0]);
        let got = w.blend(&[Some((0.04, 1.0)), Some((0.00, 1.0))]).unwrap();
        assert!((got - 0.03).abs() < 1e-12, "3:1 weighting");
        // Inverse-variance quality on top of equal configured weights.
        let e = MtfWindows::equal([360u32, 2_880]);
        let got = e.blend(&[Some((0.04, 1.0)), Some((0.00, 3.0))]).unwrap();
        assert!((got - 0.01).abs() < 1e-12);
        // A garbage weight entry falls back to 1.0 instead of dropping the leg.
        let bad = MtfWindows::new(vec![360, 2_880], vec![f64::NAN, 1.0]);
        let got = bad.blend(&[Some((0.04, 1.0)), Some((0.02, 1.0))]).unwrap();
        assert!((got - 0.03).abs() < 1e-12);
    }
}
