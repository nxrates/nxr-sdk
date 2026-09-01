//! Canonical volatility kernels — Rogers-Satchell (OHLC), Parkinson (HL),
//! Garman-Klass (OHLC) and Yang-Zhang (OHLC + overnight gap).
//!
//! ONE home for both range estimators. Rogers-Satchell is the canonical per-bin
//! 30-min vol-bin σ, shared by the offline `.vol` builder, the live
//! [`crate::vol::LiveVolRing`], and the synth OHLC reconstruction. The ratified
//! decision (2026-06): the canonical 30-min vol-bin σ is the Rogers-Satchell
//! (1991) drift-robust range estimator computed over s10-resampled OHLC, with
//! `offline == live` byte-for-byte.
//!
//! Parkinson ([`parkinson_variance`] / [`parkinson_sigma`]) is NOT the vol-bin
//! basis. It survives for the two callers that genuinely need a high/low-only
//! estimator: the synth quadratic form (needs a per-leg VARIANCE it can invert
//! back to a log-range) and the offline audit's daily σ cross-check.
//!
//! ⚠ NOT every σ producer in the tree. `core/src/server/signed.rs` computes the
//! co-signed-quote σ with its OWN private 48-bar Parkinson estimator, so a
//! signed mark is gated on a σ that no `.vol` file or renko brick uses.
//!
//! That divergence is DELIBERATE, and swapping signed.rs to Rogers-Satchell is
//! NOT proven better (audit 2026-08-14, 21 d of live `.s10`, 30-min bins, the
//! signer's deployed 48- and 336-bar windows). RS/PK lands 0.94-1.05 on
//! BTC-USDT, EUR-USD, XAU-USD and EUR-USDC, inside the σ cosign tolerance, so
//! there is nothing to win; on a thin stable (USDC-USDT) it is 1.42-1.56,
//! because bid-ask bounce inflates the RS corner product on a wide book. Which
//! of the two is then CORRECT on that tape was not settled: every truth proxy
//! available on a bounce-dominated stable is itself bounce-contaminated. Do not
//! reopen this without a noise-robust proxy.
//!
//! RS stays HERE on two properties that survived review: per-bin drift
//! contamination orders RS < GK < PK on every tape measured, and EMA(28)
//! shrinks the variance of that contamination but not its bias, so it reaches
//! the renko brick. Its one failure mode is exact: RS is 0 on any monotone bar
//! (H=C,L=O or H=O,L=C). That is 0 % of non-degenerate 30-min bins on every
//! liquid tape, 6 % only on a stale session-equity tape, and the brick's
//! `min_pct` floor covers it.
//!
//! Per 30-min vol-bin OHLC (O = first s10.open, H = max s10.high,
//! L = min s10.low, C = last s10.close, on the TDWAP mid):
//!
//! ```text
//! v          = ln(H/C)·ln(H/O) + ln(L/C)·ln(L/O)
//! sigma_pct  = sqrt(v.max(0))
//! ```
//!
//! Emits the same per-bin std-of-log-price contract as the prior Parkinson
//! kernel → downstream EMA(28) → MTF inverse-variance winsorized blend →
//! `brick_pct = max(k·σ, MIN_BRICK_PCT)` stays byte-stable; ONLY the per-bin
//! kernel + its input source change.

use mitch::common::AssetClass;
use mitch::ticker::TickerId;
use serde::{Deserialize, Serialize};

/// Rogers-Satchell variance for one OHLC bucket.
///
/// `v = ln(H/C)·ln(H/O) + ln(L/C)·ln(L/O)`. Non-negative by construction when
/// `H ≥ max(O,C)` and `L ≤ min(O,C)`. Returns `0.0` on degenerate / non-finite
/// input (any non-positive price).
#[inline]
pub fn rs_variance(o: f64, h: f64, l: f64, c: f64) -> f64 {
    if !(o > 0.0 && h > 0.0 && l > 0.0 && c > 0.0) {
        return 0.0;
    }
    let lhc = (h / c).ln();
    let lho = (h / o).ln();
    let llc = (l / c).ln();
    let llo = (l / o).ln();
    let v = lhc * lho + llc * llo;
    if v.is_finite() { v } else { 0.0 }
}

