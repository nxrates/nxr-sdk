//! Feature engineering for supervised ML models.
//!
//! Public surface:
//!   * [`Candle`]: ergonomic OHLCV type consumed by builders.
//!   * [`compute_all_features`], [`feature_names`], [`compute_lookbacks`]:
//!     MTF feature pipeline (29 core + optional time features).
//!   * [`compute_labels`]: binary direction labels over a horizon.
//!   * Pure indicator helpers in [`indicators`] (RSI, Parkinson, ADX, ...).

pub mod candle;
pub mod indicators;
pub mod labels;
pub mod mtf;

pub use candle::Candle;
pub use indicators::{
    adx, autocorrelation, efficiency_ratio, hurst_rs, lin_reg_slope, mean_slice,
    parkinson_vol, rsi, sma_std, zscore,
};
pub use labels::compute_labels;
pub use mtf::{
    compute_all_features, compute_lookbacks, feature_names,
    DEFAULT_L, DEFAULT_M, DEFAULT_S, MIN_BARS, N_CORE_FEATURES, N_FEATURES_WITH_TIME,
};
