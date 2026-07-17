//! Authenticated, replay-resistant forwarder -> aggregator UDP envelope.
//!
//! Wire: `NXR1 | version | flags | key_id | sequence | source_ms | len |
//! payload | HMAC-SHA256`. All integers are big-endian. The HMAC covers the
//! entire header and payload. Inner MITCH bytes are not decoded until the tag
//! has verified.

use std::collections::{HashMap, HashSet};
use std::sync::atomic::{AtomicU64, Ordering};

use anyhow::{Context, Result, bail, ensure};
use hmac::{Hmac, Mac};
use sha2::Sha256;

use crate::pipeline_config::UdpAuthYml;

pub const MAGIC: &[u8; 4] = b"NXR1";
pub const VERSION: u8 = 1;
pub const HEADER_LEN: usize = 26;
pub const TAG_LEN: usize = 32;
pub const MAX_DATAGRAM_LEN: usize = HEADER_LEN + 56 + TAG_LEN;
const KEY_LEN: usize = 32;
const INDEX_LEN: usize = 56;
const HEARTBEAT_LEN: usize = 32;
type HmacSha256 = Hmac<Sha256>;

#[derive(Debug)]
pub struct UdpAuthSender {
    key_id: u16,
    key: [u8; KEY_LEN],
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
        Ok(Self::new(key_id, key, crate::now_ns() as u64))
    }

    pub fn new(key_id: u16, key: [u8; KEY_LEN], sequence_seed: u64) -> Self {
        Self {
            key_id,
            key,
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
        let sequence = self.sequence.fetch_add(1, Ordering::Relaxed);
        ensure!(sequence != u64::MAX, "UDP auth sequence exhausted");
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
        let mut mac = HmacSha256::new_from_slice(&self.key).expect("fixed HMAC key length");
        mac.update(out);
        out.extend_from_slice(&mac.finalize().into_bytes());
        Ok(sequence)
    }
}

#[derive(Debug)]
struct KeyPolicy {
    key: [u8; KEY_LEN],
    allowed_provider_ids: HashSet<u16>,
}

#[derive(Debug, Default, Clone, Copy)]
struct ReplayState {
    highest: u64,
    bitmap: u64,
    initialized: bool,
}

