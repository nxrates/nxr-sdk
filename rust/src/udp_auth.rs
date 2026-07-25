//! Authenticated, replay-resistant forwarder -> aggregator UDP envelope.
//!
//! Wire: `NXR1 | version | flags | key_id | sequence | source_ms | len |
//! payload | HMAC-SHA256`. All integers are big-endian. The HMAC covers the
//! entire header and payload. Inner MITCH bytes are not decoded until the tag
//! has verified.

use std::collections::{HashMap, HashSet};
use std::fmt;
use std::net::SocketAddr;
use std::sync::atomic::{AtomicU64, Ordering};

use anyhow::{Context, Result, ensure};
use hmac::{Hmac, Mac};
use sha2::Sha256;

use crate::pipeline_config::{UdpAuthMode, UdpAuthYml};

pub const MAGIC: &[u8; 4] = b"NXR1";
pub const VERSION: u8 = 1;
pub const HEADER_LEN: usize = 26;
pub const TAG_LEN: usize = 32;
pub const MAX_DATAGRAM_LEN: usize = HEADER_LEN + 56 + TAG_LEN;
const KEY_LEN: usize = 32;
const INDEX_LEN: usize = 56;
const HEARTBEAT_LEN: usize = 32;
/// Replay bitmap capacity. The window must exceed `max_age_ms` x peak frame
/// rate, otherwise a duplicate that is still fresh enough to pass the
/// freshness gate falls off the map and is re-accepted. 1024 covers the live
/// 221 frame/s feed at max_age_ms=500 with ~9x headroom.
const REPLAY_BITS: u64 = 1024;
const REPLAY_WORDS: usize = (REPLAY_BITS / 64) as usize;
/// Frame rate the replay-window sizing is checked against. Deliberately ~2x the
/// measured live rate (221 frame/s, 2026-07-25) so the drain-grace bound below
/// stays valid through growth without anyone re-deriving it.
const ASSUMED_PEAK_FRAME_RATE_HZ: u64 = 500;

/// Largest drain grace the replay bitmap can safely cover, in ms.
///
/// **Why widening staleness is NOT a security relaxation.** The freshness bound
/// (`max_age_ms`) is a heuristic; the REPLAY BITMAP is the actual anti-replay
/// control. A frame that passes freshness still has to present an unseen
/// sequence ([`UdpAuthVerifier::check_and_mark_replay`]). So admitting older
/// frames during a bounded drain does not let an attacker replay anything —
/// PROVIDED the bitmap still spans the widened window. If it does not, the
/// window slides, old sequences fall off the map and become re-acceptable, which
/// reopens exactly the hole that raising `replay_window` from 64 to 1024 closed.
///
/// Hence: `max_age_ms + grace_ms <= replay_window / peak_frame_rate`. Asserted at
/// boot in [`UdpAuthVerifier::from_config`], not left as a comment — it is a
/// relationship between two independently-tunable knobs, and those break
/// silently. Do not "simplify" the bitmap to make this go away.
#[inline]
const fn max_drain_grace_ms(replay_window: u16, max_age_ms: u64) -> u64 {
    let span_ms = (replay_window as u64) * 1_000 / ASSUMED_PEAK_FRAME_RATE_HZ;
    span_ms.saturating_sub(max_age_ms)
}
/// Bound on distinct authenticated senders tracked at once. Entries are only
/// created by a sender that already passed the tag check, so this is not an
/// attacker-reachable allocation; the bound exists because forwarders bind an
/// ephemeral source port and therefore present a new peer on every restart.
const MAX_REPLAY_PEERS: usize = 64;
type HmacSha256 = Hmac<Sha256>;

/// Why a datagram was refused. Carried instead of a formatted string so the
/// ingress path can label a metric without parsing prose (F2: one opaque
/// `bad_auth` bucket could not distinguish a forged tag from clock lag).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AuthReject {
    /// Structurally invalid: length, magic, version, flags or payload length.
    Malformed(&'static str),
    UnknownKey,
    Tag,
    Future,
    Stale,
    Replay,
    OutsideWindow,
    /// Envelope-less datagram under `mode: strict`.
    Unauthenticated,
}

impl AuthReject {
    /// Stable metric label. Never reword: alerts key on these.
    pub fn label(self) -> &'static str {
        match self {
            Self::Malformed(_) => "malformed",
            Self::UnknownKey => "unknown_key",
            Self::Tag => "tag_mismatch",
            Self::Future => "future_ts",
            Self::Stale => "stale",
            Self::Replay => "replay",
            Self::OutsideWindow => "outside_window",
            Self::Unauthenticated => "unauthenticated",
        }
    }
}

impl fmt::Display for AuthReject {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Malformed(d) => write!(f, "{d}"),
            Self::UnknownKey => f.write_str("unknown UDP auth key_id"),
            Self::Tag => f.write_str("UDP auth tag mismatch"),
            Self::Future => f.write_str("authenticated datagram is from the future"),
            Self::Stale => f.write_str("authenticated datagram is stale"),
            Self::Replay => f.write_str("authenticated datagram replayed"),
            Self::OutsideWindow => f.write_str("authenticated datagram is outside replay window"),
            Self::Unauthenticated => {
                f.write_str("unauthenticated UDP frame rejected (udp_auth.mode = strict)")
            }
        }
    }
}

