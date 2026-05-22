//! PyClass wrappers · IndexRecord, Bar, Tick.
//!
//! Carry one decoded sample; bulk decode uses NumPy structured arrays
//! (see `decoders.rs`) — these classes are for single-record ergonomics.

use pyo3::prelude::*;

use mitch::bar::Bar as MBar;
use mitch::header::MitchHeader;
use mitch::index::Index as MIndex;
use mitch::tick::Tick as MTick;
use mitch::timestamp;
use nxr_sdk::ipc::record::IndexRecord as NIndexRecord;

/// 56-byte IndexRecord = MitchHeader (16B) + Index body (40B).
#[pyclass(name = "IndexRecord", module = "nxr_sdk._native")]
#[derive(Clone, Copy)]
pub struct IndexRecord {
    pub inner: NIndexRecord,
}

#[pymethods]
impl IndexRecord {
    /// Construct from primitive fields. Timestamps are unix epoch milliseconds.
    #[new]
    #[pyo3(signature = (ts_ms, provider, ticker, bid, ask, ci=0, vbid=0, vask=0, tick_count=1, confidence=1, accepted=1, rejected=0, sequence=0))]
    #[allow(clippy::too_many_arguments)]
    fn new(
        ts_ms: i64,
        provider: u16,
        ticker: u64,
        bid: f64,
        ask: f64,
        ci: u16,
        vbid: u32,
        vask: u32,
        tick_count: u16,
        confidence: u8,
        accepted: u8,
        rejected: u8,
        sequence: u16,
    ) -> Self {
        let mts = timestamp::from_epoch_ms(ts_ms);
        let mut header = MitchHeader::new(b'i', provider, mts, 1);
        header.set_sequence(sequence);
        let index = MIndex::new(
            ticker, bid, ask, ci, vbid, vask, tick_count, confidence, accepted, rejected,
        );
        Self { inner: NIndexRecord::new(header, index) }
    }

    /// Unix-epoch ms decoded from the 16B header's u48 mts.
    #[getter]
    fn ts_ms(&self) -> i64 {
        // Local copy to dodge unaligned-ref lint on packed struct field.
        let h = self.inner.header;
        timestamp::to_epoch_ms(h.get_timestamp())
    }

    #[getter]
    fn ticker(&self) -> u64 { self.inner.index.ticker }

    #[getter]
    fn provider(&self) -> u16 { self.inner.header.provider_id() }

    #[getter]
    fn sequence(&self) -> u16 { self.inner.header.sequence }

    #[getter]
    fn bid(&self) -> f64 { self.inner.index.bid }

    #[getter]
    fn ask(&self) -> f64 { self.inner.index.ask }

    #[getter]
    fn mid(&self) -> f64 { self.inner.index.mid() }

    #[getter]
    fn spread_bps(&self) -> f64 { self.inner.index.spread_bps() }

    #[getter]
    fn ci(&self) -> u16 { self.inner.index.ci }

    #[getter]
    fn ci_price(&self) -> f64 { self.inner.index.ci_price() }

    #[getter]
    fn vbid(&self) -> u32 { self.inner.index.vbid }

    #[getter]
    fn vask(&self) -> u32 { self.inner.index.vask }

    #[getter]
    fn tick_count(&self) -> u16 { self.inner.index.tick_count }

    #[getter]
    fn confidence(&self) -> u8 { self.inner.index.confidence }

    #[getter]
    fn accepted(&self) -> u8 { self.inner.index.accepted }

    #[getter]
    fn rejected(&self) -> u8 { self.inner.index.rejected }

    /// Wire-format bytes (56 B).
    fn to_bytes<'py>(&self, py: Python<'py>) -> Bound<'py, pyo3::types::PyBytes> {
        pyo3::types::PyBytes::new_bound(py, bytemuck::bytes_of(&self.inner))
    }

    fn __repr__(&self) -> String {
        format!(
            "IndexRecord(ts_ms={}, provider={}, ticker={}, bid={}, ask={}, mid={}, ci={}, accepted={}, rejected={}, confidence={})",
            self.ts_ms(), self.provider(), self.ticker(),
            self.bid(), self.ask(), self.mid(),
            self.ci(), self.accepted(), self.rejected(), self.confidence(),
        )
    }
}

/// 96-byte canonical enriched Bar (OHLCV + microstructure).
#[pyclass(name = "Bar", module = "nxr_sdk._native")]
#[derive(Clone, Copy)]
pub struct Bar {
    pub inner: MBar,
}

