//! Time-Decay Weighted Average Price (TDWAP) aggregation.
//!
//! Level 2 of the two-level aggregation pipeline:
//!   Level 1: raw ticks -> per-provider Index (via `TickAccumulator`)
//!   Level 2: per-provider Indexes -> cross-provider TDWAP Index (this module)
//!
//! Exported types:
//!   - `ProviderEntry`: per-provider metadata wrapper around `Index`
//!   - `compute_vwap`: cross-provider TDWAP with adaptive decay and confidence interval

use std::time::Instant;

use mitch::Index;

use crate::agg::is_valid_tick;

/// Smoothing factor for the inter-arrival time EMA.
/// alpha = 0.1 -> converges to true inter-arrival time after ~10 updates.
const IPI_ALPHA: f64 = 0.1;

/// Half-life multiplier: weight halves after `IPI_K x ema_ipi` seconds.
/// Larger values -> more tolerance for stale quotes.
const IPI_K: f64 = 3.0;

/// Scale factor for the sqrt-compressed CI u16 encoding.
///
/// Canonical definition lives in [`mitch::common::CI_SCALE`]; re-exported here
/// so existing `nxr_sdk::tdwap::CI_SCALE` call sites keep working without a
/// rename. See the mitch definition for encoding semantics.
pub use mitch::common::CI_SCALE;

/// Encode a confidence interval (micro basis points of mid) into the u16 wire format.
///
/// Formula: `encoded = round(sqrt(ci_ubp) * CI_SCALE)`, clamped to `[0, u16::MAX]`.
///
/// Square-root compression was chosen over `ln(1 + x)` because:
///  - Closed-form inverse is trivial: `(encoded / CI_SCALE)^2`.
///  - At low values, encoded grows linearly in sqrt, so adjacent bins remain
///    well-separated (e.g. 1 ubp -> 16, 4 ubp -> 32, 9 ubp -> 48).
///  - Variance / standard-deviation semantics: CI is a 1-sigma interval, and
///    combining independent sigmas goes as sqrt(sum of squares). Encoding in
///    sqrt space is natural for downstream ops (addition of variances).
#[inline]
pub fn encode_ci_ubp(ci_ubp: f64) -> u16 {
    if !(ci_ubp.is_finite()) || ci_ubp <= 0.0 {
        return 0;
    }
    let v = ci_ubp.sqrt() * CI_SCALE;
    v.round().clamp(0.0, u16::MAX as f64) as u16
}

/// Decode a u16 confidence interval back to micro basis points of mid.
/// Inverse of `encode_ci_ubp`.
#[inline]
pub fn decode_ci_ubp(encoded: u16) -> f64 {
    let x = encoded as f64 / CI_SCALE;
    x * x
}

/// Per-provider, per-ticker TDWAP metadata.
///
/// Wraps a MITCH `Index` (the provider's latest aggregated quote) with the
/// time-decay weighted average price (TDWAP) fields needed for cross-provider
/// aggregation. No accumulator logic - forwarders aggregate raw ticks locally
/// via `TickAccumulator` before sending to the sink.
///
/// All time-dependent operations accept an explicit `now: Instant`, so the
/// same struct is used by the live path (which passes `Instant::now()`) and
/// by backtest/replay consumers (which advance a simulated clock anchored at
/// the first observation). Wall-clock-convenience wrappers (`new`, `update`,
/// `inject`, `effective_weight`) call `Instant::now()` internally.
#[derive(Debug, Clone)]
pub struct ProviderEntry {
    /// Latest per-provider aggregate (MITCH canonical type).
    pub index: Index,
    /// Volume-normalized weight from ticker-params.json (1.0 = median exchange).
    pub base_weight: f64,
    /// Last update time (for time-decay calculation).
    pub last_update: Instant,
    /// EMA of inter-arrival time in seconds (alpha = 0.1).
    /// Initialized to 5 s - crypto adapts down quickly, FX stays near actual cadence.
    pub ema_ipi_secs: f64,
}

impl ProviderEntry {
    /// Create from an Index (received from a forwarder or first flush).
    #[inline]
    pub fn new(index: Index, base_weight: f64) -> Self {
        Self::new_at(index, base_weight, Instant::now())
    }

