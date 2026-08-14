//! MITCH fixed-width decode / encode across the C ABI.
//!
//! The wire structs are `#[repr(C, packed)]`, which is hostile to a foreign
//! caller (unaligned f64s, u48 timestamps, derived fields). Each one is
//! therefore projected onto a naturally-aligned, explicitly-padded `*C`
//! mirror whose layout the Java side restates as a `MemoryLayout`. Sizes are
//! asserted at compile time so a field edit here breaks the build rather than
//! silently shifting offsets under Java.

use mitch::bar::Bar as MBar;
use mitch::common::message_sizes;
use mitch::header::MitchHeader;
use mitch::index::Index as MIndex;
use mitch::tick::Tick as MTick;
use mitch::timestamp;
use nxr_sdk::ipc::record::IndexRecord as NIndexRecord;

use crate::{NXR_ERR_BUF_LEN, NXR_ERR_CAPACITY, NXR_ERR_NULL, NXR_OK};

/// Wire size of an `IndexRecord`: 16 B header + 40 B Index body.
pub const NXR_IDX_SIZE: usize = 56;
/// Wire size of a `Bar`.
pub const NXR_BAR_SIZE: usize = message_sizes::BAR;
/// Wire size of a MITCH `Tick` body.
pub const NXR_TICK_SIZE: usize = message_sizes::TICK;

/// Decoded `IndexRecord`. `ci_price` is derived (the sqrt-compressed `ci`
/// encoding lives in mitch) and is ignored by [`nxr_encode_idx`].
#[repr(C)]
#[derive(Clone, Copy, Default)]
pub struct NxrIndexRecord {
    pub ts_ms: i64,
    pub ticker: u64,
    pub bid: f64,
    pub ask: f64,
    pub ci_price: f64,
    pub vbid: u32,
    pub vask: u32,
    pub provider: u16,
    pub sequence: u16,
    pub ci: u16,
    pub tick_count: u16,
    pub confidence: u8,
    pub accepted: u8,
    pub rejected: u8,
    pub flags: u8,
    pub _pad: [u8; 4],
}
const _: () = assert!(size_of::<NxrIndexRecord>() == 64);

/// Decoded `Bar`. Every wire field round-trips through [`nxr_encode_bar`].
#[repr(C)]
#[derive(Clone, Copy, Default)]
pub struct NxrBar {
    pub open_ms: i64,
    pub close_ms: i64,
    pub open: f64,
    pub high: f64,
    pub low: f64,
    pub close: f64,
    pub vbid: u32,
    pub vask: u32,
    pub tick_count: u32,
    pub realized_var: f32,
    pub bipower_var: f32,
    pub drift: f32,
    pub vol_imbalance: f32,
    pub avg_spread_bps: f32,
    pub max_abs_return: f32,
    pub avg_ci_ubp: u16,
    pub reject_rate: u16,
    pub kind: u8,
    pub flags: u8,
    pub _pad: [u8; 6],
}
const _: () = assert!(size_of::<NxrBar>() == 96);

/// Decoded MITCH `Tick` body.
#[repr(C)]
#[derive(Clone, Copy, Default)]
pub struct NxrTick {
    pub ticker: u64,
    pub bid: f64,
    pub ask: f64,
    pub vbid: u32,
    pub vask: u32,
}
const _: () = assert!(size_of::<NxrTick>() == 32);

/// Validate a record slab and hand back its records.
///
/// Matches the check the core applies to every fixed-width slab it reads
/// (`core/src/server/signed.rs::decode_blob`, `sdk/rust/src/bar_reader.rs`):
/// empty is rejected, and so is any length that is not a whole multiple of the
/// record size. A short buffer therefore fails here instead of being read past.
///
/// # Safety
/// `buf` must be valid for `len` bytes.
unsafe fn slab<'a>(buf: *const u8, len: usize, stride: usize) -> Result<&'a [u8], i32> {
    if buf.is_null() {
        return Err(NXR_ERR_NULL);
    }
    if len == 0 || !len.is_multiple_of(stride) {
        return Err(NXR_ERR_BUF_LEN);
    }
    Ok(unsafe { std::slice::from_raw_parts(buf, len) })
}

/// Decode `len / 56` records from `buf` into `out`.
///
/// Returns the record count, or a negative `NXR_ERR_*`. `cap` is the number of
/// `NxrIndexRecord` slots `out` can hold.
///
/// # Safety
/// `buf` must be valid for `len` bytes and `out` for `cap` records.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn nxr_decode_idx(
    buf: *const u8,
    len: usize,
    out: *mut NxrIndexRecord,
    cap: usize,
) -> isize {
    let src = match unsafe { slab(buf, len, NXR_IDX_SIZE) } {
        Ok(s) => s,
        Err(e) => return e as isize,
    };
    let n = len / NXR_IDX_SIZE;
    if out.is_null() {
        return NXR_ERR_NULL as isize;
    }
    if n > cap {
        return NXR_ERR_CAPACITY as isize;
    }
    for (i, chunk) in src.chunks_exact(NXR_IDX_SIZE).enumerate() {
        // Unaligned read: the wire struct is packed and `buf` is foreign memory.
        let r: NIndexRecord = bytemuck::pod_read_unaligned(chunk);
        let (h, ix) = (r.header, r.index);
        unsafe {
            *out.add(i) = NxrIndexRecord {
                ts_ms: timestamp::to_epoch_ms(h.get_timestamp()),
                ticker: ix.ticker,
                bid: ix.bid,
                ask: ix.ask,
                ci_price: ix.ci_price(),
                vbid: ix.vbid,
                vask: ix.vask,
                provider: h.provider_id(),
                sequence: h.sequence,
                ci: ix.ci,
                tick_count: ix.tick_count,
                confidence: ix.confidence,
                accepted: ix.accepted,
                rejected: ix.rejected,
                flags: ix.flags,
                _pad: [0; 4],
            };
        }
    }
    n as isize
}

