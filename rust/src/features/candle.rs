//! Canonical OHLCV candle type for ML feature extraction.
//!
//! Separate from [`mitch::Bar`] (the 128B wire format with u48 timestamps and
//! enrichment fields): this is an ergonomic, user-facing struct for feeding
//! historical series into feature builders and label generators.

use serde::{Deserialize, Serialize};

/// OHLCV candle. Timestamps are unix ms.
#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
pub struct Candle {
    /// Bar close timestamp, unix ms.
    pub ts: u64,
    pub o: f64,
    pub h: f64,
    pub l: f64,
    pub c: f64,
    pub v: f64,
}

impl Candle {
    #[inline]
    pub const fn new(ts: u64, o: f64, h: f64, l: f64, c: f64, v: f64) -> Self {
        Self { ts, o, h, l, c, v }
    }
}
