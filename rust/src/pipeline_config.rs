//! Unified `nxrates.yml` schema — single source of truth shared by every
//! offline tool that consumes the pipeline configuration.
//!
//! Consolidated schema: each offline tool used to carry a near-identical
//! private copy of the `series.{renko,vol,calibration,pipeline}` schema
//! (renko_from_idx, nxr_calibrate, renko_trailing_from_idx, mtf_sweep,
//! generate_renko_from_ticks, fetch_crypto_history). They were not strictly
//! identical — some held a `target_bpd_by_class` table, some used `i64` vs
//! `usize` for `k_fit_windows_days`, some omitted `cexs`. The union here
//! supersedes all of them; bins import [`PipelineYml`] and access the
//! fields they need.
//!
//! `#[serde(default)]` is used on the leaf-level fields that only some
//! bins exercise so individual `nxrates.yml` files can omit them without
//! serde rejecting the parse. The required field set is the intersection
//! of what every bin needs.

use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

use crate::vol::VolConfig;

/// Default min 24h USD volume below which the core weights builder skips
/// emitting a synth injection rule (stablecoin- or USD-quoted pairs).
/// Was: hardcoded `100_000.0` at `core/src/weights.rs:190,211`. Phase
/// 59.R3.C2.O1 (2026-05-30).
pub const DEFAULT_MIN_VOLUME_INJECTION_USD: f64 = 100_000.0;

/// Default Binance historical-archive URL prefixes.
/// Fallback for `cexs.exchanges.binance.archive_url_template.*` when unset.
/// Phase 59.R3.C2.O4 (2026-05-30).
pub const DEFAULT_ARCHIVE_URL_BINANCE_MONTHLY: &str =
    "https://data.binance.vision/data/spot/monthly/aggTrades/{sym}/";
pub const DEFAULT_ARCHIVE_URL_BINANCE_DAILY: &str =
    "https://data.binance.vision/data/spot/daily/aggTrades/{sym}/";
pub const DEFAULT_ARCHIVE_URL_BINANCE_PROBE: &str = "https://data.binance.vision/data/spot/monthly/aggTrades/{sym}/{sym}-aggTrades-{y:04}-{m:02}.zip";

/// Default Bybit historical-archive URL prefixes.
pub const DEFAULT_ARCHIVE_URL_BYBIT_MONTHLY: &str = "https://public.bybit.com/spot/{sym}/";
pub const DEFAULT_ARCHIVE_URL_BYBIT_DAILY: &str = "https://public.bybit.com/spot/{sym}/";
pub const DEFAULT_ARCHIVE_URL_BYBIT_PROBE: &str =
    "https://public.bybit.com/trading/{sym}/{sym}{y:04}-{m:02}.csv.gz";

/// Default Bitget historical-archive URL prefix. Bitget exposes only a
/// daily (per-sequence) bucket — no monthly archives. Phase 59.R3.C3.O1
/// (2026-05-30).
pub const DEFAULT_ARCHIVE_URL_BITGET_MONTHLY: &str = "";
pub const DEFAULT_ARCHIVE_URL_BITGET_DAILY: &str =
    "https://img.bitgetimg.com/online/trades/SPBL/{sym}/";
pub const DEFAULT_ARCHIVE_URL_BITGET_PROBE: &str =
    "https://img.bitgetimg.com/online/trades/SPBL/{sym}/{ds}_{seq:03}.zip";

/// Default OKX historical-archive URL prefixes. OKX exposes both monthly
/// and daily traderecords buckets. Phase 59.R3.C3.O1 (2026-05-30).
pub const DEFAULT_ARCHIVE_URL_OKX_MONTHLY: &str =
    "https://static.okx.com/cdn/okex/traderecords/trades/monthly/{y:04}{m:02}/";
pub const DEFAULT_ARCHIVE_URL_OKX_DAILY: &str =
    "https://static.okx.com/cdn/okex/traderecords/trades/daily/{ds}/";
pub const DEFAULT_ARCHIVE_URL_OKX_PROBE: &str = "https://static.okx.com/cdn/okex/traderecords/trades/monthly/{y:04}{m:02}/{sym}-trades-{y:04}-{m:02}.zip";

/// Top-level wrapper matching the layout of `nxrates.yml`.
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct PipelineYml {
    #[serde(default)]
    pub cexs: CexsYml,
    pub series: SeriesYml,
    #[serde(default)]
    pub network: NetworkYml,
    #[serde(default)]
    pub server: ServerYml,
    /// Synth-pair registry. Currently:
    ///   - `initial_pairs`: launch synth-pair list (was: hardcoded
    ///     `sdk/rust/src/synth/pairs.rs::INITIAL_SYNTH_PAIRS`).
    #[serde(default)]
    pub synths: SynthsYml,
    /// Stablecoin allowlist for the FX/metal/stablecoin auto-cross
    /// triangulator (`core::triangulator::build_auto_cross_rules`).
    #[serde(default)]
    pub triangulation: TriangulationYml,
    /// Runtime tuning knobs for the forwarders, fx provider server, and core
    /// REST/WS layer. Phase 59.R3.C2.O5 (2026-05-30) — was hardcoded
    /// `const FORWARDER_HEARTBEAT_SECS / PROVIDER_STALE_SECS / FRAME_BUF_MAX
    /// / STALE_SECS / REFRESH_OFFSET_SECS / FLUSH_MS` in
    /// `crypto/src/main.rs`, `crypto/src/bin/fx.rs`,
    /// `core/src/server/{rest,mod,ws}.rs`.
    #[serde(default)]
    pub runtime: RuntimeYml,
    /// Oracle price relays (`nxr-oracle` forwarder). Keyed by market-provider
    /// name (must exist in `mitch/ids/market-providers.csv`, e.g. `pyth`).
    #[serde(default)]
    pub oracles: OraclesYml,
    /// cTrader Open API brokers consumed by `nxr-ctrader`. Absent = disabled.
    #[serde(default)]
    pub ctrader: CtraderYml,
    /// BTR DEX signed-quote endpoint (`/v1/quote/signed`). Absent = disabled.
    /// Domain binds to the DEPLOYED ExternalOracle (chain_id + address) —
    /// per-deployment config, never hardcoded. Signing key: env
    /// `NXR_SIGNER_KEY` + 32-byte-hex `NXR_COSIGN_SECRET` (both required when
    /// this block is present; never logged).
    #[serde(default)]
    pub signed_quotes: Option<SignedQuotesYml>,
}

/// `signed_quotes:` block — NXR-signed EIP-712 quote blobs for the BTR DEX
/// `ExternalOracle.batchPushSigned` relay path. Wire format is the FROZEN
/// spec `btr/dex/ORACLE_SIGNED_PUSH_SPEC.md` (24 B/feed packed records).
/// EIP-712 domain `name` default: the currently deployed consumer-contract
/// domain string. Kept as a per-deployment config default so the digest stays
/// byte-identical while no consumer brand is hardcoded in the signing path.
fn default_domain_name() -> String {
    "BTR ExternalOracle".to_string()
}

/// Label of the domain synthesized from the transitional singleton keys, and
/// the fallback `default_domain` when exactly one domain is declared.
pub const DEFAULT_DOMAIN_LABEL: &str = "default";

/// One allow-listed EIP-712 domain. Signers are CONSUMER AGNOSTIC: the domain
/// is a per-REQUEST parameter validated against this allow-list, never pod
/// identity, so one replica set serves every consumer.
#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct SignedDomainYml {
    /// EIP-712 domain `name`. Binds every signature to the consumer contract's
    /// domain string; changing it changes the digest.
    #[serde(default = "default_domain_name")]
    pub name: String,
    /// EIP-712 domain chainId.
    pub chain_id: u64,
    /// Deployed ExternalOracle address (0x-hex, 20 bytes) — EIP-712
    /// `verifyingContract`.
    pub oracle: String,
}

