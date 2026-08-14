//! NXR SDK · python bindings (pyo3 0.22)
//!
//! Thin shim over `nxr-sdk` (Rust). Exposes:
//! - bulk decode/encode for the 56B `IndexRecord` and 96B `Bar` wire types
//!   as NumPy structured arrays (zero-copy on decode where possible)
//! - per-record `IndexRecord` and `Bar` pyclass wrappers (ergonomic access)
//! - MITCH ticker_id resolve / reverse
//! - synth tick composer
//! - blocking UDP multicast subscriber (`MulticastSubscriber`) iter / aiter
//! - blocking HTTP fetcher (`Client.fetch_idx`, `Client.fetch_ohlc`)
//!
//! Caveman: zero-copy where possible, fail loud on bad input.

#![allow(unsafe_op_in_unsafe_fn)] // pyo3 macros expand to unsafe blocks already gated by the harness

mod decoders;
mod resolve;
mod multicast;
mod http;
mod synth_py;
mod types;

use pyo3::prelude::*;

/// Module entry — registered as `nxr_sdk._native`. The Python wrapper at
/// `python/nxr_sdk/__init__.py` re-exports every public symbol under the
/// top-level `nxr_sdk` namespace.
#[pymodule]
fn _native(m: &Bound<'_, PyModule>) -> PyResult<()> {
    // Wire sizes — useful constants for buffer slicing on the python side.
    m.add("INDEX_RECORD_SIZE", 56usize)?;
    m.add("BAR_SIZE", 96usize)?;
    m.add("HEADER_SIZE", 16usize)?;
    m.add("INDEX_BODY_SIZE", 40usize)?;
    m.add("TICK_SIZE", 32usize)?;
    m.add("EPOCH_MS_2010", mitch::timestamp::EPOCH_MS)?;
    m.add("CI_SCALE", mitch::common::CI_SCALE)?;

    // ── Decoders / encoders ───────────────────────────────────────
    m.add_function(wrap_pyfunction!(decoders::decode_idx_bytes, m)?)?;
    m.add_function(wrap_pyfunction!(decoders::decode_bar_bytes, m)?)?;
    m.add_function(wrap_pyfunction!(decoders::decode_tick_bytes, m)?)?;
    m.add_function(wrap_pyfunction!(decoders::encode_idx_record, m)?)?;
    m.add_function(wrap_pyfunction!(decoders::encode_bar, m)?)?;
    m.add_function(wrap_pyfunction!(decoders::index_record_dtype, m)?)?;
    m.add_function(wrap_pyfunction!(decoders::bar_dtype, m)?)?;
    m.add_function(wrap_pyfunction!(decoders::tick_dtype, m)?)?;

    // ── PyClasses ─────────────────────────────────────────────────
    m.add_class::<types::IndexRecord>()?;
    m.add_class::<types::Bar>()?;
    m.add_class::<types::Tick>()?;

    // ── Ticker resolve ────────────────────────────────────────────
    m.add_function(wrap_pyfunction!(resolve::resolve_ticker_id, m)?)?;
    m.add_function(wrap_pyfunction!(resolve::try_resolve_ticker_id, m)?)?;
    m.add_function(wrap_pyfunction!(resolve::resolve_ticker, m)?)?;
    m.add_function(wrap_pyfunction!(resolve::get_market_provider, m)?)?;

    // ── Synth composition ─────────────────────────────────────────
    m.add_function(wrap_pyfunction!(synth_py::compute_synth_tick, m)?)?;

    // ── Real-time subscriber ──────────────────────────────────────
    m.add_class::<multicast::MulticastSubscriber>()?;

    // ── HTTP client ───────────────────────────────────────────────
    m.add_class::<http::Client>()?;

    Ok(())
}