impl std::error::Error for AuthReject {}

#[derive(Debug)]
pub struct UdpAuthSender {
    key_id: u16,
    /// Key schedule (ipad/opad) precomputed once. Cloning the keyed state per
    /// frame skips 2 of the 5 SHA-256 compressions an HMAC over a 82-byte
    /// frame needs -- measured -41% vs re-keying (574 -> 337 ns/frame).
    mac: HmacSha256,
    sequence: AtomicU64,
}

impl UdpAuthSender {
    pub fn from_env() -> Result<Self> {
        let key_id: u16 = std::env::var("NXR_UDP_AUTH_KEY_ID")
            .context("NXR_UDP_AUTH_KEY_ID unset")?
            .parse()
            .context("NXR_UDP_AUTH_KEY_ID must be a non-zero u16")?;
        ensure!(key_id != 0, "NXR_UDP_AUTH_KEY_ID must be non-zero");
        let raw = std::env::var("NXR_UDP_AUTH_KEY").context("NXR_UDP_AUTH_KEY unset")?;
        let key = decode_key(&raw, "NXR_UDP_AUTH_KEY")?;
        // Seed 0, NOT a wall clock. A ns-clock seed made a backward NTP step
        // across a forwarder restart land below the verifier's `highest`, which
        // rejected every subsequent frame permanently and could only be cleared
        // by restarting the RECEIVER. Restart recovery is now the verifier's
        // job (per-peer state + idle expiry), so the sender can use a plain
        // 0-based counter -- which also makes `sequence` directly readable as
        // "frames sealed since process start" for gap/loss inference.
        Ok(Self::new(key_id, key, 0))
    }

    pub fn new(key_id: u16, key: [u8; KEY_LEN], sequence_seed: u64) -> Self {
        Self {
            key_id,
            mac: HmacSha256::new_from_slice(&key).expect("fixed HMAC key length"),
            sequence: AtomicU64::new(sequence_seed),
        }
    }

    /// Seal into reusable caller storage. This is one local HMAC and has no
    /// network/consensus round trip.
    pub fn seal_into(&self, payload: &[u8], source_ts_ms: u64, out: &mut Vec<u8>) -> Result<u64> {
        ensure!(
            matches!(payload.len(), INDEX_LEN | HEARTBEAT_LEN),
            "invalid inner frame length {}",
            payload.len()
        );
        // Wrapping is unreachable in practice (2^64 frames at the 70k/s design
        // ceiling is ~8 million years); the old `sequence != u64::MAX` guard
        // tested the value already handed out and so could never prevent it.
        let sequence = self.sequence.fetch_add(1, Ordering::Relaxed);
        out.clear();
        out.reserve(HEADER_LEN + payload.len() + TAG_LEN);
        out.extend_from_slice(MAGIC);
        out.push(VERSION);
        out.push(0);
        out.extend_from_slice(&self.key_id.to_be_bytes());
        out.extend_from_slice(&sequence.to_be_bytes());
        out.extend_from_slice(&source_ts_ms.to_be_bytes());
        out.extend_from_slice(&(payload.len() as u16).to_be_bytes());
        out.extend_from_slice(payload);
        let mut mac = self.mac.clone();
        mac.update(out);
        out.extend_from_slice(&mac.finalize().into_bytes());
        Ok(sequence)
    }
}

#[derive(Debug)]
struct KeyPolicy {
    /// Pre-keyed verify state, cloned per frame. Same -41% as the sender side.
    mac: HmacSha256,
    allowed_provider_ids: HashSet<u16>,
}

/// Sliding replay window as a ring of bits indexed by `sequence % REPLAY_BITS`
/// (the WireGuard construction). A ring avoids shifting a 1024-bit map on every
/// frame; advancing only has to clear the slots skipped over.
#[derive(Debug, Clone)]
struct ReplayState {
    highest: u64,
    bits: [u64; REPLAY_WORDS],
    /// Verifier-clock ms of the last accepted frame, for idle expiry.
    last_ms: u64,
    initialized: bool,
}

impl Default for ReplayState {
    fn default() -> Self {
        Self {
            highest: 0,
            bits: [0; REPLAY_WORDS],
            last_ms: 0,
            initialized: false,
        }
    }
}

impl ReplayState {
    #[inline]
    fn slot(sequence: u64) -> (usize, u64) {
        let idx = sequence % REPLAY_BITS;
        ((idx / 64) as usize, 1u64 << (idx % 64))
    }

    fn reset_to(&mut self, sequence: u64, now_ms: u64) {
        self.bits = [0; REPLAY_WORDS];
        let (w, m) = Self::slot(sequence);
        self.bits[w] = m;
        self.highest = sequence;
        self.last_ms = now_ms;
        self.initialized = true;
    }
}

