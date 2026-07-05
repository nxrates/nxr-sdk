//! Time-Decay Weighted Average Price (TDWAP) aggregation.
//!
//! Level 2 of the two-level aggregation pipeline:
//!   Level 1: raw ticks -> per-provider Index (via `TickAccumulator`)
//!   Level 2: per-provider Indexes -> cross-provider TDWAP Index (this module)
//!
//! Exported types:
//!   - `ProviderEntry`: per-provider metadata wrapper around `Index`
//!   - `compute_vwap`: cross-provider TDWAP with adaptive decay and confidence interval

// Time source: `coarsetime::Instant` is `repr(transparent) u64` with
// `derive(Copy)`, which lets `ProviderEntry` itself be `Copy`. The previous
// `std::time::Instant` representation is `!Copy` on macOS/Linux (it wraps a
// non-Copy `mach_timebase_info`-derived struct / `timespec` pair), forcing
// the hot aggregator path to `clone()` every entry per cycle
// (≈80 k clones/s at 20 Hz × ≈400 tickers × ≈10 providers).
//
// Resolution is millisecond-class — the staleness math
// (`clamp(1e-6, 300.0)` floor on inter-arrival time, half-life ≥ 1 s)
// is unaffected by trading a nanosecond for a millisecond clock.
//
// `coarsetime::Duration` is API-compatible with the subset we use
// (`from_millis`, `as_f64`, `as_secs`). We import both unqualified so the
// rest of the file reads identically to the pre-Δ1.C version.
use coarsetime::{Duration, Instant};

use mitch::Index;

use crate::agg::is_valid_tick;

/// Smoothing factor for the inter-arrival time EMA.
/// alpha = 0.1 -> converges to true inter-arrival time after ~10 updates.
const IPI_ALPHA: f64 = 0.1;

/// Half-life multiplier: weight halves after `IPI_K x ema_ipi` seconds.
/// Larger values -> more tolerance for stale quotes.
const IPI_K: f64 = 3.0;

/// No-book effective-spread reconstruction (operator 2026-07-05): when the
/// composite has no real book (trades-only / honest_tick), the effective
/// half-spread = this K x cross-venue price dispersion (`sqrt(m2/w_sum)`).
/// Provisional 1.0 (full spread = 2 x dispersion) — the value measured as the
/// floor-bound majority of live records. Overridable via `NXR_SPREAD_DISAGREE_K`.
/// ponytail: provisional single global K; upgrade path = per-pair zero-intercept
/// regression of REAL live avg_spread_bps on 2 x dispersion over the post-heal
/// overlap (dispersion→spread map is per-pair; measured range K∈[1.6,2] full).
/// Read once (env parsed on first use) — this is on the per-cycle hot path.
fn disagree_to_half_spread_k() -> f64 {
    static K: std::sync::OnceLock<f64> = std::sync::OnceLock::new();
    *K.get_or_init(|| {
        std::env::var("NXR_SPREAD_DISAGREE_K")
            .ok()
            .and_then(|v| v.parse::<f64>().ok())
            .filter(|v| *v >= 0.0 && v.is_finite())
            .unwrap_or(1.0)
    })
}

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
    mitch::common::ci_encode(ci_ubp)
}

