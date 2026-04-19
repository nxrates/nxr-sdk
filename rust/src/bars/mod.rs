//! Real-time and backtest bar generation.
//!
//! Public surface:
//!   * [`RenkoConfig`], [`RenkoBar`], [`RenkoGenerator`] for adaptive renko
//!   * [`VolSource`], [`VolConfig`], [`MtfParkinsonCalculator`] for volatility input
//!   * [`RenkoFeatureExtractor`], [`compute_renko_features`] for ML features
//!   * Grid helpers: [`snap_to_25_grid`], [`snap_to_grid`], [`grid_step_for_brick`]

pub mod grid;
pub mod parkinson;
pub mod renko;
pub mod tracker;

pub use grid::{grid_step_for_brick, snap_to_25_grid, snap_to_grid};
pub use parkinson::{MtfParkinsonCalculator, VolConfig, VolSource};
pub use renko::{RenkoBar, RenkoConfig, RenkoGenerator};
pub use tracker::{RenkoFeatureExtractor, compute_renko_features, renko_feature_names};
