//! # NXR SDK
//!
//! Single canonical SDK for the NX Rates stack.
//!
//! Includes:
//! - MITCH wire types (re-exported from the `mitch` crate)
//! - IPC primitives: [`AppendLog`] (append-only .idx files), [`IndexRecord`]
//! - Aggregation: [`TickAccumulator`], [`RunningStats`], [`compute_vwap`]
//! - Resolution: [`resolve_ticker`], [`resolve_ticker_id`], [`TickerIdCache`]
//! - Provider lookup: [`get_market_provider_by_id`]
//! - Statistics: full-period monthly-geo sharpe/sortino helpers, OLS, drawdown
//! - Configuration: [`NxrConfig`] (environment-based)
//! - Logging: [`logging::init`]

pub mod agg;
pub mod asset_class;
pub mod bar_builder;
pub mod bar_reader;
pub mod client;
pub mod compress;
pub mod config;
pub mod consumer;
pub mod errors;
pub mod f64_frame;
pub mod grid;
pub mod ipc;
pub mod logging;
pub mod memory;
pub mod metrics;
pub mod ohlc;
pub mod parkinson;
pub mod pipeline_config;
pub mod providers;
pub mod publisher;
pub mod renko;
pub mod resolve;
pub mod shard;
pub mod stats;
pub mod synth;
pub mod tdwap;
pub mod ticker;
pub mod transport;
pub mod weights_schema;
pub mod ws_client;

// ---- MITCH types ----

pub use mitch;
pub use mitch::timestamp;
pub use mitch::index::Index;
pub use mitch::tick::Tick;
pub use mitch::bar::Bar;

// ---- IPC primitives ----

pub use ipc::append_log::{self, AppendLog};
pub use ipc::record::IndexRecord;

// ---- Zero-copy .bars mmap reader ----
//
// Consumed by external workspaces (btr/prime/crates/bin/*). Keep re-exported;
// see audit Wave 2.C deferral note.
pub use bar_reader::{BarFile, write_bars};

// ---- Daily-shard storage layer ----

pub use shard::{BarShardWriter, IdxShardWriter};

// ---- Renko engine (live + offline shared) ----

pub use parkinson::{MtfParkinsonCalculator, VolConfig, VolSource};
pub use renko::{RenkoConfig, RenkoGenerator};

// ---- Ticker resolution ----

pub use ticker::{TickerIdCache, resolve_ticker_id, split_pair, split_pair_multi};
pub use resolve::resolve_ticker;

// ---- Provider lookups ----

pub use providers::{get_market_provider_by_id, get_market_provider_id_by_name};

// ---- Aggregation primitives ----

pub use agg::{
    TickAccumulator, RunningStats, is_valid_tick, parkinson_sigma,
    now_ns, now_ms, now_sec, now_mts,
};

// ---- TDWAP aggregation ----

pub use tdwap::{
    ProviderEntry, WeightCache, compute_vwap, compute_vwap_at, compute_vwap_throttled,
    default_refresh_interval_ms,
};

// ---- Bar builder ----

pub use bar_builder::{BarAccumulator, flat_bar};

// ---- Configuration ----

pub use config::NxrConfig;

// ---- Consumer client (REST + WS) ----

pub use client::NxrClient;

// ---- Plan-tier typed errors ----

pub use errors::{PlanErrorCode, PlanLimitError, PlanLimitErrorBody, PLAN_ERROR_DISCRIMINANT};