/// `#[serde(default)]` for a bool that must stay on when the YAML is silent.
fn default_true() -> bool {
    true
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct SignedQuotesYml {
    /// DEPRECATED and IGNORED. The blob's own `version` byte is the only
    /// layout discriminator, so a config enum would be a second mechanism.
    ///
    /// Accepted-and-ignored rather than removed: `SignedQuotesYml` is
    /// `deny_unknown_fields` and the live ConfigMaps (`nxr-signer-config`,
    /// `nxr-signer-ref-config`, `nxr-signer-arc-config`,
    /// `nxr-signer-sepolia-config`) still carry `record_format: idx24`, so
    /// removing the key crashloops the fleet on the image roll. Boot warns.
    /// REMOVE once no signer ConfigMap carries the key.
    #[serde(default)]
    pub record_format: Option<String>,
    /// Allow-listed EIP-712 domains, label → domain. A request names one via
    /// `?domain=<label>`; an unknown label is refused. Signing is consumer
    /// agnostic, so this is the ONLY place a consumer's domain is declared.
    #[serde(default)]
    pub domains: BTreeMap<String, SignedDomainYml>,
    /// Label served when a request names no domain. Optional when exactly one
    /// domain is declared.
    #[serde(default)]
    pub default_domain: Option<String>,
    /// DEPRECATED alias for a one-entry [`Self::domains`] map, labelled
    /// [`DEFAULT_DOMAIN_LABEL`]: the singleton domain that predates the
    /// per-request allow-list.
    ///
    /// It exists ONLY so one release can parse a not-yet-migrated ConfigMap.
    /// [`SignedQuotesYml`] is `deny_unknown_fields`, so without these fields
    /// the image and the ConfigMap can never be rolled in either order.
    /// REMOVE once every signer ConfigMap carries `domains`.
    #[serde(default)]
    pub oracle: Option<String>,
    #[serde(default)]
    pub chain_id: Option<u64>,
    #[serde(default)]
    pub domain_name: Option<String>,
    /// Required blob rebuild/cache floor in milliseconds. Must be in 1..=10
    /// so a newly observed provider tick is not hidden behind a stale cache.
    pub min_interval_ms: u64,
    /// Multi-timeframe Parkinson σ legs, in MINUTES, with optional per-leg
    /// weights. Default 6 h / 2 d / 1 w, EQUAL-weighted.
    ///
    /// Each leg is a Parkinson σ over the same 30 m bar and differs only in
    /// sample length, so the blend needs no annualisation. A leg that cannot
    /// reach the arming floor is DROPPED and the weights renormalise over the
    /// survivors; σ is never emitted from a short sample.
    ///
    /// Weights are EQUAL by design, not inverse-variance. Parkinson's sampling
    /// variance falls like `1/n`, so inverse-variance would give the 336-bar
    /// weekly leg ~28x the 12-bar 6 h leg and the blend would be the weekly
    /// window with extra steps: exactly the lag this replaces. On the money
    /// path σ is a risk premium with asymmetric loss (understating it is the
    /// attack; overstating only widens the band and is separately capped at
    /// co-sign), so responsiveness is bought deliberately with efficiency.
    /// Override per deployment if a venue justifies it.
    #[serde(default)]
    pub sigma_windows_min: Option<crate::mtf::MtfWindows>,
    /// DEPRECATED alias for [`Self::sigma_windows_min`]: the single 30 m
    /// Parkinson window, in BARS, that predates the MTF blend.
    ///
    /// It exists ONLY so one release can parse a not-yet-migrated ConfigMap.
    /// [`SignedQuotesYml`] is `deny_unknown_fields`, so without this field the
    /// image and the ConfigMap can never be rolled in either order: migrating
    /// the ConfigMap first crashloops the old image, rolling the image first
    /// crashloops the new one. Accepting both keys makes IMAGE-FIRST valid.
    ///
    /// REMOVE once all three signer ConfigMaps (`nxr-signer-config`,
    /// `nxr-signer-ref-config`, `nxr-signer-arc-config`) carry
    /// `sigma_windows_min` and no longer carry this key.
    #[serde(default)]
    pub sigma_lookback_bars: Option<u32>,
    /// Required maximum age in milliseconds of the last real upstream provider
    /// observation — the GLOBAL default freshness tier (sub-second CEX legs).
    /// Must be in 1..=500; heartbeat emits never advance it. Per-feed
    /// `max_age_ms` overrides it for slower-cadence sources (Pyth-Lazer).
    pub mark_max_age_ms: u64,
    /// Required minimum accepted provider count on every direct/bridge leg.
    /// Must be >= 2. Provider identity authentication is a separate layer.
    pub min_accepted_providers: u8,
    /// Required minimum composite freshness in bps (1..=10_000), derived from
    /// the Index confidence freshness byte on every direct/bridge leg.
    pub min_composite_freshness_bps: u16,
    /// Peer replicas for k-of-n co-signing (exclude self). Every URL is pinned
    /// to the exact signer address expected in its response.
    #[serde(default)]
    pub peers: Vec<SignedPeerYml>,
    /// Minimum total signatures per served quote (self + peers). Required and
    /// must be >= 2; a build below quorum fails closed (503).
    pub quorum: Option<u8>,
    /// Co-sign sourceTs forward-skew bound (ms) vs this replica's clock.
    /// Default 5000. Backward bound is `mark_max_age_ms`.
    #[serde(default)]
    pub cosign_max_skew_ms: Option<i64>,
    /// Maximum proposed sourceTs lead over a co-signer's own real provider
    /// observation (ms). Default 250, hard-capped at 500.
    #[serde(default)]
    pub provenance_tolerance_ms: Option<i64>,
    /// Signable catalog (per-feed policy; signing itself is universal). Every
    /// row carries an EXPLICIT `idx`, its 0-based ordinal in the consumer
    /// contract's append-only `feedIds[]`. Array position is NOT the ordinal
    /// and must never be used as one. `/v1/quote/signed/meta` publishes each
    /// `idx`; consumers subscribe via `GET /v1/quote/signed?idxs=…` (explicit
    /// idx values) or `?symbols=…`, and both absent defaults to this full list.
    pub feeds: Vec<SignedFeedYml>,
    /// Asset-level blacklist for UNIVERSAL signing. Any NON-catalog requested
    /// symbol whose base OR quote ASSET is listed here is refused (400) even
    /// when it resolves to a routable ticker. An entry may name the asset in
    /// any identifier form: its token SYMBOL (`USDF`, `BUSD`), its long human
    /// NAME, or its MITCH ASSET ID (decimal global id). Match is by asset
    /// identity, never by ticker id, so one entry covers every pair carrying
    /// that asset on either side, regardless of which exchange materialized it.
    /// Catalog feeds are NOT gated by this: an operator-declared feed carries
    /// its own explicit per-feed policy, so it may legitimately name a
    /// blacklisted asset. Empty (default) = universal signing for every
    /// routable symbol.
    #[serde(default)]
    pub blacklisted_assets: Vec<String>,
    /// LIGHT NODE MODE (opt-in). When `true`, the aggregator restricts
    /// its ticker universe to ONLY the symbols this signer must sign (every
    /// `feeds[].symbol`, plus each composed feed's LEGS found through the graph
    /// at sign time) instead of the full config universe (2000+ tickers). The
    /// UDP registry gate then drops all other
    /// frames, so the emit cycle + s10/renko/σ producers run for ~30 tickers,
    /// not thousands (~50-100x less CPU). σ for the signed symbols is
    /// bit-identical to the full-universe path (same per-ticker pipeline); only
    /// the ticker SET differs. `false` (default) keeps full-replica behavior.
    #[serde(default)]
    pub sign_only: bool,
    /// LIGHT NODE retention, in whole days of sealed shards to keep BESIDES
    /// today's (so `1` = yesterday + today = 2 files per ticker). Honored ONLY
    /// when `sign_only` is true: a full node's retention is the API contract
    /// (365 d `/v1/bars`) and is not this knob.
    ///
    /// This bounds the INDEX tree (`indexes/*.idx`), which is the tree that
    /// actually costs disk: one 56 B record per emit cycle per ticker. Measured
    /// 2026-07-25: with no cap at all a light node wrote 1.3 GiB in 14 h and
    /// grew forever, and the index is nearly all of it. Sigma reads `.s10`
    /// bars, never the index, so this stays at 1 day while the bar trees are
    /// retained for as long as the longest σ leg needs
    /// ([`Self::bars_retention_days`]).
    #[serde(default = "default_light_retention_days")]
    pub retention_days: u16,
}

/// The index tree is the disk hog and σ never reads it: 1 sealed day + today.
fn default_light_retention_days() -> u16 {
    1
}

/// Bar width the σ estimator rolls up to, in minutes.
pub const SIGMA_BAR_MIN: u32 = 30;

impl SignedQuotesYml {
    /// σ legs for this signer, defaulted to 6 h / 2 d / 1 w equal-weighted.
    ///
    /// Resolves the deprecated [`Self::sigma_lookback_bars`] alias: neither key
    /// gives the default blend, the alias alone gives the single pre-MTF window
    /// it always meant (48 bars x 30 m = 1440 min, weight 1.0, numerically the
    /// old single-window path), and `sigma_windows_min` wins when both are set.
    pub fn sigma_windows(&self) -> crate::mtf::MtfWindows {
        match (&self.sigma_windows_min, self.sigma_lookback_bars) {
            (Some(w), _) => w.clone(),
            (None, Some(bars)) => crate::mtf::MtfWindows::new(
                vec![bars.max(1).saturating_mul(SIGMA_BAR_MIN)],
                vec![1.0],
            ),
            (None, None) => crate::mtf::MtfWindows::default(),
        }
    }

    /// Allow-listed domains, resolving the deprecated singleton alias: the map
    /// wins when both are set, and the singletons alone synthesize the
    /// one-entry map they always meant. Empty here is a boot error, raised by
    /// the signer (which also parses the addresses and rejects aliasing
    /// separators), not silently defaulted.
    pub fn domain_map(&self) -> BTreeMap<String, SignedDomainYml> {
        if !self.domains.is_empty() {
            return self.domains.clone();
        }
        let Some(oracle) = self.oracle.clone() else {
            return BTreeMap::new();
        };
        BTreeMap::from([(
            DEFAULT_DOMAIN_LABEL.to_string(),
            SignedDomainYml {
                name: self.domain_name.clone().unwrap_or_else(default_domain_name),
                chain_id: self.chain_id.unwrap_or_default(),
                oracle,
            },
        )])
    }

    /// Label used when a request names no domain: the declared one, else the
    /// sole declared domain. `None` = ambiguous (several domains, no default),
    /// which the signer refuses at boot.
    pub fn default_domain_label(&self) -> Option<String> {
        if let Some(l) = &self.default_domain {
            return Some(l.clone());
        }
        let m = self.domain_map();
        (m.len() == 1).then(|| m.into_keys().next().expect("len 1"))
    }

    /// True when the deprecated alias is present alongside the current key, so
    /// the loader can name the offending file in its WARN.
    fn has_stale_sigma_alias(&self) -> bool {
        self.sigma_windows_min.is_some() && self.sigma_lookback_bars.is_some()
    }

    /// Whole days of `.s10` / `.renko` shards a light node must keep for the
    /// LONGEST configured σ leg to be able to fill, derived rather than tuned.
    ///
    /// Derived, not a free knob, but also not the expensive tree: `.s10` is
    /// 96 B x 8640 bars/day = 810 KiB/day/ticker, so a 26-feed signer holding
    /// 14 days of bars is ~290 MiB. The 1.3 GiB/14 h figure that forced the
    /// original 1-day cap is the INDEX tree, which keeps its own short window.
    ///
    /// The `+1` covers the partial day at the window's far edge: a window of
    /// exactly N days spans N+1 UTC shards unless it lands on midnight.
    ///
    /// SESSION TAPES: a leg is N REAL-tick bars, and FX trades ~5 days in 7, so
    /// 336 real 30 m bars span ~9-10 CALENDAR days, not 7. That is what the
    /// per-feed `sigma_window_bars` scan horizon declares, so the widest one
    /// configured is folded in here: retention can never be shorter than the
    /// scan a feed is allowed to ask for. This is why per-asset-class WINDOW
    /// SETS are not needed: the leg definition ("336 real bars") is universal,
    /// only the calendar horizon it is hunted over is asset-class specific.
    pub fn bars_retention_days(&self) -> u16 {
        let leg_days = self.sigma_windows().max_days(SIGMA_BAR_MIN);
        let scan_days = self
            .feeds
            .iter()
            .filter_map(|f| f.sigma_window_bars)
            .max()
            .map_or(0, |b| {
                (u64::from(b) * u64::from(SIGMA_BAR_MIN)).div_ceil(1_440) as u16
            });
        leg_days.max(scan_days).saturating_add(1)
    }

    /// The exact ticker-symbol set this signer must aggregate to sign: every
    /// feed symbol, uppercased/deduped. A composed feed's LEGS are found
    /// through the cross graph at sign time (via `sigma_key`'s route), not
    /// declared here, which is what makes the universe derived rather than a
    /// hand-maintained catalogue.
    pub fn signed_symbols(&self) -> std::collections::BTreeSet<String> {
        let mut s = std::collections::BTreeSet::new();
        for f in &self.feeds {
            s.insert(f.symbol.to_uppercase());
        }
        s
    }
}

/// One countersigning replica. `signer` is the expected ECDSA address recovered
/// from this URL's response; a different key never counts toward quorum.
#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct SignedPeerYml {
    pub url: String,
    pub signer: String,
}

