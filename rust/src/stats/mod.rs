//! Statistical primitives. Single canonical home for the stats used by
//! aggregation (TDWAP CI), series calibration, and live monitoring.
//!
//! Flat re-export: callers use `stats::median`, never `stats::descriptive::median`.

pub mod descriptive;

pub use descriptive::{
    mad, mean, median, median_and_mad, median_by, percentile, round_to_sig_digits, std_dev,
};