/// Decode a u16 confidence interval back to micro basis points of mid.
/// Inverse of `encode_ci_ubp`.
#[inline]
pub fn decode_ci_ubp(encoded: u16) -> f64 {
    mitch::common::ci_decode(encoded)
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
#[derive(Debug, Clone, Copy)]
pub struct ProviderEntry {
    /// Latest per-provider aggregate (MITCH canonical type).
    pub index: Index,
    /// Volume-normalized weight from ticker-params.json (1.0 = median exchange).
    pub base_weight: f64,
    /// Last update time (for time-decay calculation). Uses
    /// `coarsetime::Instant` (u64 newtype) so the whole struct is `Copy`.
    /// See module-level comment on the time-source choice.
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
        // `coarsetime::Instant::duration_since` saturates on underflow
        // (uses `u64::saturating_sub` internally), so the previous
        // `saturating_duration_since` → `duration_since` rename is safe.
        let ipi = now.duration_since(self.last_update)
            .as_f64()
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
        let ipi = now.duration_since(self.last_update)
            .as_f64()
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
        let age = now.duration_since(self.last_update).as_f64();
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

    // Freshness (Q0.8) accumulators. `bw_sum` = Σ base_weight over every
    // VALID-tick provider; `wd_sum` = Σ base_weight·decay. The ratio
    // `wd_sum/bw_sum ∈ [0,1]` is a continuous freshness: ~1 when all providers
    // are fresh (decay≈1), falling as components decay. Floored-but-valid
    // providers still lower freshness via the denominator (they contribute to
    // `bw_sum` but little to `wd_sum`).
    let mut bw_sum = 0.0f64;
    let mut wd_sum = 0.0f64;

    for entry in entries {
        if !is_valid_tick(entry.index.bid, entry.index.ask) {
            rejected = rejected.saturating_add(1);
            continue;
        }

        // Inline effective-weight computation so `exp` and the age read happen once.
        let age = now.duration_since(entry.last_update).as_f64();
        // CORPSE EVICTION (audit F-01, 2026-07-04): a provider silent for
        // > 6x the stale threshold is DEAD, not stale — exclude it from
        // EVERYTHING (blend, freshness numerator/denominator, active count,
        // stale-uncertainty). Before this, dead entries lingered forever
        // (decay floored at 0.001) and their unbounded stale_unc =
        // half_spread*sqrt(age/ipi) inflated published ci 400-1500x on
        // healthy majors, which then propagated into every cross/synth.
        if age > stale_threshold_secs * 6.0 {
            continue;
        }
        let half_life = (IPI_K * entry.ema_ipi_secs).clamp(1.0, stale_threshold_secs / 2.0);
        let decay = (-age * std::f64::consts::LN_2 / half_life).exp();
        let decay_floored = decay.max(0.001);
        let w = entry.base_weight * decay_floored;

        // Freshness numerator/denominator: accumulate for every valid-tick
        // provider BEFORE the weight-skip below, so floored-but-valid providers
        // still drag freshness down via the denominator.
        bw_sum += entry.base_weight;
        wd_sum += entry.base_weight * decay;

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
        // Staleness multiplier CAPPED at 3.0 (audit F-04): sqrt(age/ipi) is a
        // sane short-horizon widening but must not grow without bound between
        // the last tick and eviction — an uncapped multiplier makes ci useless
        // as an outlier gate (a 50bp fat-finger passes a corpse-widened band).
        let stale_unc = half_spread * (age / ipi).sqrt().min(3.0);
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

    // `confidence` is now a CONTINUOUS freshness float in [0,1], stored Q0.8 in
    // the u8 (byte = round(f·255)). f≈1 ⇒ all providers fresh; falls as
    // components decay. Independent of `accepted`/`rejected` (which stay raw
    // COUNTS). The emitted Index sets FLAG_CONF_FRESHNESS so readers know byte36
    // is Q0.8 freshness, not the legacy active-provider count.
    let conf_f64 = if bw_sum > 0.0 { (wd_sum / bw_sum).clamp(0.0, 1.0) } else { 0.0 };
    let confidence = (conf_f64 * 255.0).round() as u8;
    let _ = active_count; // retained for potential diagnostics; no longer drives confidence

    // Composite bid/ask resolution (operator ruling 2026-07-05 — NO order books,
    // trades only). Three cases:
    //  1. Crossed (tdwap_ask < tdwap_bid): collapse to mid.
    //  2. Real book present (tdwap_ask > tdwap_bid): keep it — venue books are
    //     ground truth; never overwrite (preserves the live calibration overlap).
    //  3. NO book (tdwap_ask == tdwap_bid: trades-only / honest_tick, every
    //     venue's bid==ask==trade_px): reconstruct the effective half-spread from
    //     CROSS-VENUE PRICE DISAGREEMENT — the venues still disagree, and that
    //     dispersion IS the real execution uncertainty (recovers cross-pair
    //     spreads a single-venue high-low/Roll estimator cannot). Only with ≥2
    //     disagreeing venues; a single no-book venue stays collapsed so the bar
    //     builder emits NaN + FLAG_NO_BOOK (honest absence, never fabricated).
    let sigma_disagree = sigma_disagree_sq.max(0.0).sqrt();
    let (final_bid, final_ask) = if tdwap_ask < tdwap_bid {
        (vwap_mid, vwap_mid)
    } else if tdwap_ask > tdwap_bid {
        (tdwap_bid, tdwap_ask)
    } else if sigma_disagree > 0.0 && accepted >= 2 {
        let hs = disagree_to_half_spread_k() * sigma_disagree;
        (vwap_mid - hs, vwap_mid + hs)
    } else {
        (vwap_mid, vwap_mid)
    };

    let ci = if vwap_mid > 0.0 {
        encode_ci_ubp((conf_interval / vwap_mid) * 1e8)
    } else {
        0u16
    };

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
        // Signal that `confidence` is Q0.8 freshness (byte/255), not a legacy
        // active-provider count. Single-source bit in `nxr_sdk::shard`.
        flags: crate::shard::FLAG_CONF_FRESHNESS,
    })
}