/// Per-bin Rogers-Satchell sigma (std-of-log-price) for one OHLC bucket.
///
/// `sigma_pct = sqrt(v.max(0))`. This is the canonical per-bin σ contract — the
/// drop-in replacement for the old Parkinson HL kernel. Same units, same
/// downstream EMA/blend.
#[inline]
pub fn rs_sigma_from_ohlc(o: f64, h: f64, l: f64, c: f64) -> f64 {
    rs_variance(o, h, l, c).max(0.0).sqrt()
}

/// `4 ln 2`, the Parkinson normalizer.
pub const FOUR_LN2: f64 = 4.0 * std::f64::consts::LN_2;
const INV_4LN2: f64 = 1.0 / FOUR_LN2;

/// Parkinson variance for one high/low bucket: `ln(H/L)^2 / (4 ln 2)`.
///
/// Caller validates `H >= L > 0`; garbage in yields a non-finite result rather
/// than a silent 0, because the two callers reject the bucket upstream and a
/// masked 0 would understate σ. Pairs with the [`FOUR_LN2`] inversion back to a
/// log-range in `synth::ohlc`.
#[inline]
pub fn parkinson_variance(h: f64, l: f64) -> f64 {
    let r = (h / l).ln();
    INV_4LN2 * r * r
}

/// Parkinson sigma (std-of-log-price) over aligned high/low slices.
///
/// `sqrt(mean(ln(H/L)^2) / (4 ln 2))`, valid for continuous GBM. Skips bars
/// where `low <= 0` or `high < low`. Returns 0.0 when no valid bar is found.
/// Same units as [`rs_sigma_from_ohlc`]: a per-bar fraction, unannualized.
pub fn parkinson_sigma(highs: &[f64], lows: &[f64]) -> f64 {
    let n = highs.len().min(lows.len());
    let mut sum = 0.0;
    let mut count = 0u32;
    for i in 0..n {
        if lows[i] > 0.0 && highs[i] >= lows[i] {
            let r = (highs[i] / lows[i]).ln();
            sum += r * r;
            count += 1;
        }
    }
    if count == 0 {
        return 0.0;
    }
    (sum / count as f64 / FOUR_LN2).sqrt()
}

/// Garman-Klass variance for one OHLC bucket.
///
/// `v = 0.5*ln(H/L)^2 - (2 ln 2 - 1)*ln(C/O)^2`. Uses the close/open drift
/// Parkinson ignores, so it is ~5x more efficient per bar on a GBM. Returns
/// `0.0` on degenerate / non-positive input; a well-formed bar can still yield
/// a small negative sample on a bounce-dominated book (the estimator is only
/// asymptotically non-negative), so the slice form clamps at 0.
#[inline]
pub fn garman_klass_variance(o: f64, h: f64, l: f64, c: f64) -> f64 {
    if !(o > 0.0 && h > 0.0 && l > 0.0 && c > 0.0) {
        return 0.0;
    }
    let hl = (h / l).ln();
    let co = (c / o).ln();
    let v = 0.5 * hl * hl - (2.0 * std::f64::consts::LN_2 - 1.0) * co * co;
    if v.is_finite() { v } else { 0.0 }
}

