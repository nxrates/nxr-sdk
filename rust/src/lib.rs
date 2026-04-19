//! # NXR SDK
//!
//! Single canonical SDK for the NX Rates stack.
//!
//! Includes:
//! - MITCH wire types (re-exported from the `mitch` crate)
//! - IPC primitives: [`AppendLog`] (append-only .idx files), [`IndexRecord`]
//! - Aggregation: [`TickAccumulator`], [`RunningStats`], [`compute_vwap`], [`TDWAP`]
//! - Resolution: [`resolve_ticker`], [`resolve_asset`], [`resolve_ticker_id`], [`TickerIdCache`]
//! - Provider lookup: [`get_market_provider_by_id`], [`find_market_provider`]
//! - Statistics: full-period + monthly-geo sharpe/sortino, variance, OLS, drawdown
//! - Configuration: [`NxrConfig`] (environment-based)
//! - Logging: [`logging::init`]
//! - Consumer transports: [`NxrClient`] (REST), [`WsStream`] (WebSocket),
//!   [`MulticastStream`] (UDP multicast)
//!
//! ## Quick start
//!
//! ```rust,no_run
//! use nxr_sdk::{NxrClient, MulticastStream, WsStream};
//!
//! #[tokio::main]
//! async fn main() -> anyhow::Result<()> {
//!     let nxr = NxrClient::new("https://api.nxrates.io");
//!     let btc = nxr.resolve("BTC/USDT").await?;
//!
//!     let mut ws = WsStream::connect("wss://ws.nxrates.io/v1/stream").await?;
//!     while let Some(msg) = ws.recv().await {
//!         match msg {
//!             nxr_sdk::WsMessage::Index(records) => {
//!                 for r in &records {
//!                     println!("ticker={} mid={:.2}", r.ticker, r.mid);
//!                 }
//!             }
//!             nxr_sdk::WsMessage::Tick(records) => {
//!                 for r in &records {
//!                     println!("ticker={} bid={:.4} ask={:.4}", r.ticker, r.bid, r.ask);
//!                 }
//!             }
//!         }
//!     }
//!
//!     let mut rx = MulticastStream::<mitch::Index>::bind_default().await?;
//!     while let Some(idx) = rx.recv().await {
//!         if idx.ticker == btc as u16 {
//!             println!("BTC mid={:.2}", idx.mid());
//!         }
//!     }
//!     Ok(())
//! }
//! ```

pub mod agg;
pub mod bar_builder;
pub mod bars;
pub mod client;
pub mod compress;
pub mod config;
pub mod features;
pub mod ipc;
pub mod logging;
pub mod providers;
pub mod resolve;
pub mod stats;
pub mod tdwap;
pub mod ticker;
pub mod weights_schema;

// ---- MITCH types ----

pub use mitch;
pub use mitch::timestamp;
pub use mitch::header::MitchHeader;
pub use mitch::frame::TickFrame;
pub use mitch::index::Index;
pub use mitch::tick::Tick;
pub use mitch::bar::Bar;

// ---- IPC primitives ----

pub use ipc::append_log::{self, AppendLog};
pub use ipc::record::IndexRecord;

// ---- Ticker resolution ----

pub use ticker::{TickerIdCache, resolve_ticker_id};
pub use resolve::{
    resolve_ticker, resolve_asset, resolve_asset_in_class,
    get_asset_by_id, get_asset_by_global_id,
};

// ---- Provider lookups ----

pub use providers::{find_market_provider, get_market_provider_by_id, get_market_provider_id_by_name};

// ---- Aggregation primitives ----

pub use agg::{
    TickAccumulator, RunningStats, is_valid_tick, parkinson_sigma,
    now_ns, now_ms, now_sec, now_mts,
};

// ---- TDWAP aggregation ----

pub use tdwap::{ProviderEntry, compute_vwap};

// ---- Bar builder ----

pub use bar_builder::{BarAccumulator, flat_bar};

// ---- Configuration ----

pub use config::NxrConfig;

// ---- Consumer client ----

pub use client::{
    NxrClient, WsStream, MulticastStream,
    WsMessage, WsIndex, WsTick, TickerResponse,
    DEFAULT_MCAST_ADDR, DEFAULT_MCAST_PORT,
};

// ---- Adaptive bars + Renko features ----

pub use bars::{
    MtfParkinsonCalculator, RenkoBar, RenkoConfig, RenkoFeatureExtractor, RenkoGenerator,
    VolConfig, VolSource, compute_renko_features, grid_step_for_brick, renko_feature_names,
    snap_to_25_grid, snap_to_grid,
};

// ---- ML feature engineering ----

pub use features::{
    Candle, MIN_BARS, N_CORE_FEATURES, N_FEATURES_WITH_TIME, compute_all_features,
    compute_labels, compute_lookbacks, feature_names,
};