#[derive(Debug)]
pub struct UdpAuthVerifier {
    keys: HashMap<u16, KeyPolicy>,
    /// Keyed by `(key_id, peer)`, NOT key_id alone. Two processes holding the
    /// same key_id (a rolling forwarder deploy overlaps two pods; `strategy:
    /// Recreate` narrows but does not close the window) own independent
    /// sequence spaces -- sharing one window made the draining pod's every
    /// frame fail as outside-window.
    replay: HashMap<(u16, SocketAddr), ReplayState>,
    max_age_ms: u64,
    max_future_ms: u64,
    replay_window: u16,
    mode: UdpAuthMode,
    /// Configured drain grace, validated against the replay bitmap at boot
    /// (see [`max_drain_grace_ms`]). 0 disables the mechanism entirely.
    drain_grace_ms: u64,
    /// `Some(deadline_ms)` while a post-boot backlog drain is being tolerated.
    /// Armed by [`Self::arm_drain_grace`], cleared by [`Self::disarm_drain_grace`].
    drain_grace_until_ms: Option<u64>,
    /// Frames admitted ONLY because the grace was armed. The caller publishes
    /// this so the mechanism is provably idle in steady state; `metrics` is an
    /// optional dep here, so the sdk counts and the core emits.
    grace_admitted: u64,
}

/// Outcome of [`UdpAuthVerifier::accept`].
#[derive(Debug)]
pub enum Ingress<'a> {
    /// Carried an `NXR1` envelope that passed every check.
    Sealed(VerifiedFrame<'a>),
    /// No envelope; admitted only because the mode is `Permissive`. Carries no
    /// `key_id`, so the caller's provider-authority gate cannot be applied --
    /// identical to pre-udp_auth behavior, and exactly why this is transitional.
    Legacy(&'a [u8]),
}

#[derive(Debug)]
pub struct VerifiedFrame<'a> {
    pub key_id: u16,
    pub sequence: u64,
    pub source_ts_ms: u64,
    pub payload: &'a [u8],
}

impl UdpAuthVerifier {
    pub fn from_config(cfg: &UdpAuthYml) -> Result<Self> {
        ensure!(
            cfg.max_age_ms > 0 && cfg.max_age_ms <= 5_000,
            "network.udp_auth.max_age_ms must be in [1, 5000]"
        );
        ensure!(
            cfg.max_future_ms <= 5_000,
            "network.udp_auth.max_future_ms must be <= 5000"
        );
        ensure!(
            (1..=REPLAY_BITS as u16).contains(&cfg.replay_window),
            "network.udp_auth.replay_window must be in [1, {REPLAY_BITS}]"
        );
        ensure!(!cfg.keys.is_empty(), "network.udp_auth.keys is empty");
        // Grace vs replay-bitmap capacity. See `max_drain_grace_ms`: staleness is
        // a heuristic, the bitmap is the real anti-replay control, so the grace is
        // only safe while the bitmap still spans it.
        let grace_cap = max_drain_grace_ms(cfg.replay_window, cfg.max_age_ms);
        ensure!(
            cfg.drain_grace_ms <= grace_cap,
            "network.udp_auth.drain_grace_ms = {} exceeds what replay_window = {} can cover \
             at {} frame/s (max {} ms with max_age_ms = {}). Raise replay_window (<= {}) in the \
             SAME change or lower the grace — otherwise the replay window slides and old \
             sequences become re-acceptable.",
            cfg.drain_grace_ms,
            cfg.replay_window,
            ASSUMED_PEAK_FRAME_RATE_HZ,
            grace_cap,
            cfg.max_age_ms,
            REPLAY_BITS
        );
        let mut keys = HashMap::with_capacity(cfg.keys.len());
        for item in &cfg.keys {
            ensure!(item.key_id != 0, "UDP auth key_id 0 is reserved");
            ensure!(
                !item.allowed_provider_ids.is_empty(),
                "UDP auth key {} has no allowed_provider_ids",
                item.key_id
            );
            let raw = std::env::var(&item.key_env)
                .with_context(|| format!("UDP auth secret env {} unset", item.key_env))?;
            let key = decode_key(&raw, &item.key_env)?;
            let policy = KeyPolicy {
                mac: HmacSha256::new_from_slice(&key).expect("fixed HMAC key length"),
                allowed_provider_ids: item.allowed_provider_ids.iter().copied().collect(),
            };
            ensure!(
                keys.insert(item.key_id, policy).is_none(),
                "duplicate UDP auth key_id {}",
                item.key_id
            );
        }
        Ok(Self {
            keys,
            replay: HashMap::with_capacity(cfg.keys.len()),
            max_age_ms: cfg.max_age_ms,
            max_future_ms: cfg.max_future_ms,
            replay_window: cfg.replay_window,
            mode: cfg.mode,
            drain_grace_ms: cfg.drain_grace_ms,
            drain_grace_until_ms: None,
            grace_admitted: 0,
        })
    }

    #[inline]
    pub fn mode(&self) -> UdpAuthMode {
        self.mode
    }

