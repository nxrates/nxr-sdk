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
//! - sizing, honestly stated: 4 MiB is 16x below tungstenite's own 64 MiB default
//!   message cap. The largest frames known to us are those gemini and bitstamp
//!   full-book snapshots: a whole book in one frame, with no documented size
//!   limit. Their real size on our symbol list has NOT been measured — nothing
//!   in this tree emits a per-frame size histogram yet — so 4 MiB is a bound
//!   derived from that 64 MiB default and from the snapshots being the only
//!   known unbounded frames, not from a capture.
//!   `nxr_cex_ws_over_cap_total` counts the `Capacity` errors that would prove
//!   the bound too small: a non-zero rate there means raise the ceiling.
//! - write buffer 8 KiB: the only writes on these sockets are subscription and
//!   heartbeat frames, and buffering 128 KiB before the flush would just delay
//!   the subscription.
//!
//! Both caps are env-overridable for a venue that outgrows 4 MiB:
//! `NXR_WS_MAX_MESSAGE_BYTES` and `NXR_WS_MAX_FRAME_BYTES`. A frame cap larger
//! than the message cap is clamped down to it: the frame limit is checked first
//! at read time, so a bigger frame cap would let a single frame allocate past
//! the message bound before the message check ever runs. Both overrides are
//! themselves bounded by [`MAX_CAP_CEILING_BYTES`]: an override is a way to raise
//! a cap, never a way to turn it back into the remote OOM it exists to stop.

use std::sync::atomic::{AtomicBool, Ordering};

use tokio_tungstenite::tungstenite::protocol::WebSocketConfig;

/// Inbound message cap when nothing overrides it: 4 MiB.
pub const DEFAULT_MAX_MESSAGE_BYTES: usize = 4 << 20;
/// Inbound frame cap when nothing overrides it: equal to the message cap, on
/// purpose (see the module docs for the reconnect loop it prevents).
pub const DEFAULT_MAX_FRAME_BYTES: usize = DEFAULT_MAX_MESSAGE_BYTES;
/// Hard ceiling on an env override: 64 MiB, i.e. tungstenite's own default. Above
/// it a typo (`4194304000`) sets a per-socket cap larger than the process, which
/// is the unbounded peer allocation the caps exist to prevent.
pub const MAX_CAP_CEILING_BYTES: usize = 64 << 20;
/// Bytes written before a flush. Subscription frames are small; a large buffer
/// only delays them.
pub const WRITE_BUFFER_BYTES: usize = 8 << 10;

/// Set on the first clamp, so a 40-venue fanout logs the override once per
/// process instead of once per socket.
static CLAMP_LOGGED: AtomicBool = AtomicBool::new(false);

/// Resolve one cap from `raw` (the env value, if any). Unset, empty, unparsable
/// or zero falls back to `default` so a typo can never remove the cap, and
/// anything above [`MAX_CAP_CEILING_BYTES`] is clamped down to it.
fn resolve_bytes(name: &str, raw: Option<&str>, default: usize) -> usize {
    let parsed = raw
        .and_then(|v| v.trim().parse::<usize>().ok())
        .filter(|v| *v > 0);
    let Some(parsed) = parsed else { return default };
    if parsed <= MAX_CAP_CEILING_BYTES {
        return parsed;
    }
    if !CLAMP_LOGGED.swap(true, Ordering::Relaxed) {
        tracing::warn!(
            name,
            requested = parsed,
            ceiling = MAX_CAP_CEILING_BYTES,
            "NXR WS cap override clamped to the ceiling"
        );
    }
    MAX_CAP_CEILING_BYTES
}

/// The (message, frame) caps for the given raw overrides. Pure so the clamps are
/// testable without mutating the process environment.
fn caps(message_raw: Option<&str>, frame_raw: Option<&str>) -> (usize, usize) {
    let message = resolve_bytes(
        "NXR_WS_MAX_MESSAGE_BYTES",
        message_raw,
        DEFAULT_MAX_MESSAGE_BYTES,
    );
    let frame =
        resolve_bytes("NXR_WS_MAX_FRAME_BYTES", frame_raw, DEFAULT_MAX_FRAME_BYTES).min(message);
    (message, frame)
}

