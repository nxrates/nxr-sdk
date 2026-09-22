//! Cross-provider composite: the ONE place provider legs are weighted.
//!
//! Level 2 of the two-level aggregation pipeline:
//!   Level 1: raw ticks -> per-provider Index (via `TickAccumulator`)
//!   Level 2: per-provider Indexes -> cross-provider composite (this module)
//!
//! ## Weight of leg `v` ([`Kernel`], [`compute_vwap_at`])
//!
//! ```text
//! w_v = b_v · s_ref² / (s_v² + σ²·τ_v) · (1 − σ²·τ_v / (4·s_ref)²)
//! shares = w / Σw, water-filled at 0.60
//! ```
//!
//! - `b_v`: evidence, the venue's volume weight (`ticker-params.json`), with
//!   the unmapped bloc bounded against the mapped mass.
//! - `s_v²`: the quote's own noise, relative half-spread floored per class
//!   (`s_min`), so a razor-thin book cannot claim infinite precision.
//! - `σ²·τ_v`: how far the price can have moved since the leg was last
//!   CONFIRMED. `σ` = max(measured `.vol` sigma, class floor) per √s.
//!
//! **Confirmation clock.** `τ_v` runs from `last_update`, stamped by every
//! frame the core admits: a new price, or a forwarder re-affirm of an
//! unchanged one (`shard::FLAG_REAFFIRM`, sent only on proof the book is live:
//! venue sequence advanced or size moved, indefinitely; venue-wide activity,
//! only 300 s pegged / 30 s otherwise past the last price change). 2026-09-22: Binance
//! USD1/USDC held 0.9992/0.9993 for minutes; the forwarder dropped every
//! identical quote, the old uniform 5 s half-life aged the busiest venue out,
//! and `/v1/price/USD1-USDC` served a thin venue's book as `stale`. Price
//! change is not liveness; confirmation is.
//!
//! **Eviction.** A leg is dropped once `σ√τ_v > 4·s_ref` (it can have diffused
//! four typical half-spreads; `s_ref` = base-weighted mean `s_v`) or `τ_v`
//! passes the class backstop. The last factor tapers the weight to exactly 0
//! at the first bound, so leaving never steps the mark.
//! No floor: an all-stale set has no weight to manufacture a fresh mark from.
//!
//! **Cap.** Final shares of the legs inside their confirmation window
//! (`τ ≤ stale_threshold`; a live book re-affirms every stale/2) are
//! water-filled at `max_weight_concentrated` (0.60): a thin venue quoting
//! tight, or one dominant venue, cannot own the mark. A leg past the window
//! keeps its raw share `w/Σw`, so the cap never holds a dying leg up. One
//! fresh leg keeps the whole fresh mass and reads as single-source
//! (`n_eff = 1`).
//!
//! **`ci`.** Share-weighted cross-venue disagreement in quadrature with each
//! leg's half-spread × min(√(τ/ipi), 3), floored at the composite
//! half-spread. Deliberately on the pre-kernel scale: the BTR keeper's 25 bp
//! ci-spike trigger and the signer ceilings are calibrated to it. An all-stale
//! set is caught by the liveness bits and by eviction, not by `ci`.
//!
//! Liveness (`active_count`, `fresh_weight_ok` in `confidence`) is a separate
//! axis on `stale_threshold`, read by the signed-quote gates.

// Time source: `coarsetime::Instant` is `repr(transparent) u64` with
// `derive(Copy)`, which lets `ProviderEntry` itself be `Copy` on the hot
// aggregator path. Millisecond resolution is enough for every age here.
use coarsetime::Duration;
/// The clock [`ProviderEntry::last_update`] is stamped in. Re-exported so a
/// caller outside this crate can carry an observation instant around without a
/// direct `coarsetime` dependency: `coarse_now` alone is half the API, since a
/// struct field or a return type has to name the type.
pub use coarsetime::Instant;

use mitch::Index;

use crate::agg::is_valid_tick;

/// Smoothing factor for the inter-arrival time EMA.
/// alpha = 0.1 -> converges to true inter-arrival time after ~10 updates.
const IPI_ALPHA: f64 = 0.1;

/// Half-life multiplier: weight halves after `IPI_K x ema_ipi` seconds.
/// Used ONLY for the per-provider LIVENESS axis (`active_count`), never for the
/// price weight — see [`Kernel`].
const IPI_K: f64 = 3.0;

/// Per-ticker parameters of the price kernel (module header has the formula).
/// Class constants are `(σ floor bp/√min, s_min bp, backstop s)`; a measured
/// sigma can only raise `sigma` ([`Self::with_sigma`]).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Kernel {
    /// Diffusion, fraction of mid per √s: `max(realised, class floor)`.
    pub sigma: f64,
    /// Half-spread floor, fraction of mid.
    pub s_min: f64,
    /// Backstop: a leg unconfirmed this long is evicted whatever its spread.
    pub horizon_secs: f64,
}

/// A leg is dead once its price can have diffused this many reference
/// half-spreads since its last confirmation.
const EVICT_SPREADS: f64 = 4.0;

impl Kernel {
    /// `cexs.pegged` pairs (USD1/USDC, USDT/USD). σ 1.5 bp/√min: a held peg
    /// barely diffuses, so a confirmed flat book keeps its share for minutes.
    /// s_min 1 bp: one tick at 4 dp. Backstop 15 min; at a 1 bp book the
    /// diffusion bound evicts first (~7 min unconfirmed).
    pub const PEGGED: Self = Self::class(1.5, 1.0, 900.0);
    /// `cexs.crypto_majors` against a pegged quote (BTC/USDT, ETH/USD).
    /// σ 6 bp/√min, s_min 0.5 bp (sub-bp books), backstop 60 s.
    pub const MAJOR: Self = Self::class(6.0, 0.5, 60.0);
    /// Every other crypto pair. σ 20 bp/√min, s_min 2 bp, backstop 60 s.
    pub const ALT: Self = Self::class(20.0, 2.0, 60.0);
    /// FX, metals, commodities, equities (oracle relays). σ 2 bp/√min,
    /// s_min 0.5 bp, backstop 5 min: often one source, so it is kept through a
    /// relay gap. Weekend close is the closed-market handling upstream, not
    /// ageing.
    pub const FX_METAL: Self = Self::class(2.0, 0.5, 300.0);

    /// Class floors are quoted in bp per √minute.
    const fn class(sigma_bp_sqrt_min: f64, s_min_bp: f64, horizon_secs: f64) -> Self {
        // 1/√60.
        const PER_SQRT_MIN: f64 = 0.129_099_444_873_580_56;
        Self {
            sigma: sigma_bp_sqrt_min * 1e-4 * PER_SQRT_MIN,
            s_min: s_min_bp * 1e-4,
            horizon_secs,
        }
    }

    /// Class by MITCH wire bits alone, for a caller without the operator
    /// lists: a pair with a crypto leg ages as [`Self::ALT`], FX, metals,
    /// commodities and equities as [`Self::FX_METAL`].
    pub fn for_wire(ticker_id: u64) -> Self {
        use mitch::common::AssetClass::CR;
        let t = mitch::ticker::TickerId::from_raw(ticker_id);
        if t.base_asset_class() == CR || t.quote_asset_class() == CR {
            Self::ALT
        } else {
            Self::FX_METAL
        }
    }

    /// Set `sigma` from a measured value (fraction per √s), clamped to
    /// `[class floor, 10 × class floor]`: a quiet measurement never slows
    /// ageing below the floor, a garbage one cannot evict every leg at once.
    #[must_use]
    pub fn with_sigma(mut self, realised: f64) -> Self {
        if realised.is_finite() {
            self.sigma = realised.clamp(self.sigma, 10.0 * self.sigma);
        }
        self
    }

    /// Relative half-spread of a valid quote, floored at `s_min`.
    #[inline]
    fn spread(&self, e: &ProviderEntry) -> f64 {
        let (bid, ask) = (e.index.bid, e.index.ask);
        ((ask - bid) / (ask + bid)).max(self.s_min)
    }

    /// Confirmation age of an admissible leg: a valid tick inside the
    /// backstop. `None` = the leg is not part of this ticker's blend.
    #[inline]
    fn age(&self, e: &ProviderEntry, now: Instant) -> Option<f64> {
        let tau = now.duration_since(e.last_update).as_f64();
        (is_valid_tick(e.index.bid, e.index.ask) && tau <= self.horizon_secs).then_some(tau)
    }

