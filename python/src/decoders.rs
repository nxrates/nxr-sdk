//! Bulk MITCH decoders / encoders backed by NumPy structured dtypes.
//!
//! Strategy: build a `numpy.dtype` mirroring the wire layout exactly (every
//! field at the same offset as the `#[repr(C, packed)]` Rust struct), then
//! reinterpret the input `bytes` buffer via `numpy.frombuffer`. Result is a
//! 1-D structured ndarray; zero-copy because numpy holds the bytes object
//! as the array's base.
//!
//! Caveman: ! redundant copy. ! offset arithmetic in python. dtype = wire.

use pyo3::prelude::*;
use pyo3::types::{PyBytes, PyDict, PyList, PyTuple};

use mitch::bar::Bar as MBar;
use mitch::common::message_sizes;
use mitch::header::MitchHeader;
use mitch::index::Index as MIndex;
use mitch::timestamp;
use nxr_sdk::ipc::record::IndexRecord as NIndexRecord;

const IDX_REC_SIZE: usize = 56;

// ──────────────────────────────────────────────────────────────────────
// dtype factories — invoked once, cached on Python side
// ──────────────────────────────────────────────────────────────────────

fn np_dtype<'py>(py: Python<'py>, fields: &[(&str, &str, usize)], itemsize: usize) -> PyResult<Bound<'py, PyAny>> {
    // Build a dtype dict per numpy's documented protocol:
    //   {'names': [...], 'formats': [...], 'offsets': [...], 'itemsize': N}
    let names = PyList::empty_bound(py);
    let formats = PyList::empty_bound(py);
    let offsets = PyList::empty_bound(py);
    for (n, f, o) in fields {
        names.append(*n)?;
        formats.append(*f)?;
        offsets.append(*o)?;
    }
    let d = PyDict::new_bound(py);
    d.set_item("names", names)?;
    d.set_item("formats", formats)?;
    d.set_item("offsets", offsets)?;
    d.set_item("itemsize", itemsize)?;
    let numpy = py.import_bound("numpy")?;
    numpy.getattr("dtype")?.call1((d,))
}

/// NumPy structured dtype matching the 56-byte on-wire `IndexRecord`.
///
/// Field offsets:
/// ```text
///  0  type_provider  u16     (low 4b msg type, hi 12b provider_id)
///  2  mts_raw        ('u1',6) raw u48 LE ticks since 2010 (16us / tick)
///  8  count          u8
///  9  flags          u8
/// 10  sequence       u16
/// 12  _reserved      ('u1',4)
/// 16  ticker         u64
/// 24  bid            f64
/// 32  ask            f64
/// 40  vbid           u32
/// 44  vask           u32
/// 48  ci             u16
/// 50  tick_count     u16
/// 52  confidence     u8
/// 53  accepted       u8
/// 54  rejected       u8
/// 55  flags_body     u8
/// ```
#[pyfunction]
pub fn index_record_dtype(py: Python<'_>) -> PyResult<PyObject> {
    let fields: &[(&str, &str, usize)] = &[
        ("type_provider", "<u2", 0),
        ("mts_raw", "(6,)u1", 2),
        ("count", "u1", 8),
        ("flags", "u1", 9),
        ("sequence", "<u2", 10),
        ("ticker", "<u8", 16),
        ("bid", "<f8", 24),
        ("ask", "<f8", 32),
        ("vbid", "<u4", 40),
        ("vask", "<u4", 44),
        ("ci", "<u2", 48),
        ("tick_count", "<u2", 50),
        ("confidence", "u1", 52),
        ("accepted", "u1", 53),
        ("rejected", "u1", 54),
        ("flags_body", "u1", 55),
    ];
    Ok(np_dtype(py, fields, IDX_REC_SIZE)?.unbind())
}

/// NumPy structured dtype for the 96-byte `Bar`.
#[pyfunction]
pub fn bar_dtype(py: Python<'_>) -> PyResult<PyObject> {
    let fields: &[(&str, &str, usize)] = &[
        ("open_ts", "(6,)u1", 0),
        ("close_ts", "(6,)u1", 6),
        ("open", "<f8", 12),
        ("high", "<f8", 20),
        ("low", "<f8", 28),
        ("close", "<f8", 36),
        ("vbid", "<u4", 44),
        ("vask", "<u4", 48),
        ("tick_count", "<u4", 52),
        ("realized_var", "<f4", 64),
        ("bipower_var", "<f4", 68),
        ("drift", "<f4", 72),
        ("vol_imbalance", "<f4", 76),
        ("avg_spread_bps", "<f4", 80),
        ("max_abs_return", "<f4", 84),
        ("avg_ci_ubp", "<u2", 88),
        ("reject_rate", "<u2", 90),
        ("kind", "u1", 92),
    ];
    Ok(np_dtype(py, fields, message_sizes::BAR)?.unbind())
}