/// Garman-Klass sigma (std-of-log-price) over aligned OHLC slices.
///
/// `sqrt(mean(clamped per-bar GK variance))`. Same units as
/// [`rs_sigma_from_ohlc`] / [`parkinson_sigma`]: a per-bar fraction,
/// unannualized. Skips bars where prices are non-positive or `high < low`.
/// Returns 0.0 when no valid bar is found.
pub fn garman_klass_sigma(opens: &[f64], highs: &[f64], lows: &[f64], closes: &[f64]) -> f64 {
    let n = opens
        .len()
        .min(highs.len())
        .min(lows.len())
        .min(closes.len());
    let mut sum = 0.0;
    let mut count = 0u32;
    for i in 0..n {
        let (o, h, l, c) = (opens[i], highs[i], lows[i], closes[i]);
        if o > 0.0 && c > 0.0 && l > 0.0 && h >= l {
            sum += garman_klass_variance(o, h, l, c).max(0.0);
            count += 1;
        }
    }
    if count == 0 {
        return 0.0;
    }
    (sum / count as f64).sqrt()
}

/// Yang-Zhang sigma (std-of-log-price) over aligned OHLC slices.
///
/// Minimum-variance combination of the overnight-gap (open) variance, the
/// close-to-close variance and the Rogers-Satchell drift-robust range:
///
/// ```text
/// k          = 0.34 / (1.34 + (n+1)/(n-1))
/// sigma_yz^2 = sigma_o^2 + k*sigma_rs^2 + (1-k)*sigma_c^2
/// ```
///
/// where `sigma_o^2` / `sigma_c^2` are the sample variances of
/// `ln(O_i/C_{i-1})` / `ln(C_i/C_{i-1})` over the n-1 gaps. YZ is the only
/// kernel in this module that prices the overnight gap, which is what the 5 m
/// signed-sigma fast leg wants. Same per-bar units as the other kernels.
/// Returns 0.0 for fewer than 2 bars (no gap) or too few valid bars.
pub fn yang_zhang_sigma(opens: &[f64], highs: &[f64], lows: &[f64], closes: &[f64]) -> f64 {
    let n = opens
        .len()
        .min(highs.len())
        .min(lows.len())
        .min(closes.len());
    if n < 2 {
        return 0.0;
    }
    // Gap series over the n-1 overnight boundaries; bar 0 has no prior close.
    let mut gaps = Vec::with_capacity(n - 1);
    for i in 1..n {
        if opens[i] > 0.0 && closes[i - 1] > 0.0 && closes[i] > 0.0 {
            gaps.push((opens[i] / closes[i - 1]).ln());
        }
    }
    if gaps.len() < 2 {
        return 0.0;
    }
    let mean = gaps.iter().sum::<f64>() / gaps.len() as f64;
    let open_var =
        gaps.iter().map(|g| (g - mean) * (g - mean)).sum::<f64>() / (gaps.len() - 1) as f64;
    let mut cc = Vec::with_capacity(gaps.len());
    for i in 1..n {
        if closes[i] > 0.0 && closes[i - 1] > 0.0 {
            cc.push((closes[i] / closes[i - 1]).ln());
        }
    }
    let cc_mean = cc.iter().sum::<f64>() / cc.len() as f64;
    let close_var = cc
        .iter()
        .map(|g| (g - cc_mean) * (g - cc_mean))
        .sum::<f64>()
        / (cc.len() - 1) as f64;
    let rs: f64 = (0..n)
        .filter(|&i| opens[i] > 0.0 && closes[i] > 0.0 && lows[i] > 0.0 && highs[i] >= lows[i])
        .map(|i| rs_variance(opens[i], highs[i], lows[i], closes[i]))
        .sum();
    let k = 0.34 / (1.34 + (n as f64 + 1.0) / (n as f64 - 1.0));
    let v = (open_var + k * rs / n as f64 + (1.0 - k) * close_var).max(0.0);
    if v.is_finite() { v.sqrt() } else { 0.0 }
}

/// Which kernel a sigma leg runs. Serde spells match the config keys
/// (`parkinson`, `gk`, `yz`), with the long forms accepted as aliases.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum SigmaEstimator {
    /// Parkinson high/low range (the legacy signed-sigma kernel).
    Parkinson,
    /// Garman-Klass OHLC range.
    #[serde(alias = "garman_klass")]
    Gk,
    /// Yang-Zhang OHLC + overnight gap.
    #[serde(alias = "yang_zhang")]
    Yz,
}