/// One signed-quote feed: an NXR symbol this deployment may sign, plus its
/// signing policy and its on-chain ordinal.
#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct SignedFeedYml {
    /// This feed's ordinal in the consumer contract's append-only `feedIds[]`.
    /// 0-BASED as deployed (the Arc map starts at 0; the sepolia map is the
    /// misnumbered one), and an EXPLICIT value: it is NOT the row's array
    /// position and must never be derived from position. No longer a wire key
    /// (records are keyed by MITCH ticker id); it survives only as the
    /// `?idxs=` subscription selector and the keeper's slot binding.
    /// A tier signs a SUBSET of one contract's `feedIds[]`, so a configured
    /// set need be neither contiguous nor start at 0; only uniqueness is
    /// enforced (signed.rs boot). `None` = no on-chain slot: a universally
    /// synthesized feed, never selectable by `?idxs=`.
    pub idx: Option<u16>,
    /// NXR symbol whose mark/σ/CI back the feed (e.g. `BTC-USDC`).
    pub symbol: String,
    /// Required maximum deviation, in basis points, between a proposed mark
    /// and each co-signer's own live mark. Runtime policy accepts 0.01..=5.0.
    /// One basis point is 0.01%; for example, 0.25 bps is 0.0025%.
    pub cosign_tolerance_bps: f64,
    /// Per-feed freshness tier (ms) for THIS feed's own source leg, by source
    /// cadence: unset = global `mark_max_age_ms` (sub-second CEX books); a
    /// composed feed's legs stay on the global CEX bound. Bounds 1..=1500
    /// enforced at boot (signed.rs `validate_feed_max_age_ms`).
    #[serde(default)]
    pub max_age_ms: Option<u64>,
    /// DECLARED single-source allowance: this feed may be signed while only ONE
    /// provider leg is live (`active_count == 1`), instead of the default floor of
    /// `MIN_ACTIVE_PROVIDERS` (2).
    ///
    /// DECLARED, never inferred. The whole point is that a feed we have decided is
    /// legitimately single-source passes at 1, while a feed that silently DEGRADES
    /// from 2 legs to 1 still fails closed. Runtime leg counts must never grant this
    /// allowance to themselves.
    ///
    /// PEGGED-ONLY, enforced structurally at boot (`signed.rs` refuses a
    /// `single_source` feed whose symbol is not in the pegged class) — never by
    /// convention. See that validator for the safety argument.
    #[serde(default)]
    pub single_source: bool,
    /// When true, a failed `own_view` omits this feed from the signed blob instead
    /// of 503ing the whole quote. Required (non-optional) feeds still fail closed.
    /// Cosign accepts an ordered subsequence that includes every required feed.
    /// Use for newly armed assets (FX) so a dark leg cannot darken stables.
    #[serde(default)]
    pub optional: bool,
    /// Per-feed override for the LIVE ticking-leg floor (`MIN_ACTIVE_PROVIDERS`).
    /// `None` = global const (2). Set `1` for declared single-venue FX (e.g. Pyth
    /// AUD/USD) where `single_source` is illegal because the pair is not pegged.
    /// Bounds 1..=64 enforced at boot.
    #[serde(default)]
    pub min_active_providers: Option<u8>,
    /// Per-feed σ-understatement allowance (PBPS) at co-sign, overriding the
    /// global `SIGMA_UNDERSTATEMENT_TOL_PBPS` (100). A thin feed whose
    /// independent 30 m Parkinson windows legitimately diverge more than the
    /// absolute floor refuses every blob that includes it (XAUT, 2026-07-29:
    /// 3-5 % replica divergence at σ > 2500 PBPS). Bounds 100..=2000 enforced
    /// at boot (signed.rs); absent = global default.
    #[serde(default)]
    pub sigma_tol_pbps: Option<u32>,
    /// Per-feed DISK-SCAN horizon for the σ window, in 30 m units. `None` = the
    /// longest configured σ leg (`sigma_windows_min`, 336 bars = 7 d), which is
    /// a CRYPTO shape: it assumes the tape never stops, so 7 d always holds 336
    /// real bars.
    ///
    /// A session-traded instrument breaks that assumption. USD/BRL ticks only
    /// 12:00-21:00 UTC Mon-Fri (MEASURED over 2026-07-25..08-03: 9 h/day, 0
    /// bars at any weekend hour), so a 24 h scan holds ~18 real bars mid-week
    /// but ZERO from session close until the next open. σ then refuses for the
    /// first 4 h of every session (8 bars x 30 m) even though the tape is live
    /// and the mark is fresh: idx 27 was excluded as `mark_unavailable` for
    /// exactly that reason on 2026-08-03 12:00-16:00 UTC.
    ///
    /// This widens ONLY the scan horizon. The estimator still takes the newest
    /// `sigma_lookback_bars` REAL-tick bars (`tick_count > 0`), so dark slots
    /// are never counted and σ is not smoothed across a wider sample. Bounded
    /// by `retention_days` (7 d = 336) at boot: a scan cannot outrun the shards
    /// retention keeps.
    #[serde(default)]
    pub sigma_window_bars: Option<u32>,
    /// Publish the RECIPROCAL of `symbol`: the mark is `1/mid(symbol)` and the
    /// record carries the reciprocal pair's MITCH ticker id.
    ///
    /// Set where the feed is DENOMINATED in a pair NXR only serves the other
    /// way up. BTR declares that split per asset (`sdk/src/venues/nxr.ts`
    /// `NXR_MARKS`: `nxrSymbol` = what the record means, `nxrQuote` = the pair
    /// actually served), and its generator emits `invert: true` for exactly the
    /// four USD-base FX legs whose `nxrQuote` is set: `USD-CAD`, `USD-BRL`,
    /// `USD-JPY`, `USD-KRW`. Dropping the flip there publishes 1.387 into a
    /// CAD/USD slot: a plausible number, upside down.
    ///
    /// σ and CI are RELATIVE (log-return σ, bps confidence) and invariant under
    /// inversion, so only the mid flips.
    #[serde(default)]
    pub invert: bool,
}

/// `oracles:` block — Pyth Pro (Lazer) push providers consumed by the
/// `nxr-oracle` forwarder.
#[derive(Debug, Clone, Default, Deserialize, Serialize)]
pub struct OraclesYml {
    /// Oracle forwarder flush cadence (ms). Default 250 when absent.
    #[serde(default)]
    pub aggregation_interval_ms: Option<u64>,
    #[serde(default)]
    pub providers: BTreeMap<String, OracleProviderYml>,
}

/// `oracles.providers.<name>:` — one Lazer subscription + its feed manifest.
/// Bearer token comes from env `NXR_ORACLE_TOKEN_<NAME>`; endpoint override
/// from env `NXR_ORACLE_URL_<NAME>` (comma-separated).
#[derive(Debug, Clone, Default, Deserialize, Serialize)]
pub struct OracleProviderYml {
    /// Canonical "BASE/QUOTE" → DECIMAL Lazer feed id. Gated / coming-soon
    /// feeds may be listed: the server refuses them per-feed
    /// (ignoreInvalidFeeds) and the forwarder resubscribes hourly, so a
    /// state/entitlement flip onboards without a restart.
    #[serde(default)]
    pub symbols: BTreeMap<String, String>,
    /// Subscription channel (e.g. "fixed_rate@200ms"). NEVER `real_time`:
    /// only ~30 stable feeds carry it; `ignoreInvalidFeeds` silently drops
    /// the rest (verified 2026-07-12 profiling).
    #[serde(default)]
    pub channel: String,
    /// Stream endpoints, ALL consumed concurrently with first-arrival dedup
    /// on `feedUpdateTimestamp` (any endpoint may die; no gap).
    #[serde(default)]
    pub urls: Vec<String>,
}

/// `ctrader:` block — cTrader Open API spot providers consumed by the
/// `nxr-ctrader` forwarder. One entry per broker (IC Markets, Pepperstone,
/// Tickmill are all cTrader brokers and share one Spotware application).
#[derive(Debug, Clone, Default, Deserialize, Serialize)]
pub struct CtraderYml {
    /// Forwarder flush cadence (ms). Default 250 when absent.
    #[serde(default)]
    pub aggregation_interval_ms: Option<u64>,
    #[serde(default)]
    pub providers: BTreeMap<String, CtraderProviderYml>,
}

/// `ctrader.providers.<name>:` — one broker session + its symbol manifest.
/// `<name>` must resolve in `mitch/ids/market-providers.csv`.
/// Secrets come from env, never YAML: `NXR_CTRADER_CLIENT_ID`,
/// `NXR_CTRADER_CLIENT_SECRET`, `NXR_CTRADER_ACCESS_TOKEN` for `openapi`, and
/// `NXR_CTRADER_FIX_PASSWORD` for `fix`, each with an optional `_<NAME>`
/// suffixed override (the app credentials are shared across brokers; the
/// access token and the FIX password are per account).
#[derive(Debug, Clone, Default, Deserialize, Serialize)]
pub struct CtraderProviderYml {
    /// Inbound transport: `openapi` (Open API WS, needs an approved Spotware
    /// application) or `fix` (FIX 4.4 QUOTE session, credentials self-served
    /// from the platform UI). Defaults to `openapi`.
    #[serde(default)]
    pub transport: String,
    /// FIX gateway host. Not published: it ships with the FIX credentials.
    #[serde(default)]
    pub fix_host: String,
    /// FIX gateway port, likewise credential-supplied.
    #[serde(default)]
    pub fix_port: u16,
    /// TLS on the FIX socket. Defaults to true; only a broker that publishes a
    /// plaintext gateway should ever set it false.
    #[serde(default = "default_true")]
    pub fix_tls: bool,
    /// SenderCompID exactly as the credentials page shows it:
    /// `<environment>.<brokerUID>.<traderLogin>` (e.g. `live.theBroker.12345`).
    /// TargetCompID (CSERVER) and the QUOTE sub-ids are fixed by the protocol.
    #[serde(default)]
    pub fix_sender_comp_id: String,
    /// Canonical "BASE/QUOTE" → broker-side cTrader symbol name (e.g.
    /// "XAU/USD" → "XAUUSD"). Symbol IDs are NOT configured: they differ per
    /// broker, so they are resolved at connect via ProtoOASymbolsListReq.
    #[serde(default)]
    pub symbols: BTreeMap<String, String>,
    /// Endpoint host: `demo.ctraderapi.com` or `live.ctraderapi.com`.
    /// Demo and live are fully separate account systems.
    #[serde(default)]
    pub host: String,
    /// `traderLogin` of the account to authenticate (the login shown in the
    /// cTrader UI). Selects one `ctidTraderAccountId` out of the token grant.
    #[serde(default)]
    pub trader_login: i64,
}

/// `runtime:` block — forwarder + server tuning knobs. All `Option<…>`
/// fields fall back to their per-callsite `DEFAULT_RUNTIME_*` constant when
/// the YAML is silent (so old `config.yml` files keep working).
#[derive(Debug, Clone, Default, Deserialize, Serialize)]
pub struct RuntimeYml {
    /// Forwarder heartbeat cadence (seconds). Was: hardcoded
    /// `FORWARDER_HEARTBEAT_SECS = 5` in `crypto/src/main.rs:34`.
    #[serde(default)]
    pub forwarder_heartbeat_secs: Option<u64>,
    /// REST `/health` stale-forwarder threshold (seconds). Was: hardcoded
    /// `STALE_SECS = 20` in `core/src/server/rest.rs:786`.
    #[serde(default)]
    pub health_stale_secs: Option<u64>,
    /// Daily-refresh wait offset past UTC midnight (seconds). Was: hardcoded
    /// `REFRESH_OFFSET_SECS = 30` in `core/src/server/mod.rs:232`.
    #[serde(default)]
    pub daily_refresh_offset_secs: Option<u64>,
    /// WS flush cadence (ms). Was: hardcoded `FLUSH_MS = 100` in
    /// `core/src/server/ws.rs:92`.
    #[serde(default)]
    pub ws_flush_ms: Option<u64>,
}

/// Per-callsite runtime defaults. Mirror the prior `const` values so
/// YAML-silent configs preserve audit-frozen behaviour.
pub const DEFAULT_RUNTIME_FORWARDER_HEARTBEAT_SECS: u64 = 5;
pub const DEFAULT_RUNTIME_PROVIDER_STALE_SECS: u64 = 30;
pub const DEFAULT_RUNTIME_FRAME_BUF_MAX: usize = 2 * 1024 * 1024;
pub const DEFAULT_RUNTIME_HEALTH_STALE_SECS: u64 = 20;
pub const DEFAULT_RUNTIME_DAILY_REFRESH_OFFSET_SECS: u64 = 30;
pub const DEFAULT_RUNTIME_WS_FLUSH_MS: u64 = 200;

/// `synths:` block — optional manual override for synth-pipeline pairs.
/// Leave `initial_pairs` empty: the live kernel derives crosses from
/// `cexs.cross_pairs` via [`crate::synth::cross_expand`].
#[derive(Debug, Clone, Default, Deserialize, Serialize)]
pub struct SynthsYml {
    #[serde(default)]
    pub initial_pairs: Vec<SynthPairYml>,
}

/// YAML-side mirror of [`crate::synth::pairs::SynthPairSpec`] — owned strings
/// so deserialization works without `'static` lifetime gymnastics.
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct SynthPairYml {
    pub synth_sym: String,
    pub base_sym: String,
    pub quote_sym: String,
}

/// `triangulation:` block — the ONE piece of the auto-cross-triangulation
/// eligibility test that can't be derived from existing ticker_id metadata.
/// FX (any currency pair) and CM-spot (metals/commodities, excluding futures
/// like expirable gold contracts) are both auto-detected from the ticker_id's
/// own asset-class + instrument-type bits (see
/// `core::triangulator::build_auto_cross_rules`) — zero config needed there.
/// Stablecoins are all `AssetClass::CR` (same class as BTC/ETH/...), which
/// carries no sub-class distinguishing "pegged to USD" from "not" - hence
/// this explicit, small, YAML-owned list rather than a Rust-hardcoded one.
#[derive(Debug, Clone, Default, Deserialize, Serialize)]
pub struct TriangulationYml {
    /// Base symbols (no "/USD" suffix) of USD-pegged stablecoins eligible for
    /// automatic cross-triangulation against every other eligible leg
    /// (FX majors, metals, other stablecoins). Each must already be
    /// registered as `<SYM>/USD` under some oracle provider's `symbols:`.
    #[serde(default)]
    pub stablecoins: Vec<String>,
}

/// Source of the default `NXR_CONFIG` fallback path. Determines what
/// the resolver returns when the env var is unset.
///
/// - [`ConfigHint::Runtime`] — long-running cluster services (core sink,
///   crypto forwarders, REST/WS server). Default = `/etc/nxr/config.yml`
///   (matches container layout where the chart mounts the values yml).
/// - [`ConfigHint::Bin`] — offline tools / one-shot binaries
///   (`nxr_calibrate`, `merge_idx`, `mtf_sweep`, …). Default = `./config.yml`
///   (matches dev workflow: `cd nx-rates && cargo run --bin …`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConfigHint {
    Runtime,
    Bin,
}