    /// Ingress classifier for the UDP hot path.
    ///
    /// Dispatch is on the `NXR1` magic, NOT on the mode: anything claiming to
    /// be sealed is verified in full and its failures are returned as errors.
    /// A forged/corrupt tag therefore never degrades into a legacy accept, so
    /// `Permissive` cannot be used to bypass authentication -- it only decides
    /// whether an envelope-LESS datagram is admitted.
    pub fn accept<'a>(
        &mut self,
        datagram: &'a [u8],
        peer: SocketAddr,
        now_ms: u64,
    ) -> std::result::Result<Ingress<'a>, AuthReject> {
        if datagram.len() >= MAGIC.len() && &datagram[..MAGIC.len()] == MAGIC {
            return self.verify(datagram, peer, now_ms).map(Ingress::Sealed);
        }
        match self.mode {
            UdpAuthMode::Permissive => Ok(Ingress::Legacy(datagram)),
            UdpAuthMode::Strict => Err(AuthReject::Unauthenticated),
        }
    }

    /// Authenticate length/version/tag/freshness/replay before returning any
    /// inner MITCH bytes to the caller.
    pub fn verify<'a>(
        &mut self,
        datagram: &'a [u8],
        peer: SocketAddr,
        now_ms: u64,
    ) -> std::result::Result<VerifiedFrame<'a>, AuthReject> {
        use AuthReject::Malformed;
        if datagram.len() < HEADER_LEN + TAG_LEN {
            return Err(Malformed("authenticated datagram too short"));
        }
        if &datagram[..4] != MAGIC {
            return Err(Malformed("bad UDP auth magic"));
        }
        if datagram[4] != VERSION {
            return Err(Malformed("unsupported UDP auth version"));
        }
        if datagram[5] != 0 {
            return Err(Malformed("unsupported UDP auth flags"));
        }
        let key_id = u16::from_be_bytes(datagram[6..8].try_into().unwrap());
        let sequence = u64::from_be_bytes(datagram[8..16].try_into().unwrap());
        let source_ts_ms = u64::from_be_bytes(datagram[16..24].try_into().unwrap());
        let payload_len = u16::from_be_bytes(datagram[24..26].try_into().unwrap()) as usize;
        if !matches!(payload_len, INDEX_LEN | HEARTBEAT_LEN) {
            return Err(Malformed("invalid authenticated payload length"));
        }
        if datagram.len() != HEADER_LEN + payload_len + TAG_LEN {
            return Err(Malformed("authenticated datagram length mismatch"));
        }

        let policy = self.keys.get(&key_id).ok_or(AuthReject::UnknownKey)?;
        let signed_len = HEADER_LEN + payload_len;
        let mut mac = policy.mac.clone();
        mac.update(&datagram[..signed_len]);
        mac.verify_slice(&datagram[signed_len..])
            .map_err(|_| AuthReject::Tag)?;

        if source_ts_ms > now_ms.saturating_add(self.max_future_ms) {
            return Err(AuthReject::Future);
        }
        // Freshness, widened ONLY while a bounded post-boot drain is armed. A
        // queued frame is legitimately older than the drain instant, so without
        // this a backlog would be rejected wholesale as stale the moment
        // forwarders start sealing. Anti-replay is unaffected — see
        // `max_drain_grace_ms` for why, and for the bound that keeps it true.
        let age_ms = now_ms.saturating_sub(source_ts_ms);
        if age_ms > self.max_age_ms {
            let grace = match self.drain_grace_until_ms {
                Some(deadline) if now_ms <= deadline => self.drain_grace_ms,
                Some(_) => {
                    self.drain_grace_until_ms = None; // expired: latch back to strict
                    0
                }
                None => 0,
            };
            if age_ms > self.max_age_ms + grace {
                return Err(AuthReject::Stale);
            }
            self.grace_admitted += 1;
        }
        self.check_and_mark_replay(key_id, peer, sequence, now_ms)?;
        Ok(VerifiedFrame {
            key_id,
            sequence,
            source_ts_ms,
            payload: &datagram[HEADER_LEN..signed_len],
        })
    }

    /// Arm a one-shot drain grace for `drain_grace_ms` from `now_ms`.
    ///
    /// Call ONLY where a backlog is genuinely expected: the ingest listener
    /// adopting a socket that was pre-bound across the process's own boot. Not a
    /// general staleness relaxation — it self-expires, and
    /// [`Self::disarm_drain_grace`] should be called as soon as the queue is
    /// observed drained so the strict bound returns early rather than at expiry.
    pub fn arm_drain_grace(&mut self, now_ms: u64) -> u64 {
        if self.drain_grace_ms == 0 {
            return 0;
        }
        self.drain_grace_until_ms = Some(now_ms.saturating_add(self.drain_grace_ms));
        self.drain_grace_ms
    }

    /// Latch back to the strict freshness bound. Idempotent.
    pub fn disarm_drain_grace(&mut self) {
        self.drain_grace_until_ms = None;
    }

    /// True while a drain grace is armed and unexpired.
    #[inline]
    pub fn drain_grace_armed(&self) -> bool {
        self.drain_grace_until_ms.is_some()
    }

    /// Count of frames admitted only because the grace was armed.
    #[inline]
    pub fn grace_admitted(&self) -> u64 {
        self.grace_admitted
    }

    #[inline]
    pub fn provider_allowed(&self, key_id: u16, provider_id: u16) -> bool {
        self.keys
            .get(&key_id)
            .is_some_and(|k| k.allowed_provider_ids.contains(&provider_id))
    }

    fn check_and_mark_replay(
        &mut self,
        key_id: u16,
        peer: SocketAddr,
        sequence: u64,
        now_ms: u64,
    ) -> std::result::Result<(), AuthReject> {
        self.evict_replay_state(now_ms);
        let window = self.replay_window as u64;
        let max_age_ms = self.max_age_ms;
        let state = self.replay.entry((key_id, peer)).or_default();
        // A sender that has been silent for longer than the freshness window
        // has, from this verifier's point of view, no in-flight frames left to
        // protect: nothing older than max_age_ms can pass the staleness gate
        // above. Re-initializing here is what makes a forwarder restart (new
        // sequence space, possibly LOWER than the old one) recover on its own
        // instead of being rejected until the verifier itself restarts.
        if !state.initialized || now_ms.saturating_sub(state.last_ms) > max_age_ms {
            state.reset_to(sequence, now_ms);
            return Ok(());
        }
        if sequence > state.highest {
            // Clear the slots stepped over so a stale mark from the previous
            // wrap cannot be mistaken for this generation's.
            let advance = sequence - state.highest;
            if advance >= REPLAY_BITS {
                state.reset_to(sequence, now_ms);
                return Ok(());
            }
            for s in (state.highest + 1)..=sequence {
                let (w, m) = ReplayState::slot(s);
                state.bits[w] &= !m;
            }
            let (w, m) = ReplayState::slot(sequence);
            state.bits[w] |= m;
            state.highest = sequence;
            state.last_ms = now_ms;
            return Ok(());
        }
        if state.highest - sequence >= window {
            return Err(AuthReject::OutsideWindow);
        }
        let (w, m) = ReplayState::slot(sequence);
        if state.bits[w] & m != 0 {
            return Err(AuthReject::Replay);
        }
        state.bits[w] |= m;
        state.last_ms = now_ms;
        Ok(())
    }

    /// Drop per-peer windows that can no longer refuse anything (idle past the
    /// freshness gate), and hard-bound the map: forwarders bind an ephemeral
    /// source port, so every restart presents a new peer.
    fn evict_replay_state(&mut self, now_ms: u64) {
        let max_age_ms = self.max_age_ms;
        if self.replay.len() > MAX_REPLAY_PEERS {
            self.replay
                .retain(|_, s| now_ms.saturating_sub(s.last_ms) <= max_age_ms);
        }
        if self.replay.len() > MAX_REPLAY_PEERS {
            // Still over bound => many live senders on one key. Evict the
            // least-recently-used so the map cannot grow without limit.
            if let Some(&victim) = self
                .replay
                .iter()
                .min_by_key(|(_, s)| s.last_ms)
                .map(|(k, _)| k)
            {
                self.replay.remove(&victim);
            }
        }
    }
}

