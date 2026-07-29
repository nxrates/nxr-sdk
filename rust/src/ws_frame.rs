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

/// Binary WS message type for `Index` batches CARRYING `flags` (lane v2).
///
/// A separate message type, NOT a wider [`MSG_INDEX`]: the 8-byte frame header
/// publishes `count` but not the stride, so every existing consumer hardcodes
/// `count × 9`. Appending a tenth lane to `MSG_INDEX` would therefore not fail
/// loudly — it would slide every record after the first by one lane and hand
/// clients silently wrong PRICES. v2 is opt-in via `/v1/stream?v=2`;
/// `MSG_INDEX` is frozen at stride 9 forever.
pub const MSG_INDEX_V2: u8 = 2;

/// f64 lanes per v2 `Index` record: [`INDEX_STRIDE`] + `flags`.
///
/// `flags` is what makes the `confidence` lane decodable at all: its meaning is
/// flag-selected (`FLAG_CONF_ACTIVE` ⇒ packed ticking-leg count, else the legacy
/// `byte/255` fraction), and v1 never shipped the selector.
pub const INDEX_V2_STRIDE: usize = INDEX_STRIDE + 1;

/// Byte size of one serialised v2 `Index` record.
pub const INDEX_V2_RECORD_BYTES: usize = INDEX_V2_STRIDE * 8;

/// 8-byte frame header: `[msg_type, _pad, count_lo, count_hi, _pad × 4]`.
pub const FRAME_HEADER_BYTES: usize = 8;
