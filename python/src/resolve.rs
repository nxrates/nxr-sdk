//! MITCH ticker_id resolution + reverse + provider lookup.

use pyo3::prelude::*;
use pyo3::types::PyDict;

/// Resolve a canonical NXR symbol string ("BTC/USDT", "EURUSD") to its 64-bit
/// MITCH ticker id. Falls back to FNV1a-64 if the resolver cannot match — same
/// behaviour as `nxr-sdk::resolve_ticker_id`.
///
/// `instrument_type` is currently unused (the resolver always emits SPOT) but
/// accepted for API parity with the Rust side.
#[pyfunction]
#[pyo3(signature = (symbol, _instrument_type=None))]
pub fn resolve_ticker_id(symbol: &str, _instrument_type: Option<&str>) -> u64 {
    nxr_sdk::resolve_ticker_id(symbol)
}

/// Reverse a 64-bit MITCH ticker id back into (base, quote, instrument_type).
///
/// Returns a 3-tuple of strings. `instrument_type` is the enum debug name
/// (`"SPOT"`, `"PERP"`, `"FUT"`, ...). For ids that did not come from the
/// resolver (e.g. FNV1a fallback), the base is a hex string and the quote
/// is empty.
#[pyfunction]
pub fn resolve_ticker(ticker_id: u64) -> (String, String, String) {
    use mitch::ticker::TickerId;
    let tid = TickerId::from_raw(ticker_id);
    let base = nxr_sdk::resolve::get_asset_by_id(tid.base_asset_class(), tid.base_asset_id());
    let quote = nxr_sdk::resolve::get_asset_by_id(tid.quote_asset_class(), tid.quote_asset_id());
    let it = format!("{:?}", tid.instrument_type());
    match (base, quote) {
        (Some(b), Some(q)) => (b.name.to_uppercase(), q.name.to_uppercase(), it),
        _ => (format!("0x{:016x}", ticker_id), String::new(), it),
    }
}

/// Look up market provider metadata by numeric id. Returns a dict with
/// `id` and `name` keys, or None if unknown.
#[pyfunction]
pub fn get_market_provider(py: Python<'_>, id: u16) -> PyResult<Option<PyObject>> {
    let Some(mp) = nxr_sdk::get_market_provider_by_id(id) else { return Ok(None); };
    let d = PyDict::new_bound(py);
    d.set_item("id", id)?;
    d.set_item("name", mp.name)?;
    Ok(Some(d.unbind().into_any()))
}
