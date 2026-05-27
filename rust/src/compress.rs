//! Shared decompression helpers used by crypto exchange handlers (WS binary
//! frames) and series-factory data sources (Binance/Bybit archive downloads).

use flate2::read::{GzDecoder, ZlibDecoder};
use std::io::Read;

/// Decompress a gzip frame to raw bytes.
pub(crate) fn decode_gzip_bytes(data: &[u8]) -> Option<Vec<u8>> {
    let mut decoder = GzDecoder::new(data);
    let mut out = Vec::new();
    decoder.read_to_end(&mut out).ok()?;
    Some(out)
}

/// Decompress a zlib/deflate frame to raw bytes.
pub(crate) fn decode_zlib_bytes(data: &[u8]) -> Option<Vec<u8>> {
    let mut decoder = ZlibDecoder::new(data);
    let mut out = Vec::new();
    decoder.read_to_end(&mut out).ok()?;
    Some(out)
}

/// Decompress a gzip frame to a UTF-8 string.
pub fn decode_gzip_string(data: &[u8]) -> Option<String> {
    String::from_utf8(decode_gzip_bytes(data)?).ok()
}

/// Decompress a zlib/deflate frame to a UTF-8 string.
pub fn decode_zlib_string(data: &[u8]) -> Option<String> {
    String::from_utf8(decode_zlib_bytes(data)?).ok()
}
