//! Synthetic symbol composition library.
//!
//! Ports `@btr-protocol/sdk/types/{paths,synth-ohlc}` to Rust.
//!
//! ## Scope
//!
//! - [`cross`]: THE crossing resolver. A graph over MITCH asset ids whose edges
//!   are the live primary tickers; any pair is routed from it on demand.
//! - [`paths`]: signed-leg types (`synth = Π leg_i^{e_i}`, `e_i ∈ {+1, -1}`).
//! - [`tick`]: instantaneous synth tick composition (`compute_synth_tick`).
//! - [`ohlc`]: synth OHLC reconstruction via Parkinson / Rogers-Satchell variance.
//! - [`bar`]: full `mitch::Bar` reconstruction (OHLC + microstructure inheritance).
//! - [`rolling`]: Welford-style rolling Pearson correlation accumulator.
//!
//! ## Math (kept verbatim with BTR source-of-truth)
//!
//! Tick composition (`compute_synth_tick`):
//! ```text
//! mid = Π (k_i.mid)^{e_i}
//! bid = Π ( e=+1 ? k_i.bid : 1/k_i.ask )^|e|
//! ask = Π ( e=+1 ? k_i.ask : 1/k_i.bid )^|e|
//! conf = min_i k_i.conf
//! ```
//!
//! OHLC reconstruction (`reconstruct_synth_ohlc`):
//! ```text
//!   Parkinson:        v_i = ln(H/L)² / (4·ln2)
//!   Rogers-Satchell:  v_i = ln(H/C)·ln(H/O) + ln(L/C)·ln(L/O)
//!   V = Σ e_i²·v_i + 2·Σ_{i<j} e_i·e_j·ρ_ij·√(v_i·v_j)   (floored at 1e-12)
//!   R_S = √(4·ln2 · V)             (Parkinson inversion → synth log-range)
//!   O_S = Π O_i^{e_i};  C_S = Π C_i^{e_i}
//!   M_S = √(O_S · C_S)             (geometric mid)
//!   H_S = M_S · exp(+R_S/2);  L_S = M_S · exp(-R_S/2)
//! ```
//!
//! ## NXR vs BTR symbol convention
//!
//! BTR canonical quote is `USDC` (collector source-of-truth). NXR canonical quote
//! is exchange-native (`USDT` for crypto, `USD` for FX), and the wire format uses
//! a slash separator (e.g. `BTC/USDT`). NXR declares no per-symbol route table at
//! all: [`cross`] derives every route from the primaries that are actually live,
//! so a convention difference is data, not code. The maths (`tick`, `ohlc`,
//! `bar`, `rolling`) are verbatim ports.

pub mod bar;
pub mod compose;
pub mod cross;
pub mod cross_expand;
pub mod idx_source;
pub mod ohlc;
pub mod pairs;
pub mod paths;
pub mod pipeline_pairs;
pub mod replay;
pub mod rolling;
pub mod tick;
pub mod triangulation_rules;

pub use compose::compose_cross_s10;
pub use idx_source::{DEFAULT_EPHEMERAL_CAPACITY, EphemeralIdxSource};
pub use replay::{
    LEG_STALE_TTL_MS, SYNTH_KERNEL_PROVIDER_ID, SynthReplayState, compute_synth_index,
};

pub use bar::{
    DEFAULT_RHO_WINDOW_BUCKETS, RhoCache, build_rolling_rho_cache, reconstruct_synth_bar_series,
    reconstruct_synth_bar_series_at_base_tf_then_rollup, reconstruct_synth_bar_series_rolling_rho,
    rho_cache_callback,
};
pub use ohlc::{
    OhlcLite, OhlcWithRange, TimedOhlc, TimedOhlcCount, VarianceEstimator, reconstruct_synth_ohlc,
    reconstruct_synth_series, reconstruct_synth_series_at_base_tf_then_rollup,
};
pub use cross::{AssetId, Composed, CrossGraph, LegQuote, Route, RouteLeg, ticker_assets};
pub use paths::{Leg, SynthPath, normalize_to_slash};
pub use rolling::RollingCorrelation;
pub use tick::{LegTick, SynthTick, compose_legs, compute_synth_tick};
