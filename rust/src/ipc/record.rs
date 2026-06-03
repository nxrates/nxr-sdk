//! On-disk and wire record that pairs a `MitchHeader` with an `Index` body.
//!
//! ## Why
//!
//! The MITCH `Index` body (40B) has no timestamp of its own: the observation
//! `mts` (u48, 16 us ticks since 2010-01-01) lives in the 16B `MitchHeader`.
//! Persisting only the body to `.idx` strips the audit-trail timestamp; this
//! record type keeps them co-located so every row is self-dating.
//!
//! ## Layout (56 bytes, `#[repr(C, packed)]`)
//!
//! ```text
//! Offset | Size | Field   | Notes
//! -------|------|---------|--------------------------------------------------
//! 0      | 16   | header  | MitchHeader (mts, type, provider, seq)
//! 16     | 40   | index   | MITCH Index body
//! ```
//!
//! Readers: each row is exactly 56 bytes. `bytemuck::cast_slice::<u8, IndexRecord>`
//! after a file read recovers the stream. See `AppendLog<IndexRecord>`.
//!
//! ## Timestamp conversion
//!
//! `mts = header.get_timestamp()` is 16 us ticks since 2010-01-01.
//! `ns  = mts * 16_000 + EPOCH_NS_2010_01_01` (use `mitch::timestamp::to_epoch_ns`).
//! `ms  = mts * 16 / 1000 + EPOCH_MS_2010_01_01` (use `mitch::timestamp::to_epoch_ms`).

use mitch::header::MitchHeader;
use mitch::index::Index;

/// 56-byte self-dating on-disk record: header carries the observation mts.
#[repr(C, packed)]
#[derive(Debug, Clone, Copy)]
pub struct IndexRecord {
    /// 16-byte MITCH header. Carries u48 mts, provider, sequence.
    pub header: MitchHeader,
    /// 40-byte MITCH Index body (timestamp-free).
    pub index: Index,
}

// Compile-time size assertion: 16 + 40 = 56.
const _: () = assert!(core::mem::size_of::<IndexRecord>() == 56, "IndexRecord must be exactly 56 bytes");

// SAFETY: Both MitchHeader and Index are bytemuck::Pod under the `bytemuck`
// feature (enabled in this crate). IndexRecord is `#[repr(C, packed)]` with
// only Pod fields and no compiler-inserted padding, so it is also Pod.
unsafe impl bytemuck::Zeroable for IndexRecord {}
unsafe impl bytemuck::Pod for IndexRecord {}

impl IndexRecord {
    /// Construct from an explicit header and body.
    #[inline]
    pub fn new(header: MitchHeader, index: Index) -> Self {
        Self { header, index }
    }
}

impl super::AsFrameBytes for IndexRecord {
    #[inline]
    fn as_frame_bytes(&self) -> &[u8] {
        bytemuck::bytes_of(self)
    }
}