/// The shared WebSocket configuration: [`DEFAULT_MAX_MESSAGE_BYTES`] message cap,
/// the same frame cap, [`WRITE_BUFFER_BYTES`] write buffer.
///
/// Callers pass it to `connect_async_with_config(.., Some(config), /* nodelay */ true)`:
/// the `true` is TCP_NODELAY before the handshake. Without it Nagle holds a
/// small frame until the delayed ACK, i.e. up to 40 ms of added quote latency on
/// every message.
pub fn ws_config() -> WebSocketConfig {
    let message_raw = std::env::var("NXR_WS_MAX_MESSAGE_BYTES").ok();
    let frame_raw = std::env::var("NXR_WS_MAX_FRAME_BYTES").ok();
    let (max_message_size, max_frame_size) = caps(message_raw.as_deref(), frame_raw.as_deref());
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

    /// An unset override falls back to the default rather than removing the cap
    /// (`max_frame_size(None)` = unbounded).
    #[test]
    fn unset_override_keeps_the_default_cap() {
        assert_eq!(resolve_bytes("NXR_WS_TEST_UNSET_VAR_XYZ", None, 4096), 4096);
    }

    /// Garbage, empty and zero are all "no usable override": the default stands.
    /// A typo must never widen or remove a cap.
    #[test]
    fn garbage_zero_and_empty_keep_the_default_cap() {
        for raw in ["abc", "", "  ", "0", "-1", "1e6", "12MiB"] {
            assert_eq!(
                resolve_bytes("NXR_WS_TEST_GARBAGE", Some(raw), 4096),
                4096,
                "raw {raw:?} must not override the default"
            );
        }
    }

    /// A usable override is honoured, and whitespace around it does not defeat it.
    #[test]
    fn valid_override_is_honoured() {
        assert_eq!(resolve_bytes("NXR_WS_TEST_VALID", Some("2048"), 4096), 2048);
        assert_eq!(
            resolve_bytes("NXR_WS_TEST_VALID", Some(" 2048\n"), 4096),
            2048
        );
    }

    /// A typo that sets a multi-gigabyte cap is clamped to the ceiling instead:
    /// the override raises the bound, it does not remove it.
    #[test]
    fn over_ceiling_override_is_clamped() {
        assert_eq!(
            resolve_bytes("NXR_WS_TEST_OVER_CAP", Some("4294967296"), 4096),
            MAX_CAP_CEILING_BYTES
        );
        assert_eq!(
            resolve_bytes(
                "NXR_WS_TEST_OVER_CAP",
                Some(&(MAX_CAP_CEILING_BYTES + 1).to_string()),
                4096
            ),
            MAX_CAP_CEILING_BYTES
        );
        // The ceiling itself is allowed, one byte more is not.
        assert_eq!(
            resolve_bytes("NXR_WS_TEST_OVER_CAP", Some("67108863"), 4096),
            67108863
        );
        assert_eq!(MAX_CAP_CEILING_BYTES, 67108864);
    }

    /// The frame clamp still holds when both overrides are set, including the
    /// case where the frame override alone would exceed the message cap.
    #[test]
    fn caps_clamp_frame_to_message_for_both_overrides() {
        assert_eq!(
            caps(Some("8388608"), Some("16777216")),
            (8 << 20, 8 << 20),
            "frame override above the message override must clamp down"
        );
        assert_eq!(
            caps(Some("8388608"), Some("1048576")),
            (8 << 20, 1 << 20),
            "a smaller frame override is kept"
        );
        assert_eq!(
            caps(Some("999999999999"), Some("999999999999")),
            (MAX_CAP_CEILING_BYTES, MAX_CAP_CEILING_BYTES),
            "both overrides clamp at the ceiling and stay ordered"
        );
        assert_eq!(
            caps(Some("abc"), None),
            (DEFAULT_MAX_MESSAGE_BYTES, DEFAULT_MAX_FRAME_BYTES),
            "garbage message override falls back, and the frame default then fits"
        );
    }
}