/// Decode `len / 96` bars. See [`nxr_decode_idx`].
///
/// # Safety
/// As [`nxr_decode_idx`].
#[unsafe(no_mangle)]
pub unsafe extern "C" fn nxr_decode_bar(
    buf: *const u8,
    len: usize,
    out: *mut NxrBar,
    cap: usize,
) -> isize {
    let src = match unsafe { slab(buf, len, NXR_BAR_SIZE) } {
        Ok(s) => s,
        Err(e) => return e as isize,
    };
    let n = len / NXR_BAR_SIZE;
    if out.is_null() {
        return NXR_ERR_NULL as isize;
    }
    if n > cap {
        return NXR_ERR_CAPACITY as isize;
    }
    for (i, chunk) in src.chunks_exact(NXR_BAR_SIZE).enumerate() {
        let b: MBar = bytemuck::pod_read_unaligned(chunk);
        unsafe {
            *out.add(i) = NxrBar {
                open_ms: b.open_time_ms(),
                close_ms: b.close_time_ms(),
                open: b.open,
                high: b.high,
                low: b.low,
                close: b.close,
                vbid: b.vbid,
                vask: b.vask,
                tick_count: b.tick_count,
                realized_var: b.realized_var,
                bipower_var: b.bipower_var,
                drift: b.drift,
                vol_imbalance: b.vol_imbalance,
                avg_spread_bps: b.avg_spread_bps,
                max_abs_return: b.max_abs_return,
                avg_ci_ubp: b.avg_ci_ubp,
                reject_rate: b.reject_rate,
                kind: b.kind,
                flags: b.flags,
                _pad: [0; 6],
            };
        }
    }
    n as isize
}

/// Decode `len / 32` MITCH ticks. See [`nxr_decode_idx`].
///
/// # Safety
/// As [`nxr_decode_idx`].
#[unsafe(no_mangle)]
pub unsafe extern "C" fn nxr_decode_tick(
    buf: *const u8,
    len: usize,
    out: *mut NxrTick,
    cap: usize,
) -> isize {
    let src = match unsafe { slab(buf, len, NXR_TICK_SIZE) } {
        Ok(s) => s,
        Err(e) => return e as isize,
    };
    let n = len / NXR_TICK_SIZE;
    if out.is_null() {
        return NXR_ERR_NULL as isize;
    }
    if n > cap {
        return NXR_ERR_CAPACITY as isize;
    }
    for (i, chunk) in src.chunks_exact(NXR_TICK_SIZE).enumerate() {
        let t: MTick = bytemuck::pod_read_unaligned(chunk);
        unsafe {
            *out.add(i) = NxrTick {
                ticker: t.ticker,
                bid: t.bid,
                ask: t.ask,
                vbid: t.vbid,
                vask: t.vask,
            };
        }
    }
    n as isize
}

/// Encode one record into 56 wire bytes. `ci_price` is derived and ignored.
///
/// # Safety
/// `rec` must be readable and `out` writable for `cap` bytes.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn nxr_encode_idx(
    rec: *const NxrIndexRecord,
    out: *mut u8,
    cap: usize,
) -> i32 {
    if rec.is_null() || out.is_null() {
        return NXR_ERR_NULL;
    }
    if cap < NXR_IDX_SIZE {
        return NXR_ERR_CAPACITY;
    }
    let r = unsafe { *rec };
    let mut header = MitchHeader::new(b'i', r.provider, timestamp::from_epoch_ms(r.ts_ms), 1);
    header.set_sequence(r.sequence);
    let mut index = MIndex::new(
        r.ticker,
        r.bid,
        r.ask,
        r.ci,
        r.vbid,
        r.vask,
        r.tick_count,
        r.confidence,
        r.accepted,
        r.rejected,
    );
    index.flags = r.flags;
    let record = NIndexRecord::new(header, index);
    unsafe {
        std::ptr::copy_nonoverlapping(
            bytemuck::bytes_of(&record).as_ptr(),
            out,
            NXR_IDX_SIZE,
        )
    };
    NXR_OK
}

/// Encode one bar into 96 wire bytes. Lossless: the microstructure fields
/// `MBar::new_ohlcv` leaves zeroed are copied across explicitly.
///
/// # Safety
/// As [`nxr_encode_idx`].
#[unsafe(no_mangle)]
pub unsafe extern "C" fn nxr_encode_bar(bar: *const NxrBar, out: *mut u8, cap: usize) -> i32 {
    if bar.is_null() || out.is_null() {
        return NXR_ERR_NULL;
    }
    if cap < NXR_BAR_SIZE {
        return NXR_ERR_CAPACITY;
    }
    let b = unsafe { *bar };
    let mut m = MBar::new_ohlcv(
        timestamp::from_epoch_ms(b.open_ms),
        timestamp::from_epoch_ms(b.close_ms),
        b.open,
        b.high,
        b.low,
        b.close,
        b.vbid,
        b.vask,
        b.tick_count,
    );
    m.realized_var = b.realized_var;
    m.bipower_var = b.bipower_var;
    m.drift = b.drift;
    m.vol_imbalance = b.vol_imbalance;
    m.avg_spread_bps = b.avg_spread_bps;
    m.max_abs_return = b.max_abs_return;
    m.avg_ci_ubp = b.avg_ci_ubp;
    m.reject_rate = b.reject_rate;
    m.kind = b.kind;
    m.flags = b.flags;
    unsafe {
        std::ptr::copy_nonoverlapping(bytemuck::bytes_of(&m).as_ptr(), out, NXR_BAR_SIZE)
    };
    NXR_OK
}