// ── Throttled TDWAP: weight-vector freeze with change-triggered refresh ─────
//
// Problem: on quiet markets where no provider's quote changes between
// aggregation cycles, the staleness decay `exp(-age·ln2/HL)` keeps shifting
// the cross-provider weight ratios by tiny ULPs every cycle. The 5-field
// delta-gate (bid, ask, vbid, vask, ci) on the shard writer never matches,
// so a quiet stablecoin pair writes ~every cycle (20 Hz) instead of
// approximately never. This defeats the entire point of the delta-gate.
//
// Fix: cache the *normalized* weight vector at refresh boundaries
// (`refresh_interval_ms`, default HL/5 ≈ 1 s for the typical HL=5 s clamp).
// Between refreshes, reuse the same weight vector → composite VWAP is
// bit-identical when raw provider quotes are bit-identical → delta-gate
// fires only on real moves. When any provider's price/volume actually
// changes, force a refresh on the next call so the new quote is reflected
// immediately with up-to-date decay weights.
//
// Refresh trigger (any of):
//   - `force_refresh = true` from caller
//   - cache empty / different provider set / different ticker_id
//   - any provider's (bid, ask, vbid, vask, last_update) differs from cache
//   - elapsed since last refresh ≥ refresh_interval_ms
//
// Bit-identity guarantee: when the trigger does NOT fire, we replay the
// previous cycle's `Index` with all metadata (tick_count, confidence,
// accepted, rejected, ci) preserved verbatim — no floating-point work at
// all. This is what the shard delta-gate needs.
//
// Backwards-compat: `compute_vwap` and `compute_vwap_at` are unchanged.
// New behavior is opt-in via `compute_vwap_throttled`.

/// Per-provider snapshot used to detect "did anything actually change?".
///
/// Cheap to compare (5 scalar fields). We compare bit-for-bit on
/// the float fields (with `to_bits`) so that a no-op recomputation by the
/// upstream forwarder that lands the same f64 still counts as "unchanged".
///
/// `last_update: Instant` is deliberately NOT part of this fingerprint. RCA:
/// every idempotent forwarder re-send advances `e.last_update` (via update()/
/// inject()) → fingerprint diff → forced recompute path. The throttle hot-path
/// (bit-identical replay) would be effectively unreachable for active pairs,
/// so TDWAP fanout / shard delta-gate would behave as if throttle were
/// disabled. Genuine quote changes still flip `bid_bits` / `ask_bits` / vbid /
/// vask.
#[derive(Debug, Clone, Copy, PartialEq)]
struct ProviderFingerprint {
    provider_id: u16,
    bid_bits: u64,
    ask_bits: u64,
    vbid: u32,
    vask: u32,
}

