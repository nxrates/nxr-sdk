//! Compose a synthetic cross S10 bar from its native leg S10 bars.
//!
//! A cross is a pure function of its persisted legs. We never store per-cross
//! S10 (that was the eager pipeline that starved the native feed + OOM'd on
//! per-cross rings); `/v1/ohlc` and the hot tier derive the cross bar on demand
//! from the two native leg bars via this ONE canonical fn.
//!
//! Bars carry OHLC as *mid* prices (no bid/ask — see `mitch::Bar`), so the bar
//! body is a pure multiplicative composition, `Π leg^exp`. The signed-exponent
//! product and the MAX_PRICE poison guards are the same ones the live-tick path
//! uses (`super::tick::compute_synth_tick`); this fn is their bar-level twin.

use mitch::Bar;

use super::paths::Leg;
use crate::shard::FLAG_COMPOSED;
use crate::tdwap::{decode_ci_ubp, encode_ci_ubp};

/// Compose a cross-rate S10 bar from N signed leg bars (2 for a simple cross).
///
/// `legs[i]` is `(symbol, exponent ±1)`; `bars[i]` is that leg's S10 bar for the
/// SAME 10 s bucket (caller aligns them — all leg bars must share the bucket
/// window; the composed bar inherits it from `bars[0]`).
///
/// Field math (`p = Π x^exp` over legs):
/// - `open/close`     = product of leg opens / closes — exact synthetic endpoints.
/// - `high/low`       = ALIGNED corners: envelope of {open, close, Π highᵢ^expᵢ,
///   Π lowᵢ^expᵢ} — assumes leg extremes co-occur (correlated legs sharing a
///   pivot quote, the overwhelmingly common case). The outward corner
///   (+1 leg high ÷ −1 leg low) was REJECTED 2026-07-15: it multiplies leg
///   ranges and paints large symmetric synthetic wicks on every cross,
///   especially after rollup to high TFs. Aligned corners can under-range
///   anti-correlated moves inside one 10 s bucket — accepted; `FLAG_COMPOSED`
///   marks the bar derived.
/// - `avg_ci_ubp`     = `encode(√Σ decode(ci_i)²)` — relative-CI quadrature.
/// - `avg_spread_bps` = `Σ spread_i` — additive, non-compounding.
/// - `vbid/vask/tick_count` = min over legs (the bottleneck leg bounds it).
/// - `reject_rate`    = max over legs (worst leg).
/// - `realized_var/bipower_var/drift/vol_imbalance/max_abs_return` = 0 (path-
///   dependent HF accumulators, not composable from summary bars).
///
/// Returns `None` if `legs`/`bars` are empty or mismatched, if any leg OHLC is
/// non-finite / non-positive / `> mitch::MAX_PRICE` (poison guard, 2026-07-10
/// incident) or has `high < low`, or if the composed result is out of range.
pub fn compose_cross_s10(legs: &[Leg], bars: &[Bar]) -> Option<Bar> {
    if legs.is_empty() || legs.len() != bars.len() {
        return None;
    }

    let (mut open, mut close, mut high, mut low) = (1.0_f64, 1.0_f64, 1.0_f64, 1.0_f64);
    let mut ci_sq = 0.0_f64;
    let mut spread = 0.0_f32;
    let (mut vbid, mut vask, mut tick_count) = (u32::MAX, u32::MAX, u32::MAX);
    let mut reject_rate = 0u16;

    for (leg, b) in legs.iter().zip(bars) {
        let (o, h, l, c) = (b.open, b.high, b.low, b.close);
        // Poison guard on EVERY OHLC field before it multiplies into the cross:
        // a finite-but-astronomical leg would otherwise silently corrupt it.
        for v in [o, h, l, c] {
            if !(v.is_finite() && v > 0.0 && v <= mitch::MAX_PRICE) {
                return None;
            }
        }
        if h < l {
            return None;
        }
        if leg.exp == 1 {
            open *= o;
            close *= c;
            high *= h;
            low *= l;
        } else {
            // Aligned inversion: this leg's high divides the cross "high
            // candidate" (extremes assumed co-occurring), NOT the outward
            // 1/low corner — see module doc.
            open /= o;
            close /= c;
            high /= h;
            low /= l;
        }
        let ci = decode_ci_ubp(b.avg_ci_ubp);
        ci_sq += ci * ci;
        spread += b.avg_spread_bps;
        vbid = vbid.min(b.vbid);
        vask = vask.min(b.vask);
        tick_count = tick_count.min(b.tick_count);
        reject_rate = reject_rate.max(b.reject_rate);
    }

    // Envelope: with aligned corners the h/l candidate products are not
    // ordered (an inverted leg with the larger range flips them) — the bar's
    // high/low is the envelope over endpoints + both candidates.
    let (hi_cand, lo_cand) = (high, low);
    high = open.max(close).max(hi_cand).max(lo_cand);
    low = open.min(close).min(hi_cand).min(lo_cand);
    // Same poison guard as per-leg, applied to the composed fields: a product
    // of capped legs can itself exceed MAX_PRICE.
    for v in [open, high, low, close] {
        if !(v.is_finite() && v > 0.0 && v <= mitch::MAX_PRICE) {
            return None;
        }
    }

    let mut bar = Bar::new_ohlcv(
        bars[0].open_mts(),
        bars[0].close_mts(),
        open,
        high,
        low,
        close,
        vbid,
        vask,
        tick_count,
    );
    bar.avg_ci_ubp = encode_ci_ubp(ci_sq.sqrt());
    bar.avg_spread_bps = spread;
    bar.reject_rate = reject_rate;
    bar.flags = FLAG_COMPOSED;
    Some(bar)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn bar(o: f64, h: f64, l: f64, c: f64) -> Bar {
        // Grid-aligned S10 bucket (mts ticks); values are placeholders for the window.
        let mut b = Bar::new_ohlcv(1_783_382_400_000, 1_783_382_410_000, o, h, l, c, 100, 120, 50);
        b.avg_ci_ubp = encode_ci_ubp(300.0); // ~3 bps relative CI per leg
        b.avg_spread_bps = 1.5;
        b
    }

    fn legs2() -> Vec<Leg> {
        vec![Leg::new("ETH/USDT", 1), Leg::new("BTC/USDT", -1)]
    }

    #[test]
    fn direct_and_inverse_endpoints_exact() {
        // ETH/BTC = ETH/USDT × (BTC/USDT)^-1.
        let eth = bar(1800.0, 1820.0, 1790.0, 1810.0);
        let btc = bar(60000.0, 60500.0, 59800.0, 60200.0);
        let x = compose_cross_s10(&legs2(), &[eth, btc]).unwrap();
        // Copy out of the packed struct before asserting (E0793).
        let (open, high, low, close, flags, rv, ofi) =
            (x.open, x.high, x.low, x.close, x.flags, x.realized_var, x.vol_imbalance);
        assert!((open - 1800.0 / 60000.0).abs() < 1e-12);
        assert!((close - 1810.0 / 60200.0).abs() < 1e-12);
        // Aligned corners: candidates are ETH.high/BTC.high and ETH.low/BTC.low;
        // high/low = envelope over {open, close, candidates}.
        let cands = [
            1800.0 / 60000.0,
            1810.0 / 60200.0,
            1820.0 / 60500.0,
            1790.0 / 59800.0,
        ];
        let want_hi = cands.iter().cloned().fold(f64::MIN, f64::max);
        let want_lo = cands.iter().cloned().fold(f64::MAX, f64::min);
        assert!((high - want_hi).abs() < 1e-12);
        assert!((low - want_lo).abs() < 1e-12);
        assert!(high >= low);
        assert_eq!(flags, FLAG_COMPOSED);
        assert_eq!(rv, 0.0);
        assert_eq!(ofi, 0.0);
    }

    #[test]
    fn ci_is_relative_quadrature() {
        let eth = bar(1800.0, 1820.0, 1790.0, 1810.0);
        let btc = bar(60000.0, 60500.0, 59800.0, 60200.0);
        let x = compose_cross_s10(&legs2(), &[eth, btc]).unwrap();
        let (ci, spread) = (x.avg_ci_ubp, x.avg_spread_bps);
        let got = decode_ci_ubp(ci);
        let want = (300.0_f64 * 300.0 + 300.0 * 300.0).sqrt();
        assert!((got - want).abs() / want < 0.02, "ci {got} vs {want}");
        assert!((spread - 3.0).abs() < 1e-6); // additive
    }

    #[test]
    fn min_max_aggregation() {
        let mut a = bar(2.0, 2.1, 1.9, 2.0);
        a.vbid = 50;
        a.tick_count = 10;
        a.reject_rate = 100;
        let mut b = bar(4.0, 4.1, 3.9, 4.0);
        b.vbid = 80;
        b.tick_count = 40;
        b.reject_rate = 300;
        let x = compose_cross_s10(&[Leg::new("A/USDT", 1), Leg::new("B/USDT", 1)], &[a, b]).unwrap();
        let (vbid, tc, rr) = (x.vbid, x.tick_count, x.reject_rate);
        assert_eq!(vbid, 50); // min
        assert_eq!(tc, 10); // min
        assert_eq!(rr, 300); // max
    }

    #[test]
    fn poison_leg_rejected() {
        let good = bar(1800.0, 1820.0, 1790.0, 1810.0);
        let poison = bar(1.0, mitch::MAX_PRICE * 2.0, 1.0, 1.0);
        assert!(compose_cross_s10(&legs2(), &[good, poison]).is_none());
    }

    #[test]
    fn zero_and_nonfinite_rejected() {
        let good = bar(1800.0, 1820.0, 1790.0, 1810.0);
        let zero = bar(0.0, 1.0, 0.0, 0.0);
        assert!(compose_cross_s10(&legs2(), &[good.clone(), zero]).is_none());
        let nan = bar(f64::NAN, 1.0, 1.0, 1.0);
        assert!(compose_cross_s10(&legs2(), &[good, nan]).is_none());
    }

    #[test]
    fn arity_guards() {
        assert!(compose_cross_s10(&[], &[]).is_none());
        let one = bar(1.0, 1.0, 1.0, 1.0);
        assert!(compose_cross_s10(&legs2(), &[one]).is_none()); // len mismatch
    }
}