impl ConfigHint {
    /// Default path applied when `NXR_CONFIG` is unset.
    pub const fn default_path(self) -> &'static str {
        match self {
            ConfigHint::Runtime => "/etc/nxr/config.yml",
            ConfigHint::Bin => "config.yml",
        }
    }
}

impl PipelineYml {
    /// Every symbol the config declares directly: `cexs.cross_pairs` plus every
    /// `oracles.providers.<p>.symbols` key, uppercased. Does NOT include derived
    /// auto-cross outputs (those come from `build_auto_cross_rules`, whose owner
    /// is the aggregator) nor the `NXR_SYMBOLS` env base list.
    ///
    /// SINGLE SOURCE for "what symbols does this deployment declare". Both
    /// `core`'s `register_config_symbols` (REST/WS resolution + the UDP registry
    /// gate) and `nxr-calibrate`'s roster read it, so the set a ticker must be in
    /// to be *served* can never drift from the set it must be in to be
    /// *calibrated*. That drift is exactly what hid 17 of the 23 DEX pool assets
    /// from calibration: the calibrator keyed its roster off `pair_volumes` (CEX
    /// volume), so Pyth-only stables/metals/FX — which have full tick history and
    /// need no volume for the fit — were never candidates (found 2026-07-25).
    pub fn configured_symbols(&self) -> std::collections::BTreeSet<String> {
        let mut out = std::collections::BTreeSet::new();
        for sym in &self.cexs.cross_pairs {
            out.insert(sym.to_uppercase());
        }
        for prov in self.oracles.providers.values() {
            for sym in prov.symbols.keys() {
                out.insert(sym.to_uppercase());
            }
        }
        // Broker symbols count too: this set is what the aggregator admits, so a
        // section missing here is silently dropped as `unknown_ticker` no matter
        // how healthy the forwarder is.
        for prov in self.ctrader.providers.values() {
            for sym in prov.symbols.keys() {
                out.insert(sym.to_uppercase());
            }
        }
        out
    }

    /// Every symbol a RELAY forwarder observes directly: `oracles.providers.*`
    /// (`nxr-oracle`) plus `ctrader.providers.*` (`nxr-ctrader`). This is
    /// `configured_symbols()` minus the cross-only remainder of
    /// `cexs.cross_pairs`, and it is the half that gets a persisted `.idx`.
    ///
    /// Used by the offline tools that must not touch a cross: a cross is a pure
    /// function of its legs, is composed on read, and is gated out of the sink
    /// by `core::aggregator::append_idx_unless_composed`, so backfilling one or
    /// reporting its absent `.idx` as a fault is always wrong. A relay symbol
    /// that is ALSO listed as a cross stays here: observed beats derived, the
    /// same precedence `composed_gate_set` applies.
    pub fn relay_symbols(&self) -> std::collections::BTreeSet<String> {
        let mut out = std::collections::BTreeSet::new();
        for prov in self.oracles.providers.values() {
            for sym in prov.symbols.keys() {
                out.insert(sym.to_uppercase());
            }
        }
        for prov in self.ctrader.providers.values() {
            for sym in prov.symbols.keys() {
                out.insert(sym.to_uppercase());
            }
        }
        out
    }

    /// Relay provider names (`oracles.providers` ∪ `ctrader.providers` keys).
    /// Each must resolve in `mitch/ids/market-providers.csv`, so a per-provider
    /// staging tree (`indexes/<provider>/…`) is named after this key exactly the
    /// way `cexs.exchanges` keys are. Offline re-aggregation needs the union:
    /// keying only off `cexs.exchanges` makes an oracle- or broker-sourced
    /// ticker look sourceless.
    pub fn relay_providers(&self) -> Vec<String> {
        self.oracles
            .providers
            .keys()
            .chain(self.ctrader.providers.keys())
            .cloned()
            .collect()
    }

    /// Read and parse a pipeline-yaml file from disk. Single source of truth
    /// for the 6+ `serde_yaml::from_str(&fs::read_to_string(p)?)?` callsites
    /// in `series-factory/src/bin/*`. Uses `serde_yml` (the maintained fork);
    /// schema is forward-compatible with serde_yaml-emitted files because
    /// only the `Deserialize` derives are exercised.
    pub fn load(path: &std::path::Path) -> anyhow::Result<Self> {
        use anyhow::Context;
        let s = std::fs::read_to_string(path)
            .with_context(|| format!("read pipeline yaml {}", path.display()))?;
        let parsed = serde_yml::from_str::<Self>(&s)
            .with_context(|| format!("parse pipeline yaml {}", path.display()))?;
        if parsed
            .signed_quotes
            .as_ref()
            .is_some_and(SignedQuotesYml::has_stale_sigma_alias)
        {
            tracing::warn!(
                config = %path.display(),
                "signed_quotes carries BOTH sigma_windows_min and the DEPRECATED \
                 sigma_lookback_bars: sigma_windows_min wins and the stale key is \
                 ignored — delete sigma_lookback_bars from this file"
            );
        }
        Ok(parsed)
    }

    /// Resolve the canonical NXR config path: env `NXR_CONFIG` if set,
    /// else `hint.default_path()`. Single SDK home for the 8+ ad-hoc
    /// `std::env::var("NXR_CONFIG").unwrap_or_else(...)` callsites in
    /// `crypto/`, `core/`, and `series-factory/bin/*`.
    pub fn resolve_path(hint: ConfigHint) -> std::path::PathBuf {
        std::env::var("NXR_CONFIG")
            .map(std::path::PathBuf::from)
            .unwrap_or_else(|_| std::path::PathBuf::from(hint.default_path()))
    }

    /// Load using the canonical NXR_CONFIG resolution policy. Phase 59.R3.L1.
    pub fn load_default(hint: ConfigHint) -> anyhow::Result<Self> {
        Self::load(&Self::resolve_path(hint))
    }
}

/// `cexs.exchanges.<name>:` per-exchange metadata. New fields are wholly
/// optional so legacy configs (`mitch_id` + `weight` + `cmc_slug` only) keep
/// parsing. WS / REST URLs were hardcoded in
/// `crypto/src/exchange/*.rs` until phase 59.R2C.4; once moved to YAML
/// the per-handler struct factory reads them at startup.
#[derive(Debug, Clone, Default, Deserialize, Serialize)]
pub struct ExchangeYml {
    #[serde(default)]
    pub mitch_id: Option<u16>,
    #[serde(default)]
    pub cmc_slug: Option<String>,
    #[serde(default)]
    pub weight: Option<f64>,
    /// WebSocket endpoint (audit-frozen URL — preserved from prior hardcode).
    #[serde(default)]
    pub ws_url: Option<String>,
    /// REST markets / exchangeInfo endpoint (audit-frozen).
    #[serde(default)]
    pub rest_url: Option<String>,
    /// Per-exchange quote-asset suffix list used by `normalize_symbol`
    /// (suffix-split mode). Was hardcoded in 13 adapters
    /// (`crypto/src/exchange/{binance,bybit,bitget,...}.rs::quote_suffixes`).
    /// Empty ⇒ fall back to `nxr_sdk::resolve::DEFAULT_QUOTE_SUFFIXES`.
    /// Phase 59.R3.H1 (2026-05-30).
    #[serde(default)]
    pub quote_suffixes: Vec<String>,
    /// Fallback ticker list used by `parse_markets_response` when REST returns
    /// a malformed / geo-blocked response. Was hardcoded in
    /// `crypto/src/exchange/{bullish,bitunix,weex}.rs` (phase 59.R3.C2.O2,
    /// 2026-05-30). Empty ⇒ adapter receives an empty market list (failing
    /// loudly is preferable to silently shipping a stale audit-frozen set).
    #[serde(default)]
    pub fallback_markets: Vec<String>,
    /// Per-exchange symbol-alias map for `normalize_symbol` / `format_symbol`.
    /// Keyed by the exchange-native code → canonical NXR code (e.g. Kraken
    /// `XBT → BTC`, Bitfinex `UST → USDT`). `format_symbol` walks the same
    /// map in reverse. Was hardcoded `.replace("XBT","BTC")` and
    /// `("UST","USDT")` literal arrays in `kraken.rs` / `bitfinex.rs`
    /// (phase 59.R3.C2.O3, 2026-05-30). Distinct from the top-level
    /// `cexs.aliases` map which the weights scraper uses globally.
    #[serde(default)]
    pub aliases: BTreeMap<String, String>,
    /// Historical-archive URL template(s) used by `series-factory` sources.
    /// Was hardcoded in `series-factory/src/sources/{binance,bybit}.rs` and
    /// `series-factory/src/bin/fetch_crypto_history.rs` (phase 59.R3.C2.O4,
    /// 2026-05-30). `None` ⇒ fall back to the per-source `DEFAULT_ARCHIVE_URL_*`
    /// const.
    #[serde(default)]
    pub archive_url_template: Option<ArchiveUrlTemplate>,
    /// Canonical "BASE/QUOTE" pairs to drop from this exchange's resolved
    /// symbol list even when present in both `NXR_SYMBOLS` and the
    /// exchange's own live markets response. For cross-exchange ticker
    /// collisions: a short/generic base symbol (e.g. "U") can denote two
    /// completely unrelated assets on different exchanges (confirmed
    /// 2026-07-12: Bybit's & XT.com's "U" is an unrelated ~$0.0003 token,
    /// not United Stables the ~$1 stablecoin every other connected exchange
    /// lists under the same ticker) - mixing them into one VWAP composite
    /// silently corrupts the aggregate. Checked in `client.rs::get_symbols`
    /// after the existing supported-set intersection.
    #[serde(default)]
    pub exclude_symbols: Vec<String>,
    /// Per-provider inbound-silence watchdog threshold, seconds. Applied
    /// PER SUBSCRIBED SYMBOL: a pair whose ticker has been silent this long,
    /// while the session keeps streaming other pairs, has lost its subscription
    /// venue-side and is silently shipping stale marks rather than showing an
    /// outage (kraken, documented 2026-07-22 in
    /// `ops/k0s/services/nxr-config-configmap.yaml:1278-1284`, then again
    /// 2026-07-25: three stablecoin legs frozen at the subscribe instant for
    /// 10.7 h with zero log lines). Enforced in
    /// `crypto/src/client.rs::connect_and_process`: some symbols dark ⇒
    /// unsubscribe+resubscribe those pairs only; all dark ⇒ reconnect with
    /// backoff. `None` ⇒ `nxr_crypto::exchange::DEFAULT_STALE_TIMEOUT_MS`, `0`
    /// disables. Raise for genuinely low-volume venues whose top-of-book can
    /// legitimately sit unchanged for minutes; never set below the venue's own
    /// `heartbeat_interval_ms`, or the watchdog will thrash.
    #[serde(default)]
    pub stale_timeout_s: Option<u64>,
    /// Hard ceiling on how many markets this venue may be subscribed to at once.
    /// Bounds the weights-driven subscription set (`crypto/src/markets.rs`),
    /// which grows with whatever `nxr-weights` last observed and is therefore
    /// not bounded by anything in this repo. WS gateways cap subscriptions per
    /// connection, so an unbounded set is a silent venue, not a rich one.
    /// `None` ⇒ `nxr_crypto::exchange::DEFAULT_MAX_SUBSCRIPTIONS`.
    #[serde(default)]
    pub max_subscriptions: Option<usize>,
}

/// Per-exchange historical-archive URL prefixes. Each field is the URL stem
/// up to (and including) the trailing `/`; the adapter appends
/// `{filename}` (already `format!`-built with sym/date) to compose the full
/// download URL. `None` fields fall back to the per-source default const.
///
/// `monthly` / `daily` are used by the bulk-fetch adapters
/// (`sources/{binance,bybit}.rs`); `probe` is used by `fetch_crypto_history`
/// to HEAD-probe coverage (bybit's probe path differs from its bulk path).
#[derive(Debug, Clone, Default, Deserialize, Serialize)]
pub struct ArchiveUrlTemplate {
    /// Monthly archive URL prefix. Interpolates `{sym}` for the symbol token.
    /// Example: `https://data.binance.vision/data/spot/monthly/aggTrades/{sym}/`.
    #[serde(default)]
    pub monthly: Option<String>,
    /// Daily archive URL prefix. Interpolates `{sym}`.
    #[serde(default)]
    pub daily: Option<String>,
    /// Probe URL template — used by `fetch_crypto_history` coverage probe.
    /// Interpolates `{sym}`, `{y}`, `{m}` (zero-padded year/month).
    #[serde(default)]
    pub probe: Option<String>,
}