/// NumPy structured dtype for the 32-byte MITCH `Tick` body.
#[pyfunction]
pub fn tick_dtype(py: Python<'_>) -> PyResult<PyObject> {
    let fields: &[(&str, &str, usize)] = &[
        ("ticker", "<u8", 0),
        ("bid", "<f8", 8),
        ("ask", "<f8", 16),
        ("vbid", "<u4", 24),
        ("vask", "<u4", 28),
    ];
    Ok(np_dtype(py, fields, message_sizes::TICK)?.unbind())
}

// ──────────────────────────────────────────────────────────────────────
// Bulk decoders — np.frombuffer over the supplied bytes buffer
// ──────────────────────────────────────────────────────────────────────

fn frombuffer<'py>(py: Python<'py>, buf: &Bound<'py, PyBytes>, dtype_fn: fn(Python<'_>) -> PyResult<PyObject>, stride: usize) -> PyResult<PyObject> {
    let n = buf.as_bytes().len();
    if n % stride != 0 {
        return Err(pyo3::exceptions::PyValueError::new_err(format!(
            "buffer length {n} is not a multiple of record size {stride}"
        )));
    }
    let dtype = dtype_fn(py)?;
    let numpy = py.import_bound("numpy")?;
    let kwargs = PyDict::new_bound(py);
    kwargs.set_item("dtype", dtype)?;
    let arr = numpy.getattr("frombuffer")?.call((buf,), Some(&kwargs))?;
    Ok(arr.unbind())
}

/// Decode a contiguous buffer of `IndexRecord`s (length % 56 == 0) into a
/// 1-D NumPy structured array. Zero-copy: numpy holds the bytes object.
///
/// Note: `mts_raw` is the raw u48 little-endian header timestamp. Convert to
/// unix-ms in pure Python with `EPOCH_MS_2010 + int.from_bytes(mts_raw, 'little') * 16 // 1000`.
#[pyfunction]
pub fn decode_idx_bytes(py: Python<'_>, buf: &Bound<'_, PyBytes>) -> PyResult<PyObject> {
    frombuffer(py, buf, index_record_dtype, IDX_REC_SIZE)
}

/// Decode a contiguous buffer of `Bar`s (length % 96 == 0) into a 1-D NumPy
/// structured array. Zero-copy.
#[pyfunction]
pub fn decode_bar_bytes(py: Python<'_>, buf: &Bound<'_, PyBytes>) -> PyResult<PyObject> {
    frombuffer(py, buf, bar_dtype, message_sizes::BAR)
}

/// Decode a contiguous buffer of MITCH `Tick`s (length % 32 == 0) into a 1-D
/// NumPy structured array.
#[pyfunction]
pub fn decode_tick_bytes(py: Python<'_>, buf: &Bound<'_, PyBytes>) -> PyResult<PyObject> {
    frombuffer(py, buf, tick_dtype, message_sizes::TICK)
}

// ──────────────────────────────────────────────────────────────────────
// Encoders — take a dict (or kwargs) and emit wire bytes
// ──────────────────────────────────────────────────────────────────────