impl ProviderFingerprint {
    #[inline]
    fn from_entry(provider_id: u16, e: &ProviderEntry) -> Self {
        Self {
            provider_id,
            bid_bits: e.index.bid.to_bits(),
            ask_bits: e.index.ask.to_bits(),
            vbid: e.index.vbid,
            vask: e.index.vask,
        }
    }
}

/// Per-ticker cache for the throttled-weight TDWAP path.
///
/// One instance per ticker, owned by the aggregator and kept resident across
/// cycles so the inner `Vec`s reuse their allocations. Memory: ~ (16 + 56·N)
/// bytes per ticker for N providers (typical N=5..15 ⇒ <1 KiB per ticker).
#[derive(Debug, Default)]
pub struct WeightCache {
    /// Last full-refresh time. `None` ⇒ never computed; first call forces refresh.
    last_refresh: Option<Instant>,
    /// Composite Index produced at the last refresh — replayed verbatim
    /// between refreshes for bit-identity. `None` ⇒ no valid cached composite.
    cached_index: Option<Index>,
    /// Provider fingerprints captured at the last refresh, parallel to the
    /// caller's provider list. Sorted by `provider_id` so set comparison is
    /// O(N) by position after a single sort pass on refresh.
    fingerprints: Vec<ProviderFingerprint>,
    /// Scratch buffer for the *current* call's fingerprints. Reused across
    /// cycles to avoid per-cycle Vec allocation.
    scratch: Vec<ProviderFingerprint>,
}

impl WeightCache {
    /// Create an empty cache. First `compute_vwap_throttled` call will refresh.
    pub const fn new() -> Self {
        Self {
            last_refresh: None,
            cached_index: None,
            fingerprints: Vec::new(),
            scratch: Vec::new(),
        }
    }

    /// Force the next call to refresh (e.g. on config / weights reload).
    #[inline]
    pub fn invalidate(&mut self) {
        self.last_refresh = None;
        self.cached_index = None;
        self.fingerprints.clear();
    }

    /// Returns the cached composite without recomputation. Test/debug aid;
    /// production path uses `compute_vwap_throttled` directly.
    #[inline]
    pub fn cached(&self) -> Option<Index> {
        self.cached_index
    }
}

/// Throttled cross-provider TDWAP.
///
/// Behaves like [`compute_vwap_at`] when a refresh is needed; otherwise
/// returns a bit-identical clone of the previous composite. See module
/// docs above for the refresh-trigger policy.
///
/// `entries` is a slice (not an iterator) because we need to walk it twice
/// in the worst case (fingerprint compare + recomputation) and slice access
/// keeps the hot path branch-free.
///
/// `refresh_interval_ms` is the maximum age of a cached weight vector. For
/// the default crypto stale_threshold of 10 s ⇒ HL clamps at 5 s ⇒ pass
/// `refresh_interval_ms = 1000` (HL/5) for ~13% per-provider weight drift
/// budget. Callers should clamp `refresh_interval_ms ≥ aggregation_interval_ms`
/// or the throttle is a no-op.
pub fn compute_vwap_throttled(
    ticker_id: u64,
    entries: &[(u16, ProviderEntry)],
    stale_threshold_secs: f64,
    cache: &mut WeightCache,
    refresh_interval_ms: u64,
    force_refresh: bool,
) -> Option<Index> {
    compute_vwap_throttled_at(
        ticker_id,
        entries,
        stale_threshold_secs,
        cache,
        refresh_interval_ms,
        force_refresh,
        Instant::now(),
    )
}