/// `network:` block — UDP + listener cfg. Today these knobs are env-driven via
/// `NxrConfig::from_env()`; the schema is here so `PipelineYml::load` accepts a
/// YAML that documents the values. Reading them at runtime stays env-first
/// (operator policy: per-pod override without YAML re-render).
#[derive(Debug, Clone, Default, Deserialize, Serialize)]
pub struct NetworkYml {
    #[serde(default)]
    pub listen_port: Option<u16>,
    #[serde(default)]
    pub aggregation_interval_ms: Option<u64>,
    #[serde(default)]
    pub stale_threshold_ms: Option<u64>,
    #[serde(default)]
    pub heartbeat_interval_ms: Option<u64>,
    #[serde(default)]
    pub server_host: Option<String>,
    #[serde(default)]
    pub server_port: Option<u16>,
    #[serde(default)]
    pub multicast_addr_a: Option<String>,
    #[serde(default)]
    pub multicast_port_a: Option<u16>,
    #[serde(default)]
    pub multicast_addr_b: Option<String>,
    #[serde(default)]
    pub multicast_port_b: Option<u16>,
    /// Bar feeds — distinct multicast legs for s10 / renko / synth flavors.
    /// Was: `core::bars_s10::MCAST_S10_*` + `core::bars_renko::MCAST_RENKO_*`
    /// (phase 59.R2C.6).
    #[serde(default)]
    pub bars: BarsMcastYml,
    /// Authenticated forwarder -> aggregator ingress. When `signed_quotes` is
    /// configured this block is mandatory and raw MITCH UDP is refused.
    #[serde(default)]
    pub udp_auth: Option<UdpAuthYml>,
}

/// Versioned HMAC envelope policy for the private UDP ingest leg. Key bytes
/// are read from the named environment variables and never stored in YAML.
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct UdpAuthYml {
    pub max_age_ms: u64,
    pub max_future_ms: u64,
    /// Sliding replay window (1..=1024 sequence numbers). MUST exceed
    /// `max_age_ms` x peak frame rate, else a duplicate that is still fresh
    /// enough to pass the staleness gate has already fallen out of the window.
    pub replay_window: u16,
    pub keys: Vec<UdpAuthKeyYml>,
    /// Unauthenticated-frame disposition. Defaults to `strict` so an operator
    /// can never silently weaken ingest by omitting the field.
    #[serde(default)]
    pub mode: UdpAuthMode,
    /// One-shot staleness grace, in ms, for the backlog the ingest socket queues
    /// across the core's own boot (the socket is bound before the boot gates, so
    /// a restart leaves frames in `SO_RCVBUF` that are legitimately older than
    /// the drain instant). Without it, sealing forwarders would have every
    /// drained frame rejected as `stale`.
    ///
    /// Defaults to 0 = mechanism off, so it can never be enabled by omission.
    /// **Bounded by the replay bitmap** and asserted at boot — see
    /// `udp_auth::max_drain_grace_ms`. It does NOT weaken anti-replay: the
    /// bitmap, not the freshness bound, is the control. Raise `replay_window` in
    /// the SAME change if you raise this.
    #[serde(default)]
    pub drain_grace_ms: u64,
}

/// How the aggregator treats a datagram that carries NO `NXR1` envelope.
///
/// This knob exists ONLY to make the forwarder cutover zero-downtime. Core and
/// the forwarders cannot switch atomically: enabling auth on core first would
/// reject every raw frame in flight, and sealing forwarders first would have
/// core decode the 26 B header as MITCH. `Permissive` bridges that window.
///
/// It NEVER weakens the sealed path: a datagram beginning with `NXR1` is fully
/// verified (tag, freshness, replay, provider authority) in BOTH modes, and a
/// bad tag is rejected rather than retried as legacy. The only difference is
/// whether an envelope-less frame is admitted.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Deserialize, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum UdpAuthMode {
    /// Envelope-less datagrams are REJECTED. The real invariant; the only mode
    /// permitted once `signed_quotes` is armed.
    #[default]
    Strict,
    /// Envelope-less datagrams are accepted as legacy raw MITCH. TRANSITIONAL
    /// ONLY -- ingest is spoofable exactly as it was before udp_auth existed.
    Permissive,
}

/// One independently rotatable forwarder credential and its exact MITCH
/// provider authority. Provider 0 must be listed to authorize heartbeats.
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct UdpAuthKeyYml {
    pub key_id: u16,
    pub key_env: String,
    pub allowed_provider_ids: Vec<u16>,
}

/// `network.bars:` — per-feed multicast addr+port. Empty fields fall back to
/// the audit-frozen const values to keep deploy stable when YAML is silent.
/// 2026-06-01: synth shares the same leg as native (operator: "s10_synth and
/// s10 are the same object") — only 2 legs total now (s10, renko).
#[derive(Debug, Clone, Default, Deserialize, Serialize)]
pub struct BarsMcastYml {
    #[serde(default)]
    pub s10_addr: Option<String>,
    #[serde(default)]
    pub s10_port: Option<u16>,
    #[serde(default)]
    pub renko_addr: Option<String>,
    #[serde(default)]
    pub renko_port: Option<u16>,
}

/// `server:` block — REST/WS layer cfg. Phase 59.R2C.5 moved
/// `core::server::security` defaults here.
#[derive(Debug, Clone, Default, Deserialize, Serialize)]
pub struct ServerYml {
    /// Default CORS origins applied when `NXR_CORS_ORIGINS` is unset. Was:
    /// `core::server::security::DEFAULT_CORS_ORIGINS`.
    #[serde(default)]
    pub cors_origins: Vec<String>,
    /// Per-IP token-bucket + global concurrency caps. Was:
    /// `core::server::security::{DEFAULT_MAX_CONCURRENCY, DEFAULT_RL_BURST,
    /// DEFAULT_RL_PER_SEC}`.
    #[serde(default)]
    pub rate_limits: RateLimitsYml,
}

#[derive(Debug, Clone, Default, Deserialize, Serialize)]
pub struct RateLimitsYml {
    #[serde(default)]
    pub max_concurrency: Option<usize>,
    #[serde(default)]
    pub burst: Option<u32>,
    #[serde(default)]
    pub per_sec: Option<u64>,
}

/// `cexs:` block — exchange + asset metadata + classification lists.
///
/// CANONICAL HOME for soft / changeable asset lists. All previously
/// hardcoded `const` arrays in `core/`, `weights/`, `series-factory/bin/*`
/// now read from these fields (operator mandate 2026-05-29: NO hardcoded
/// vars; consolidated config). Empty `Vec` = falls back to sdk default
/// when callsite needs one (e.g. bridge_stables defaults to FNV-frozen
/// audit list).
#[derive(Debug, Clone, Default, Deserialize, Serialize)]
pub struct CexsYml {
    /// Min 24h USD volume for a volatile asset to be eligible (weights scraper).
    /// Was: `weights::config::default_min_volatile`.
    #[serde(default)]
    pub min_volume_volatile_usd: Option<f64>,
    /// Min 24h USD volume for a stablecoin to be eligible (weights scraper).
    /// Stablecoin lower bar is intentional (most stable liquidity is on-chain).
    /// Was: `weights::config::default_min_stable`.
    #[serde(default)]
    pub min_volume_stable_usd: Option<f64>,
    /// Min 24h USD volume threshold used by `core::weights` injection-rule
    /// builder (`load_file`) to decide whether to emit a synth injection for
    /// a stablecoin-quoted or USD-quoted pair. Was: hardcoded `100_000.0`
    /// literal at `core/src/weights.rs:190,211` (phase 59.R3.C2.O1,
    /// 2026-05-30). Distinct from the scraper's eligibility threshold above.
    /// `None` ⇒ falls back to [`DEFAULT_MIN_VOLUME_INJECTION_USD`].
    #[serde(default)]
    pub min_volume_injection_usd: Option<f64>,
    /// Weights scraper poll cadence (minutes). Was:
    /// `weights::config::default_scrape_interval`.
    #[serde(default)]
    pub scrape_interval_minutes: Option<u64>,
    /// Path to the rendered weights file consumed by the NXR collector.
    /// Was: `weights::config::default_weights_path`.
    #[serde(default)]
    pub weights_path: Option<String>,
    /// Ticker alias map: non-standard → canonical (e.g. `XBT → BTC`). Was:
    /// `weights::config::Cexs::aliases`.
    #[serde(default)]
    pub aliases: BTreeMap<String, String>,
    /// USD-pegged subset of `assets`: a PRICE property, not universe membership.
    /// Selects the tight ingest band plus absolute peg anchor, the peg-tight
    /// signed-quote ci ceiling, and it is the only thing that makes
    /// `signed_quotes.single_source` legal on a feed. Distinct from
    /// `triangulation.stablecoins`, which selects auto-cross leg eligibility.
    #[serde(default)]
    pub pegged: Vec<String>,
    /// Bridge-quoted stables (subset of `pegged` used for synth-USD
    /// derivation). If empty, callers fall back to `pegged`.
    #[serde(default)]
    pub bridge_stables: Vec<String>,
    /// USD fiat quote aliases recognized by the synth-injection builder as
    /// the "raw USD" leg (distinct from on-chain stables). Was: hardcoded
    /// `"USD"` literal at `core/src/weights.rs:226` (phase 59.R3.C5.A3,
    /// 2026-05-30). When the quote is one of these, the builder emits a
    /// `target × bridge_stable/USD` (inverted) injection rule.
    /// Defaults to `["USD"]` if YAML key is absent.
    #[serde(default)]
    pub usd_aliases: Vec<String>,
    /// Major-cap crypto asset symbols (base side). Was:
    /// `series_factory/{nxr_calibrate,renko_trailing,mtf_sweep}::CRYPTO_MAJORS`.
    #[serde(default)]
    pub crypto_majors: Vec<String>,
    /// FX major currency symbols. Was: `series_factory::nxr_calibrate::FX_MAJORS`.
    #[serde(default)]
    pub fx_majors: Vec<String>,
    /// All scrape-able assets (input list for the weights scraper).
    #[serde(default)]
    pub assets: Vec<String>,
    /// Cross-currency / cross-base crypto pairs forwarded by upstream providers
    /// (e.g. `BTC/EUR`, `ETH/BTC`, `KZT/USDT`). Was: `core::main::CRYPTO_CROSS_PAIRS`
    /// (phase 59.R2C.2). The core sink pre-registers ticker IDs for each pair
    /// in `symbol_map` so they round-trip through the REST/WS API.
    #[serde(default)]
    pub cross_pairs: Vec<String>,
    /// Per-exchange metadata. Keyed by lowercase exchange name (e.g. `binance`).
    /// Mitch IDs canonical here (phase 59.R2C.3 dropped the Rust mirror).
    /// URLs canonical here (phase 59.R2C.4 dropped the per-handler hardcode).
    #[serde(default)]
    pub exchanges: BTreeMap<String, ExchangeYml>,
    /// Volume scraper config — URLs + intervals. Was: `weights::scraper::CMC_PAIRS_URL`.
    #[serde(default)]
    pub scraper: ScraperYml,
    /// Per-asset market ranking + storage denomination
    /// (`docs/internal/storage-quote.md`).
    #[serde(default)]
    pub storage: StorageYml,
}

