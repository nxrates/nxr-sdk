//! Binary WebSocket index-batch wire constants (producer + consumer).
//!
//! Shared by `core::server::ws` (producer) and `ws_client` (consumer) so the
//! server does not depend on the optional client crate feature.

/// Binary WS message type for `Index` batches.
pub const MSG_INDEX: u8 = 1;

/// Number of f64 lanes per `Index` record on the wire.
pub const INDEX_STRIDE: usize = 9;

/// Byte size of one serialised `Index` record (STRIDE × 8 bytes).
pub const INDEX_RECORD_BYTES: usize = INDEX_STRIDE * 8;

/// 8-byte frame header: `[msg_type, _pad, count_lo, count_hi, _pad × 4]`.
pub const FRAME_HEADER_BYTES: usize = 8;