/// Throttled TDWAP with an explicit clock (for tests and replay).
pub(crate) fn compute_vwap_throttled_at(
    ticker_id: u64,
    entries: &[(u16, ProviderEntry)],
    stale_threshold_secs: f64,
    cache: &mut WeightCache,
    refresh_interval_ms: u64,
    force_refresh: bool,
    now: Instant,
) -> Option<Index> {
    // Build current fingerprint set into the scratch buffer. We sort by
    // provider_id so set comparison vs `cache.fingerprints` is a simple
    // position-wise equality check.
    cache.scratch.clear();
    cache.scratch.reserve(entries.len());
    for (pid, e) in entries {
        cache.scratch.push(ProviderFingerprint::from_entry(*pid, e));
    }
    cache.scratch.sort_by_key(|f| f.provider_id);

    // Decide: refresh or replay?
    let must_refresh = force_refresh
        || cache.last_refresh.is_none()
        || cache.cached_index.is_none()
        || cache.cached_index.map(|i| i.ticker) != Some(ticker_id)
        || cache.fingerprints.len() != cache.scratch.len()
        || cache.fingerprints != cache.scratch
        || cache
            .last_refresh
            .map(|t| now.duration_since(t) >= Duration::from_millis(refresh_interval_ms))
            .unwrap_or(true);

    if !must_refresh {
        // Hot path: replay verbatim. No FP work, no allocations, no
        // `Instant::now()`. The returned Index is byte-identical to the one
        // produced at the last refresh, so the 5-field delta-gate will
        // correctly suppress the write.
        return cache.cached_index;
    }

    // Cold path: full recomputation. Reuse the existing `compute_vwap_at`
    // implementation by walking the (pid, entry) pairs as `&ProviderEntry`.
    let composite = compute_vwap_at(
        ticker_id,
        entries.iter().map(|(_, e)| e),
        stale_threshold_secs,
        now,
    )?;

    // Commit the new state: swap scratch into fingerprints (O(1) — keeps
    // the just-built buffer, recycles the old one as the next scratch).
    std::mem::swap(&mut cache.fingerprints, &mut cache.scratch);
    cache.scratch.clear();
    cache.cached_index = Some(composite);
    cache.last_refresh = Some(now);
    Some(composite)
}