/// `cexs.storage:` block: per-asset market ranking, weight caps and the published
/// storage denomination. Genuinely YAML-sourced: no env indirection, no Rust
/// literal that outranks the file (the mistake `max_weight_per_source` and
/// friends still carry).
///
/// Serde ignores unknown keys, so a ConfigMap still carrying the retired
/// `pivot:` key falls back to defaults rather than failing: roll the ConfigMap
/// with the image that reads `storage:`.
#[derive(Debug, Clone, Default, Deserialize, Serialize)]
pub struct StorageYml {
    /// Markets aggregated per asset (top-N by volume).
    #[serde(default)]
    pub max_markets_per_asset: Option<usize>,
    /// Trust floor per market, either side.
    #[serde(default)]
    pub min_market_volume_usd: Option<f64>,
    /// Weight cap at `n == min_providers_for_cap`.
    #[serde(default)]
    pub max_weight_at_min_markets: Option<f64>,
    /// Weight cap at `n >= max_markets_per_asset`.
    #[serde(default)]
    pub max_weight_at_max_markets: Option<f64>,
    /// PUBLISHED denomination for every CR asset: it decides the ticker_id, and
    /// therefore the `.idx`/`.s10` directory name, so it is fixed for the
    /// process. Every market of an asset is converted straight into it.
    /// Absent = `USD`.
    pub storage_quote: Option<String>,
    /// Per-asset exceptions, keyed by base asset symbol. Keep EMPTY: it exists
    /// for an asset with no credible route to the storage quote, not as a
    /// tuning surface. An unreachable storage quote must fail loudly at boot
    /// rather than publish in a foreign denomination, which would put two units
    /// in one series.
    #[serde(default)]
    pub storage_quote_overrides: std::collections::BTreeMap<String, String>,
}

impl StorageYml {
    /// Storage quote for `asset`: its override, else the global, else `USD`.
    pub fn storage_quote_for(&self, asset: &str) -> String {
        self.storage_quote_overrides
            .get(&asset.to_ascii_uppercase())
            .or(self.storage_quote.as_ref())
            .cloned()
            .unwrap_or_else(|| "USD".to_string())
    }
}

/// `cexs.scraper:` block — endpoints + selectors for CEX volume scraping.
#[derive(Debug, Clone, Default, Deserialize, Serialize)]
pub struct ScraperYml {
    /// CoinMarketCap pairs endpoint. Defaults to the production CMC API
    /// when absent. Override here to test against a staging mirror or to
    /// pin against a specific API version without rebuilding.
    #[serde(default)]
    pub cmc_pairs_url: Option<String>,
}

/// `series:` block — pipeline-internal config.
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct SeriesYml {
    pub renko: RenkoYml,
    pub vol: VolConfig,
    pub calibration: CalibrationYml,
    pub pipeline: PipelineParamsYml,
}

/// `series.renko:` block. `max_pct` dropped 2026-05-24 (operator: markets be
/// markets); serde tolerates extra keys so a stale `max_pct:` in older yml
/// is silently ignored.
#[derive(Debug, Clone, Copy, Deserialize, Serialize)]
pub struct RenkoYml {
    pub min_pct: f32,
}

fn default_rolling_window_days() -> usize {
    365
}
fn default_bracket_max_iters() -> usize {
    12
}
fn default_accept_tol() -> f64 {
    0.05
}

/// `series.calibration:` block. Mirrors `series_factory::bar_construction::
/// calibrate::CalibrationConfig` plus the per-class target table.
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct CalibrationYml {
    pub target_bpd: f64,
    /// Single trailing window (days) over which the per-UTC-day brick-count
    /// MEDIAN is computed and driven to `target_bpd`. Replaces the legacy MTF
    /// `k_fit_windows_days` blend — one window, one median, one engine
    /// (`docs/renko-methodology.md` §3). Default 365 (one full bull/bear/chop
    /// rotation; 2× the longest σ-blend window).
    #[serde(default = "default_rolling_window_days")]
    pub rolling_window_days: usize,
    pub min_window_days: usize,
    /// Iteration cap for the BOUNDED log-k bracket fallback inside
    /// `scale_to_target_k` (the wrong-tread safety net; methodology §4 step 6).
    /// Renamed from `max_rounds`. Default 12.
    #[serde(default = "default_bracket_max_iters", alias = "max_rounds")]
    pub bracket_max_iters: usize,
    /// Warm-start accept tolerance: the direct scale-to-target step accepts when
    /// `|median/target − 1| ≤ accept_tol`. ALSO the advisory achieved-err warn
    /// threshold (NOT a ticker-drop gate — methodology §4 step 8). Default 0.05.
    #[serde(default = "default_accept_tol", alias = "tolerance")]
    pub accept_tol: f64,
    pub mult_bounds: [f64; 2],
    /// Per-pair `target_bpd` overrides keyed by pair string (e.g. "USDC/USDT"
    /// → 50). Highest-priority lookup; reserve for genuine per-pair exceptions
    /// the class default can't express (a specific LSD/LST, a one-off regime).
    #[serde(default)]
    pub target_bpd_overrides: BTreeMap<String, f64>,
    /// Per-asset-class `target_bpd` defaults keyed by `AssetClassBucket::as_key`
    /// (e.g. "crypto_stable" → 50). Applied when no per-pair override matches,
    /// using the class detected by `asset_class::bucket_for_pair` (which reads
    /// MITCH wire bits + the `cexs.pegged` membership list). Lets every
    /// stable/stable pair inherit the low target by CLASS, so a newly listed
    /// stablecoin can't silently fall back to the flat 300 default just because
    /// nobody added it to the per-pair override list.
    #[serde(default)]
    pub target_bpd_by_class: BTreeMap<String, f64>,
    /// PART B4 (2026-06-09): per-pair FORCED renko-k escape hatch keyed by pair
    /// string (e.g. "BTC/USDT" → 0.42). When present and within
    /// `[K_FLOOR, K_MAX_SAFETY]` (no upper market ceiling — only the numeric
    /// safety cap), the calibrator EMITS this k directly and SKIPS the fit for
    /// that pair. Reserved for "structural-floor" tickers the
    /// staircase prevents the fit from landing within `RENKO_BPD_ACCEPT_TOL` of
    /// target (surfaced by the per-ticker `achieved_err` log). Empty by default —
    /// add entries only after the fit log shows a ticker's floor exceeds tol.
    #[serde(default)]
    pub renko_k_overrides: BTreeMap<String, f64>,
}

impl CalibrationYml {
    /// Resolve `target_bpd` for a given pair string (e.g. "BTC/USDT") WITHOUT
    /// class detection. Lookup: per-pair override → flat top-level `target_bpd`.
    /// Prefer `target_for_pair_classed` where the caller knows the asset class.
    pub fn target_for_pair(&self, pair: &str) -> f64 {
        self.target_bpd_overrides
            .get(pair)
            .copied()
            .unwrap_or(self.target_bpd)
    }

    /// Resolve `target_bpd` using the detected asset-class bucket.
    /// Lookup order: per-pair override → per-class default → flat `target_bpd`.
    /// `class_key` is `AssetClassBucket::as_key()` (e.g. "crypto_stable").
    /// Never skips — operator policy: the calibrator always has a target.
    pub fn target_for_pair_classed(&self, pair: &str, class_key: &str) -> f64 {
        if let Some(&v) = self.target_bpd_overrides.get(pair) {
            return v;
        }
        if let Some(&v) = self.target_bpd_by_class.get(class_key) {
            return v;
        }
        self.target_bpd
    }

    /// Assert the YAML `mult_bounds` are sane bracket bounds for the calibrator.
    ///
    /// LOWER bound is a hard floor: `mult_bounds[0]` MUST equal
    /// `renko::MULT_LOWER_BOUND` (= K_FLOOR). The bisection does NOT auto-expand
    /// downward — a too-flat asset that can't reach target even at K_FLOOR is
    /// legitimately dropped (operator directive 2026-06-09: "Maybe a minimum").
    ///
    /// UPPER bound is now only the INITIAL bracket HINT for the full-history
    /// log-k bisection — NOT a ceiling. The bisection auto-EXPANDS `k_hi` upward
    /// (doubling) when the median==target crossing sits above `mult_bounds[1]`,
    /// so a storming crypto calibrates beyond the old 4.0 wall (operator
    /// directive: "K should not have a max"). We therefore only require it to be
    /// a positive, above-the-floor seed — no equality to a fixed ceiling.
    ///
    /// Returns an error (callers `bail!`/`expect` at startup) rather than
    /// panicking inline, so config-load surfaces a clean diagnostic.
    pub fn assert_bounds_consistent(&self) -> Result<(), String> {
        use crate::renko::MULT_LOWER_BOUND;
        if (self.mult_bounds[0] - MULT_LOWER_BOUND).abs() > f64::EPSILON {
            return Err(format!(
                "calibration.mult_bounds[0]={} disagrees with renko::MULT_LOWER_BOUND={} (K_FLOOR)",
                self.mult_bounds[0], MULT_LOWER_BOUND
            ));
        }
        if !(self.mult_bounds[1] > self.mult_bounds[0]) {
            return Err(format!(
                "calibration.mult_bounds[1]={} must be > mult_bounds[0]={} (it seeds the \
                 bisection's INITIAL upper bracket; the search auto-expands it upward as needed)",
                self.mult_bounds[1], self.mult_bounds[0]
            ));
        }
        Ok(())
    }
}

/// `series.pipeline:` block — replay / backfill knobs.
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct PipelineParamsYml {
    pub bootstrap_days: i64,
    #[serde(default)]
    pub max_bars: usize,
    #[serde(default)]
    pub max_mem_gb: usize,
    #[serde(default)]
    pub exchanges: Vec<String>,
    #[serde(default)]
    pub pairs: Vec<String>,
    /// Disk-footprint guard knobs for the backfill orchestrator. Single-source
    /// YAML (operator mandate, no hardcoded magic in the binary). See
    /// `backfill-all`'s pre-flight headroom guard + monthly stream-and-delete.
    #[serde(default)]
    pub backfill: BackfillDiskYml,
}

/// `series.pipeline.backfill:` — disk-footprint guard knobs for `backfill-all`.
///
/// The orchestrator now fetches raw `.ticks` one calendar month at a time,
/// folds each month into the per-provider `.idx`, then deletes that month's
/// raw `.ticks` before fetching the next. Peak raw footprint is therefore
/// bounded to ~1 month × n_exchanges (not the full backfill range). These
/// knobs drive the pre-flight headroom guard that aborts the run if the
/// projected monthly raw peak would not fit the free space on the data PVC.
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct BackfillDiskYml {
    /// Conservative estimate of raw `.ticks` bytes produced per exchange per
    /// calendar day for a liquid pair (binance/bybit monthly aggTrades for a
    /// top pair run ~1-2 GiB/month ≈ 30-70 MiB/day; we size for the worst
    /// liquid pair). Used by the pre-flight guard to project the monthly peak.
    pub bytes_per_exchange_day: u64,
    /// Safety factor applied to free space in the headroom guard: the run
    /// aborts unless `projected_peak <= free_bytes * safety_factor`. `< 1.0`
    /// leaves a margin (e.g. 0.8 = require 25% headroom above the projection).
    pub headroom_safety_factor: f64,
}