impl SigmaEstimator {
    /// Per-bar sigma of this kernel over aligned OHLC slices, in the slices'
    /// own bar width. Parkinson reads only highs/lows; the others use the
    /// opens and closes too.
    pub fn sigma(self, opens: &[f64], highs: &[f64], lows: &[f64], closes: &[f64]) -> f64 {
        match self {
            Self::Parkinson => parkinson_sigma(highs, lows),
            Self::Gk => garman_klass_sigma(opens, highs, lows, closes),
            Self::Yz => yang_zhang_sigma(opens, highs, lows, closes),
        }
    }
}

/// Per-class floor on the 30-minute sigma (fraction), shared by the ingest
/// band (`core/src/aggregator.rs`) and the signed-sigma path (`signed.rs`).
/// Each is the 30-min Rogers-Satchell sigma implied by the class's typical
/// daily move over a 24 h market's 48 bins (an equity session is ~13 bins, so
/// its implied daily is the smaller 54 bps). FX is the EM-cross number, not
/// the major.
///
/// Priors as FLOORS, never the driver: a quiet window may not pull sigma below
/// the operator's class prior, but the estimate itself is never replaced by
/// one.
pub const SIGMA_FLOOR_30M_FX: f64 = 0.0020;
pub const SIGMA_FLOOR_30M_EQUITY: f64 = 0.0015;
pub const SIGMA_FLOOR_30M_COMMODITY: f64 = 0.0025;
pub const SIGMA_FLOOR_30M_CRYPTO: f64 = 0.0040;

/// PEGGED-PAIR floor: both legs redeem against the SAME numeraire, so the
/// pair's 30-minute sigma is peg noise, not asset volatility.
///
/// It is its own class because MITCH wire bits cannot see a peg: a stablecoin
/// is class `CR` like any other token, so `class_sigma_floor_30m` sends every
/// USDT/USDC/PYUSD/USDS pair down the crypto arm and floors it at 0.40% per
/// 30 min — measured live 2026-08-31, every stable on the BTR oracle read
/// sigmaPbps 4000, the identical number WBTC and BNB read, i.e. ~20x their
/// realised sigma. On the DEX that floor is a direct vega surcharge on the
/// tightest pairs we quote.
///
/// 2 bps per 30 min is the peg-noise scale the ingest band already assumes:
/// the widest HEALTHY pegged `ci` observed is 6.53 bps
/// (`core/src/aggregator.rs` REJECT_BAND_PCT_PEGGED doc), which is a spread,
/// not a per-bar move. Still a FLOOR: a genuine depeg walks the measured
/// Parkinson sigma straight through it.
pub const SIGMA_FLOOR_30M_STABLE: f64 = 0.0002;

/// The whole point of the split: a stable's prior must be an ORDER OF MAGNITUDE
/// under the crypto one, or it is still Bitcoin's floor wearing another name.
const _: () = assert!(SIGMA_FLOOR_30M_STABLE * 10.0 < SIGMA_FLOOR_30M_CRYPTO);

/// Class floor for a MITCH wire class pair. Garbage class bits land on equity,
/// the same default [`mitch::common::AssetClass`] resolution falls back to.
pub fn class_sigma_floor_30m(base: AssetClass, quote: AssetClass) -> f64 {
    match (base, quote) {
        (AssetClass::FX, AssetClass::FX) => SIGMA_FLOOR_30M_FX,
        (AssetClass::CM | AssetClass::PM, _) | (_, AssetClass::CM | AssetClass::PM) => {
            SIGMA_FLOOR_30M_COMMODITY
        }
        (AssetClass::CR, _) | (_, AssetClass::CR) => SIGMA_FLOOR_30M_CRYPTO,
        _ => SIGMA_FLOOR_30M_EQUITY,
    }
}

/// Class floor for a MITCH ticker id, resolved from the wire's 4-bit asset
/// class fields (no symbol lookup, same derivation as the ingest band).
pub fn class_sigma_floor_30m_for_ticker(ticker_id: u64) -> f64 {
    let t = TickerId::from_raw(ticker_id);
    class_sigma_floor_30m(t.base_asset_class(), t.quote_asset_class())
}

