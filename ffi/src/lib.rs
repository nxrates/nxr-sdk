//! NXR SDK · C ABI surface.
//!
//! Thin `extern "C"` shim over `nxr-sdk`, built as a `cdylib` and bound from
//! Java with the FFM API (`sdk/java`). Mirrors `sdk/python` minus the NumPy
//! dtype helpers, the multicast subscriber, and the HTTP client (Java's
//! stdlib covers UDP and HTTP; see `sdk/java/README.md`).
//!
//! Conventions:
//! - Every fallible call returns an `i32` status (`NXR_OK` or a negative
//!   `NXR_ERR_*`), results land in out-params.
//! - Strings cross the boundary as owned `*mut c_char` and MUST be released
//!   with [`nxr_string_free`]. [`nxr_string_live_count`] exposes the
//!   outstanding count so callers can assert they leaked nothing.
//! - Byte buffers are borrowed for the duration of the call only.

mod codec;
mod resolve;
mod synth;

use std::ffi::{CStr, CString, c_char};
use std::sync::atomic::{AtomicI64, Ordering};

pub use codec::*;
pub use resolve::*;
pub use synth::*;

pub const NXR_OK: i32 = 0;
/// A required pointer argument was null.
pub const NXR_ERR_NULL: i32 = -1;
/// An input string was not valid UTF-8.
pub const NXR_ERR_UTF8: i32 = -2;
/// The symbol has no MITCH ticker id.
pub const NXR_ERR_UNRESOLVED: i32 = -3;
/// Buffer length is zero or not a whole multiple of the record size.
pub const NXR_ERR_BUF_LEN: i32 = -4;
/// The caller-supplied output buffer is too small.
pub const NXR_ERR_CAPACITY: i32 = -5;
/// The call produced no value (a synth leg was missing or non-positive).
pub const NXR_ERR_NO_RESULT: i32 = -6;
/// An argument was outside its permitted domain.
pub const NXR_ERR_ARG: i32 = -7;

/// Strings handed out and not yet freed. Exists so the Java test suite can
/// prove the binding leaks nothing, which RSS sampling cannot do reliably.
static LIVE_STRINGS: AtomicI64 = AtomicI64::new(0);

/// Borrow a C string argument as `&str`.
///
/// # Safety
/// `p` must be null or a NUL-terminated string valid for the call.
pub(crate) unsafe fn cstr<'a>(p: *const c_char) -> Result<&'a str, i32> {
    if p.is_null() {
        return Err(NXR_ERR_NULL);
    }
    unsafe { CStr::from_ptr(p) }.to_str().map_err(|_| NXR_ERR_UTF8)
}

/// Hand `s` to the caller as an owned C string, counted against
/// [`LIVE_STRINGS`]. An interior NUL (impossible for asset / provider names)
/// degrades to an empty string rather than a panic across the ABI.
pub(crate) fn out_string(s: &str) -> *mut c_char {
    LIVE_STRINGS.fetch_add(1, Ordering::Relaxed);
    CString::new(s).unwrap_or_default().into_raw()
}

/// Release a string returned by this library. Null is a no-op.
///
/// # Safety
/// `p` must come from this library and must not be freed twice.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn nxr_string_free(p: *mut c_char) {
    if p.is_null() {
        return;
    }
    LIVE_STRINGS.fetch_sub(1, Ordering::Relaxed);
    drop(unsafe { CString::from_raw(p) });
}

/// Number of strings handed out and not yet passed to [`nxr_string_free`].
#[unsafe(no_mangle)]
pub extern "C" fn nxr_string_live_count() -> i64 {
    LIVE_STRINGS.load(Ordering::Relaxed)
}