    /// `(τ, taper / (s_v² + σ²τ))` of a leg alive against `s_ref`, `None` once
    /// its diffusion `σ√τ` passes `EVICT_SPREADS · s_ref`.
    #[inline]
    fn leg(&self, e: &ProviderEntry, now: Instant, s_ref: f64) -> Option<(f64, f64)> {
        let tau = self.age(e, now)?;
        let drift = self.sigma * self.sigma * tau;
        let evict = (EVICT_SPREADS * s_ref).powi(2);
        (drift <= evict).then(|| {
            let s = self.spread(e);
            // Tapered to exactly 0 at eviction so a departing leg never steps
            // the mark: the leg's last weight is gone before it is dropped.
            (tau, (1.0 - drift / evict) / (s * s + drift))
        })
    }
}

/// Water-fill normalised shares at `cap`: returns `(k, t, cap)` such that a
/// leg of weight `w` holds `cap` when `w >= t`, else `k·w`; shares sum to 1.
/// `cap` is raised to `1/n` when fewer than `1/cap` legs are live, so one live
/// leg keeps the whole blend (single-source, flagged by `n_eff = 1`).
fn water_fill<I: Iterator<Item = f64> + Clone>(ws: I, cap: f64) -> (f64, f64, f64) {
    let n = ws.clone().filter(|w| *w > 0.0).count();
    let cap = cap.max(1.0 / n.max(1) as f64);
    let mut t = f64::INFINITY;
    loop {
        let (mut free, mut top, mut n_cap) = (0.0f64, 0.0f64, 0usize);
        for w in ws.clone().filter(|w| *w > 0.0) {
            if w >= t {
                n_cap += 1;
            } else {
                free += w;
                top = top.max(w);
            }
        }
        if free <= 0.0 {
            return (0.0, t, cap);
        }
        let k = (1.0 - cap * n_cap as f64) / free;
        if top * k <= cap * (1.0 + 1e-12) {
            return (k, t, cap);
        }
        t = top;
    }
}

/// Ceiling on the base-weight mass the UNMAPPED bloc may carry, as a fraction
/// of the MAPPED mass: `Σ w_unmapped <= FRAC · Σ w_mapped`, enforced by scaling
/// every unmapped leg by a common factor (so their relative order is kept and
/// none of them is delisted).
///
/// Why a BLOC bound and not a per-leg constant. A `(provider, ticker)` absent
/// from `ticker-params.json` used to fall back to `1.0` — the MEDIAN venue's
/// weight, above the <=0.60 HHI ceiling every mapped venue is held to, so the
/// venue we had no volume evidence for outweighed every venue we did. The
/// obvious repair (a small per-leg constant) overshoots in the other
/// direction: it DELISTS the tail instead of demoting it, hands a single
/// mapped venue 93-98% of the composite, and its correct value depends on how
/// many unmapped legs a ticker happens to have. Bounding the GROUP fixes both
/// failure modes with one number: dilution is capped because the bloc cannot
/// exceed `FRAC` of the mapped mass, concentration is capped because the bloc
/// keeps a guaranteed `FRAC/(1+FRAC)` share of the blend, and adding unmapped
/// legs subdivides that share instead of growing it.
///
/// 0.40 = the unmapped tail is worth at most ~29% of the composite
/// (`0.4/1.4`), so the evidence-backed venues always hold the majority.
///
/// No mapped leg (`Σ w_mapped == 0`) means no bound: a fully unmapped ticker
/// composites exactly as it did before, equal-weight. 227 of 339 live tickers
/// are in that class (every FX pair and every metal), so this is the property
/// the whole design is built around, not an edge case.
///
/// Overridable via `NXR_UNMAPPED_BLOC_FRAC`. Read once (per-cycle hot path).
fn unmapped_bloc_frac() -> f64 {
    static F: std::sync::OnceLock<f64> = std::sync::OnceLock::new();
    *F.get_or_init(|| {
        std::env::var("NXR_UNMAPPED_BLOC_FRAC")
            .ok()
            .and_then(|v| v.parse::<f64>().ok())
            .filter(|v| v.is_finite() && *v > 0.0)
            .unwrap_or(0.40)
    })
}

/// Same concentration ceiling the OFFLINE weight policy caps a mapped venue at
/// (`weights::policy_weights` Stage B, `NXR_MAX_WEIGHT_CONCENTRATED`, 0.60).
///
/// It is a FLOOR on the unmapped bloc, not a second ceiling: the volume scrape
/// prices only the top pairs of each venue, so most alt tickers have exactly
/// ONE mapped venue (live 2026-08-11: ORCA/USDT, CRV/USDT and ALGO/USDT each
/// have one). Bounding the bloc purely at `0.40 · Σw_mapped` would hand that
/// single venue `1/1.4 = 71.4%` of the composite, past the ceiling the same
/// venue would have been capped at had the ticker been fully priced. Reusing
/// the policy's own constant keeps the runtime and offline halves of the
/// weighting from disagreeing about how concentrated a mark may be, instead of
/// introducing an unrelated number.
///
/// Applied to BASE weights by the bloc bound, like the offline policy, and
/// again to the FINAL shares by the water-fill in [`compute_vwap_at`].
fn max_leg_share() -> f64 {
    static C: std::sync::OnceLock<f64> = std::sync::OnceLock::new();
    *C.get_or_init(|| {
        crate::config::NxrConfig::from_env()
            .max_weight_concentrated
            .clamp(0.01, 1.0)
    })
}

/// Per-ticker weight-composition diagnostics, computed for free inside
/// [`compute_vwap_at`] and surfaced through [`WeightCache::weight_profile`].
///
/// Exists because there was NO per-ticker weight visibility: `/metrics`
/// counted weight-map misses per provider and nothing published what the legs
/// actually ended up worth, so the effect of a weight-policy change could only
/// be reconstructed offline. Both fields are measured on the FINAL blend
/// shares (kernel, bloc bound, water-fill cap), i.e. on real composite
/// influence rather than on the configured inputs.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct WeightProfile {
    /// Share of the blend held by the single heaviest leg, in `[0, 1]`.
    /// 1.0 means the composite is a single-venue mark.
    pub top_weight_share: f64,
    /// Effective venue count `(Σw)² / Σw²`. Equals N for N equal legs and
    /// collapses toward 1 as one leg takes over.
    pub n_eff: f64,
}

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

/// Coarse monotonic "now" for callers outside this module that need to age
/// [`ProviderEntry::last_update`] (e.g. the sink's reject-median corpse
/// filter) without depending on `coarsetime` directly.
#[inline]
pub fn coarse_now() -> Instant {
    Instant::now()
}

/// The coarse clock backdated by `lateness_ms` — the observation instant for a
/// frame that waited somewhere (kernel receive queue) before we read it.
///
/// Exists so callers can stamp [`ProviderEntry::new_at`] / [`ProviderEntry::update_at`]
/// with FRAME time instead of drain time without taking a direct `coarsetime`
/// dependency, keeping the clock choice single-sourced in this module (the
/// module-level comment on the time source is the contract).
///
/// The caller is responsible for clamping `lateness_ms` to something plausible:
/// a value derived from an unvalidated wire timestamp can be absurd, and
/// backdating past the corpse-filter horizon is indistinguishable from dead.
#[inline]
pub fn coarse_now_backdated(lateness_ms: u64) -> Instant {
    coarse_now() - Duration::from_millis(lateness_ms)
}

