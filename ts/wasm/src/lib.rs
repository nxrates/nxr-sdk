//! WASM accelerator for the NXR TypeScript SDK.
//!
//! Exposes `decode_idx_batch` / `decode_idx_record` / `decode_bar_batch` to JS.
//! Built with `wasm-pack build --target web --out-dir ../dist/wasm`.
//!
//! Wire layout is shared with `mitch` (this crate path-deps it). The decode
//! path uses `bytemuck::cast_slice` for zero-copy aliasing into the wasm
//! linear memory buffer.

use mitch::bar::Bar;
use mitch::header::MitchHeader;
use mitch::index::Index;
use mitch::timestamp;
use serde::Serialize;
use wasm_bindgen::prelude::*;

const CI_SCALE: f64 = 16.0;
const SIZE_HEADER: usize = 16;
const SIZE_INDEX: usize = 40;
const SIZE_INDEX_RECORD: usize = SIZE_HEADER + SIZE_INDEX; // 56
const SIZE_BAR: usize = 96;

/// JS-shaped IndexRecord (flat, matches TS `IndexRecord` interface).
#[derive(Serialize)]
struct JsIndexRecord {
    ts_ms: f64, // f64 fits all epoch_ms values exactly through year 287396
    provider: u16,
    /// Serialized as a JS string (bigint) to preserve full u64 precision in JS.
    ticker: String,
    bid: f64,
    ask: f64,
    mid: f64,
    ci_ubp: f64,
    accepted: u8,
    rejected: u8,
    confidence: u8,
    vbid: u32,
    vask: u32,
    tick_count: u16,
    sequence: u16,
}

/// JS-shaped Bar (flat).
#[derive(Serialize)]
struct JsBar {
    open_ms: f64,
    close_ms: f64,
    open: f64,
    high: f64,
    low: f64,
    close: f64,
    vbid: u32,
    vask: u32,
    tick_count: u32,
    realized_var: f32,
    bipower_var: f32,
    drift: f32,
    vol_imbalance: f32,
    avg_spread_bps: f32,
    max_abs_return: f32,
    avg_ci_ubp: u16,
    reject_rate: u16,
    kind: u8,
}

fn decode_one_idx(bytes: &[u8]) -> JsIndexRecord {
    // SAFETY: caller validated `bytes.len() >= 56`.
    let hdr: MitchHeader = unsafe { (bytes.as_ptr() as *const MitchHeader).read_unaligned() };
    let idx: Index =
        unsafe { (bytes[SIZE_HEADER..].as_ptr() as *const Index).read_unaligned() };

    let mts = hdr.get_timestamp();
    let ts_ms = timestamp::to_epoch_ms(mts) as f64;
    let bid = idx.bid;
    let ask = idx.ask;
    let mid = (bid + ask) * 0.5;
    let ci_x = idx.ci as f64 / CI_SCALE;
    let ci_ubp = ci_x * ci_x;

    JsIndexRecord {
        ts_ms,
        provider: hdr.provider_id(),
        ticker: idx.ticker.to_string(),
        bid,
        ask,
        mid,
        ci_ubp,
        accepted: idx.accepted,
        rejected: idx.rejected,
        confidence: idx.confidence,
        vbid: idx.vbid,
        vask: idx.vask,
        tick_count: idx.tick_count,
        sequence: hdr.sequence,
    }
}

fn decode_one_bar(bytes: &[u8]) -> JsBar {
    let b: Bar = unsafe { (bytes.as_ptr() as *const Bar).read_unaligned() };
    JsBar {
        open_ms: b.open_time_ms() as f64,
        close_ms: b.close_time_ms() as f64,
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
    }
}

#[wasm_bindgen]
pub fn decode_idx_record(buf: &[u8]) -> Result<JsValue, JsValue> {
    if buf.len() < SIZE_INDEX_RECORD {
        return Err(JsValue::from_str("buffer too small for IndexRecord (56B)"));
    }
    let rec = decode_one_idx(&buf[..SIZE_INDEX_RECORD]);
    serde_wasm_bindgen::to_value(&rec).map_err(|e| JsValue::from_str(&e.to_string()))
}

#[wasm_bindgen]
pub fn decode_idx_batch(buf: &[u8]) -> Result<JsValue, JsValue> {
    let n = buf.len() / SIZE_INDEX_RECORD;
    let mut out = Vec::with_capacity(n);
    for i in 0..n {
        let off = i * SIZE_INDEX_RECORD;
        out.push(decode_one_idx(&buf[off..off + SIZE_INDEX_RECORD]));
    }
    serde_wasm_bindgen::to_value(&out).map_err(|e| JsValue::from_str(&e.to_string()))
}

#[wasm_bindgen]
pub fn decode_bar_batch(buf: &[u8]) -> Result<JsValue, JsValue> {
    let n = buf.len() / SIZE_BAR;
    let mut out = Vec::with_capacity(n);
    for i in 0..n {
        let off = i * SIZE_BAR;
        out.push(decode_one_bar(&buf[off..off + SIZE_BAR]));
    }
    serde_wasm_bindgen::to_value(&out).map_err(|e| JsValue::from_str(&e.to_string()))
}
