//! # NXR SDK
//!
//! Single canonical SDK for the NX Rates stack.
//!
//! Includes:
//! - MITCH wire types (re-exported from the `mitch` crate)
//! - IPC primitives: [`AppendLog`] (append-only .idx files), [`IndexRecord`]
//! - Aggregation: [`TickAccumulator`], [`RunningStats`], [`compute_vwap`], [`TDWAP`]
//! - Resolution: [`resolve_ticker`], [`resolve_asset_in_class`], [`resolve_ticker_id`], [`TickerIdCache`]
//! - Provider lookup: [`get_market_provider_by_id`]
//! - Statistics: full-period monthly-geo sharpe/sortino helpers, OLS, drawdown
//! - Configuration: [`NxrConfig`] (environment-based)
//! - Logging: [`logging::init`]

pub mod agg;
pub mod bar_builder;
pub mod compress;
pub mod config;
pub mod consumer;
pub mod ipc;
pub mod logging;
pub mod memory;
pub mod metrics;
pub mod ohlc;
pub mod providers;
pub mod publisher;
pub mod resolve;
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

// ---- Ticker resolution ----

pub use ticker::{TickerIdCache, resolve_ticker_id};
pub use resolve::{resolve_ticker, resolve_asset_in_class, get_asset_by_id};

// ---- Provider lookups ----

pub use providers::{get_market_provider_by_id, get_market_provider_id_by_name};

// ---- Aggregation primitives ----

pub use agg::{
    TickAccumulator, RunningStats, is_valid_tick, parkinson_sigma,
    now_ns, now_ms, now_sec, now_mts,
};

// ---- TDWAP aggregation ----

pub use tdwap::{ProviderEntry, compute_vwap, compute_vwap_at};

// ---- Bar builder ----

pub use bar_builder::{BarAccumulator, flat_bar};

// ---- Configuration ----

pub use config::NxrConfig;