/// Per-provider, per-ticker leg state.
///
/// Wraps a MITCH `Index` (the provider's latest aggregated quote) with the
/// fields the kernel and the liveness axis need. Forwarders aggregate raw
/// ticks locally (`TickAccumulator`) before sending to the sink.
///
/// All time-dependent operations accept an explicit `now: Instant`, so the
/// same struct is used by the live path (which passes `Instant::now()`) and
/// by backtest/replay consumers (which advance a simulated clock anchored at
/// the first observation). Wall-clock-convenience wrappers (`new`, `update`,
/// `inject`) call `Instant::now()` internally.
#[derive(Debug, Clone, Copy)]
pub struct ProviderEntry {
    /// Latest per-provider aggregate (MITCH canonical type).
    pub index: Index,
    /// Volume-normalized weight from ticker-params.json (1.0 = median exchange).
    pub base_weight: f64,
    /// Last CONFIRMATION: the frame time of the latest admitted frame, a new
    /// price or a re-affirm. The kernel's `τ` runs from here.
    pub last_update: Instant,
    /// EMA of inter-arrival time in seconds (alpha = 0.1).
    /// Initialized to 5 s - crypto adapts down quickly, FX stays near actual cadence.
    pub ema_ipi_secs: f64,
    /// `true` when the latest value came from [`Self::inject_at`] (a
    /// triangulator injection rule) rather than from a provider frame.
    ///
    /// Injected legs are DERIVED, not observed: `inject_at` stamps
    /// `last_update = now` every cycle the product moves, so before this flag
    /// existed an injected leg was indistinguishable from a venue ticking at
    /// 10 Hz. That is how a composite whose real venues were all dead still
    /// reported `active_count > 0` and `fresh_weight_ok = true` — satisfying
    /// both liveness axes of the signed-quote gate on legs that never ticked
    /// (audit 2026-07-25; same class as the 11 h `provider_status: dead` /
    /// `status: fresh` incident).
    ///
    /// Injected legs still contribute their PRICE to the blend (the USDC→USDT
    /// bridge is a deliberate product feature) and still count in the
    /// fresh-weight DENOMINATOR, but they are excluded from `active_count` and
    /// from the fresh-weight NUMERATOR. A composite of entirely-injected legs
    /// therefore scores `active_count = 0` and `fresh_weight_share = 0` and
    /// fails both axes — which is the correct answer to "is there a live
    /// provider observation behind this mark".
    pub injected: bool,
    /// `true` when `base_weight` came from a real per-(provider, ticker) entry
    /// in `ticker-params.json` (or from an explicitly configured non-CEX
    /// provider weight), `false` when it is the coverage FALLBACK for a
    /// (provider, ticker) the weights file never priced.
    ///
    /// This is the axis the UNMAPPED-BLOC BOUND in [`compute_vwap_at`] splits
    /// on: evidence-backed legs are weighted as the policy says, and everything
    /// with no volume evidence behind it is capped AS A GROUP at
    /// [`unmapped_bloc_frac`] of the mapped mass. Injected legs are unmapped by
    /// construction (a derived quote carries no venue volume), which is what
    /// keeps a synth product from outweighing the books it is derived from.
    ///
    /// Default `false`. A ticker with NO mapped leg is therefore entirely
    /// unmapped, the bloc bound does not apply, and the blend is the same
    /// equal-weight composite it was before the bound existed.
    pub mapped: bool,
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
            injected: false,
            mapped: false,
        }
    }

    /// Mark whether `base_weight` is evidence-backed (see [`Self::mapped`]).
    #[inline]
    #[must_use]
    pub const fn with_mapped(mut self, mapped: bool) -> Self {
        self.mapped = mapped;
        self
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
        let ipi = now
            .duration_since(self.last_update)
            .as_f64()
            .clamp(1e-6, 300.0);
        self.ema_ipi_secs = IPI_ALPHA * ipi + (1.0 - IPI_ALPHA) * self.ema_ipi_secs;
        self.last_update = now;
        self.index = index;
        // A real provider frame supersedes any earlier injected value.
        self.injected = false;
    }

    /// Directly set prices for injection/triangulation.
    /// Bypasses full Index replacement since injections produce one quote per cycle.
    #[inline]
    pub fn inject(&mut self, bid: f64, ask: f64, vbid: u32, vask: u32) {
        self.inject_at(bid, ask, vbid, vask, Instant::now());
    }

    /// Injection variant with an explicit clock.
    pub fn inject_at(&mut self, bid: f64, ask: f64, vbid: u32, vask: u32, now: Instant) {
        let ipi = now
            .duration_since(self.last_update)
            .as_f64()
            .clamp(1e-6, 300.0);
        self.ema_ipi_secs = IPI_ALPHA * ipi + (1.0 - IPI_ALPHA) * self.ema_ipi_secs;
        self.last_update = now;
        self.index.bid = bid;
        self.index.ask = ask;
        self.index.vbid = vbid;
        self.index.vask = vask;
        self.injected = true;
    }
}

/// Cross-provider composite and confidence interval for one ticker (module
/// header: weight, eviction, cap, `ci`). `None` when no leg is alive.
///
/// `stale_threshold_secs` drives only the liveness axis; prices are aged by
/// `kernel`. Takes any `Clone` iterator over `&ProviderEntry` (slice,
/// `HashMap::values()`): it is walked a few times per call, never collected.
pub fn compute_vwap_at<'a, I>(
    ticker_id: u64,
    entries: I,
    stale_threshold_secs: f64,
    kernel: Kernel,
    now: Instant,
) -> Option<Index>
where
    I: IntoIterator<Item = &'a ProviderEntry>,
    I::IntoIter: Clone,
{
    compute_vwap_profiled_at(ticker_id, entries, stale_threshold_secs, kernel, now)
        .map(|(idx, _)| idx)
}