/// Encode an `IndexRecord` from a dict of primitive fields into 56 wire bytes.
///
/// Required keys: `ts_ms` (i64 unix-ms), `provider` (u16), `ticker` (u64),
/// `bid` (f64), `ask` (f64).
/// Optional: `vbid`, `vask` (u32, default 0); `ci`, `tick_count` (u16,
/// default 0/1); `confidence`, `accepted`, `rejected` (u8, default 1/1/0);
/// `sequence` (u16, default 0).
#[pyfunction]
pub fn encode_idx_record<'py>(py: Python<'py>, rec: &Bound<'py, PyDict>) -> PyResult<Bound<'py, PyBytes>> {
    let ts_ms: i64 = rec.get_item("ts_ms")?
        .ok_or_else(|| pyo3::exceptions::PyKeyError::new_err("ts_ms"))?
        .extract()?;
    let provider: u16 = rec.get_item("provider")?
        .ok_or_else(|| pyo3::exceptions::PyKeyError::new_err("provider"))?
        .extract()?;
    let ticker: u64 = rec.get_item("ticker")?
        .ok_or_else(|| pyo3::exceptions::PyKeyError::new_err("ticker"))?
        .extract()?;
    let bid: f64 = rec.get_item("bid")?
        .ok_or_else(|| pyo3::exceptions::PyKeyError::new_err("bid"))?
        .extract()?;
    let ask: f64 = rec.get_item("ask")?
        .ok_or_else(|| pyo3::exceptions::PyKeyError::new_err("ask"))?
        .extract()?;
    let vbid: u32 = opt_u32(rec, "vbid")?.unwrap_or(0);
    let vask: u32 = opt_u32(rec, "vask")?.unwrap_or(0);
    let ci: u16 = opt_u16(rec, "ci")?.unwrap_or(0);
    let tick_count: u16 = opt_u16(rec, "tick_count")?.unwrap_or(1);
    let confidence: u8 = opt_u8(rec, "confidence")?.unwrap_or(1);
    let accepted: u8 = opt_u8(rec, "accepted")?.unwrap_or(1);
    let rejected: u8 = opt_u8(rec, "rejected")?.unwrap_or(0);
    let sequence: u16 = opt_u16(rec, "sequence")?.unwrap_or(0);

    let mts = timestamp::from_epoch_ms(ts_ms);
    let mut header = MitchHeader::new(b'i', provider, mts, 1);
    header.set_sequence(sequence);
    let index = MIndex::new(ticker, bid, ask, ci, vbid, vask, tick_count, confidence, accepted, rejected);
    let record = NIndexRecord::new(header, index);
    Ok(PyBytes::new_bound(py, bytemuck::bytes_of(&record)))
}

/// Encode a Bar dict into 96 wire bytes. Keys: open_ms, close_ms, open, high,
/// low, close (required); vbid, vask, tick_count (optional u32, default 0).
#[pyfunction]
pub fn encode_bar<'py>(py: Python<'py>, bar: &Bound<'py, PyDict>) -> PyResult<Bound<'py, PyBytes>> {
    let open_ms: i64 = bar.get_item("open_ms")?
        .ok_or_else(|| pyo3::exceptions::PyKeyError::new_err("open_ms"))?
        .extract()?;
    let close_ms: i64 = bar.get_item("close_ms")?
        .ok_or_else(|| pyo3::exceptions::PyKeyError::new_err("close_ms"))?
        .extract()?;
    let open: f64 = bar.get_item("open")?
        .ok_or_else(|| pyo3::exceptions::PyKeyError::new_err("open"))?
        .extract()?;
    let high: f64 = bar.get_item("high")?
        .ok_or_else(|| pyo3::exceptions::PyKeyError::new_err("high"))?
        .extract()?;
    let low: f64 = bar.get_item("low")?
        .ok_or_else(|| pyo3::exceptions::PyKeyError::new_err("low"))?
        .extract()?;
    let close: f64 = bar.get_item("close")?
        .ok_or_else(|| pyo3::exceptions::PyKeyError::new_err("close"))?
        .extract()?;
    let vbid: u32 = opt_u32(bar, "vbid")?.unwrap_or(0);
    let vask: u32 = opt_u32(bar, "vask")?.unwrap_or(0);
    let tick_count: u32 = opt_u32(bar, "tick_count")?.unwrap_or(0);

    let open_mts = timestamp::from_epoch_ms(open_ms);
    let close_mts = timestamp::from_epoch_ms(close_ms);
    let bar = MBar::new_ohlcv(open_mts, close_mts, open, high, low, close, vbid, vask, tick_count);
    Ok(PyBytes::new_bound(py, bytemuck::bytes_of(&bar)))
}

// ──────────────────────────────────────────────────────────────────────
// Helpers
// ──────────────────────────────────────────────────────────────────────

fn opt_u32(d: &Bound<'_, PyDict>, k: &str) -> PyResult<Option<u32>> {
    match d.get_item(k)? { Some(v) => Ok(Some(v.extract()?)), None => Ok(None) }
}
fn opt_u16(d: &Bound<'_, PyDict>, k: &str) -> PyResult<Option<u16>> {
    match d.get_item(k)? { Some(v) => Ok(Some(v.extract()?)), None => Ok(None) }
}
fn opt_u8(d: &Bound<'_, PyDict>, k: &str) -> PyResult<Option<u8>> {
    match d.get_item(k)? { Some(v) => Ok(Some(v.extract()?)), None => Ok(None) }
}

// Silence unused warnings for items kept for crate-level docs / re-export plans.
#[allow(dead_code)]
fn _keepalive(_t: &PyTuple) {}