fn decode_key(raw: &str, label: &str) -> Result<[u8; KEY_LEN]> {
    let bytes = hex::decode(raw.trim().strip_prefix("0x").unwrap_or(raw.trim()))
        .with_context(|| format!("{label} must be 32-byte hex"))?;
    bytes
        .try_into()
        .map_err(|v: Vec<u8>| anyhow::anyhow!("{label} must be 32 bytes, got {}", v.len()))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The forwarder as the verifier sees it. Replay state is per-peer, so a
    /// second address is a DIFFERENT sequence space (see `peers_do_not_share_a_window`).
    const PEER: SocketAddr =
        SocketAddr::new(std::net::IpAddr::V4(std::net::Ipv4Addr::LOCALHOST), 5001);
    const PEER2: SocketAddr =
        SocketAddr::new(std::net::IpAddr::V4(std::net::Ipv4Addr::LOCALHOST), 5002);

    fn verifier(key: [u8; 32]) -> UdpAuthVerifier {
        verifier_mode(key, UdpAuthMode::Strict)
    }

    fn verifier_mode(key: [u8; 32], mode: UdpAuthMode) -> UdpAuthVerifier {
        UdpAuthVerifier {
            keys: HashMap::from([(
                7,
                KeyPolicy {
                    mac: HmacSha256::new_from_slice(&key).unwrap(),
                    allowed_provider_ids: HashSet::from([0, 101]),
                },
            )]),
            replay: HashMap::new(),
            max_age_ms: 100,
            max_future_ms: 10,
            replay_window: 64,
            mode,
            drain_grace_ms: 0,
            drain_grace_until_ms: None,
            grace_admitted: 0,
        }
    }

    /// A raw MITCH Index frame: 56 B with no `NXR1` envelope, i.e. exactly what
    /// the forwarders send before the sealed rollout.
    fn raw_mitch() -> [u8; INDEX_LEN] {
        [0x42; INDEX_LEN]
    }

    fn frame(sender: &UdpAuthSender, ts: u64) -> Vec<u8> {
        let mut out = Vec::new();
        sender.seal_into(&[0x42; INDEX_LEN], ts, &mut out).unwrap();
        out
    }

    #[test]
    fn tamper_replay_unknown_key_stale_and_provider_binding() {
        let key = [9u8; 32];
        let sender = UdpAuthSender::new(7, key, 100);
        let good = frame(&sender, 1_000);
        let mut v = verifier(key);
        let verified = v.verify(&good, PEER, 1_001).unwrap();
        assert!(v.provider_allowed(verified.key_id, 101));
        assert!(!v.provider_allowed(verified.key_id, 102));
        assert!(
            v.verify(&good, PEER, 1_001)
                .unwrap_err()
                .to_string()
                .contains("replayed")
        );
        let mut tampered = frame(&sender, 1_000);
        tampered[HEADER_LEN + 3] ^= 1;
        assert!(
            v.verify(&tampered, PEER, 1_001)
                .unwrap_err()
                .to_string()
                .contains("tag")
        );
        let unknown = UdpAuthSender::new(8, key, 200);
        assert!(
            v.verify(&frame(&unknown, 1_000), PEER, 1_001)
                .unwrap_err()
                .to_string()
                .contains("unknown")
        );
        assert!(
            v.verify(&frame(&sender, 800), PEER, 1_001)
                .unwrap_err()
                .to_string()
                .contains("stale")
        );
    }

    // ── Transition-mode matrix (permissive cutover) ──────────────────────
    // The security claim under test: mode changes ONLY the disposition of an
    // envelope-less datagram. Everything wearing an `NXR1` header is verified
    // identically in both modes.

    /// 1. Sealed frames are accepted in BOTH modes, and permissive does not
    ///    alter the recovered payload/key binding.
    #[test]
    fn sealed_frame_accepted_in_both_modes() {
        let key = [11u8; 32];
        for mode in [UdpAuthMode::Strict, UdpAuthMode::Permissive] {
            let sender = UdpAuthSender::new(7, key, 1);
            let sealed = frame(&sender, 1_000);
            match verifier_mode(key, mode)
                .accept(&sealed, PEER, 1_001)
                .unwrap()
            {
                Ingress::Sealed(v) => {
                    assert_eq!(v.key_id, 7, "{mode:?}");
                    assert_eq!(v.payload, &[0x42; INDEX_LEN], "{mode:?}");
                }
                Ingress::Legacy(_) => panic!("{mode:?}: sealed frame classified as legacy"),
            }
        }
    }

    /// 2. Permissive admits raw MITCH — the property that keeps the feed up
    ///    while forwarders still send unsealed frames.
    #[test]
    fn raw_frame_accepted_in_permissive() {
        let raw = raw_mitch();
        match verifier_mode([1u8; 32], UdpAuthMode::Permissive)
            .accept(&raw, PEER, 1_000)
            .unwrap()
        {
            Ingress::Legacy(payload) => assert_eq!(payload, &raw[..]),
            Ingress::Sealed(_) => panic!("raw frame must not classify as sealed"),
        }
    }

    /// 3. Strict rejects raw MITCH — the invariant `signed_quotes` depends on.
    #[test]
    fn raw_frame_rejected_in_strict() {
        let err = verifier_mode([1u8; 32], UdpAuthMode::Strict)
            .accept(&raw_mitch(), PEER, 1_000)
            .unwrap_err()
            .to_string();
        assert!(err.contains("unauthenticated"), "unexpected error: {err}");
    }

    /// 4. THE load-bearing test: a frame claiming `NXR1` with a bad tag is
    ///    REJECTED even in permissive. If this ever falls back to Legacy, an
    ///    attacker downgrades authentication by corrupting one byte.
    #[test]
    fn bad_tag_rejected_even_in_permissive() {
        let key = [4u8; 32];
        let sender = UdpAuthSender::new(7, key, 1);
        let mut tampered = frame(&sender, 1_000);
        tampered[HEADER_LEN + 3] ^= 1;
        let err = verifier_mode(key, UdpAuthMode::Permissive)
            .accept(&tampered, PEER, 1_001)
            .unwrap_err()
            .to_string();
        assert!(err.contains("tag"), "expected tag rejection, got: {err}");

        // Same for a wrong-key sender and a truncated envelope: neither may
        // degrade to a legacy accept.
        let wrong = UdpAuthSender::new(7, [5u8; 32], 1);
        assert!(
            verifier_mode(key, UdpAuthMode::Permissive)
                .accept(&frame(&wrong, 1_000), PEER, 1_001)
                .is_err()
        );
        let mut truncated = frame(&sender, 1_000);
        truncated.truncate(HEADER_LEN + INDEX_LEN);
        assert!(
            verifier_mode(key, UdpAuthMode::Permissive)
                .accept(&truncated, PEER, 1_001)
                .is_err()
        );
    }

    /// 5. Replay protection still applies through `accept`, in permissive too.
    #[test]
    fn replay_rejected_through_accept_in_permissive() {
        let key = [6u8; 32];
        let sender = UdpAuthSender::new(7, key, 1);
        let sealed = frame(&sender, 1_000);
        let mut v = verifier_mode(key, UdpAuthMode::Permissive);
        assert!(v.accept(&sealed, PEER, 1_001).is_ok());
        let err = v.accept(&sealed, PEER, 1_001).unwrap_err().to_string();
        assert!(err.contains("replayed"), "unexpected error: {err}");
    }

    /// 6. Provider authority is unchanged by mode: a sealed frame's key may
    ///    still only speak for its allowlisted MITCH providers.
    #[test]
    fn provider_authority_enforced_in_permissive() {
        let key = [8u8; 32];
        let sender = UdpAuthSender::new(7, key, 1);
        let sealed = frame(&sender, 1_000);
        let mut v = verifier_mode(key, UdpAuthMode::Permissive);
        let Ingress::Sealed(verified) = v.accept(&sealed, PEER, 1_001).unwrap() else {
            panic!("sealed frame classified as legacy");
        };
        assert!(v.provider_allowed(verified.key_id, 101));
        assert!(!v.provider_allowed(verified.key_id, 102));
    }

    /// Mode must default to strict when the YAML omits it — an operator can
    /// never weaken ingest by forgetting a field.
    #[test]
    fn mode_defaults_to_strict() {
        let yml: crate::pipeline_config::UdpAuthYml = serde_yml::from_str(
            "max_age_ms: 500\nmax_future_ms: 100\nreplay_window: 64\n\
             keys:\n  - { key_id: 1, key_env: X, allowed_provider_ids: [0] }\n",
        )
        .expect("schema parses without `mode`");
        assert_eq!(yml.mode, UdpAuthMode::Strict);
    }

    /// F4: two processes holding the SAME key_id own independent sequence
    /// spaces. Sharing one window made the draining pod of a rolling forwarder
    /// deploy fail every frame as outside-window (the `bad_auth=143` shape).
    #[test]
    fn peers_do_not_share_a_window() {
        let key = [12u8; 32];
        let mut v = verifier(key);
        // Old pod, well into its sequence space.
        let old = UdpAuthSender::new(7, key, 10_000);
        assert!(v.verify(&frame(&old, 1_000), PEER, 1_001).is_ok());
        // New pod, fresh 0-based counter, different ephemeral source port.
        let new = UdpAuthSender::new(7, key, 0);
        assert!(
            v.verify(&frame(&new, 1_000), PEER2, 1_001).is_ok(),
            "second sender on the same key must not inherit the first's window"
        );
        // Both keep flowing, interleaved.
        assert!(v.verify(&frame(&old, 1_000), PEER, 1_001).is_ok());
        assert!(v.verify(&frame(&new, 1_000), PEER, 1_001).is_err()); // seq 1 << PEER's highest
        assert!(v.verify(&frame(&new, 1_000), PEER2, 1_001).is_ok());
    }

    /// F5: a sender that restarts with a LOWER sequence used to be rejected
    /// forever, recoverable only by restarting the verifier. It must self-heal
    /// once the peer has been idle past the freshness gate -- safe because no
    /// frame older than `max_age_ms` can reach the replay check at all.
    #[test]
    fn restart_with_lower_sequence_recovers_after_idle() {
        let key = [13u8; 32];
        let mut v = verifier(key); // max_age_ms = 100
        let before = UdpAuthSender::new(7, key, 500_000);
        assert!(v.verify(&frame(&before, 1_000), PEER, 1_000).is_ok());
        // Same peer address, restarted process, counter back to 0.
        let after = UdpAuthSender::new(7, key, 0);
        assert_eq!(
            v.verify(&frame(&after, 1_000), PEER, 1_000).unwrap_err(),
            AuthReject::OutsideWindow,
            "while the old space is still live, a backward jump is a replay signal"
        );
        // now_ms advanced past max_age_ms with nothing accepted => window is
        // dead weight and is re-initialized.
        assert!(
            v.verify(&frame(&after, 1_500), PEER, 1_500).is_ok(),
            "restart must recover without touching the verifier"
        );
    }

    /// The window is only meaningful if it outlives the freshness gate; a 64
    /// entry map could not cover 500 ms at the live 221 frame/s.
    #[test]
    fn window_spans_more_than_64_sequences() {
        let key = [14u8; 32];
        let mut v = verifier(key);
        v.replay_window = 1024;
        let s = UdpAuthSender::new(7, key, 0);
        let frames: Vec<Vec<u8>> = (0..600).map(|_| frame(&s, 1_000)).collect();
        // Deliver newest-first: every one of the 600 is inside the window.
        for f in frames.iter().rev() {
            assert!(v.verify(f, PEER, 1_000).is_ok());
        }
        // And each is still individually refused as a duplicate.
        for f in frames.iter() {
            assert_eq!(v.verify(f, PEER, 1_000).unwrap_err(), AuthReject::Replay);
        }
    }

    /// Reject reasons must stay distinguishable: F2 showed one opaque
    /// `bad_auth` bucket cannot tell a forged tag from clock lag.
    #[test]
    fn reject_labels_are_distinct() {
        let key = [15u8; 32];
        let sender = UdpAuthSender::new(7, key, 1);
        let mut v = verifier(key);
        assert_eq!(
            v.verify(&frame(&sender, 800), PEER, 1_001)
                .unwrap_err()
                .label(),
            "stale"
        );
        assert_eq!(
            v.verify(&frame(&sender, 2_000), PEER, 1_001)
                .unwrap_err()
                .label(),
            "future_ts"
        );
        let mut tampered = frame(&sender, 1_000);
        tampered[HEADER_LEN + 1] ^= 1;
        assert_eq!(
            v.verify(&tampered, PEER, 1_001).unwrap_err().label(),
            "tag_mismatch"
        );
        assert_eq!(
            v.verify(&frame(&UdpAuthSender::new(9, key, 1), 1_000), PEER, 1_001)
                .unwrap_err()
                .label(),
            "unknown_key"
        );
        let good = frame(&sender, 1_000);
        assert!(v.verify(&good, PEER, 1_001).is_ok());
        assert_eq!(v.verify(&good, PEER, 1_001).unwrap_err().label(), "replay");
    }

    #[test]
    fn strict_length_rejects_appended_bytes() {
        let key = [3u8; 32];
        let sender = UdpAuthSender::new(7, key, 1);
        let mut bytes = frame(&sender, 10);
        bytes.push(0);
        assert!(verifier(key).verify(&bytes, PEER, 10).is_err());
    }

    #[test]
    fn auth_hot_path_measurement() {
        let key = [5u8; 32];
        let sender = UdpAuthSender::new(7, key, 1);
        let mut bytes = Vec::with_capacity(MAX_DATAGRAM_LEN);
        const N: u64 = 10_000;
        let start = std::time::Instant::now();
        for i in 0..N {
            sender
                .seal_into(&[0u8; INDEX_LEN], 1_000 + i, &mut bytes)
                .unwrap();
            std::hint::black_box(&bytes);
        }
        let seal_ns = start.elapsed().as_nanos() as f64 / N as f64;

        let source = UdpAuthSender::new(7, key, 100_000);
        let frames: Vec<Vec<u8>> = (0..N)
            .map(|i| {
                let mut frame = Vec::with_capacity(MAX_DATAGRAM_LEN);
                source
                    .seal_into(&[0u8; INDEX_LEN], 50_000 + i, &mut frame)
                    .unwrap();
                frame
            })
            .collect();
        let mut verifier = verifier(key);
        let start = std::time::Instant::now();
        for (i, frame) in frames.iter().enumerate() {
            std::hint::black_box(verifier.verify(frame, PEER, 50_000 + i as u64).unwrap());
        }
        let verify_ns = start.elapsed().as_nanos() as f64 / N as f64;
        eprintln!(
            "udp auth: seal={seal_ns:.1} ns/frame ({:.0}/s), verify={verify_ns:.1} ns/frame ({:.0}/s)",
            1e9 / seal_ns,
            1e9 / verify_ns,
        );
    }

    // ── Drain grace (post-boot backlog) ──────────────────────────────────────

    /// A frame queued across the core's boot is legitimately older than the
    /// drain instant. Armed, it must be admitted; disarmed, it must be rejected
    /// as stale — the grace is a bounded one-shot, not a looser bound.
    #[test]
    fn drain_grace_admits_a_backlog_frame_only_while_armed() {
        let key = [3u8; 32];
        let sender = UdpAuthSender::new(7, key, 0);
        let mut v = verifier(key); // max_age_ms = 100
        v.drain_grace_ms = 2_000;

        // 1.5 s old: far outside max_age, inside the grace.
        let stale = frame(&sender, 1_000);
        assert_eq!(
            v.verify(&stale, PEER, 2_500).map(|_| ()),
            Err(AuthReject::Stale),
            "unarmed: a 1.5 s-old frame must be stale"
        );

        assert_eq!(v.arm_drain_grace(2_500), 2_000);
        assert!(v.drain_grace_armed());
        let stale2 = frame(&sender, 1_000);
        assert!(
            v.verify(&stale2, PEER, 2_500).is_ok(),
            "armed: the same age must be admitted"
        );
        assert_eq!(v.grace_admitted(), 1, "grace admits must be counted");

        v.disarm_drain_grace();
        let stale3 = frame(&sender, 1_000);
        assert_eq!(
            v.verify(&stale3, PEER, 2_500).map(|_| ()),
            Err(AuthReject::Stale),
            "disarmed: back to the strict bound immediately"
        );
    }

    /// The grace self-expires even if nobody disarms it, so a wedged drain
    /// cannot leave freshness permanently widened.
    #[test]
    fn drain_grace_expires_on_its_own() {
        let key = [4u8; 32];
        let sender = UdpAuthSender::new(7, key, 0);
        let mut v = verifier(key);
        v.drain_grace_ms = 2_000;
        v.arm_drain_grace(1_000); // deadline 3_000

        let f = frame(&sender, 2_000);
        assert_eq!(
            v.verify(&f, PEER, 3_500).map(|_| ()),
            Err(AuthReject::Stale),
            "past the deadline the grace must not apply"
        );
        assert!(!v.drain_grace_armed(), "expiry must latch back to strict");
    }

    /// THE security property: widening staleness does not weaken anti-replay,
    /// because the bitmap — not the freshness bound — is the control. A
    /// duplicate sequence must be refused in BOTH states.
    #[test]
    fn drain_grace_does_not_weaken_replay_protection() {
        let key = [5u8; 32];
        let sender = UdpAuthSender::new(7, key, 0);
        let mut v = verifier(key);
        v.drain_grace_ms = 2_000;
        v.arm_drain_grace(2_500);

        let f = frame(&sender, 1_000);
        assert!(v.verify(&f, PEER, 2_500).is_ok(), "first admit under grace");
        assert_eq!(
            v.verify(&f, PEER, 2_500).map(|_| ()),
            Err(AuthReject::Replay),
            "the SAME sealed bytes must be refused as a replay even under grace"
        );
    }

    /// The grace/bitmap relationship is asserted at boot, not left as a comment:
    /// it couples two independently-tunable knobs and would break silently.
    #[test]
    fn grace_exceeding_the_replay_bitmap_is_a_boot_error() {
        // 1024 slots at the assumed 500 frame/s = 2048 ms of span; max_age 500
        // leaves 1548 ms for grace.
        assert_eq!(max_drain_grace_ms(1024, 500), 1_548);
        // A narrow window leaves nothing: 64 slots = 128 ms < max_age.
        assert_eq!(max_drain_grace_ms(64, 500), 0);
        // Raising the window raises the ceiling in lock-step, which is the
        // documented remedy.
        assert!(max_drain_grace_ms(1024, 100) > max_drain_grace_ms(512, 100));
    }

}
