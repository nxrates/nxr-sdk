//! REST f64 wire frame layout — shared header + encoders.
//!
//! Layout (8B header + count × stride × LE f64):
//!
//! ```text
//! [0..4]  magic   = b"NXRF"
//! [4]     kind    : u8   (1=idx, 2=bar, 3=ohlc)
//! [5]     stride  : u8   (#f64 per record)
//! [6..8]  count   : u16 LE
//! [8..]   count × stride × f64 LE
//! ```
//!
//! Single source of truth for the REST handlers that previously inlined the
//! same header layout in `core/src/server/rest.rs::{f64_frame_header,
//! f64_wrap, F64_*}` (Wave 2.D of repo-redundancy audit).
//!
//! The WebSocket binary frame in [`crate::ws_client`] uses a DIFFERENT 8B
//! header (msg_type-prefixed, no NXRF magic) and is intentionally NOT routed
//! through this module — they encode different wire products.

/// Magic prefix for the REST f64 wire frame (`"NXRF"`).
pub const F64_MAGIC: [u8; 4] = *b"NXRF";

/// Frame header length (4B magic + 1B kind + 1B stride + 2B count).
pub const F64_FRAME_HEADER_LEN: usize = 8;

/// Kind discriminator: `Index` records.
pub const F64_KIND_IDX: u8 = 1;
/// Kind discriminator: `Bar` records (s10 / renko).
pub const F64_KIND_BAR: u8 = 2;
/// Kind discriminator: `Ohlc` records.
pub const F64_KIND_OHLC: u8 = 3;

/// Stride (#f64 per record) for `idx` frames.
pub const F64_STRIDE_IDX: u8 = 11;
/// Stride (#f64 per record) for `bar` frames.
pub const F64_STRIDE_BAR: u8 = 20;
/// Stride (#f64 per record) for `ohlc` frames.
pub const F64_STRIDE_OHLC: u8 = 7;

/// Build the 8B frame header. `count` clamped to `u16::MAX` by the caller.
#[inline]
pub fn frame_header(kind: u8, stride: u8, count: u16) -> [u8; F64_FRAME_HEADER_LEN] {
    let mut h = [0u8; F64_FRAME_HEADER_LEN];
    h[0..4].copy_from_slice(&F64_MAGIC);
    h[4] = kind;
    h[5] = stride;
    h[6..8].copy_from_slice(&count.to_le_bytes());
    h
}

/// Pack the `count × stride` f64 payload into a `Vec<u8>`, prepended with the
/// frame header. Caller fills `payload` row-by-row in the documented order.
#[inline]
pub fn wrap_payload(kind: u8, stride: u8, payload: Vec<f64>) -> Vec<u8> {
    let count = (payload.len() / stride as usize).min(u16::MAX as usize) as u16;
    let mut out = Vec::with_capacity(F64_FRAME_HEADER_LEN + payload.len() * 8);
    out.extend_from_slice(&frame_header(kind, stride, count));
    out.extend_from_slice(bytemuck::cast_slice::<f64, u8>(&payload));
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn header_layout_is_canonical() {
        let h = frame_header(F64_KIND_IDX, F64_STRIDE_IDX, 42);
        assert_eq!(&h[0..4], b"NXRF");
        assert_eq!(h[4], 1);
        assert_eq!(h[5], 11);
        assert_eq!(u16::from_le_bytes([h[6], h[7]]), 42);
    }

    #[test]
    fn wrap_payload_round_trip() {
        let payload: Vec<f64> = vec![1.0, 2.0, 3.0, 4.0, 5.0, 6.0, 7.0];
        let out = wrap_payload(F64_KIND_OHLC, F64_STRIDE_OHLC, payload.clone());
        assert_eq!(out.len(), F64_FRAME_HEADER_LEN + payload.len() * 8);
        let count = u16::from_le_bytes([out[6], out[7]]);
        assert_eq!(count, 1);
        let body: &[f64] = bytemuck::cast_slice(&out[F64_FRAME_HEADER_LEN..]);
        assert_eq!(body, payload.as_slice());
    }
}