    /// Create from an Index anchored at an explicit time (for simulated clocks).
    pub fn new_at(index: Index, base_weight: f64, now: Instant) -> Self {
        Self {
            index,
            base_weight,
            last_update: now,
            ema_ipi_secs: 5.0,
        }
    }

    /// Replace the stored Index and update timing.
    #[inline]
    pub fn update(&mut self, index: Index) {
        self.update_at(index, Instant::now());
    }

    /// Replace the stored Index and update timing against an explicit clock.
    pub fn update_at(&mut self, index: Index, now: Instant) {
        let ipi = now.saturating_duration_since(self.last_update)
            .as_secs_f64()
            .clamp(1e-6, 300.0);
        self.ema_ipi_secs = IPI_ALPHA * ipi + (1.0 - IPI_ALPHA) * self.ema_ipi_secs;
        self.last_update = now;
        self.index = index;
    }

    /// Directly set prices for injection/triangulation.
    /// Bypasses full Index replacement since injections produce one quote per cycle.
    #[inline]
    pub fn inject(&mut self, bid: f64, ask: f64, vbid: u32, vask: u32) {
        self.inject_at(bid, ask, vbid, vask, Instant::now());
    }

    /// Injection variant with an explicit clock.
    pub fn inject_at(&mut self, bid: f64, ask: f64, vbid: u32, vask: u32, now: Instant) {
        let ipi = now.saturating_duration_since(self.last_update)
            .as_secs_f64()
            .clamp(1e-6, 300.0);
        self.ema_ipi_secs = IPI_ALPHA * ipi + (1.0 - IPI_ALPHA) * self.ema_ipi_secs;
        self.last_update = now;
        self.index.bid = bid;
        self.index.ask = ask;
        self.index.vbid = vbid;
        self.index.vask = vask;
    }

    /// Adaptive exponential decay.
    ///
    /// Half-life = clamp(IPI_K x ema_ipi_secs, 1 s, stale_threshold/2).
    ///
    /// Behaviour:
    ///   - Crypto at 100 ms cadence:  ema -> 0.1 s -> half-life ~ 0.3 s (tight)
    ///   - FX at 10 s cadence:        ema -> 10 s  -> half-life ~ 30 s (lenient)
    ///   - Cold start (ema = 5 s):    half-life = 15 s (safe for both)
    #[inline]
    pub fn effective_weight(&self, stale_threshold_secs: f64) -> f64 {
        self.effective_weight_at(stale_threshold_secs, Instant::now())
    }

    /// Effective-weight variant with an explicit clock.
    pub fn effective_weight_at(&self, stale_threshold_secs: f64, now: Instant) -> f64 {
        let age = now.saturating_duration_since(self.last_update).as_secs_f64();
        let half_life = (IPI_K * self.ema_ipi_secs).clamp(1.0, stale_threshold_secs / 2.0);
        let decay = (-age * std::f64::consts::LN_2 / half_life).exp();
        self.base_weight * decay.max(0.001)
    }
}

/// Compute time-decay VWAP and confidence interval across providers for one ticker.
/// Returns None when no entry has a non-negligible effective weight.
///
/// ## Confidence interval methodology
///
/// Two independent uncertainty sources are combined in quadrature:
///
/// 1. **Inter-provider disagreement** (sigma_disagree):
///    Weighted standard deviation of provider mid-prices from the VWAP mid.
///    Computed in a single pass via Welford's algorithm (algebraically
///    identical to deviation-around-vwap_mid because weighted-mean-of-mids
///    equals (TDWAP_bid + TDWAP_ask) / 2).
///
/// 2. **Staleness widening** (sigma_stale):
///    Each provider's quote grows uncertain as time passes with no update.
///    Modelled as a random-walk: uncertainty grows as `(ask-bid)/2 x sqrt(age / ema_ipi)`.
///
/// Combined: `conf_interval = sqrt(sigma_disagree^2 + sigma_stale^2)`
///
/// Floor: `max(conf_interval, (ask-bid)/2)` - never tighter than the spread itself.
#[inline]
pub fn compute_vwap<'a, I>(
    ticker_id: u64,
    entries: I,
    stale_threshold_secs: f64,
) -> Option<Index>
where
    I: IntoIterator<Item = &'a ProviderEntry>,
{
    compute_vwap_at(ticker_id, entries, stale_threshold_secs, Instant::now())
}

