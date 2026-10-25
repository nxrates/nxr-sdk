//! One inbound WebSocket configuration for every tungstenite client in this
//! tree: the CEX forwarder (`nxr-crypto`), the cTrader Open API forwarder and
//! the NXR consumer client. Three copies of these numbers is how two of the
//! three kept tungstenite's defaults (64 MiB message / 16 MiB frame, no
//! TCP_NODELAY) long after the first one was tuned.
//!
//! Caps are explicit because an unbounded peer allocation is a remote OOM:
//!
//! - message cap 4 MiB, frame cap EQUAL to it. A frame cap BELOW the message cap
//!   is a permanent reconnect loop rather than a bound: gemini and bitstamp send
//!   a full-book snapshot as ONE frame with no size limit, and every consumer
//!   here tears the session down on a read error, so each reconnect re-requests
//!   the same oversized snapshot and dies on it again — the venue never gets a
//!   chance to fall back to deltas.
//! - write buffer 8 KiB: the only writes on these sockets are subscription and
//!   heartbeat frames, and buffering 128 KiB before the flush would just delay
//!   the subscription.
//!
//! Both caps are env-overridable for a venue that outgrows 4 MiB:
//! `NXR_WS_MAX_MESSAGE_BYTES` and `NXR_WS_MAX_FRAME_BYTES`. A frame cap larger
//! than the message cap is clamped down to it: the frame limit is checked first
//! at read time, so a bigger frame cap would let a single frame allocate past
//! the message bound before the message check ever runs.

use tokio_tungstenite::tungstenite::protocol::WebSocketConfig;

/// Inbound message cap when nothing overrides it: 4 MiB.
pub const DEFAULT_MAX_MESSAGE_BYTES: usize = 4 << 20;
/// Inbound frame cap when nothing overrides it: equal to the message cap, on
/// purpose (see the module docs for the reconnect loop it prevents).
pub const DEFAULT_MAX_FRAME_BYTES: usize = DEFAULT_MAX_MESSAGE_BYTES;
/// Bytes written before a flush. Subscription frames are small; a large buffer
/// only delays them.
pub const WRITE_BUFFER_BYTES: usize = 8 << 10;

/// Read `name` as a byte count, ignoring an unset, empty or unparsable value so a
/// typo falls back to the default instead of disabling the cap.
fn env_bytes(name: &str, default: usize) -> usize {
    std::env::var(name)
        .ok()
        .and_then(|v| v.trim().parse::<usize>().ok())
        .filter(|v| *v > 0)
        .unwrap_or(default)
}

/// The shared WebSocket configuration: [`DEFAULT_MAX_MESSAGE_BYTES`] message cap,
/// the same frame cap, [`WRITE_BUFFER_BYTES`] write buffer.
///
/// Callers pass it to `connect_async_with_config(.., Some(config), /* nodelay */ true)`:
/// the `true` is TCP_NODELAY before the handshake. Without it Nagle holds a
/// small frame until the delayed ACK, i.e. up to 40 ms of added quote latency on
/// every message.
pub fn ws_config() -> WebSocketConfig {
    let max_message_size = env_bytes("NXR_WS_MAX_MESSAGE_BYTES", DEFAULT_MAX_MESSAGE_BYTES);
    let max_frame_size =
        env_bytes("NXR_WS_MAX_FRAME_BYTES", DEFAULT_MAX_FRAME_BYTES).min(max_message_size);
    WebSocketConfig::default()
        .max_message_size(Some(max_message_size))
        .max_frame_size(Some(max_frame_size))
        .write_buffer_size(WRITE_BUFFER_BYTES)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The frame cap must never exceed the message cap: at read time the frame
    /// limit is enforced first, so a larger frame cap would let one frame
    /// allocate past the message bound.
    #[test]
    fn frame_cap_never_exceeds_message_cap() {
        let cfg = ws_config();
        let msg = cfg.max_message_size.expect("message cap is set");
        let frame = cfg.max_frame_size.expect("frame cap is set");
        assert!(frame <= msg, "frame cap {frame} > message cap {msg}");
        assert_eq!(cfg.write_buffer_size, WRITE_BUFFER_BYTES);
    }

    /// An unparsable override falls back to the default rather than removing the
    /// cap (`max_frame_size(None)` = unbounded).
    #[test]
    fn unparsable_override_keeps_the_default_cap() {
        assert_eq!(env_bytes("NXR_WS_TEST_UNSET_VAR_XYZ", 4096), 4096);
    }
}
