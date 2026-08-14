//! Synth tick composition across the C ABI.

use std::collections::HashMap;
use std::ffi::c_char;

use nxr_sdk::synth::{Leg, LegTick, SynthPath, compute_synth_tick};

use crate::{NXR_ERR_ARG, NXR_ERR_NO_RESULT, NXR_ERR_NULL, NXR_OK, cstr};

/// Composed synth quote.
#[repr(C)]
#[derive(Clone, Copy, Default)]
pub struct NxrSynthTick {
    pub bid: f64,
    pub ask: f64,
    pub mid: f64,
    pub conf: u16,
    pub _pad: [u8; 6],
}
const _: () = assert!(size_of::<NxrSynthTick>() == 32);

/// Compose a synthetic tick from `n` parallel arrays.
///
/// Leg `i` is `(syms[i], exps[i])` quoted by `(bids[i], asks[i], mids[i],
/// confs[i])`. Unlike the Python binding, which takes a separate symbol-keyed
/// snapshot map, quotes are positional: the composer only ever looks each leg
/// up by its own symbol, so the map is redundant across an ABI that has no
/// dictionary type. A duplicated symbol therefore takes the quote at its own
/// index rather than a single shared entry.
///
/// `n == 0` is the identity path and yields `(1, 1, 1, conf=10000)`.
/// Returns `NXR_ERR_NO_RESULT` when a leg quote is non-positive.
///
/// # Safety
/// Every array must be valid for `n` elements; `syms` entries must be
/// NUL-terminated; `out` must be writable.
#[unsafe(no_mangle)]
#[allow(clippy::too_many_arguments)]
pub unsafe extern "C" fn nxr_compute_synth_tick(
    syms: *const *const c_char,
    exps: *const i8,
    n: usize,
    bids: *const f64,
    asks: *const f64,
    mids: *const f64,
    confs: *const u16,
    out: *mut NxrSynthTick,
) -> i32 {
    if out.is_null() {
        return NXR_ERR_NULL;
    }
    if n > 0 && (syms.is_null() || exps.is_null() || bids.is_null() || asks.is_null() || mids.is_null() || confs.is_null()) {
        return NXR_ERR_NULL;
    }

    let mut legs: Vec<Leg> = Vec::with_capacity(n);
    let mut quotes: Vec<(String, LegTick)> = Vec::with_capacity(n);
    for i in 0..n {
        let sym = match unsafe { cstr(*syms.add(i)) } {
            Ok(s) => s,
            Err(e) => return e,
        };
        let exp = unsafe { *exps.add(i) };
        if exp != 1 && exp != -1 {
            return NXR_ERR_ARG;
        }
        legs.push(Leg::new(sym.to_string(), exp));
        quotes.push((sym.to_string(), unsafe {
            LegTick {
                bid: *bids.add(i),
                ask: *asks.add(i),
                mid: *mids.add(i),
                conf: *confs.add(i),
            }
        }));
    }

    let path = SynthPath { sym: String::new(), legs };
    let map: HashMap<&str, LegTick> = quotes.iter().map(|(k, v)| (k.as_str(), *v)).collect();
    let Some(t) = compute_synth_tick(&path, &map) else {
        return NXR_ERR_NO_RESULT;
    };
    unsafe {
        *out = NxrSynthTick { bid: t.bid, ask: t.ask, mid: t.mid, conf: t.conf, _pad: [0; 6] };
    }
    NXR_OK
}