/// Variant of [`compute_vwap`] that uses an explicit clock instead of
/// `Instant::now()`. Live code should prefer `compute_vwap`; replay/backtest
/// consumers (e.g. series-factory) pass a simulated `Instant` anchored at
/// the first tick so decay is computed against data time, not wall-clock.
///
/// Accepts any iterator over `&ProviderEntry` so callers can pass a slice, a
/// `HashMap::values()`, or a `SmallVec` without cloning.
pub fn compute_vwap_at<'a, I>(
    ticker_id: u64,
    entries: I,
    stale_threshold_secs: f64,
    now: Instant,
) -> Option<Index>
where
    I: IntoIterator<Item = &'a ProviderEntry>,
{
    let mut w_bid_sum = 0.0f64;
    let mut w_ask_sum = 0.0f64;
    let mut w_sum = 0.0f64;
    let mut total_bid_vol: u64 = 0;
    let mut total_ask_vol: u64 = 0;
    let mut accepted: u8 = 0;
    let mut rejected: u8 = 0;
    // Count of providers with non-floored decay ≥ 0.1 — the schema-defined
    // "active provider count" per `mitch::Index::confidence` (must be ≤ accepted).
    let mut active_count: u32 = 0;

    // Welford-style weighted variance accumulator for sigma_disagree.
    // The weighted mean of per-provider mids equals (TDWAP_bid + TDWAP_ask) / 2,
    // so m2 / w_sum is identical to the original two-pass formulation's
    // `w_sq_dev_sum / w_sum`.
    let mut mean_mid = 0.0f64;
    let mut m2 = 0.0f64;

    let mut w_stale_sq_sum = 0.0f64;

    for entry in entries {
        if !is_valid_tick(entry.index.bid, entry.index.ask) {
            rejected = rejected.saturating_add(1);
            continue;
        }

        // Inline effective-weight computation so `exp` and the age read happen once.
        let age = now.saturating_duration_since(entry.last_update).as_secs_f64();
        let half_life = (IPI_K * entry.ema_ipi_secs).clamp(1.0, stale_threshold_secs / 2.0);
        let decay = (-age * std::f64::consts::LN_2 / half_life).exp();
        let decay_floored = decay.max(0.001);
        let w = entry.base_weight * decay_floored;

        // Active provider: any with non-floored decay >= 10 percent, regardless
        // of whether its weight contributes to TDWAP this cycle. Count, not sum
        // of base_weights — `Index::confidence` is documented as "active provider
        // count" and must satisfy `confidence ≤ accepted` (see `Index::validate`).
        if decay >= 0.1 {
            active_count = active_count.saturating_add(1);
        }

        if w <= 1e-9 {
            continue;
        }

        let bid = entry.index.bid;
        let ask = entry.index.ask;
        let mid = (bid + ask) * 0.5;
        let half_spread = (ask - bid) * 0.5;

        w_bid_sum += bid * w;
        w_ask_sum += ask * w;

        let w_new = w_sum + w;
        let delta = mid - mean_mid;
        mean_mid += (w / w_new) * delta;
        m2 += w * delta * (mid - mean_mid);
        w_sum = w_new;

        let ipi = entry.ema_ipi_secs.max(1e-6);
        let stale_unc = half_spread * (age / ipi).sqrt();
        w_stale_sq_sum += w * stale_unc * stale_unc;

        total_bid_vol += entry.index.vbid as u64;
        total_ask_vol += entry.index.vask as u64;
        accepted = accepted.saturating_add(1);
    }

    if w_sum < 1e-12 {
        return None;
    }

    let tdwap_bid = w_bid_sum / w_sum;
    let tdwap_ask = w_ask_sum / w_sum;
    let vwap_mid = (tdwap_bid + tdwap_ask) * 0.5;

    let sigma_disagree_sq = m2 / w_sum;
    let sigma_stale_sq = w_stale_sq_sum / w_sum;
    let raw_ci = (sigma_disagree_sq + sigma_stale_sq).sqrt();
    let half_spread_agg = (tdwap_ask - tdwap_bid).abs() * 0.5;
    let conf_interval = raw_ci.max(half_spread_agg);

    // `confidence` is the active provider count, clamped to u8. `accepted` is a
    // u8 incremented only when a provider also contributes weight to the TDWAP
    // this cycle, so `confidence >= accepted` is possible at the unclamped level
    // (an active-but-floored provider counts toward active but not accepted).
    // We must therefore clamp to `accepted` to preserve the schema invariant
    // enforced by `Index::validate`.
    let confidence = (active_count.min(255) as u8).min(accepted);

    // Guard against crossed market: weighted ask must not be tighter than weighted bid.
    // If crossed, collapse to mid with zero spread (bid == ask == mid).
    let (final_bid, final_ask) = if tdwap_ask < tdwap_bid {
        (vwap_mid, vwap_mid)
    } else {
        (tdwap_bid, tdwap_ask)
    };

    let ci = if vwap_mid > 0.0 {
        encode_ci_ubp((conf_interval / vwap_mid) * 1e8)
    } else {
        0u16
    };

    // Regression guard: confidence must never exceed accepted (Index::validate
    // rejects otherwise — see the 100%-error ZEC-USDT backfill incident).
    debug_assert!(
        confidence <= accepted,
        "TDWAP confidence ({confidence}) > accepted ({accepted}); active_count={active_count}"
    );

    Some(Index {
        ticker: ticker_id,
        bid: final_bid,
        ask: final_ask,
        vbid: total_bid_vol.min(u32::MAX as u64) as u32,
        vask: total_ask_vol.min(u32::MAX as u64) as u32,
        ci,
        tick_count: accepted as u16,
        confidence,
        accepted,
        rejected,
        flags: 0,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ci_roundtrip_preserves_order_of_magnitude() {
        // Round-trip through encode/decode for a spread of plausible CI values.
        // Tolerance is loose: quantization by the sqrt-then-u16 cast introduces
        // up to ~(2 * sqrt(x) / CI_SCALE + 1 / CI_SCALE^2) absolute error in ubp.
        for &ci_ubp in &[0.0, 1.0, 10.0, 100.0, 1_000.0, 10_000.0, 100_000.0, 1_000_000.0] {
            let encoded = encode_ci_ubp(ci_ubp);
            let decoded = decode_ci_ubp(encoded);
            // Error bound for sqrt-u16 quantization
            let err_bound = 2.0 * ci_ubp.sqrt() / CI_SCALE + 1.0 / (CI_SCALE * CI_SCALE);
            assert!(
                (decoded - ci_ubp).abs() <= err_bound + 1e-9,
                "ci_ubp={ci_ubp} encoded={encoded} decoded={decoded} err_bound={err_bound}"
            );
        }
    }

    #[test]
    fn ci_does_not_saturate_at_10_percent_mid() {
        // 10% of mid = 1e7 ubp - must not saturate (prior linear encoding capped at 65535 ubp = 0.065%).
        let encoded = encode_ci_ubp(1e7);
        assert!(encoded < u16::MAX, "10% CI should not saturate, got {encoded}");
        let decoded = decode_ci_ubp(encoded);
        assert!((decoded - 1e7).abs() / 1e7 < 0.01, "decoded {decoded} differs from 1e7 by >1%");
    }

    #[test]
    fn ci_saturation_threshold_exceeds_old_limit() {
        // Old encoding saturated at 65535 ubp. New encoding must not saturate there.
        let encoded = encode_ci_ubp(65535.0);
        assert!(encoded < u16::MAX, "new encoding must not saturate at old-linear-max, got {encoded}");
    }

    #[test]
    fn ci_zero_and_negative() {
        assert_eq!(encode_ci_ubp(0.0), 0);
        assert_eq!(encode_ci_ubp(-1.0), 0);
        assert_eq!(encode_ci_ubp(f64::NAN), 0);
        assert_eq!(decode_ci_ubp(0), 0.0);
    }
}