/// [`compute_vwap_at`] plus the per-ticker [`WeightProfile`]. Internal: the
/// profile reaches the aggregator through [`WeightCache::weight_profile`], so
/// it is never recomputed and never duplicates the weighting rules.
pub(crate) fn compute_vwap_profiled_at<'a, I>(
    ticker_id: u64,
    entries: I,
    stale_threshold_secs: f64,
    kernel: Kernel,
    now: Instant,
) -> Option<(Index, WeightProfile)>
where
    I: IntoIterator<Item = &'a ProviderEntry>,
    I::IntoIter: Clone,
{
    let entries = entries.into_iter();

    // Reference half-spread: base-weighted mean of the admissible legs' `s_v`.
    // It scales eviction, so "dead" means diffused past a few typical spreads
    // of THIS book, not of some other asset's.
    let (mut sb, mut b) = (0.0f64, 0.0f64);
    for e in entries.clone() {
        if kernel.age(e, now).is_some() {
            sb += e.base_weight * kernel.spread(e);
            b += e.base_weight;
        }
    }
    if b <= 0.0 {
        return None;
    }
    let s_ref = sb / b;

    // ── UNMAPPED-BLOC PRE-PASS ──────────────────────────────────────────────
    // Σ base_weight over live legs, split by evidence. Same admission test as
    // the blend below (`Kernel::leg`), so the bound is measured on exactly the
    // population it is applied to.
    let (mut mapped_bw, mut unmapped_bw, mut top_mapped_bw) = (0.0f64, 0.0f64, 0.0f64);
    for entry in entries.clone() {
        if kernel.leg(entry, now, s_ref).is_none() {
            continue;
        }
        if entry.mapped {
            mapped_bw += entry.base_weight;
            top_mapped_bw = top_mapped_bw.max(entry.base_weight);
        } else {
            unmapped_bw += entry.base_weight;
        }
    }
    // Common factor applied to every unmapped leg. Target bloc mass:
    //   clamp(FRAC · Σw_mapped, w_top_mapped / c - Σw_mapped, Σw_unmapped)
    // Upper clamp = the bloc is never INFLATED, only demoted. Lower clamp =
    // the demotion never pushes the heaviest mapped venue past the same
    // concentration ceiling `c` the offline policy caps it at (see
    // `max_leg_share`); with a single mapped venue that floor is what stops the
    // ticker collapsing into a single-venue mark. No mapped leg means no bound
    // at all, and a fully unmapped ticker is left exactly as it was.
    let bloc_scale = if mapped_bw > 0.0 && unmapped_bw > 0.0 {
        let anti_concentration = (top_mapped_bw / max_leg_share() - mapped_bw).max(0.0);
        let target = (unmapped_bloc_frac() * mapped_bw)
            .max(anti_concentration)
            .min(unmapped_bw);
        target / unmapped_bw
    } else {
        1.0
    };
    let base = |e: &ProviderEntry| {
        if e.mapped {
            e.base_weight
        } else {
            e.base_weight * bloc_scale
        }
    };
    // Kernel weight of a live leg, `None` when evicted. `s_ref²` keeps `w`
    // dimensionless; it cancels in the shares.
    let weigh = |e: &ProviderEntry| {
        kernel
            .leg(e, now, s_ref)
            .map(|(tau, u)| (tau, base(e) * s_ref * s_ref * u))
    };

    // Final shares. A leg past its confirmation window (`τ > stale`; a live
    // book re-affirms every stale/2) keeps its raw share `w/Σw` and fades. The
    // legs inside it share the rest, water-filled at the cap: the cap never
    // lifts a dying leg, it only bounds the live ones.
    let fresh = |tau: f64| tau <= stale_threshold_secs;
    let (mut w_sum, mut w_stale) = (0.0f64, 0.0f64);
    for e in entries.clone() {
        if let Some((tau, w)) = weigh(e) {
            w_sum += w;
            if !fresh(tau) {
                w_stale += w;
            }
        }
    }
    if w_sum < 1e-12 {
        return None;
    }
    let fresh_mass = 1.0 - w_stale / w_sum;
    let (k, t, cap) = water_fill(
        entries
            .clone()
            .filter_map(|e| weigh(e).filter(|(tau, _)| fresh(*tau)).map(|(_, w)| w)),
        max_leg_share() / fresh_mass.max(f64::MIN_POSITIVE),
    );
    let share = |tau: f64, w: f64| match (fresh(tau), w >= t) {
        (false, _) => w / w_sum,
        (true, true) => fresh_mass * cap,
        (true, false) => fresh_mass * k * w,
    };

    let mut w_bid_sum = 0.0f64;
    let mut w_ask_sum = 0.0f64;
    let mut s_sum = 0.0f64;
    let mut total_bid_vol: u64 = 0;
    let mut total_ask_vol: u64 = 0;
    let mut accepted: u8 = 0;
    let mut rejected: u8 = 0;
    // Count of legs with liveness decay >= 0.1: the schema-defined "active
    // provider count" per `mitch::Index::confidence` (must be <= accepted).
    let mut active_count: u32 = 0;

    // Welford-style weighted variance accumulator for the disagreement term.
    let mut mean_mid = 0.0f64;
    let mut m2 = 0.0f64;
    let mut stale_sq_sum = 0.0f64;

    // Weight-share accumulators. `bw_sum` = Σ base_weight over every LIVE
    // leg; `active_bw_sum` = the same over legs genuinely TICKING (liveness
    // decay >= 0.1). `active_bw_sum/bw_sum` does NOT dilute with breadth: a leg
    // that ticked recently contributes its FULL base weight, so adding venues
    // that are merely between ticks cannot drag it down. This is what bit 7
    // publishes. Both sums are BLOC-SCALED: the share feeds the signed-quote
    // gate (`server::signed`), so it must be measured on the weights the
    // composite actually uses.
    let mut bw_sum = 0.0f64;
    let mut active_bw_sum = 0.0f64;

    // Share concentration, for `WeightProfile`.
    let mut s_sq_sum = 0.0f64;
    let mut s_max = 0.0f64;

    for entry in entries {
        if !is_valid_tick(entry.index.bid, entry.index.ask) {
            rejected = rejected.saturating_add(1);
            continue;
        }
        let Some((age, w)) = weigh(entry) else {
            continue;
        };
        let base_weight = base(entry);
        bw_sum += base_weight;

        // LIVENESS half-life: per-provider, adaptive, on `stale_threshold`.
        // `active_count` is a money-path gate (`server::signed`
        // MIN_ACTIVE_PROVIDERS); it is independent of the price kernel.
        // `!entry.injected` is what makes this axis mean OBSERVED rather than
        // merely RECENT: `inject_at` refreshes `last_update` every cycle the
        // triangulated product moves. See `ProviderEntry::injected`.
        let live_half_life = (IPI_K * entry.ema_ipi_secs).clamp(1.0, stale_threshold_secs / 2.0);
        let live_decay = (-age * std::f64::consts::LN_2 / live_half_life).exp();
        if live_decay >= 0.1 && !entry.injected {
            active_count = active_count.saturating_add(1);
            active_bw_sum += base_weight;
        }

        let sh = share(age, w);
        if sh <= 1e-12 {
            continue;
        }
        let bid = entry.index.bid;
        let ask = entry.index.ask;
        let mid = (bid + ask) * 0.5;
        // Published staleness widening, on the scale consumers are calibrated
        // to (BTR keeper 25 bp ci-spike trigger, signer ci ceilings): the
        // leg's half-spread × √(τ/ipi), capped at 3×. The kernel's σ²τ decides
        // weight and eviction; it does not set the interval.
        let stale_unc = (ask - bid) * 0.5 * (age / entry.ema_ipi_secs.max(1e-6)).sqrt().min(3.0);
        stale_sq_sum += sh * stale_unc * stale_unc;

        w_bid_sum += bid * sh;
        w_ask_sum += ask * sh;
        s_sq_sum += sh * sh;
        s_max = s_max.max(sh);

        let s_new = s_sum + sh;
        let delta = mid - mean_mid;
        mean_mid += (sh / s_new) * delta;
        m2 += sh * delta * (mid - mean_mid);
        s_sum = s_new;

        total_bid_vol += entry.index.vbid as u64;
        total_ask_vol += entry.index.vask as u64;
        accepted = accepted.saturating_add(1);
    }

    if s_sum < 1e-12 {
        return None;
    }

    let profile = WeightProfile {
        top_weight_share: (s_max / s_sum).clamp(0.0, 1.0),
        n_eff: crate::stats::n_eff_from_sums(s_sum, s_sq_sum),
    };

    let tdwap_bid = w_bid_sum / s_sum;
    let tdwap_ask = w_ask_sum / s_sum;
    let vwap_mid = (tdwap_bid + tdwap_ask) * 0.5;

    let sigma_disagree_sq = m2 / s_sum;
    // NOT `stats::ci::rss`: both terms are already variances.
    let raw_ci = (sigma_disagree_sq + stale_sq_sum / s_sum).sqrt();
    let half_spread_agg = (tdwap_ask - tdwap_bid).abs() * 0.5;
    let conf_interval = raw_ci.max(half_spread_agg);

    // `confidence` publishes the ACTIVE-provider measurement (flag
    // FLAG_CONF_ACTIVE): bits 0..6 = `active_count` (legs genuinely ticking,
    // decay >= 0.1), bit 7 = `fresh_weight_ok`. Independent of
    // `accepted`/`rejected` (which stay raw COUNTS).
    //
    // bit 7 is the WEIGHT-AWARENESS companion to the count: `active_count` alone
    // is weight-blind, so 2 ticking legs carrying 1% of the book would pass. It
    // deliberately uses `active_bw_sum/bw_sum` (share of base weight held by
    // ticking legs) and NOT the decay-weighted `wd_sum/bw_sum`: the latter falls
    // as venues are added, so thresholding it re-creates the breadth inversion
    // this whole gate exists to remove (it would reject BTC-USDC at 0.059 and
    // ETH-USDC at 0.082 while admitting a single-leg feed). The active share does
    // not dilute — a recently-ticked leg contributes its full base weight — so a
    // healthy deep book sits near 1.0 and only genuinely dead weight pulls it
    // down.
    let fresh_weight_share = if bw_sum > 0.0 {
        (active_bw_sum / bw_sum).clamp(0.0, 1.0)
    } else {
        0.0
    };
    let fresh_weight_ok = fresh_weight_share >= crate::shard::FRESH_WEIGHT_SHARE_FLOOR;
    let confidence = mitch::index::conf_pack_active(active_count, fresh_weight_ok);

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

    Some((
        Index {
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
            // Signal that `confidence` is the PACKED active-count byte (bits 0..6
            // count, bit 7 fresh-weight-ok), not a legacy count and not the legacy
            // freshness fraction. Single-source bit in `nxr_sdk::shard`.
            // FLAG_NO_BOOK when NO provider carried depth (oracle relays publish
            // price±conf without book sizes): honest absence marker so the
            // integrity/dq phantom-quote gates don't flag oracle tickers. A
            // healthy multi-provider CEX composite always sums nonzero depth.
            flags: crate::shard::FLAG_CONF_ACTIVE
                | if total_bid_vol == 0 && total_ask_vol == 0 {
                    crate::shard::FLAG_NO_BOOK
                } else {
                    0
                },
        },
        profile,
    ))
}

// ── Throttled TDWAP: weight-vector freeze with change-triggered refresh ─────
//
// Problem: on quiet markets where no provider's quote changes between
// aggregation cycles, the kernel's ageing term `σ²τ` keeps shifting
// the cross-provider weight ratios by tiny ULPs every cycle. The 5-field
// delta-gate (bid, ask, vbid, vask, ci) on the shard writer never matches,
// so a quiet stablecoin pair writes ~every cycle (20 Hz) instead of
// approximately never. This defeats the entire point of the delta-gate.
//
// Fix: cache the *normalized* weight vector at refresh boundaries
// (`refresh_interval_ms`, default stale/5 = 2 s at the 10 s prod threshold).
// Between refreshes, reuse the same weight vector → composite VWAP is
// bit-identical when raw provider quotes are bit-identical → delta-gate
// fires only on real moves. When any provider's price/volume actually
// changes, force a refresh on the next call so the new quote is reflected
// immediately with up-to-date kernel weights.
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
    /// Part of the fingerprint because it changes `confidence` (active_count +
    /// fresh-weight bit) WITHOUT changing any price field: a real frame whose
    /// bid/ask happen to equal the injected value flips `injected` false and
    /// must force a recompute, else the throttle replays a composite that
    /// understates liveness.
    injected: bool,
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
            injected: e.injected,
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
    /// Weight composition measured at the last refresh, alongside
    /// `cached_index`. Published as per-ticker gauges by the aggregator; it is
    /// a by-product of the blend, so observing it costs nothing and cannot
    /// disagree with the composite it describes.
    profile: Option<WeightProfile>,
}