#[derive(Debug)]
pub struct UdpAuthVerifier {
    keys: HashMap<u16, KeyPolicy>,
    replay: HashMap<u16, ReplayState>,
    max_age_ms: u64,
    max_future_ms: u64,
    replay_window: u8,
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
            (1..=64).contains(&cfg.replay_window),
            "network.udp_auth.replay_window must be in [1, 64]"
        );
        ensure!(!cfg.keys.is_empty(), "network.udp_auth.keys is empty");
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
                key,
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
        })
    }

    /// Authenticate length/version/tag/freshness/replay before returning any
    /// inner MITCH bytes to the caller.
    pub fn verify<'a>(&mut self, datagram: &'a [u8], now_ms: u64) -> Result<VerifiedFrame<'a>> {
        ensure!(
            datagram.len() >= HEADER_LEN + TAG_LEN,
            "authenticated datagram too short"
        );
        ensure!(&datagram[..4] == MAGIC, "bad UDP auth magic");
        ensure!(
            datagram[4] == VERSION,
            "unsupported UDP auth version {}",
            datagram[4]
        );
        ensure!(datagram[5] == 0, "unsupported UDP auth flags");
        let key_id = u16::from_be_bytes(datagram[6..8].try_into().unwrap());
        let sequence = u64::from_be_bytes(datagram[8..16].try_into().unwrap());
        let source_ts_ms = u64::from_be_bytes(datagram[16..24].try_into().unwrap());
        let payload_len = u16::from_be_bytes(datagram[24..26].try_into().unwrap()) as usize;
        ensure!(
            matches!(payload_len, INDEX_LEN | HEARTBEAT_LEN),
            "invalid authenticated payload length {payload_len}"
        );
        ensure!(
            datagram.len() == HEADER_LEN + payload_len + TAG_LEN,
            "authenticated datagram length mismatch"
        );

        let policy = self.keys.get(&key_id).context("unknown UDP auth key_id")?;
        let signed_len = HEADER_LEN + payload_len;
        let mut mac = HmacSha256::new_from_slice(&policy.key).expect("fixed HMAC key length");
        mac.update(&datagram[..signed_len]);
        mac.verify_slice(&datagram[signed_len..])
            .map_err(|_| anyhow::anyhow!("UDP auth tag mismatch"))?;

        ensure!(
            source_ts_ms <= now_ms.saturating_add(self.max_future_ms),
            "authenticated datagram is from the future"
        );
        ensure!(
            now_ms.saturating_sub(source_ts_ms) <= self.max_age_ms,
            "authenticated datagram is stale"
        );
        self.check_and_mark_replay(key_id, sequence)?;
        Ok(VerifiedFrame {
            key_id,
            sequence,
            source_ts_ms,
            payload: &datagram[HEADER_LEN..signed_len],
        })
    }

    #[inline]
    pub fn provider_allowed(&self, key_id: u16, provider_id: u16) -> bool {
        self.keys
            .get(&key_id)
            .is_some_and(|k| k.allowed_provider_ids.contains(&provider_id))
    }

    fn check_and_mark_replay(&mut self, key_id: u16, sequence: u64) -> Result<()> {
        let state = self.replay.entry(key_id).or_default();
        if !state.initialized {
            *state = ReplayState {
                highest: sequence,
                bitmap: 1,
                initialized: true,
            };
            return Ok(());
        }
        if sequence > state.highest {
            let shift = sequence - state.highest;
            state.bitmap = if shift >= 64 {
                1
            } else {
                (state.bitmap << shift) | 1
            };
            state.highest = sequence;
            return Ok(());
        }
        let delta = state.highest - sequence;
        if delta >= self.replay_window as u64 {
            bail!("authenticated datagram is outside replay window");
        }
        let bit = 1u64 << delta;
        ensure!(state.bitmap & bit == 0, "authenticated datagram replayed");
        state.bitmap |= bit;
        Ok(())
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

    fn verifier(key: [u8; 32]) -> UdpAuthVerifier {
        UdpAuthVerifier {
            keys: HashMap::from([(
                7,
                KeyPolicy {
                    key,
                    allowed_provider_ids: HashSet::from([0, 101]),
                },
            )]),
            replay: HashMap::new(),
            max_age_ms: 100,
            max_future_ms: 10,
            replay_window: 64,
        }
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
        let verified = v.verify(&good, 1_001).unwrap();
        assert!(v.provider_allowed(verified.key_id, 101));
        assert!(!v.provider_allowed(verified.key_id, 102));
        assert!(
            v.verify(&good, 1_001)
                .unwrap_err()
                .to_string()
                .contains("replayed")
        );
        let mut tampered = frame(&sender, 1_000);
        tampered[HEADER_LEN + 3] ^= 1;
        assert!(
            v.verify(&tampered, 1_001)
                .unwrap_err()
                .to_string()
                .contains("tag")
        );
        let unknown = UdpAuthSender::new(8, key, 200);
        assert!(
            v.verify(&frame(&unknown, 1_000), 1_001)
                .unwrap_err()
                .to_string()
                .contains("unknown")
        );
        assert!(
            v.verify(&frame(&sender, 800), 1_001)
                .unwrap_err()
                .to_string()
                .contains("stale")
        );
    }

    #[test]
    fn strict_length_rejects_appended_bytes() {
        let key = [3u8; 32];
        let sender = UdpAuthSender::new(7, key, 1);
        let mut bytes = frame(&sender, 10);
        bytes.push(0);
        assert!(verifier(key).verify(&bytes, 10).is_err());
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
            std::hint::black_box(verifier.verify(frame, 50_000 + i as u64).unwrap());
        }
        let verify_ns = start.elapsed().as_nanos() as f64 / N as f64;
        eprintln!(
            "udp auth: seal={seal_ns:.1} ns/frame ({:.0}/s), verify={verify_ns:.1} ns/frame ({:.0}/s)",
            1e9 / seal_ns,
            1e9 / verify_ns,
        );
    }
}