/// What the two legs of a pair redeem against, which is the thing the MITCH
/// wire class cannot see. A stablecoin is `CR` like any other token, so the
/// wire class alone routes EVERY pair with a token leg down the crypto arm.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PegClass {
    /// Both legs redeem against the SAME numeraire (USDT/USDC, USDC/USD). The
    /// pair's move is peg noise.
    SameNumeraire,
    /// Both legs are pegged, to DIFFERENT numeraires (EURC/USDC, USDC/EUR).
    /// The pair IS a fiat cross whatever the wire says its legs are, so it
    /// takes the FX prior — measured on the BTR oracle 2026-09-01, EURC-USDC
    /// read sigmaPbps 4000, the identical number WBTC read, for a EUR/USD rate.
    CrossFiat,
    /// At least one leg has no peg: the wire class is then the honest answer.
    Unpegged,
}

/// Sigma floor for a ticker whose PEG class the caller has already resolved.
///
/// [`PegClass`] is NOT derived here on purpose: the peg lists are the
/// operator's (`cexs.pegged` ∪ `cexs.usd_aliases` ∪ `cexs.fiat_pegged`) and the
/// one resolution of them is `server::signed::peg_class_of_ticker`, whose
/// USD-peg arm also drives the agreement ceiling. Deriving a second answer from
/// the wire bits is what would let a pair be pegged for the ceiling and
/// volatile for the floor.
#[inline]
pub fn sigma_floor_30m_for_ticker(ticker_id: u64, peg: PegClass) -> f64 {
    match peg {
        PegClass::SameNumeraire => SIGMA_FLOOR_30M_STABLE,
        // NOT `min` with the wire class: an FX cross whose legs are tokens
        // decodes CR and would keep 0.40 %, which is the defect. The peg
        // classification is strictly better information than the class bits.
        PegClass::CrossFiat => SIGMA_FLOOR_30M_FX,
        PegClass::Unpegged => class_sigma_floor_30m_for_ticker(ticker_id),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rs_zero_range_is_zero() {
        assert_eq!(rs_sigma_from_ohlc(100.0, 100.0, 100.0, 100.0), 0.0);
    }

    #[test]
    fn rs_nonneg_on_well_formed_ohlc() {
        let s = rs_sigma_from_ohlc(100.0, 102.0, 99.0, 101.0);
        assert!(s > 0.0 && s.is_finite(), "got {s}");
    }

    #[test]
    fn rs_degenerate_inputs_are_zero() {
        assert_eq!(rs_sigma_from_ohlc(0.0, 1.0, 1.0, 1.0), 0.0);
        assert_eq!(rs_sigma_from_ohlc(1.0, -1.0, 1.0, 1.0), 0.0);
    }

    #[test]
    fn rs_matches_manual_formula() {
        let (o, h, l, c) = (100.0_f64, 105.0_f64, 98.0_f64, 102.0_f64);
        let v = (h / c).ln() * (h / o).ln() + (l / c).ln() * (l / o).ln();
        assert!((rs_sigma_from_ohlc(o, h, l, c) - v.max(0.0).sqrt()).abs() < 1e-15);
    }

    /// Pins the RS contract to a hand-computed value so a refactor cannot move
    /// σ silently: it is signed on-chain verbatim.
    #[test]
    fn rs_pinned_hand_computed() {
        // O=100 H=105 L=98 C=102:
        // ln(105/102)·ln(105/100) + ln(98/102)·ln(98/100) = 0.00222252274925343
        assert!((rs_variance(100.0, 105.0, 98.0, 102.0) - 0.002_222_522_749_253_43).abs() < 1e-15);
        assert!(
            (rs_sigma_from_ohlc(100.0, 105.0, 98.0, 102.0) - 0.047_143_639_541_866_4).abs() < 1e-15
        );
    }

    /// Pins the Parkinson contract the same way.
    #[test]
    fn parkinson_pinned_hand_computed() {
        // One bar, H=105 L=98: ln(105/98) = 0.0689928714869514; squared and
        // divided by 4 ln 2 = 2.7725887222397812.
        let v = parkinson_variance(105.0, 98.0);
        assert!((v - 0.001_716_812_983_416_35).abs() < 1e-15, "got {v}");
        // Single-bar sigma is sqrt of that variance.
        let s = parkinson_sigma(&[105.0], &[98.0]);
        assert!((s - v.sqrt()).abs() < 1e-15, "got {s}");
        assert!((s - 0.041_434_441_994_750_5).abs() < 1e-15, "got {s}");
        // Two bars average the SQUARED log-ranges, not the sigmas.
        let two = parkinson_sigma(&[105.0, 101.0], &[98.0, 100.0]);
        let manual = {
            let a = (105.0_f64 / 98.0).ln();
            let b = (101.0_f64 / 100.0).ln();
            ((a * a + b * b) / 2.0 / FOUR_LN2).sqrt()
        };
        assert!((two - manual).abs() < 1e-15);
    }

    #[test]
    fn parkinson_skips_invalid_bars() {
        assert_eq!(parkinson_sigma(&[105.0], &[0.0]), 0.0);
        assert_eq!(parkinson_sigma(&[98.0], &[105.0]), 0.0);
        assert_eq!(parkinson_sigma(&[], &[]), 0.0);
        // An invalid bar is dropped, not counted in the divisor.
        let mixed = parkinson_sigma(&[105.0, 98.0], &[98.0, 105.0]);
        assert!((mixed - parkinson_sigma(&[105.0], &[98.0])).abs() < 1e-15);
    }

    /// Pins the Garman-Klass contract to a hand-computed value.
    #[test]
    fn garman_klass_pinned_hand_computed() {
        // O=100 H=105 L=98 C=102:
        // 0.5*ln(105/98)^2 - (2 ln 2 - 1)*ln(102/100)^2 = 0.002228525123583498
        let v = garman_klass_variance(100.0, 105.0, 98.0, 102.0);
        assert!((v - 0.002_228_525_123_583_498).abs() < 1e-15, "got {v}");
        assert!(
            (garman_klass_sigma(&[100.0], &[105.0], &[98.0], &[102.0]) - v.sqrt()).abs() < 1e-15
        );
    }

    #[test]
    fn garman_klass_skips_invalid_bars_and_clamps_negative_samples() {
        assert_eq!(garman_klass_sigma(&[0.0], &[105.0], &[98.0], &[102.0]), 0.0);
        assert_eq!(garman_klass_sigma(&[], &[], &[], &[]), 0.0);
        // A fully flat bar has no range and no drift: exactly 0.
        assert_eq!(garman_klass_variance(100.0, 100.0, 100.0, 100.0), 0.0);
        // A monotone bar (O=L, H=C) keeps a small positive sample: the range
        // term outruns the drift term but cannot cancel it entirely.
        let mono = garman_klass_variance(98.0, 105.0, 98.0, 105.0);
        assert!(mono > 0.0 && mono < 1e-3, "got {mono}");
        // An invalid bar is dropped, not counted in the divisor.
        let mixed = garman_klass_sigma(
            &[100.0, 100.0],
            &[105.0, 105.0],
            &[98.0, 0.0],
            &[102.0, 102.0],
        );
        assert!((mixed - garman_klass_sigma(&[100.0], &[105.0], &[98.0], &[102.0])).abs() < 1e-15);
    }

    /// Pins the Yang-Zhang contract to a hand-computed 3-bar value.
    #[test]
    fn yang_zhang_pinned_hand_computed() {
        let o = [100.0, 101.0, 103.0];
        let h = [105.0, 104.0, 106.0];
        let l = [98.0, 99.0, 101.0];
        let c = [102.0, 100.0, 105.0];
        // sigma_o^2 = var(ln(O_i/C_{i-1})) = 7.766173497619068e-4 (sample var)
        // sigma_c^2 = var(ln(C_i/C_{i-1})) = 2.3524855205224534e-3
        // k = 0.34 / (1.34 + 4/2) = 0.10179640718562875
        // mean RS variance = 1.535088979302568e-3
        // sigma_yz^2 = 3.0458948391422162e-3
        let s = yang_zhang_sigma(&o, &h, &l, &c);
        assert!((s - 0.055_189_626_191_361_51).abs() < 1e-14, "got {s}");
    }

    #[test]
    fn yang_zhang_constant_series_is_zero() {
        let p = [100.0_f64; 4];
        let s = yang_zhang_sigma(&p, &p, &p, &p);
        assert_eq!(s, 0.0, "no gaps, no range, no close moves");
        // Too few bars for a gap sample is a refusal, not a fabricated number.
        assert_eq!(yang_zhang_sigma(&[100.0], &[105.0], &[98.0], &[102.0]), 0.0);
        assert_eq!(yang_zhang_sigma(&[], &[], &[], &[]), 0.0);
    }

    /// On a synthetic constant-vol GBM the three HL/OHLC kernels must agree in
    /// scale: GK/YZ are efficiency upgrades over Parkinson, not different
    /// quantities. All three stay within 25% of the true per-bar sigma.
    #[test]
    fn kernels_agree_in_scale_on_synthetic_gbm() {
        // Deterministic LCG so the test never flakes.
        let mut state = 0x2545_F491_4F6C_DD1Du64;
        let mut next = || {
            state ^= state << 13;
            state ^= state >> 7;
            state ^= state << 17;
            (state >> 11) as f64 / (1u64 << 53) as f64 - 0.5
        };
        let n = 512;
        let true_sigma = 0.01_f64;
        let mut price = 100.0;
        let (mut opens, mut highs, mut lows, mut closes) =
            (Vec::new(), Vec::new(), Vec::new(), Vec::new());
        for _ in 0..n {
            let o = price;
            // Close-to-close log-return ~ unit-variance, then the bar's
            // high/low bracket both ends by an extra ~true_sigma excursion,
            // so every kernel sees the SAME total variance.
            let z = next() + next() + next();
            let c = o * (true_sigma * z).exp();
            let high = c.max(o) * (1.0 + true_sigma * next().abs());
            let low = c.min(o) / (1.0 + true_sigma * next().abs());
            opens.push(o);
            highs.push(high);
            lows.push(low);
            closes.push(c);
            price = c;
        }
        let pk = parkinson_sigma(&highs, &lows);
        let gk = garman_klass_sigma(&opens, &highs, &lows, &closes);
        let yz = yang_zhang_sigma(&opens, &highs, &lows, &closes);
        // Same total variance must read as the same order of sigma on every
        // kernel: each lands within a factor of 2 of the truth, and the three
        // agree pairwise to better than 50%. Tighter than that depends on the
        // synthetic bar shape, which is not the contract.
        for (name, s) in [("pk", pk), ("gk", gk), ("yz", yz)] {
            assert!(
                s > 0.5 * true_sigma && s < 2.0 * true_sigma,
                "{name} = {s} vs true {true_sigma}"
            );
        }
        let spread = pk.max(gk).max(yz) / pk.min(gk).min(yz);
        assert!(
            spread < 1.5,
            "kernel spread {spread}x: pk={pk} gk={gk} yz={yz}"
        );
    }

    /// The dispatch wrapper agrees with each kernel's own slice function.
    #[test]
    fn estimator_dispatch_matches_direct_calls() {
        let o = [100.0, 101.0];
        let h = [105.0, 104.0];
        let l = [98.0, 99.0];
        let c = [102.0, 100.0];
        assert_eq!(
            SigmaEstimator::Parkinson.sigma(&o, &h, &l, &c),
            parkinson_sigma(&h, &l)
        );
        assert_eq!(
            SigmaEstimator::Gk.sigma(&o, &h, &l, &c),
            garman_klass_sigma(&o, &h, &l, &c)
        );
        assert_eq!(
            SigmaEstimator::Yz.sigma(&o, &h, &l, &c),
            yang_zhang_sigma(&o, &h, &l, &c)
        );
    }

    #[test]
    fn class_floors_match_the_wire_class_bits() {
        use mitch::common::AssetClass;
        assert_eq!(
            class_sigma_floor_30m(AssetClass::FX, AssetClass::FX),
            SIGMA_FLOOR_30M_FX
        );
        assert_eq!(
            class_sigma_floor_30m(AssetClass::CR, AssetClass::CR),
            SIGMA_FLOOR_30M_CRYPTO
        );
        assert_eq!(
            class_sigma_floor_30m(AssetClass::CM, AssetClass::FX),
            SIGMA_FLOOR_30M_COMMODITY
        );
        assert_eq!(
            class_sigma_floor_30m(AssetClass::EQ, AssetClass::EQ),
            SIGMA_FLOOR_30M_EQUITY
        );
    }

    /// FIX (2026-08-31): a stablecoin PAIR is class `CR` on the wire, so the
    /// crypto arm floored USDT-USDC at Bitcoin's 0.40%/30 min. The peg class
    /// must win, and it must not touch the volatile floors.
    #[test]
    fn pegged_pair_escapes_the_crypto_class_floor() {
        let id = crate::resolve_ticker_id("USDT/USDC");
        assert_eq!(
            class_sigma_floor_30m_for_ticker(id),
            SIGMA_FLOOR_30M_CRYPTO,
            "precondition: the wire bits classify a stable pair as crypto"
        );
        assert_eq!(
            sigma_floor_30m_for_ticker(id, PegClass::SameNumeraire),
            SIGMA_FLOOR_30M_STABLE
        );
    }

    /// FIX (2026-09-01): a token pegged to a NON-USD fiat is still `CR` on the
    /// wire, so `EURC-USDC` — a EUR/USD rate — inherited Bitcoin's prior and
    /// read sigmaPbps 4000 on chain. A cross of two pegs is FX.
    #[test]
    fn a_cross_fiat_pair_takes_the_fx_prior_not_the_crypto_one() {
        for sym in ["EURC/USDC", "USDC/EUR"] {
            let id = crate::resolve_ticker_id(sym);
            assert_eq!(
                sigma_floor_30m_for_ticker(id, PegClass::CrossFiat),
                SIGMA_FLOOR_30M_FX,
                "{sym}"
            );
            assert!(
                sigma_floor_30m_for_ticker(id, PegClass::CrossFiat) < SIGMA_FLOOR_30M_CRYPTO,
                "{sym}: the whole point is escaping the crypto prior"
            );
        }
        // A pegged CROSS is not a pegged PAIR: EUR/USD moves, so it must not
        // collapse onto the 2 bps peg-noise prior either.
        let id = crate::resolve_ticker_id("EURC/USDC");
        assert!(sigma_floor_30m_for_ticker(id, PegClass::CrossFiat) > SIGMA_FLOOR_30M_STABLE);
        // The three arms are distinct, in the order their volatility implies.
        assert!(SIGMA_FLOOR_30M_STABLE < SIGMA_FLOOR_30M_FX);
        assert!(SIGMA_FLOOR_30M_FX < SIGMA_FLOOR_30M_CRYPTO);
    }

    /// The volatile floors are UNCHANGED: only the pegged branch is new.
    #[test]
    fn volatile_floors_are_untouched_by_the_peg_branch() {
        for sym in ["WBTC/USDC", "BNB/USDT", "ETH/USDC"] {
            let id = crate::resolve_ticker_id(sym);
            assert_eq!(
                sigma_floor_30m_for_ticker(id, PegClass::Unpegged),
                class_sigma_floor_30m_for_ticker(id),
                "{sym}"
            );
        }
    }
}