impl Default for BackfillDiskYml {
    fn default() -> Self {
        // Defaults sized for the launch universe (binance/bybit liquid pairs).
        // ~70 MiB/exch/day is a conservative upper bound for the heaviest
        // monthly aggTrades; safety_factor 0.8 keeps 20% slack on free space.
        Self {
            bytes_per_exchange_day: 70 * 1024 * 1024,
            headroom_safety_factor: 0.8,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The deployed `config.yml` must actually reach `StorageYml`. These two keys
    /// sat in the YAML with no field behind them, so serde dropped them and the
    /// file asserted a storage policy the code never applied. Parse the REAL
    /// file, not a fixture: a fixture would have passed the whole time.
    #[test]
    fn repo_config_storage_quote_is_load_bearing() {
        let path = concat!(env!("CARGO_MANIFEST_DIR"), "/../../config.yml");
        let Ok(raw) = std::fs::read_to_string(path) else {
            return; // submodule checked out standalone: nothing to pin
        };
        let y: PipelineYml = serde_yml::from_str(&raw).expect("config.yml parses");
        let storage = y.cexs.storage;
        assert_eq!(
            storage.storage_quote.as_deref(),
            Some("USD"),
            "config.yml declares storage_quote but StorageYml did not read it"
        );
        assert!(
            storage.storage_quote_overrides.is_empty(),
            "overrides must stay empty: they are for an asset with no USD route, not tuning"
        );
        assert_eq!(storage.storage_quote_for("BTC"), "USD");
    }

    fn cal() -> CalibrationYml {
        CalibrationYml {
            target_bpd: 300.0,
            rolling_window_days: 365,
            min_window_days: 30,
            bracket_max_iters: 12,
            accept_tol: 0.05,
            mult_bounds: [0.05, 4.0],
            target_bpd_overrides: BTreeMap::from([("USDC/USDT".to_string(), 50.0)]),
            target_bpd_by_class: BTreeMap::from([("crypto_stable".to_string(), 50.0)]),
            renko_k_overrides: BTreeMap::new(),
        }
    }

    /// Minimal `signed_quotes:` yaml with whatever σ keys the case needs.
    fn signed_yml(sigma_keys: &str) -> SignedQuotesYml {
        let y = format!(
            "oracle: \"0x0000000000000000000000000000000000000000\"\n\
             chain_id: 11155111\n\
             min_interval_ms: 5\n\
             mark_max_age_ms: 500\n\
             min_accepted_providers: 2\n\
             min_composite_freshness_bps: 500\n\
             quorum: 2\n\
             feeds: []\n{sigma_keys}"
        );
        serde_yml::from_str::<SignedQuotesYml>(&y).expect("signed_quotes parses")
    }

    #[test]
    fn sigma_alias_both_keys_absent_is_the_default_blend() {
        assert_eq!(
            signed_yml("").sigma_windows(),
            crate::mtf::MtfWindows::default()
        );
        assert_eq!(
            signed_yml("").sigma_windows().windows_min,
            crate::mtf::DEFAULT_SIGMA_WINDOWS_MIN.to_vec()
        );
    }

    /// THE rollout-safety property: an UNMIGRATED ConfigMap must still parse
    /// (`deny_unknown_fields` would otherwise crashloop the new image) and must
    /// reproduce the pre-MTF single-window σ exactly — one leg of 48 x 30 m.
    #[test]
    fn sigma_alias_alone_reproduces_the_single_window_path() {
        let w = signed_yml("sigma_lookback_bars: 48\n").sigma_windows();
        assert_eq!(w.windows_min, vec![48 * SIGMA_BAR_MIN]);
        assert_eq!(w.weights, vec![1.0]);
        assert_eq!(
            w.bars(SIGMA_BAR_MIN),
            vec![48],
            "48 bars, exactly as before"
        );
        assert_eq!(w.max_bars(SIGMA_BAR_MIN), 48);
        // One leg at weight 1 IS that leg: the blend is the identity, so σ is
        // numerically identical to the old single-window Parkinson value.
        let single = 0.0137_f64;
        assert_eq!(w.blend(&[Some((single, 1.0))]), Some(single));
    }

    #[test]
    fn sigma_windows_min_alone_is_used_verbatim() {
        let w =
            signed_yml("sigma_windows_min:\n  windows_min: [360, 2880]\n  weights: [2.0, 1.0]\n")
                .sigma_windows();
        assert_eq!(w.windows_min, vec![360, 2_880]);
        assert_eq!(w.weights, vec![2.0, 1.0]);
    }

    #[test]
    fn sigma_windows_min_wins_when_both_keys_are_present() {
        let c = signed_yml(
            "sigma_lookback_bars: 48\nsigma_windows_min:\n  windows_min: [360, 2880, 10080]\n",
        );
        assert!(c.has_stale_sigma_alias(), "loader must WARN on this file");
        assert_eq!(
            c.sigma_windows().windows_min,
            crate::mtf::DEFAULT_SIGMA_WINDOWS_MIN.to_vec()
        );
    }

    #[test]
    fn classed_resolution_order() {
        let c = cal();
        // 1. explicit per-pair override wins over everything.
        assert_eq!(
            c.target_for_pair_classed("USDC/USDT", "crypto_stable"),
            50.0
        );
        // 2. no override, but class default applies — the whole point: a stable
        //    pair NOT in the override list still gets 50 by class detection.
        assert_eq!(
            c.target_for_pair_classed("FDUSD/USDT", "crypto_stable"),
            50.0
        );
        // 3. no override, no class default → flat default.
        assert_eq!(c.target_for_pair_classed("BTC/USDT", "crypto_major"), 300.0);
        // unclassified bucket also falls back to flat default.
        assert_eq!(c.target_for_pair_classed("FOO/BAR", "default"), 300.0);
    }

    /// EURC/USDC (2026-07-21): classify_ticker (asset_class.rs) wrongly buckets
    /// it `crypto_stable` (EURC ∈ cexs.pegged, CR/CR vs USDC) even though
    /// EURC is EUR-pegged (~1.14), not a $1 peg. The real repo config.yml
    /// carries an explicit per-pair override to force it back to the
    /// FX-appropriate flat tier regardless of the (wrong) class the runtime
    /// detects it as — this is the actual end-to-end proof the override wins.
    #[test]
    fn eurc_usdc_override_beats_wrong_crypto_stable_class_in_real_config() {
        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../config.yml");
        let pl = PipelineYml::load(&root).expect("parse repo config.yml");
        assert_eq!(
            pl.series
                .calibration
                .target_for_pair_classed("EURC/USDC", "crypto_stable"),
            300.0,
            "EURC/USDC must resolve to the FX-appropriate 300bpd tier, not the 50bpd $1-stable tier"
        );
    }

    /// Sepolia 24-asset set (2026-07-21): SYRUPUSDC (Maple's yield-bearing
    /// vault share, NAV ≈1.174) deliberately is NOT in `cexs.pegged` (same
    /// reasoning as EURC/SUSDE), so classify_ticker would bucket its /USDT and
    /// /USDC crosses `crypto_alt` (no class default, flat 300bpd) without the
    /// explicit override. AUSD (Agora Dollar) IS in `cexs.pegged`, so its
    /// (CR,FX) /USD leg needs the same explicit-override treatment U/USDG/USDF/
    /// USDTB/BFUSD all needed — class detection never fires for a CR,FX pair.
    #[test]
    fn syrupusdc_and_ausd_overrides_in_real_config() {
        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../config.yml");
        let pl = PipelineYml::load(&root).expect("parse repo config.yml");
        assert_eq!(
            pl.series
                .calibration
                .target_for_pair_classed("SYRUPUSDC/USDC", "crypto_alt"),
            50.0,
            "SYRUPUSDC/USDC override must win over the crypto_alt default"
        );
        assert_eq!(
            pl.series
                .calibration
                .target_for_pair_classed("SYRUPUSDC/USDT", "crypto_alt"),
            50.0,
            "SYRUPUSDC/USDT override must win over the crypto_alt default"
        );
        assert_eq!(
            pl.series
                .calibration
                .target_for_pair_classed("SYRUPUSDC/USD", "fx_cross"),
            300.0,
            "SYRUPUSDC/USD must stay at the FX-like default - NOT overridden to the $1-stable tier"
        );
        assert_eq!(
            pl.series
                .calibration
                .target_for_pair_classed("AUSD/USD", "fx_cross"),
            50.0,
            "AUSD/USD must resolve to 50bpd via explicit override (same-class fix as U/USD)"
        );
    }

    #[test]
    fn non_classed_helper_unchanged() {
        let c = cal();
        // Legacy path: only the per-pair override map, no class layer.
        assert_eq!(c.target_for_pair("USDC/USDT"), 50.0);
        assert_eq!(c.target_for_pair("FDUSD/USDT"), 300.0); // not in override map
    }

    #[test]
    fn signed_quotes_requires_pinned_peers_and_tight_ms_freshness() {
        let current: SignedQuotesYml = serde_yml::from_str(
            "oracle: '0x1111111111111111111111111111111111111111'\n\
             chain_id: 56\nmin_interval_ms: 5\nmark_max_age_ms: 500\nmin_accepted_providers: 3\n\
             min_composite_freshness_bps: 9000\nquorum: 2\npeers:\n\
               - { url: 'http://signer.internal:8080', signer: '0x2222222222222222222222222222222222222222' }\n\
             feeds:\n  - { idx: 1, symbol: 'BTC-USDC', cosign_tolerance_bps: 2.0 }\n",
        )
        .expect("parse hardened signed_quotes schema");
        assert_eq!(current.min_interval_ms, 5);
        assert_eq!(current.mark_max_age_ms, 500);
        assert_eq!(current.peers.len(), 1);
        assert_eq!(current.feeds[0].cosign_tolerance_bps, 2.0);

        let missing_feed_tolerance = "oracle: '0x1111111111111111111111111111111111111111'\n\
             chain_id: 56\nmin_interval_ms: 5\nmark_max_age_ms: 500\nmin_accepted_providers: 3\n\
             min_composite_freshness_bps: 9000\nquorum: 2\npeers:\n\
               - { url: 'http://signer.internal:8080', signer: '0x2222222222222222222222222222222222222222' }\n\
             feeds:\n  - { idx: 1, symbol: 'BTC-USDC' }\n";
        assert!(
            serde_yml::from_str::<SignedQuotesYml>(missing_feed_tolerance).is_err(),
            "every feed must explicitly set cosign_tolerance_bps"
        );

        let legacy = "oracle: '0x1111111111111111111111111111111111111111'\n\
                      chain_id: 56\nmark_max_age_s: 120\nmin_accepted_providers: 3\n\
                      min_composite_freshness_bps: 9000\nquorum: 2\n\
                      peers: ['http://signer.internal:8080']\ncosign_tolerance_bps: 25\n\
                      feeds:\n  - { idx: 1, symbol: 'BTC-USDC' }\n";
        assert!(
            serde_yml::from_str::<SignedQuotesYml>(legacy).is_err(),
            "legacy seconds/unpinned-peer config must fail closed"
        );
    }

    /// Light-node mode: opt-in flag + the exact ticker subset it scopes
    /// the aggregator to (every feed symbol, plus each composed feed's legs).
    /// The repo `config.yml` must actually carry the MINUTE-based windows: a
    /// serde default would silently paper over a stale `_days` key.
    #[test]
    fn repo_config_declares_vol_windows_in_minutes() {
        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../config.yml");
        let pl = PipelineYml::load(&root).expect("parse repo config.yml");
        assert_eq!(
            pl.series.vol.sigma_blend_windows_min.windows_min,
            vec![20_160, 86_400, 259_200],
            "14 d / 60 d / 180 d expressed in minutes"
        );
    }

    /// Retention must be DERIVED so that the longest configured σ leg can
    /// actually fill: a leg the retention cannot feed is a σ that silently
    /// degrades forever.
    #[test]
    fn bars_retention_covers_the_longest_window() {
        let base: SignedQuotesYml = serde_yml::from_str(
            "oracle: '0x1111111111111111111111111111111111111111'\n\
             chain_id: 1\nmin_interval_ms: 5\nmark_max_age_ms: 500\nmin_accepted_providers: 1\n\
             min_composite_freshness_bps: 500\nquorum: 1\npeers: []\n\
             feeds:\n\
               - { idx: 1, symbol: 'BTC-USDC', cosign_tolerance_bps: 5.0 }\n",
        )
        .expect("parse");
        // Default legs are 6 h / 2 d / 1 w ⇒ longest = 7 d ⇒ 7 + edge day.
        assert_eq!(base.sigma_windows().max_days(SIGMA_BAR_MIN), 7);
        assert_eq!(base.bars_retention_days(), 8);
        // The INDEX window is untouched: σ never reads it and it is the tree
        // that actually costs disk.
        assert_eq!(base.retention_days, 1);

        // A longer leg pulls retention up with it, no second knob to forget.
        let long = SignedQuotesYml {
            sigma_windows_min: Some(crate::mtf::MtfWindows::equal([360u32, 2_880, 17_280])),
            ..base.clone()
        };
        assert_eq!(long.bars_retention_days(), 13, "12 d leg + edge day");

        // A session-traded feed declaring a WIDER calendar scan (its 336 real
        // bars span ~10 calendar days) must widen retention too, or the scan
        // reads shards that were already deleted.
        let mut fx = base.clone();
        fx.feeds[0].sigma_window_bars = Some(480); // 10 d
        assert_eq!(
            fx.bars_retention_days(),
            11,
            "scan horizon wins over the leg"
        );
    }

    #[test]
    fn sign_only_defaults_false_and_signed_symbols_covers_bridge_legs() {
        let sq: SignedQuotesYml = serde_yml::from_str(
            "oracle: '0x1111111111111111111111111111111111111111'\n\
             chain_id: 11155111\nmin_interval_ms: 5\nmark_max_age_ms: 500\nmin_accepted_providers: 1\n\
             min_composite_freshness_bps: 500\nquorum: 2\npeers:\n\
               - { url: 'http://s.internal:80', signer: '0x2222222222222222222222222222222222222222' }\n\
             feeds:\n\
               - { idx: 1, symbol: 'USDT-USD', cosign_tolerance_bps: 2.0 }\n\
               - { idx: 2, symbol: 'usds-usdc', cosign_tolerance_bps: 2.0 }\n\
               - { idx: 3, symbol: 'ETH-USDC', cosign_tolerance_bps: 5.0 }\n",
        )
        .expect("parse sign_only-absent schema");
        // Opt-in: absent ⇒ false, so full-replica behavior is untouched by default.
        assert!(!sq.sign_only);
        // signed_symbols() = every feed symbol (a composed feed's legs are found
        // via the graph at sign time, not declared), uppercased + deduped.
        let s = sq.signed_symbols();
        assert!(s.contains("USDT-USD"));
        assert!(s.contains("USDS-USDC"), "case-normalized feed symbol");
        assert!(s.contains("ETH-USDC"));
        assert_eq!(s.len(), 3, "USDT-USD + USDS-USDC + ETH-USDC, deduped");

        let on: SignedQuotesYml = serde_yml::from_str(
            "oracle: '0x1111111111111111111111111111111111111111'\n\
             chain_id: 1\nmin_interval_ms: 5\nmark_max_age_ms: 500\nmin_accepted_providers: 2\n\
             min_composite_freshness_bps: 9000\nquorum: 2\nsign_only: true\npeers:\n\
               - { url: 'http://s.internal:80', signer: '0x2222222222222222222222222222222222222222' }\n\
             feeds:\n  - { idx: 1, symbol: 'BTC-USDC', cosign_tolerance_bps: 5.0 }\n",
        )
        .expect("parse explicit sign_only:true");
        assert!(on.sign_only);
    }

    /// PART B4 (2026-06-09): the per-pair forced-k escape hatch. The calibrator
    /// binary short-circuits the fit when `renko_k_overrides` has the pair AND
    /// the k is in `[K_FLOOR, K_MAX_SAFETY]` (no upper market ceiling — operator
    /// directive: "K should not have a max"). This pins the field's
    /// deserialization + the exact bounds predicate the binary applies.
    #[test]
    fn renko_k_override_short_circuits_fit() {
        use crate::renko::{K_FLOOR, K_MAX_SAFETY};
        // Field deserializes from YAML and defaults to empty when absent.
        // "BIG/HI": 9.0 is now ACCEPTED (above the old 4.0 wall, below safety cap).
        let y: CalibrationYml = serde_yml::from_str(
            "target_bpd: 300\nrolling_window_days: 365\nmin_window_days: 30\n\
             bracket_max_iters: 12\naccept_tol: 0.05\nmult_bounds: [0.05, 4.0]\n\
             renko_k_overrides:\n  \"BTC/USDT\": 0.42\n  \"BAD/LOW\": 0.001\n  \"BIG/HI\": 9.0\n",
        )
        .expect("parse CalibrationYml with renko_k_overrides");

        // Present + in-bounds ⇒ the binary emits this k and skips the fit.
        let forced = y.renko_k_overrides.get("BTC/USDT").copied();
        assert_eq!(forced, Some(0.42));
        assert!(
            (K_FLOOR..=K_MAX_SAFETY).contains(&forced.unwrap()),
            "in-bounds override short-circuits"
        );

        // Below K_FLOOR is still ignored (the floor is preserved).
        assert!(
            !(K_FLOOR..=K_MAX_SAFETY).contains(&y.renko_k_overrides["BAD/LOW"]),
            "below K_FLOOR ignored"
        );
        // Above the OLD 4.0 ceiling is now ACCEPTED (no upper market cap).
        assert!(
            (K_FLOOR..=K_MAX_SAFETY).contains(&y.renko_k_overrides["BIG/HI"]),
            "k above old 4.0 wall now accepted"
        );

        // Absent pair ⇒ no override ⇒ normal fit path.
        assert!(y.renko_k_overrides.get("ETH/USDT").is_none());

        // Default (field omitted) ⇒ empty map.
        let c = cal();
        assert!(c.renko_k_overrides.is_empty());
    }

    /// Lean calibration field set (2026-06-10, `docs/renko-methodology.md`):
    /// the new `rolling_window_days` / `accept_tol` / `bracket_max_iters` parse,
    /// the legacy `max_rounds`/`tolerance` keys still deserialize via serde
    /// `alias` (forward-compat with un-migrated yml), and omitted fields take the
    /// documented defaults (365 / 0.05 / 12).
    #[test]
    fn lean_calibration_fields_parse_with_defaults_and_aliases() {
        // New canonical keys.
        let y: CalibrationYml = serde_yml::from_str(
            "target_bpd: 300\nrolling_window_days: 365\nmin_window_days: 30\n\
             bracket_max_iters: 12\naccept_tol: 0.05\nmult_bounds: [0.05, 4.0]\n",
        )
        .expect("parse lean CalibrationYml");
        assert_eq!(y.rolling_window_days, 365);
        assert_eq!(y.bracket_max_iters, 12);
        assert!((y.accept_tol - 0.05).abs() < 1e-12);

        // Legacy aliases: an un-migrated yml using `max_rounds` / `tolerance`
        // still deserializes into the renamed fields.
        let legacy: CalibrationYml = serde_yml::from_str(
            "target_bpd: 300\nrolling_window_days: 365\nmin_window_days: 30\n\
             max_rounds: 20\ntolerance: 0.08\nmult_bounds: [0.05, 4.0]\n",
        )
        .expect("parse legacy-aliased CalibrationYml");
        assert_eq!(
            legacy.bracket_max_iters, 20,
            "max_rounds aliases bracket_max_iters"
        );
        assert!(
            (legacy.accept_tol - 0.08).abs() < 1e-12,
            "tolerance aliases accept_tol"
        );

        // Omitted optional fields → documented defaults.
        let minimal: CalibrationYml =
            serde_yml::from_str("target_bpd: 300\nmin_window_days: 30\nmult_bounds: [0.05, 4.0]\n")
                .expect("parse minimal CalibrationYml");
        assert_eq!(minimal.rolling_window_days, 365);
        assert_eq!(minimal.bracket_max_iters, 12);
        assert!((minimal.accept_tol - 0.05).abs() < 1e-12);
    }

    /// A broker section must reach BOTH rosters: `configured_symbols` (what is
    /// served / calibrated) and `relay_symbols` (what is materialized, backfilled
    /// and held to account by the checkers). Every section-blind union found so
    /// far dropped the newest section silently, so pin both.
    #[test]
    fn relay_rosters_cover_every_forwarder_section() {
        let y: PipelineYml = serde_yml::from_str(
            "series:\n  renko: { min_pct: 0.001 }\n\
             \x20 vol: { ema_period: 1, winsorize_pct: [0.05, 0.95], winsorize_min_samples: 1 }\n\
             \x20 calibration: { target_bpd: 300, min_window_days: 30, mult_bounds: [0.05, 4.0] }\n\
             \x20 pipeline: { bootstrap_days: 1 }\n\
             cexs:\n  cross_pairs: [\"ETH/BTC\"]\n\
             oracles:\n  providers:\n    pyth:\n      symbols:\n        XAU/USD: \"1\"\n\
             ctrader:\n  providers:\n    pepperstone:\n      symbols:\n        eur/usd: EURUSD\n",
        )
        .expect("parse pipeline yaml with all three sections");

        let all = y.configured_symbols();
        assert!(all.contains("EUR/USD"), "broker symbol must be served");
        assert!(all.contains("ETH/BTC") && all.contains("XAU/USD"));

        let relay = y.relay_symbols();
        assert!(
            relay.contains("EUR/USD") && relay.contains("XAU/USD"),
            "every forwarder-observed symbol is materializable: {relay:?}"
        );
        assert!(
            !relay.contains("ETH/BTC"),
            "a cross composes on read and must never be backfilled or reported missing"
        );
        assert_eq!(y.relay_providers(), vec!["pyth", "pepperstone"]);
    }

    /// REGRESSION GUARD, not a style test. This block is the live ConfigMap
    /// `nxr-signer-sepolia-config` `signed_quotes.feeds`. `SignedQuotesYml` is
    /// `deny_unknown_fields`, so a schema that has dropped `idx` or `invert`
    /// does not degrade: it fails to parse and crashloops the whole signer
    /// fleet on boot. Note what it legitimately contains: the SAME symbol on
    /// two slots (BTC-USDC at idx 17 and 18, distinct ERC-20s sharing one
    /// mark), and a non-contiguous idx set (no 19, 21, 22, 23, 25) because a
    /// tier signs a SUBSET of one contract's `feedIds[]`.
    #[test]
    fn live_sepolia_configmap_feeds_parse() {
        let feeds: Vec<SignedFeedYml> = serde_yml::from_str(
            r#"
- { idx: 1, symbol: USDT-USD, cosign_tolerance_bps: 5.0, max_age_ms: 60000, single_source: true, optional: true }
- { idx: 2, symbol: USDE-USD, cosign_tolerance_bps: 5.0, max_age_ms: 60000, single_source: true, optional: true }
- { idx: 16, symbol: ETH-USDC, cosign_tolerance_bps: 5.0, max_age_ms: 30000, sigma_window_bars: 336 }
- { idx: 17, symbol: BTC-USDC, cosign_tolerance_bps: 5.0, max_age_ms: 30000, optional: true, sigma_window_bars: 336 }
- { idx: 18, symbol: BTC-USDC, cosign_tolerance_bps: 5.0, max_age_ms: 30000, optional: true, sigma_window_bars: 336 }
- { idx: 20, symbol: XAUT-USDC, cosign_tolerance_bps: 5.0, max_age_ms: 30000, optional: true, min_active_providers: 1, sigma_tol_pbps: 500, sigma_window_bars: 336 }
- { idx: 24, symbol: USD-CAD, cosign_tolerance_bps: 5.0, max_age_ms: 30000, invert: true, min_active_providers: 1, optional: true, sigma_window_bars: 336 }
- { idx: 26, symbol: USD-BRL, cosign_tolerance_bps: 5.0, max_age_ms: 30000, invert: true, min_active_providers: 1, optional: true, sigma_window_bars: 336 }
- { idx: 27, symbol: USD-JPY, cosign_tolerance_bps: 5.0, max_age_ms: 30000, invert: true, min_active_providers: 1, optional: true, sigma_window_bars: 336 }
- { idx: 28, symbol: USD-KRW, cosign_tolerance_bps: 5.0, max_age_ms: 30000, invert: true, min_active_providers: 1, optional: true, sigma_window_bars: 336 }
"#,
        )
        .expect("the live ConfigMap feeds block must parse");

        assert_eq!(
            feeds.iter().map(|f| f.idx.unwrap()).collect::<Vec<_>>(),
            vec![1, 2, 16, 17, 18, 20, 24, 26, 27, 28],
            "idx is the feed's own explicit field, never its array position"
        );
        assert_eq!(
            feeds.iter().filter(|f| f.symbol == "BTC-USDC").count(),
            2,
            "one mark on two on-chain slots is legal; only idx is unique"
        );
        assert_eq!(
            feeds.iter().filter(|f| f.invert).map(|f| f.symbol.as_str()).collect::<Vec<_>>(),
            vec!["USD-CAD", "USD-BRL", "USD-JPY", "USD-KRW"],
            "the four USD-base FX legs publish the reciprocal"
        );
        assert!(!feeds[0].invert, "invert defaults false");
    }
}
