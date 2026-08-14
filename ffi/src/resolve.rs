//! MITCH ticker_id resolution, reverse lookup, provider metadata.

use std::ffi::c_char;

use crate::{NXR_ERR_UNRESOLVED, NXR_OK, cstr, out_string};

/// Lenient resolve: writes the 64-bit MITCH ticker id for `symbol`.
///
/// WARNING: an unresolvable symbol yields an FNV1a-64 *phantom* id rather than
/// an error. It is unique but not a bit-packed `TickerId`, so
/// [`nxr_resolve_ticker`] reverses it to a hex base with an empty quote and its
/// class / instrument-type bits are hash noise. Use
/// [`nxr_try_resolve_ticker_id`] to tell a real id from a phantom.
///
/// # Safety
/// `symbol` must be a NUL-terminated string; `out` must point to a writable `u64`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn nxr_resolve_ticker_id(symbol: *const c_char, out: *mut u64) -> i32 {
    let sym = match unsafe { cstr(symbol) } {
        Ok(s) => s,
        Err(e) => return e,
    };
    if out.is_null() {
        return crate::NXR_ERR_NULL;
    }
    unsafe { *out = nxr_sdk::resolve_ticker_id(sym) };
    NXR_OK
}

/// Strict resolve: `NXR_ERR_UNRESOLVED` when the symbol has no MITCH id. No
/// FNV phantom fallback.
///
/// # Safety
/// As [`nxr_resolve_ticker_id`].
#[unsafe(no_mangle)]
pub unsafe extern "C" fn nxr_try_resolve_ticker_id(symbol: *const c_char, out: *mut u64) -> i32 {
    let sym = match unsafe { cstr(symbol) } {
        Ok(s) => s,
        Err(e) => return e,
    };
    if out.is_null() {
        return crate::NXR_ERR_NULL;
    }
    match nxr_sdk::try_resolve_ticker_id(sym) {
        Some(id) => {
            unsafe { *out = id };
            NXR_OK
        }
        None => NXR_ERR_UNRESOLVED,
    }
}

/// Reverse a ticker id into base / quote / instrument-type strings.
///
/// All three out-params receive owned strings the caller must release with
/// `nxr_string_free`. For ids that did not come from the resolver (phantoms),
/// base is `0x`-prefixed hex and quote is empty, matching `sdk/python`.
///
/// # Safety
/// Each out-param must point to a writable pointer slot.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn nxr_resolve_ticker(
    ticker_id: u64,
    out_base: *mut *mut c_char,
    out_quote: *mut *mut c_char,
    out_instrument: *mut *mut c_char,
) -> i32 {
    if out_base.is_null() || out_quote.is_null() || out_instrument.is_null() {
        return crate::NXR_ERR_NULL;
    }
    use mitch::ticker::TickerId;
    let tid = TickerId::from_raw(ticker_id);
    let base = nxr_sdk::resolve::get_asset_by_id(tid.base_asset_class(), tid.base_asset_id());
    let quote = nxr_sdk::resolve::get_asset_by_id(tid.quote_asset_class(), tid.quote_asset_id());
    let it = format!("{:?}", tid.instrument_type());
    let (b, q) = match (base, quote) {
        (Some(b), Some(q)) => (b.name.to_uppercase(), q.name.to_uppercase()),
        _ => (format!("0x{ticker_id:016x}"), String::new()),
    };
    unsafe {
        *out_base = out_string(&b);
        *out_quote = out_string(&q);
        *out_instrument = out_string(&it);
    }
    NXR_OK
}

/// Market provider name for `id`, or null when unknown. Release a non-null
/// result with `nxr_string_free`.
#[unsafe(no_mangle)]
pub extern "C" fn nxr_market_provider_name(id: u16) -> *mut c_char {
    match nxr_sdk::get_market_provider_by_id(id) {
        Some(mp) => out_string(&mp.name),
        None => std::ptr::null_mut(),
    }
}