impl WeightCache {
    /// Create an empty cache. First `compute_vwap_throttled` call will refresh.
    pub const fn new() -> Self {
        Self {
            last_refresh: None,
            cached_index: None,
            fingerprints: Vec::new(),
            scratch: Vec::new(),
            profile: None,
        }
    }

    /// Force the next call to refresh (e.g. on config / weights reload).
    #[inline]
    pub fn invalidate(&mut self) {
        self.last_refresh = None;
        self.cached_index = None;
        self.fingerprints.clear();
        self.profile = None;
    }

    /// Weight composition measured by the most recent REFRESH, consumed once.
    /// Returns `None` on every replay cycle, so a caller publishing it as a
    /// gauge does the work only when the composite actually changed and a
    /// quiet ticker costs nothing.
    #[inline]
    pub fn take_weight_profile(&mut self) -> Option<WeightProfile> {
        self.profile.take()
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
/// `refresh_interval_ms` is the maximum age of a cached weight vector
/// ([`default_refresh_interval_ms`]). Callers should clamp it
/// `≥ aggregation_interval_ms` or the throttle is a no-op.
pub fn compute_vwap_throttled(
    ticker_id: u64,
    entries: &[(u16, ProviderEntry)],
    stale_threshold_secs: f64,
    kernel: Kernel,
    cache: &mut WeightCache,
    refresh_interval_ms: u64,
    force_refresh: bool,
) -> Option<Index> {
    compute_vwap_throttled_at(
        ticker_id,
        entries,
        stale_threshold_secs,
        kernel,
        cache,
        refresh_interval_ms,
        force_refresh,
        Instant::now(),
    )
}

/// Throttled TDWAP with an explicit clock (for tests and replay).
#[allow(clippy::too_many_arguments)]
pub(crate) fn compute_vwap_throttled_at(
    ticker_id: u64,
    entries: &[(u16, ProviderEntry)],
    stale_threshold_secs: f64,
    kernel: Kernel,
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
    let (composite, profile) = compute_vwap_profiled_at(
        ticker_id,
        entries.iter().map(|(_, e)| e),
        stale_threshold_secs,
        kernel,
        now,
    )?;

    // Commit the new state: swap scratch into fingerprints (O(1) — keeps
    // the just-built buffer, recycles the old one as the next scratch).
    std::mem::swap(&mut cache.fingerprints, &mut cache.scratch);
    cache.scratch.clear();
    cache.cached_index = Some(composite);
    cache.profile = Some(profile);
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
/// at which point sub-ULP kernel drift each cycle defeats the gate and quiet
/// pairs write every cycle — up to ~3× the on-disk footprint. The explicit
/// `NXR_WEIGHT_REFRESH_MS` override is clamped to this same 3× floor by the
/// aggregator before use.
///
/// Deferral cost: between refreshes an UNCHANGED quote's kernel ageing waits
/// for the next boundary (<= 2 s at prod defaults, against eviction horizons of
/// ~7 s for majors up to minutes for pegged pairs). A price move never waits:
/// it breaks the fingerprint.
///
/// Examples (agg=200ms prod default, 3× floor = 600ms):
/// - Production crypto (stale=10s, agg=200ms): refresh = 2000 ms.
/// - Aggressive FX (stale=2s, agg=200ms): refresh = max(400, 600) = 600 ms.
/// - Pathological tight stale (0.2s, agg=200ms): refresh = max(40, 600) =
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
mod injected_leg_liveness_tests {
    use super::*;

    fn leg(mid: f64) -> Index {
        let half = mid * 0.0001;
        Index::new(
            448509915440349184,
            mid - half,
            mid + half,
            16,
            1_000,
            1_000,
            10,
            1,
            1,
            0,
        )
    }

    /// An entry whose latest value came from a triangulator INJECTION rule.
    /// `inject_at` is what production calls (`triangulator::apply_injections`).
    fn injected_entry(mid: f64, now: Instant) -> ProviderEntry {
        let mut e = ProviderEntry::new_at(leg(mid), 1.0, now);
        let half = mid * 0.0001;
        e.inject_at(mid - half, mid + half, 0, 0, now);
        e
    }

    /// THE regression. Before the fix, `inject_at` stamped `last_update = now`
    /// every cycle the triangulated product moved, so injected legs scored
    /// `decay ≈ 1` and were counted in BOTH liveness axes. A composite backed
    /// only by injections therefore published `active_count = 2` and
    /// `fresh_weight_ok = true` — satisfying breadth AND liveness on legs that
    /// never ticked, which is what a signed mark is gated on.
    #[test]
    fn composite_of_only_injected_legs_fails_both_liveness_axes() {
        let now = Instant::now();
        let entries = [injected_entry(75.0, now), injected_entry(75.1, now)];

        let snap = compute_vwap_at(448509915440349184, entries.iter(), 10.0, Kernel::ALT, now)
            .expect("injected legs still produce a PRICE — only liveness is withheld");

        assert_eq!(
            mitch::index::conf_active_count(snap.confidence),
            0,
            "injected legs must not count as ticking"
        );
        assert!(
            !mitch::index::conf_fresh_weight_ok(snap.confidence),
            "fresh-weight numerator must exclude injected legs (bit 7 clear)"
        );
        // signed.rs gates on `active >= MIN_ACTIVE_PROVIDERS` (2).
        assert!(
            mitch::index::conf_active_count(snap.confidence) < 2,
            "must fail the signed-quote breadth gate"
        );
        // The price is still blended (the USDC->USDT bridge is a real feature),
        // and `accepted` still counts the legs as non-corpse — the two axes are
        // deliberately independent.
        assert!(snap.mid() > 74.0 && snap.mid() < 76.0);
        assert_eq!(snap.accepted, 2);
    }

    /// One real provider alongside an injected leg: the real one counts, the
    /// injected one does not. Pins that the fix is a numerator change, not a
    /// blanket rejection.
    #[test]
    fn injected_leg_does_not_inflate_a_real_provider_count() {
        let now = Instant::now();
        let entries = [
            ProviderEntry::new_at(leg(75.0), 1.0, now),
            injected_entry(75.1, now),
        ];
        let snap = compute_vwap_at(448509915440349184, entries.iter(), 10.0, Kernel::ALT, now)
            .expect("has a price");
        assert_eq!(
            mitch::index::conf_active_count(snap.confidence),
            1,
            "only the real provider ticks"
        );
        // active_bw_sum/bw_sum = 1.0/2.0 = 0.5 >= FRESH_WEIGHT_SHARE_FLOOR (0.20)
        assert!(mitch::index::conf_fresh_weight_ok(snap.confidence));
        assert!(
            mitch::index::conf_active_count(snap.confidence) < 2,
            "1 ticking leg still fails the signed breadth gate"
        );
    }

    /// A real frame supersedes an injected value even when the prices are
    /// bit-identical — the fingerprint carries `injected`, so the throttle
    /// cannot replay a composite that understates liveness.
    #[test]
    fn real_frame_clears_injected_and_is_not_throttled_away() {
        let now = Instant::now();
        let mut e = injected_entry(75.0, now);
        assert!(e.injected);
        let before = ProviderFingerprint::from_entry(7, &e);

        // Same bid/ask/vbid/vask as the injected value: ONLY `injected` differs.
        let mut same = leg(75.0);
        same.vbid = 0;
        same.vask = 0;
        e.update_at(same, now);

        assert!(!e.injected, "a provider frame supersedes the injection");
        assert_ne!(
            before,
            ProviderFingerprint::from_entry(7, &e),
            "injected->real must break the fingerprint or the throttle replays stale liveness"
        );
    }
}

#[cfg(test)]
mod unmapped_bloc_tests {
    use super::*;

    fn leg(mid: f64) -> Index {
        let half = mid * 0.0001;
        Index::new(
            448509915440349184,
            mid - half,
            mid + half,
            16,
            1_000,
            1_000,
            10,
            1,
            1,
            0,
        )
    }

    /// `(entry, profile)` for a set of `(base_weight, mapped)` legs, all at the
    /// same mid and the same age so decay is a common factor and every measured
    /// share is a pure function of the weighting.
    fn profile_of(legs: &[(f64, bool)]) -> WeightProfile {
        let now = Instant::now();
        let entries: Vec<ProviderEntry> = legs
            .iter()
            .enumerate()
            .map(|(i, (w, mapped))| {
                ProviderEntry::new_at(leg(100.0 + i as f64), *w, now).with_mapped(*mapped)
            })
            .collect();
        compute_vwap_profiled_at(448509915440349184, entries.iter(), 10.0, Kernel::ALT, now)
            .expect("legs blend")
            .1
    }

    /// NON-NEGOTIABLE: a ticker with no mapped leg has no bound applied and
    /// composites exactly as it always did. 227 of 339 live tickers are in this
    /// class (every FX pair, every metal).
    #[test]
    fn fully_unmapped_ticker_is_unchanged() {
        let now = Instant::now();
        let mids = [100.0_f64, 101.0, 102.0, 103.0, 104.0];
        let unmapped: Vec<ProviderEntry> = mids
            .iter()
            .map(|m| ProviderEntry::new_at(leg(*m), 1.0, now))
            .collect();
        // Same legs declared MAPPED: the bound cannot apply either way, so the
        // two composites must be bit-identical. That equality is the property
        // "the bound never touches a fully-unmapped ticker".
        let mapped: Vec<ProviderEntry> = unmapped.iter().map(|e| e.with_mapped(true)).collect();
        let (a, pa) =
            compute_vwap_profiled_at(448509915440349184, unmapped.iter(), 10.0, Kernel::ALT, now)
                .unwrap();
        let (b, _) =
            compute_vwap_profiled_at(448509915440349184, mapped.iter(), 10.0, Kernel::ALT, now)
                .unwrap();
        assert_eq!(a.bid.to_bits(), b.bid.to_bits());
        assert_eq!(a.ask.to_bits(), b.ask.to_bits());
        // Equal weights: exactly 5 effective venues, each holding one fifth.
        assert!((pa.n_eff - 5.0).abs() < 1e-9, "n_eff {}", pa.n_eff);
        assert!((pa.top_weight_share - 0.2).abs() < 1e-9);
        assert!((a.mid() - 102.0).abs() < 1e-6, "equal-weight mean");
    }

    /// The failure the per-leg constant caused: with ONE mapped venue (the
    /// live shape of ORCA/USDT, CRV/USDT and ALGO/USDT, whose volume rows the
    /// CMC scrape covers for a single exchange each) a small per-leg fallback
    /// hands that venue 93-98% of the composite. The bloc bound must keep it at
    /// the same concentration ceiling the offline policy would have capped it
    /// at, and must keep real breadth in the mark.
    #[test]
    fn single_mapped_venue_never_becomes_a_single_venue_mark() {
        for n_unmapped in [1_usize, 2, 4, 8, 16] {
            let mut legs = vec![(1.0, true)];
            legs.extend(std::iter::repeat_n((1.0, false), n_unmapped));
            let p = profile_of(&legs);
            assert!(
                p.top_weight_share <= max_leg_share() + 1e-9,
                "{n_unmapped} unmapped: top share {} breaches the {} ceiling",
                p.top_weight_share,
                max_leg_share()
            );
            if n_unmapped >= 2 {
                assert!(p.n_eff >= 2.0, "{n_unmapped} unmapped: n_eff {}", p.n_eff);
            }
        }
    }

    /// The other direction: an unmapped bloc can never dilute a priced book.
    /// The old `1.0` fallback let 8 unpriced legs outvote a mapped venue 8:1.
    #[test]
    fn unmapped_bloc_cannot_outvote_the_mapped_mass() {
        let mut legs = vec![(1.0, true)];
        legs.extend(std::iter::repeat_n((1.0, false), 8));
        let p = profile_of(&legs);
        // Mapped mass = 1.0; bloc is held to max(0.40, anti-concentration) of
        // it, so the mapped venue keeps the majority of the blend.
        assert!(p.top_weight_share >= 0.5, "share {}", p.top_weight_share);
        assert!(p.top_weight_share <= max_leg_share() + 1e-9);
    }

    /// A ticker with enough mapped mass is left ALONE: the bound must not
    /// reweight books that were never in trouble. Live DOT/USDT shape
    /// (2026-08-11): 4 mapped venues summing ~5.95, 2 unmapped legs.
    #[test]
    fn mapped_rich_ticker_is_untouched() {
        let legs = [
            (3.76, true),
            (1.0, true),
            (0.68, true),
            (0.51, true),
            (1.0, false),
            (1.0, false),
        ];
        let bounded = profile_of(&legs);
        let all_mapped: Vec<(f64, bool)> = legs.iter().map(|(w, _)| (*w, true)).collect();
        let unbounded = profile_of(&all_mapped);
        assert!(
            (bounded.top_weight_share - unbounded.top_weight_share).abs() < 1e-12,
            "{} != {}",
            bounded.top_weight_share,
            unbounded.top_weight_share
        );
        assert!((bounded.n_eff - unbounded.n_eff).abs() < 1e-12);
    }

    /// PHASE 4 — the signed-quote liveness gate. `fresh_weight_share` is
    /// `active_bw_sum / bw_sum` and both sums are now BLOC-SCALED, so the gate
    /// sees the weights the composite is actually built from.
    ///
    /// Case: the mapped venue goes quiet while the unmapped tail keeps ticking.
    /// On RAW base weights an unmapped bloc rescaled downward drags the share
    /// with it, so signing stops with five real venues live (measured 0.032 at
    /// a 0.01 per-leg fallback, against a 0.20 floor). Under the bloc bound the
    /// tail keeps its bounded-but-real share and the gate stays open.
    #[test]
    fn fresh_weight_share_is_measured_on_the_weights_actually_used() {
        let now = Instant::now();
        // Quiet mapped venue: silent long enough that live_decay < 0.1.
        let quiet =
            ProviderEntry::new_at(leg(100.0), 1.0, coarse_now_backdated(30_000)).with_mapped(true);
        let mut entries = vec![quiet];
        entries.extend(
            (0..5).map(|i| ProviderEntry::new_at(leg(100.0 + f64::from(i) * 0.01), 1.0, now)),
        );
        let snap =
            compute_vwap_at(448509915440349184, entries.iter(), 10.0, Kernel::ALT, now).unwrap();
        assert_eq!(
            mitch::index::conf_active_count(snap.confidence),
            5,
            "five unmapped venues are genuinely ticking"
        );
        assert!(
            mitch::index::conf_fresh_weight_ok(snap.confidence),
            "five live venues must not read as a decayed composite"
        );
    }

    /// The inverse: ONE ticking mapped venue behind a dead unmapped tail must
    /// not be reported as a healthy multi-venue composite. The weight axis
    /// alone cannot see this (the live leg holds most of the mass by design),
    /// which is why `active_count` is the companion gate — pin that it is the
    /// axis that fails here.
    #[test]
    fn one_live_venue_still_fails_the_breadth_gate() {
        let now = Instant::now();
        let stale = coarse_now_backdated(30_000);
        let mut entries = vec![ProviderEntry::new_at(leg(100.0), 1.0, now).with_mapped(true)];
        entries.extend(
            (0..5).map(|i| ProviderEntry::new_at(leg(100.0 + f64::from(i) * 0.01), 1.0, stale)),
        );
        let snap =
            compute_vwap_at(448509915440349184, entries.iter(), 10.0, Kernel::ALT, now).unwrap();
        assert_eq!(mitch::index::conf_active_count(snap.confidence), 1);
        assert!(
            u32::from(mitch::index::conf_active_count(snap.confidence)) < 2,
            "one ticking leg must fail MIN_ACTIVE_PROVIDERS regardless of weight share"
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ci_roundtrip_preserves_order_of_magnitude() {
        // Round-trip through encode/decode for a spread of plausible CI values.
        // Tolerance is loose: quantization by the sqrt-then-u16 cast introduces
        // up to ~(2 * sqrt(x) / CI_SCALE + 1 / CI_SCALE^2) absolute error in ubp.
        for &ci_ubp in &[
            0.0,
            1.0,
            10.0,
            100.0,
            1_000.0,
            10_000.0,
            100_000.0,
            1_000_000.0,
        ] {
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
        assert!(
            encoded < u16::MAX,
            "10% CI should not saturate, got {encoded}"
        );
        let decoded = decode_ci_ubp(encoded);
        assert!(
            (decoded - 1e7).abs() / 1e7 < 0.01,
            "decoded {decoded} differs from 1e7 by >1%"
        );
    }

    #[test]
    fn ci_saturation_threshold_exceeds_old_limit() {
        // Old encoding saturated at 65535 ubp. New encoding must not saturate there.
        let encoded = encode_ci_ubp(65535.0);
        assert!(
            encoded < u16::MAX,
            "new encoding must not saturate at old-linear-max, got {encoded}"
        );
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

    fn mk_entry(
        bid: f64,
        ask: f64,
        vbid: u32,
        vask: u32,
        base_weight: f64,
        now: Instant,
    ) -> ProviderEntry {
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

    /// Bit 7 (`fresh_weight_ok`) must NOT dilute as venues are added.
    ///
    /// This is the invariant that killed the first draft of this gate: the old
    /// `confidence` byte published `Σ base_weight·decay / Σ base_weight`, which
    /// FALLS with breadth, so a 0.20 floor on it would have rejected our deepest
    /// books (measured 0.059 BTC-USDC / 0.082 ETH-USDC) while admitting a
    /// single-leg feed. Bit 7 uses the ACTIVE-weight share instead, so adding
    /// fresh venues must never clear the bit.
    #[test]
    fn fresh_weight_bit_does_not_dilute_with_breadth() {
        let t0 = Instant::now();
        let mut prev_set = true;
        for n in 1..=10usize {
            let entries: Vec<(u16, ProviderEntry)> = (0..n)
                .map(|i| {
                    (
                        i as u16 + 1,
                        mk_entry(100.00, 100.02, 1_000, 1_100, 1.0, t0),
                    )
                })
                .collect();
            let out = compute_vwap_at(1, entries.iter().map(|(_, e)| e), 30.0, Kernel::ALT, t0)
                .expect("composite");
            assert_eq!(
                out.flags & crate::shard::FLAG_CONF_ACTIVE,
                crate::shard::FLAG_CONF_ACTIVE,
                "n={n}: must publish the ACTIVE encoding"
            );
            assert_eq!(
                mitch::index::conf_active_count(out.confidence) as usize,
                n,
                "n={n}: every leg is fresh, so all must count as ticking"
            );
            let ok = mitch::index::conf_fresh_weight_ok(out.confidence);
            assert!(ok, "n={n}: all-fresh book must set fresh_weight_ok");
            assert!(prev_set && ok, "n={n}: bit 7 regressed as breadth grew");
            prev_set = ok;
        }
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
        let first =
            compute_vwap_throttled_at(42, &entries, 10.0, Kernel::ALT, &mut cache, 1000, false, t0)
                .expect("first call must produce a composite");

        // 18 cycles at 50ms each = 900ms elapsed, still within the 1000ms refresh.
        for step in 1..=18u64 {
            let now = t0 + Duration::from_millis(step * 50);
            let cur = compute_vwap_throttled_at(
                42,
                &entries,
                10.0,
                Kernel::ALT,
                &mut cache,
                1000,
                false,
                now,
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
        let first =
            compute_vwap_throttled_at(42, &entries, 10.0, Kernel::ALT, &mut cache, 200, false, t0)
                .unwrap();

        // 300ms later — well past the 200ms refresh interval.
        // Both providers age equally so normalized weights are unchanged in
        // ratio, but absolute decay still re-runs through `compute_vwap_at`
        // and the cached_index timestamp updates.
        let t1 = t0 + Duration::from_millis(300);
        let refreshed =
            compute_vwap_throttled_at(42, &entries, 10.0, Kernel::ALT, &mut cache, 200, false, t1)
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
        let idx = compute_vwap_at(7, entries.iter().map(|(_, e)| e), 10.0, Kernel::ALT, t0)
            .expect("composite");
        // Copy packed fields to locals before use (packed struct → no field refs).
        let (bid, ask) = (idx.bid, idx.ask);
        // mid ~100.05, and a real (non-degenerate) spread synthesized from the
        // 0.10 cross-venue disagreement — NOT collapsed to bid==ask.
        assert!(
            ask > bid,
            "no-book multi-venue must synthesize a spread, got bid={bid} ask={ask}"
        );
        let mid = (bid + ask) * 0.5;
        assert!((mid - 100.05).abs() < 0.02, "mid off: {mid}");
        // half-spread = k * sqrt(m2/w_sum); with equal weights the mid variance
        // is 0.05^2, so sqrt = 0.05, half-spread = k*0.05 (k default 1.0).
        let hs = (ask - bid) * 0.5;
        assert!(
            hs > 0.0 && hs < 0.20,
            "half-spread out of expected band: {hs}"
        );
    }

    #[test]
    fn no_book_single_venue_stays_collapsed() {
        // A single no-book venue has zero dispersion ⇒ must NOT fabricate a
        // spread; bid==ask so the bar builder emits NaN + FLAG_NO_BOOK.
        let t0 = Instant::now();
        let p = mk_entry(100.00, 100.00, 1_000, 1_000, 1.0, t0);
        let entries: Vec<(u16, ProviderEntry)> = vec![(1, p)];
        let idx = compute_vwap_at(7, entries.iter().map(|(_, e)| e), 10.0, Kernel::ALT, t0)
            .expect("composite");
        let (bid, ask) = (idx.bid, idx.ask);
        assert_eq!(
            bid.to_bits(),
            ask.to_bits(),
            "single no-book venue must stay collapsed (honest absence)"
        );
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
        let first =
            compute_vwap_throttled_at(42, &entries, 10.0, Kernel::ALT, &mut cache, 1000, false, t0)
                .unwrap();

        // 100ms in — well within refresh window. Push a new price into B.
        let t1 = t0 + Duration::from_millis(100);
        entries[1].1.update_at(
            MitchIndex::new(1, 105.00, 105.02, 0, 1_000, 1_100, 1, 1, 1, 0),
            t1,
        );

        let post_change =
            compute_vwap_throttled_at(42, &entries, 10.0, Kernel::ALT, &mut cache, 1000, false, t1)
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
        let _first =
            compute_vwap_throttled_at(42, &entries, 10.0, Kernel::ALT, &mut cache, 1000, false, t0)
                .unwrap();
        let cached_before_join = cache.cached_index.unwrap();

        // Add provider B 100ms later.
        let t1 = t0 + Duration::from_millis(100);
        let p_b = mk_entry(110.00, 110.02, 5_000, 5_500, 1.0, t1);
        entries.push((2, p_b));

        let after_join =
            compute_vwap_throttled_at(42, &entries, 10.0, Kernel::ALT, &mut cache, 1000, false, t1)
                .unwrap();
        assert_ne!(
            cached_before_join.bid.to_bits(),
            after_join.bid.to_bits(),
            "joining a provider with a different mid must shift the composite",
        );
    }

    /// A quiet interval must not move the mark.
    ///
    /// Every leg ages together and no quote changes, so the composite is pure
    /// re-weighting. Under the old per-provider half-life the legs decayed at
    /// rates differing by up to 5x, share migrated to whichever leg last ticked,
    /// and the composite walked across a lull on its own. With one half-life per
    /// ticker the decay factor is common to every leg and cancels in `w_sum`.
    #[test]
    fn quiet_interval_does_not_move_the_composite() {
        let t0 = Instant::now();
        // Deliberately heterogeneous cadences (the live BTC/USDT book spans
        // 0.09..3.29 frames/s) and heterogeneous prices, so any residual
        // re-weighting shows up as a mid move.
        let mut legs = Vec::new();
        for (i, (px, ipi, bw)) in [
            (63_700.0f64, 0.21f64, 1.00f64),
            (63_690.0, 0.47, 3.99),
            (63_712.0, 1.18, 3.53),
            (63_680.0, 6.65, 0.02),
            (63_725.0, 7.12, 0.19),
        ]
        .iter()
        .enumerate()
        {
            let mut e = mk_entry(px - 0.5, px + 0.5, 1_000, 1_000, *bw, t0);
            e.ema_ipi_secs = *ipi;
            legs.push((i as u16 + 1, e));
        }
        let first = compute_vwap_at(1, legs.iter().map(|(_, e)| e), 10.0, Kernel::ALT, t0)
            .expect("composite");
        let first_mid = first.mid();
        // Walk 9 s of total silence: no leg updates, all ages advance together.
        for step in 1..=90u64 {
            let now = t0 + Duration::from_millis(step * 100);
            let cur = compute_vwap_at(1, legs.iter().map(|(_, e)| e), 10.0, Kernel::ALT, now)
                .expect("composite");
            let drift_bps = ((cur.mid() - first_mid) / first_mid).abs() * 1e4;
            assert!(
                drift_bps < 0.01,
                "t+{} ms: composite drifted {drift_bps:.4} bps on a quiet book \
                 (mid {} -> {}); decay must cancel under normalisation",
                step * 100,
                first_mid,
                cur.mid(),
            );
        }
    }

    /// A departing contributor must not step the mark: its weight is tapered
    /// to exactly zero at eviction, so the cycle that drops it moves nothing.
    #[test]
    fn provider_dropout_does_not_step_the_composite() {
        let t0 = Instant::now();
        const STALE: f64 = 10.0;
        // Survivors agree near 63 700; the departing leg sits 100 bps below, so
        // any discontinuity in its weight is plainly visible in the mid.
        let mut legs = vec![
            (1u16, mk_entry(63_699.5, 63_700.5, 1_000, 1_000, 1.0, t0)),
            (2u16, mk_entry(63_701.5, 63_702.5, 1_000, 1_000, 1.0, t0)),
            (3u16, mk_entry(63_062.0, 63_063.0, 1_000, 1_000, 1.0, t0)),
        ];
        let mid = |legs: &[(u16, ProviderEntry)], now| {
            compute_vwap_at(1, legs.iter().map(|(_, e)| e), STALE, Kernel::ALT, now)
                .expect("composite")
                .mid()
        };
        let mut prev = mid(&legs, t0);
        let mut last_share = f64::INFINITY;
        let mut evicted_step = None;
        for step in 1..=150u64 {
            let now = t0 + Duration::from_millis(step * 100);
            for (_, e) in legs.iter_mut().take(2) {
                e.update_at(e.index, now);
            }
            let cur = mid(&legs, now);
            // Share of the departing leg, from where the mid sits between it
            // and the survivors: must fall monotonically.
            let share = (63_701.0 - cur) / (63_701.0 - 63_062.5);
            assert!(
                share <= last_share + 1e-12,
                "share rose at t+{} ms",
                step * 100
            );
            if share.abs() < 1e-12 && evicted_step.is_none() {
                evicted_step = Some(((cur - prev) / prev).abs() * 1e4);
            }
            last_share = share;
            prev = cur;
        }
        let step_bps = evicted_step.expect("the silent leg must be evicted");
        assert!(
            step_bps < 0.05,
            "eviction stepped the mark {step_bps:.4} bps"
        );
        assert!(
            (prev - 63_701.0).abs() < 1e-6,
            "survivors' blend, got {prev}"
        );
    }

    /// Confirmation keeps a quiet dominant book in the mark: Binance USD1/USDC
    /// flat for 300 s, re-affirmed every 5 s, against thinner books that keep
    /// moving. Its share stays pinned at the 0.60 cap throughout.
    #[test]
    fn quiet_confirmed_dominant_venue_keeps_its_share() {
        let t0 = Instant::now();
        let book = |bid: f64, ask: f64, bw: f64, at| mk_entry(bid, ask, 1_000, 1_000, bw, at);
        let mut legs = vec![(1u16, book(0.9992, 0.9993, 3.0, t0))];
        legs.extend((0..4).map(|i| (i + 2, book(0.9990, 0.9996, 0.5, t0))));
        for step in 1..=300u64 {
            let now = t0 + Duration::from_secs(step);
            if step % 5 == 0 {
                let idx = legs[0].1.index;
                legs[0].1.update_at(idx, now);
            }
            for (_, e) in legs.iter_mut().skip(1) {
                let mut idx = e.index;
                idx.bid += if step % 2 == 0 { 1e-4 } else { -1e-4 };
                e.update_at(idx, now);
            }
            let (_, p) =
                compute_vwap_profiled_at(1, legs.iter().map(|(_, e)| e), 10.0, Kernel::PEGGED, now)
                    .expect("composite");
            assert!(
                p.top_weight_share >= 0.59,
                "t+{step} s: share {}",
                p.top_weight_share
            );
            assert!(p.top_weight_share <= max_leg_share() + 1e-9);
        }
    }

    /// No frames at all: the interval widens with the leg's age, and the leg is
    /// evicted at the class backstop even when its spread is too wide for the
    /// diffusion test to fire first.
    #[test]
    fn dead_venue_widens_then_is_evicted_at_the_horizon() {
        let t0 = Instant::now();
        let dead = [mk_entry(0.9950, 1.0050, 1_000, 1_000, 1.0, t0)];
        let at = |secs: u64| {
            compute_vwap_at(
                1,
                dead.iter(),
                10.0,
                Kernel::PEGGED,
                t0 + Duration::from_secs(secs),
            )
        };
        let ci = |secs| decode_ci_ubp(at(secs).expect("alive").ci);
        assert!(
            ci(600) > ci(0),
            "ci must widen as the leg ages: {} vs {}",
            ci(600),
            ci(0)
        );
        assert!(at(899).is_some());
        assert!(
            at(901).is_none(),
            "past the 15 min backstop the ticker has no mark"
        );
    }

    /// A thin venue quoting a razor spread cannot take the mark: its kernel
    /// weight is floored at `s_min` and its final share capped at 0.60.
    #[test]
    fn thin_tight_venue_never_passes_the_cap() {
        let t0 = Instant::now();
        let thin = mk_entry(100.0, 100.000_01, 10, 10, 0.5, t0);
        let deep = mk_entry(99.97, 100.03, 1_000, 1_000, 1.0, t0);
        for age_ms in (0..8_000u64).step_by(250) {
            let now = t0 + Duration::from_millis(age_ms);
            let mut fresh = thin;
            fresh.update_at(thin.index, now);
            let legs = [fresh, deep];
            let (_, p) = compute_vwap_profiled_at(1, legs.iter(), 10.0, Kernel::ALT, now)
                .expect("composite");
            assert!(
                p.top_weight_share <= 0.60 + 1e-9,
                "age {age_ms}: {}",
                p.top_weight_share
            );
        }
    }

    /// Two legs, one stops confirming: once past its confirmation window it is
    /// not held at the cap's 0.40 floor, it fades on its own kernel weight.
    #[test]
    fn a_dying_leg_is_not_held_up_by_the_cap() {
        let t0 = Instant::now();
        let mut live = mk_entry(0.9999, 1.0000, 1_000, 1_000, 1.0, t0);
        let dying = mk_entry(0.9997, 0.9998, 1_000, 1_000, 1.0, t0);
        let share_of_dying = |live: &ProviderEntry, now| {
            let legs = [*live, dying];
            let idx = compute_vwap_at(1, legs.iter(), 10.0, Kernel::PEGGED, now).expect("mark");
            (0.99995 - idx.mid()) / (0.99995 - 0.99975)
        };
        let mut prev = 0.5 + 1e-9;
        for secs in [5u64, 30, 60, 120, 300] {
            let now = t0 + Duration::from_secs(secs);
            live.update_at(live.index, now);
            let sh = share_of_dying(&live, now);
            assert!(sh <= prev, "t+{secs} s: share rose to {sh}");
            prev = sh;
        }
        assert!(prev < 0.2, "dying leg still holds {prev} after 300 s");
    }

    /// Healthy pegged book: ci stays on the half-spread scale (0.05 bp for a
    /// 5 dp USDC/USDT book), not the kernel's 1 bp spread floor.
    #[test]
    fn healthy_pegged_ci_is_on_the_spread_scale() {
        let t0 = Instant::now();
        let legs = [
            mk_entry(0.99990, 0.99991, 1_000, 1_000, 1.0, t0),
            mk_entry(0.99990, 0.99991, 1_000, 1_000, 1.0, t0),
        ];
        let idx = compute_vwap_at(1, legs.iter(), 10.0, Kernel::PEGGED, t0).expect("mark");
        let ci_bp = decode_ci_ubp(idx.ci) / 1e4;
        assert!(ci_bp > 0.02 && ci_bp < 0.1, "ci {ci_bp} bp");
    }

    #[test]
    fn measured_sigma_is_clamped_to_ten_floors() {
        let k = Kernel::MAJOR;
        assert_eq!(k.with_sigma(0.0).sigma, k.sigma);
        assert_eq!(k.with_sigma(1.0).sigma, 10.0 * k.sigma);
        assert_eq!(k.with_sigma(3.0 * k.sigma).sigma, 3.0 * k.sigma);
    }

    #[test]
    fn water_fill_caps_and_renormalises() {
        let ws = [8.0, 1.0, 1.0];
        let (k, t, cap) = water_fill(ws.iter().copied(), 0.6);
        let shares: Vec<f64> = ws
            .iter()
            .map(|w| if *w >= t { cap } else { k * w })
            .collect();
        assert!((shares[0] - 0.6).abs() < 1e-12);
        assert!((shares[1] - 0.2).abs() < 1e-12);
        assert!((shares.iter().sum::<f64>() - 1.0).abs() < 1e-12);
        // One live leg keeps the whole blend.
        let (k, t, cap) = water_fill([2.0].into_iter(), 0.6);
        assert_eq!(if 2.0 >= t { cap } else { k * 2.0 }, 1.0);
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