#[pymethods]
impl Bar {
    #[new]
    #[pyo3(signature = (open_ms, close_ms, open, high, low, close, vbid=0, vask=0, tick_count=0))]
    #[allow(clippy::too_many_arguments)]
    fn new(
        open_ms: i64,
        close_ms: i64,
        open: f64,
        high: f64,
        low: f64,
        close: f64,
        vbid: u32,
        vask: u32,
        tick_count: u32,
    ) -> Self {
        let open_mts = timestamp::from_epoch_ms(open_ms);
        let close_mts = timestamp::from_epoch_ms(close_ms);
        Self {
            inner: MBar::new_ohlcv(open_mts, close_mts, open, high, low, close, vbid, vask, tick_count),
        }
    }

    #[getter]
    fn open_ms(&self) -> i64 { self.inner.open_time_ms() }

    #[getter]
    fn close_ms(&self) -> i64 { self.inner.close_time_ms() }

    #[getter]
    fn open(&self) -> f64 { self.inner.open }

    #[getter]
    fn high(&self) -> f64 { self.inner.high }

    #[getter]
    fn low(&self) -> f64 { self.inner.low }

    #[getter]
    fn close(&self) -> f64 { self.inner.close }

    #[getter]
    fn vbid(&self) -> u32 { self.inner.vbid }

    #[getter]
    fn vask(&self) -> u32 { self.inner.vask }

    #[getter]
    fn tick_count(&self) -> u32 { self.inner.tick_count }

    #[getter]
    fn realized_var(&self) -> f32 { self.inner.realized_var }

    #[getter]
    fn bipower_var(&self) -> f32 { self.inner.bipower_var }

    #[getter]
    fn drift(&self) -> f32 { self.inner.drift }

    #[getter]
    fn vol_imbalance(&self) -> f32 { self.inner.vol_imbalance }

    #[getter]
    fn avg_spread_bps(&self) -> f32 { self.inner.avg_spread_bps }

    #[getter]
    fn max_abs_return(&self) -> f32 { self.inner.max_abs_return }

    #[getter]
    fn avg_ci_ubp(&self) -> u16 { self.inner.avg_ci_ubp }

    #[getter]
    fn reject_rate(&self) -> u16 { self.inner.reject_rate }

    #[getter]
    fn kind(&self) -> u8 { self.inner.kind }

    fn to_bytes<'py>(&self, py: Python<'py>) -> Bound<'py, pyo3::types::PyBytes> {
        pyo3::types::PyBytes::new_bound(py, bytemuck::bytes_of(&self.inner))
    }

    fn __repr__(&self) -> String {
        format!(
            "Bar(open_ms={}, close_ms={}, o={}, h={}, l={}, c={}, vbid={}, vask={}, n={})",
            self.open_ms(), self.close_ms(),
            self.open(), self.high(), self.low(), self.close(),
            self.vbid(), self.vask(), self.tick_count(),
        )
    }
}

/// 32-byte MITCH Tick body (level-1 quote snapshot).
#[pyclass(name = "Tick", module = "nxr_sdk._native")]
#[derive(Clone, Copy)]
pub struct Tick {
    pub inner: MTick,
}

#[pymethods]
impl Tick {
    #[new]
    fn new(ticker: u64, bid: f64, ask: f64, vbid: u32, vask: u32) -> Self {
        Self { inner: MTick::new_unchecked(ticker, bid, ask, vbid, vask) }
    }

    #[getter]
    fn ticker(&self) -> u64 { self.inner.ticker }

    #[getter]
    fn bid(&self) -> f64 { self.inner.bid }

    #[getter]
    fn ask(&self) -> f64 { self.inner.ask }

    #[getter]
    fn vbid(&self) -> u32 { self.inner.vbid }

    #[getter]
    fn vask(&self) -> u32 { self.inner.vask }

    #[getter]
    fn mid(&self) -> f64 { (self.inner.bid + self.inner.ask) / 2.0 }

    fn to_bytes<'py>(&self, py: Python<'py>) -> Bound<'py, pyo3::types::PyBytes> {
        pyo3::types::PyBytes::new_bound(py, bytemuck::bytes_of(&self.inner))
    }

    fn __repr__(&self) -> String {
        format!(
            "Tick(ticker={}, bid={}, ask={}, vbid={}, vask={})",
            self.ticker(), self.bid(), self.ask(), self.vbid(), self.vask(),
        )
    }
}