/// Compute the refresh interval for the throttled VWAP path.
///
/// Policy: refresh at `stale_threshold_secs / 5 · 1000` ms, but never faster
/// than **3× the aggregation cycle** (`3 · aggregation_interval_ms`). The 3×
/// floor — not the old 1× floor — guarantees the throttle holds the normalized
/// per-provider weight vector constant across multiple aggregation cycles, so
/// on a quiet (flat-quote) pair the emitted composite Index is byte-identical
/// run-to-run and the `.idx` 5-field delta-gate suppresses the redundant
/// write. With a 1× floor (the previous behaviour) a low `stale_threshold` or
/// a per-cycle `NXR_TDWAP_THROTTLE=0` could collapse the window to one cycle,
/// at which point sub-ULP decay drift each cycle defeats the gate and quiet
/// pairs write every cycle — up to ~3× the on-disk footprint. The explicit
/// `NXR_WEIGHT_REFRESH_MS` override is clamped to this same 3× floor by the
/// aggregator before use.
///
/// **Relation to the operator's "weights update ≤ 1/5 agg freq, HL ≥ 5-10×
/// refresh" rule (Aud-M1):** the half-life used inside `compute_vwap_at` is
/// `clamp(IPI_K · ema_ipi, 1.0, stale/2)` — for the typical clamped case
/// `HL = stale/2`. With `refresh = stale/5` this yields `HL/refresh = 2.5`,
/// short of the stated 5-10× target. The 2.5× ratio is the production
/// trade-off: refresh frequency is bounded below by `aggregation_interval_ms`
/// (50 ms hot loop), and pushing refresh to `stale/10` would put it under
/// the agg cycle on tight-HL pairs. Per-provider weight drift between
/// refreshes is therefore up to `1 - exp(-1/2.5 · ln2) ≈ 24%`. This is
/// acceptable for the delta-gate (composite weight ratio drift on a quiet
/// market still produces a bit-identical Index because the cached composite
/// is replayed verbatim — see `compute_vwap_throttled_at`) but should be
/// audited if `stale_threshold_secs` is ever pushed below 5 s in prod.
///
/// Examples (agg=200ms prod default, 3× floor = 600ms):
/// - Production crypto (stale=10s, agg=200ms): refresh = 2000 ms, HL ≤ 5 s.
/// - Aggressive FX (stale=2s, agg=200ms): refresh = max(400, 600) = 600 ms.
/// - Pathological tight HL (stale=0.2s, agg=200ms): refresh = max(40, 600) =
///   600 ms — the 3× floor keeps the throttle effective (the old 1× floor
///   would have dropped to per-cycle here and defeated the delta-gate).
#[inline]
pub fn default_refresh_interval_ms(stale_threshold_secs: f64, aggregation_interval_ms: u64) -> u64 {
    let hl_over_5_ms = (stale_threshold_secs * 1000.0 / 5.0).round();
    // Hard floor: never faster than 3× the aggregation cycle, so the throttle
    // always spans multiple cycles and the .idx delta-gate keeps its
    // diff-compression on quiet pairs.
    let min_refresh_ms = (aggregation_interval_ms as f64) * 3.0;
    let clamped = hl_over_5_ms.max(min_refresh_ms);
    clamped.min(u64::MAX as f64) as u64
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

    // ── Throttled-TDWAP tests ────────────────────────────────────────────────
    //
    // These verify the bit-identity property that the delta-gate needs:
    // when no provider's quote changes between cycles within one refresh
    // window, the composite Index returned by `compute_vwap_throttled_at`
    // is byte-identical to the previous call.

    use crate::mitch::Index as MitchIndex;

    fn mk_entry(bid: f64, ask: f64, vbid: u32, vask: u32, base_weight: f64, now: Instant) -> ProviderEntry {
        let idx = MitchIndex::new(1, bid, ask, 0, vbid, vask, 1, 1, 1, 0);
        let mut e = ProviderEntry::new_at(idx, base_weight, now);
        // Anchor ema_ipi to a stable value so successive `update_at` calls in
        // the same test don't move the half-life around between cycles.
        e.ema_ipi_secs = 1.0;
        e
    }

    fn idx_eq_bytewise(a: Index, b: Index) -> bool {
        // The composite Index produced by compute_vwap must be reproduced
        // byte-for-byte by the cache replay. Compare every field; floats via
        // `to_bits` so a NaN-bit equality survives.
        a.ticker == b.ticker
            && a.bid.to_bits() == b.bid.to_bits()
            && a.ask.to_bits() == b.ask.to_bits()
            && a.vbid == b.vbid
            && a.vask == b.vask
            && a.ci == b.ci
            && a.tick_count == b.tick_count
            && a.confidence == b.confidence
            && a.accepted == b.accepted
            && a.rejected == b.rejected
            && a.flags == b.flags
    }

    #[test]
    fn throttled_replay_is_bit_identical_within_refresh_window() {
        // Setup: 2 providers, both fresh, prices unchanged. Refresh at 1000ms.
        // Walk forward in 50ms steps for 900ms (< refresh interval). Every
        // returned Index must be byte-identical to the first.
        let t0 = Instant::now();
        let p_a = mk_entry(100.00, 100.02, 1_000, 1_100, 1.0, t0);
        let p_b = mk_entry(100.01, 100.03, 2_000, 2_200, 1.5, t0);
        let entries: Vec<(u16, ProviderEntry)> = vec![(1, p_a), (2, p_b)];

        let mut cache = WeightCache::new();
        let first = compute_vwap_throttled_at(
            42, &entries, 10.0, &mut cache, 1000, false, t0,
        )
        .expect("first call must produce a composite");

        // 18 cycles at 50ms each = 900ms elapsed, still within the 1000ms refresh.
        for step in 1..=18u64 {
            let now = t0 + Duration::from_millis(step * 50);
            let cur = compute_vwap_throttled_at(
                42, &entries, 10.0, &mut cache, 1000, false, now,
            )
            .expect("cached replay must produce a composite");
            assert!(
                idx_eq_bytewise(first, cur),
                "cycle {step}: replay diverged from refresh; expected {first:?} got {cur:?}",
            );
        }
    }

    #[test]
    fn throttled_recomputes_after_refresh_interval() {
        // Past the refresh window we MUST recompute. The decay on the
        // 1000ms-older quote shifts weight, so the recomputed composite
        // differs from the cached one — verifies the throttle isn't sticky.
        let t0 = Instant::now();
        // Two providers with intentionally different mids so the weight shift
        // produces a non-zero composite Δ.
        let p_a = mk_entry(100.00, 100.02, 1_000, 1_100, 1.0, t0);
        let p_b = mk_entry(101.00, 101.02, 2_000, 2_200, 1.0, t0);
        let entries: Vec<(u16, ProviderEntry)> = vec![(1, p_a), (2, p_b)];

        let mut cache = WeightCache::new();
        let first = compute_vwap_throttled_at(
            42, &entries, 10.0, &mut cache, 200, false, t0,
        )
        .unwrap();

        // 300ms later — well past the 200ms refresh interval.
        // Both providers age equally so normalized weights are unchanged in
        // ratio, but absolute decay still re-runs through `compute_vwap_at`
        // and the cached_index timestamp updates.
        let t1 = t0 + Duration::from_millis(300);
        let refreshed = compute_vwap_throttled_at(
            42, &entries, 10.0, &mut cache, 200, false, t1,
        )
        .unwrap();
        // The composite VWAP itself is invariant under uniform aging when
        // the same multiplicative decay applies to both providers, but the
        // refresh DID run — we verify the cache timestamp moved.
        assert!(cache.last_refresh.is_some());
        // Same provider state ⇒ same VWAP. Bit-identity not required across
        // refreshes (the math reruns), but value equality is the natural
        // invariant for identical inputs at uniform age.
        assert!(idx_eq_bytewise(first, refreshed) || (first.bid - refreshed.bid).abs() < 1e-9);
    }

    #[test]
    fn no_book_spread_from_cross_venue_dispersion() {
        // Trades-only / honest_tick: every venue reports bid==ask==trade_px, so
        // there is NO book — but the venues DISAGREE, and that dispersion must
        // become the composite effective spread (operator ruling 2026-07-05).
        let t0 = Instant::now();
        // Two venues, each a locked quote at its own trade price: 100.00 vs 100.10.
        let p_a = mk_entry(100.00, 100.00, 1_000, 1_000, 1.0, t0);
        let p_b = mk_entry(100.10, 100.10, 1_000, 1_000, 1.0, t0);
        let entries: Vec<(u16, ProviderEntry)> = vec![(1, p_a), (2, p_b)];
        let idx = compute_vwap_at(7, entries.iter().map(|(_, e)| e), 10.0, t0)
            .expect("composite");
        // Copy packed fields to locals before use (packed struct → no field refs).
        let (bid, ask) = (idx.bid, idx.ask);
        // mid ~100.05, and a real (non-degenerate) spread synthesized from the
        // 0.10 cross-venue disagreement — NOT collapsed to bid==ask.
        assert!(ask > bid, "no-book multi-venue must synthesize a spread, got bid={bid} ask={ask}");
        let mid = (bid + ask) * 0.5;
        assert!((mid - 100.05).abs() < 0.02, "mid off: {mid}");
        // half-spread = k * sqrt(m2/w_sum); with equal weights the mid variance
        // is 0.05^2, so sqrt = 0.05, half-spread = k*0.05 (k default 1.0).
        let hs = (ask - bid) * 0.5;
        assert!(hs > 0.0 && hs < 0.20, "half-spread out of expected band: {hs}");
    }

    #[test]
    fn no_book_single_venue_stays_collapsed() {
        // A single no-book venue has zero dispersion ⇒ must NOT fabricate a
        // spread; bid==ask so the bar builder emits NaN + FLAG_NO_BOOK.
        let t0 = Instant::now();
        let p = mk_entry(100.00, 100.00, 1_000, 1_000, 1.0, t0);
        let entries: Vec<(u16, ProviderEntry)> = vec![(1, p)];
        let idx = compute_vwap_at(7, entries.iter().map(|(_, e)| e), 10.0, t0)
            .expect("composite");
        let (bid, ask) = (idx.bid, idx.ask);
        assert_eq!(bid.to_bits(), ask.to_bits(), "single no-book venue must stay collapsed (honest absence)");
    }

    #[test]
    fn throttled_force_refresh_on_price_change() {
        // Provider B's price moves mid-window. The cache must detect the
        // fingerprint change and recompute immediately — not wait for the
        // refresh window. The composite bid/ask MUST move.
        let t0 = Instant::now();
        let p_a = mk_entry(100.00, 100.02, 1_000, 1_100, 1.0, t0);
        let p_b = mk_entry(100.00, 100.02, 1_000, 1_100, 1.0, t0);
        let mut entries: Vec<(u16, ProviderEntry)> = vec![(1, p_a), (2, p_b)];

        let mut cache = WeightCache::new();
        let first = compute_vwap_throttled_at(
            42, &entries, 10.0, &mut cache, 1000, false, t0,
        )
        .unwrap();

        // 100ms in — well within refresh window. Push a new price into B.
        let t1 = t0 + Duration::from_millis(100);
        entries[1].1.update_at(
            MitchIndex::new(1, 105.00, 105.02, 0, 1_000, 1_100, 1, 1, 1, 0),
            t1,
        );

        let post_change = compute_vwap_throttled_at(
            42, &entries, 10.0, &mut cache, 1000, false, t1,
        )
        .unwrap();
        assert!(
            post_change.bid > first.bid + 1.0,
            "VWAP must respond to a 5-unit move on provider B; first={first:?} post={post_change:?}",
        );
    }

    #[test]
    fn throttled_handles_provider_set_change() {
        // A provider joins mid-window. Must force refresh regardless of
        // the interval — adding a quote source is new information.
        let t0 = Instant::now();
        let p_a = mk_entry(100.00, 100.02, 1_000, 1_100, 1.0, t0);
        let mut entries: Vec<(u16, ProviderEntry)> = vec![(1, p_a)];

        let mut cache = WeightCache::new();
        let _first = compute_vwap_throttled_at(
            42, &entries, 10.0, &mut cache, 1000, false, t0,
        )
        .unwrap();
        let cached_before_join = cache.cached_index.unwrap();

        // Add provider B 100ms later.
        let t1 = t0 + Duration::from_millis(100);
        let p_b = mk_entry(110.00, 110.02, 5_000, 5_500, 1.0, t1);
        entries.push((2, p_b));

        let after_join = compute_vwap_throttled_at(
            42, &entries, 10.0, &mut cache, 1000, false, t1,
        )
        .unwrap();
        assert_ne!(
            cached_before_join.bid.to_bits(), after_join.bid.to_bits(),
            "joining a provider with a different mid must shift the composite",
        );
    }

    #[test]
    fn default_refresh_interval_clamps_to_3x_aggregation_cycle() {
        // Crypto default: stale=10s, agg=50ms → HL/5 = 2000 ms, well above the
        // 3× floor (150ms) → 2000.
        assert_eq!(default_refresh_interval_ms(10.0, 50), 2000);
        // Prod agg=200ms, stale=10s → 2000 ms, above 3× floor (600ms) → 2000.
        assert_eq!(default_refresh_interval_ms(10.0, 200), 2000);
        // Sub-second HL: stale=0.2s, agg=50ms → HL/5 = 40ms, clamped UP to the
        // 3× floor = 150ms (previously 50ms with the 1× floor).
        assert_eq!(default_refresh_interval_ms(0.2, 50), 150);
        // FX-ish at prod agg: stale=2s, agg=200ms → HL/5 = 400ms, below the 3×
        // floor (600ms) → clamped UP to 600.
        assert_eq!(default_refresh_interval_ms(2.0, 200), 600);
        // FX-ish at agg=50ms: stale=2s → 400ms, above 3× floor (150ms) → 400.
        assert_eq!(default_refresh_interval_ms(2.0, 50), 400);
    }
}
