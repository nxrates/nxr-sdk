//! Broker session attestation (`BROKER_ATTEST`): a collector-signed beat proving a rostered collector held a broker
//! session at `ts_ms`, verifiable end to end without trusting relay hops or the seat proxy.
//! Spec: btr scratch `broker-attest-spec-1120.md` rev 3.1. Shared by nx-rates (collector, relay, seat) and keepers
//! (guard). Everything here is pure: no I/O, no clock reads, no logging. Golden vectors pin the bytes on both sides.

use ed25519_dalek::{Signature, Signer, SigningKey, VerifyingKey};
use serde::Deserialize;
use sha2::{Digest, Sha256};

/// Exact wire length. Bytes 0..16 = MITCH header (routing only, unsigned, ignored here).
pub const FRAME_LEN: usize = 128;
const SIGNED: std::ops::Range<usize> = 16..64;
const SIG_OFF: usize = 64;
pub const VERSION: u8 = 1;
pub const FLAG_LOGGED_ON: u8 = 1;
/// Frame signature domain.
pub const DOMAIN: &[u8] = b"NXR/broker-attest/v1\0";
/// Seat answer signature domain.
pub const SEAT_DOMAIN: &[u8] = b"NXR/broker-attest-seat/v1\0";
/// One-time seat key binding (EIP-191 by the quote signer, verified by the caller with k256).
pub const BIND_DOMAIN: &[u8] = b"NXR/seat-attest-bind/v1\0";
/// A broker session is live while its newest beat is at most this old. The one value; nx-rates core re-exports it.
pub const BROKER_SESSION_TIMEOUT_MS: i64 = 20_000;
/// Future-dated frames and seat `now_ms` tolerate this much clock skew.
pub const MAX_SKEW_MS: i64 = 5_000;
/// Collectors emit at least this often (timeout / 4).
pub const MAX_CADENCE_MS: i64 = BROKER_SESSION_TIMEOUT_MS / 4;

/// The signed fields of one frame.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Beat {
    pub collector_id: u16,
    pub key_id: u16,
    pub provider_id: u16,
    pub logged_on: bool,
    pub session_id: u64,
    pub boot_id: u64,
    pub ts_ms: i64,
    pub seq: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Reject {
    Length,
    Version,
    Reserved,
    UnknownCollector,
    UnknownKey,
    Denied,
    KeyWindow,
    Provider,
    Signature,
    Stale,
    Future,
    Regression,
}

impl Reject {
    /// Metric label.
    pub fn as_str(self) -> &'static str {
        match self {
            Reject::Length => "length",
            Reject::Version => "version",
            Reject::Reserved => "reserved",
            Reject::UnknownCollector => "unknown_collector",
            Reject::UnknownKey => "unknown_key",
            Reject::Denied => "denied",
            Reject::KeyWindow => "key_window",
            Reject::Provider => "provider",
            Reject::Signature => "signature",
            Reject::Stale => "stale",
            Reject::Future => "future",
            Reject::Regression => "regression",
        }
    }
}

fn u16_at(b: &[u8], o: usize) -> u16 {
    u16::from_le_bytes([b[o], b[o + 1]])
}
fn u64_at(b: &[u8], o: usize) -> u64 {
    u64::from_le_bytes(b[o..o + 8].try_into().expect("8 bytes"))
}

/// Parse the signed tail only. Exact length; no allocation; reserved bits and bytes must be zero.
pub fn parse(frame: &[u8]) -> Result<Beat, Reject> {
    if frame.len() != FRAME_LEN {
        return Err(Reject::Length);
    }
    if frame[22] != VERSION {
        return Err(Reject::Version);
    }
    if frame[23] & !FLAG_LOGGED_ON != 0 || frame[56..64].iter().any(|&b| b != 0) {
        return Err(Reject::Reserved);
    }
    Ok(Beat {
        collector_id: u16_at(frame, 16),
        key_id: u16_at(frame, 18),
        provider_id: u16_at(frame, 20),
        logged_on: frame[23] & FLAG_LOGGED_ON != 0,
        session_id: u64_at(frame, 24),
        boot_id: u64_at(frame, 32),
        ts_ms: u64_at(frame, 40) as i64,
        seq: u64_at(frame, 48),
    })
}

fn signed_message(frame: &[u8]) -> Vec<u8> {
    let mut m = Vec::with_capacity(DOMAIN.len() + SIGNED.len());
    m.extend_from_slice(DOMAIN);
    m.extend_from_slice(&frame[SIGNED]);
    m
}

/// Build and sign a frame (collector side). `header` = the 16 B MITCH routing header, copied verbatim.
pub fn sign_frame(key: &SigningKey, header: [u8; 16], b: &Beat) -> [u8; FRAME_LEN] {
    let mut f = [0u8; FRAME_LEN];
    f[..16].copy_from_slice(&header);
    f[16..18].copy_from_slice(&b.collector_id.to_le_bytes());
    f[18..20].copy_from_slice(&b.key_id.to_le_bytes());
    f[20..22].copy_from_slice(&b.provider_id.to_le_bytes());
    f[22] = VERSION;
    f[23] = if b.logged_on { FLAG_LOGGED_ON } else { 0 };
    f[24..32].copy_from_slice(&b.session_id.to_le_bytes());
    f[32..40].copy_from_slice(&b.boot_id.to_le_bytes());
    f[40..48].copy_from_slice(&(b.ts_ms as u64).to_le_bytes());
    f[48..56].copy_from_slice(&b.seq.to_le_bytes());
    let sig = key.sign(&signed_message(&f));
    f[SIG_OFF..].copy_from_slice(&sig.to_bytes());
    f
}

// ---------- roster ----------

#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct KeyEntry {
    pub key_id: u16,
    /// `ed25519:<64 hex>`.
    pub pk: String,
    pub not_before: chrono::DateTime<chrono::Utc>,
    pub not_after: chrono::DateTime<chrono::Utc>,
}

#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct CollectorEntry {
    pub id: u16,
    pub name: String,
    pub providers: Vec<u16>,
    pub keys: Vec<KeyEntry>,
}

#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct SeatEntry {
    pub name: String,
    /// On-chain quote-signer address, `0x…`.
    pub signer: String,
    pub keys: Vec<KeyEntry>,
    /// EIP-191 by `signer` over keccak([`binding_message`]) per key, in `keys` order. Verified by the caller (k256).
    #[serde(default)]
    pub binding_sigs: Vec<String>,
}

#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct DenyEntry {
    #[serde(default)]
    pub collector: Option<u16>,
    #[serde(default)]
    pub seat: Option<String>,
    pub key_id: u16,
}

/// The roster file as written (TOML or JSON; the caller picks the format).
#[derive(Debug, Clone, Deserialize, PartialEq, Eq, Default)]
#[serde(deny_unknown_fields)]
pub struct RosterFile {
    #[serde(default)]
    pub collector: Vec<CollectorEntry>,
    #[serde(default)]
    pub seat: Vec<SeatEntry>,
    #[serde(default)]
    pub deny: Vec<DenyEntry>,
}

#[derive(Debug, Clone)]
pub struct Key {
    pub key_id: u16,
    pub pk: VerifyingKey,
    pub not_before_ms: i64,
    pub not_after_ms: i64,
    pub denied: bool,
}

#[derive(Debug, Clone)]
pub struct Collector {
    pub id: u16,
    pub name: String,
    pub providers: Vec<u16>,
    pub keys: Vec<Key>,
}

#[derive(Debug, Clone)]
pub struct Seat {
    pub name: String,
    pub signer: String,
    pub keys: Vec<Key>,
}

/// A validated roster. Build with [`Roster::load`]; never mutate.
#[derive(Debug, Clone, Default)]
pub struct Roster {
    pub collectors: Vec<Collector>,
    pub seats: Vec<Seat>,
}

fn parse_pk(s: &str) -> Result<VerifyingKey, String> {
    let h = s.strip_prefix("ed25519:").ok_or_else(|| format!("pk {s:?}: missing ed25519: prefix"))?;
    let raw = hex::decode(h).map_err(|e| format!("pk {s:?}: {e}"))?;
    let raw: [u8; 32] = raw.try_into().map_err(|_| format!("pk {s:?}: not 32 bytes"))?;
    let pk = VerifyingKey::from_bytes(&raw).map_err(|e| format!("pk {s:?}: {e}"))?;
    if pk.is_weak() {
        return Err(format!("pk {s:?}: weak (small order)"));
    }
    Ok(pk)
}

fn keys(owner: &str, ks: &[KeyEntry], denied: impl Fn(u16) -> bool) -> Result<Vec<Key>, String> {
    let mut out: Vec<Key> = Vec::with_capacity(ks.len());
    for k in ks {
        if out.iter().any(|o| o.key_id == k.key_id) {
            return Err(format!("{owner}: duplicate key_id {}", k.key_id));
        }
        let (nb, na) = (k.not_before.timestamp_millis(), k.not_after.timestamp_millis());
        if na <= nb {
            return Err(format!("{owner}: key {} not_after <= not_before", k.key_id));
        }
        out.push(Key {
            key_id: k.key_id,
            pk: parse_pk(&k.pk)?,
            not_before_ms: nb,
            not_after_ms: na,
            denied: denied(k.key_id),
        });
    }
    if out.is_empty() {
        return Err(format!("{owner}: no keys"));
    }
    Ok(out)
}

impl Roster {
    /// Validate: unique ids and names, non-weak keys, sane windows, deny entries naming something real.
    pub fn load(f: &RosterFile) -> Result<Roster, String> {
        for d in &f.deny {
            if d.collector.is_some() == d.seat.is_some() {
                return Err(format!("deny entry {d:?}: exactly one of collector/seat"));
            }
        }
        let mut r = Roster::default();
        for c in &f.collector {
            if r.collectors.iter().any(|o| o.id == c.id || o.name == c.name) {
                return Err(format!("collector {} / {}: duplicate id or name", c.id, c.name));
            }
            if c.providers.is_empty() {
                return Err(format!("collector {}: no providers", c.name));
            }
            let mut p = c.providers.clone();
            p.sort_unstable();
            if p.windows(2).any(|w| w[0] == w[1]) {
                return Err(format!("collector {}: duplicate provider", c.name));
            }
            let ks = keys(&c.name, &c.keys, |k| f.deny.iter().any(|d| d.collector == Some(c.id) && d.key_id == k))?;
            r.collectors.push(Collector {
                id: c.id,
                name: c.name.clone(),
                providers: c.providers.clone(),
                keys: ks,
            });
        }
        for s in &f.seat {
            if r.seats.iter().any(|o| o.name == s.name) {
                return Err(format!("seat {}: duplicate", s.name));
            }
            if s.name.len() > 255 {
                return Err(format!("seat {}: name over 255 bytes", s.name));
            }
            let sg = s.signer.strip_prefix("0x").unwrap_or("");
            if sg.len() != 40 || !sg.bytes().all(|b| b.is_ascii_hexdigit()) {
                return Err(format!("seat {}: signer is not a 0x address", s.name));
            }
            // The k256 recover of each binding is the caller's (sdk carries no k256); count and shape are checked here.
            if s.binding_sigs.len() != s.keys.len() {
                return Err(format!("seat {}: one binding_sig per key required", s.name));
            }
            let ks = keys(&s.name, &s.keys, |k| {
                f.deny.iter().any(|d| d.seat.as_deref() == Some(s.name.as_str()) && d.key_id == k)
            })?;
            r.seats.push(Seat {
                name: s.name.clone(),
                signer: s.signer.clone(),
                keys: ks,
            });
        }
        for d in &f.deny {
            let known = match (&d.collector, &d.seat) {
                (Some(c), _) => r.collectors.iter().any(|x| x.id == *c && x.keys.iter().any(|k| k.key_id == d.key_id)),
                (_, Some(s)) => r.seats.iter().any(|x| &x.name == s && x.keys.iter().any(|k| k.key_id == d.key_id)),
                _ => false,
            };
            if !known {
                return Err(format!("deny entry {d:?}: names no rostered key"));
            }
        }
        if r.beats_cap() > MAX_ENTRIES || r.unverified_total_cap() > MAX_ENTRIES {
            return Err(format!(
                "roster too large for one answer: {} beats, {} unverified (max {MAX_ENTRIES})",
                r.beats_cap(),
                r.unverified_total_cap()
            ));
        }
        Ok(r)
    }

    pub fn collector(&self, id: u16) -> Option<&Collector> {
        self.collectors.iter().find(|c| c.id == id)
    }

    pub fn seat(&self, name: &str) -> Option<&Seat> {
        self.seats.iter().find(|s| s.name == name)
    }

    /// `unverified` cap per (collector, provider): its rostered key count + 1. Unknown collectors are never stored.
    pub fn unverified_cap(&self, collector_id: u16) -> usize {
        self.collector(collector_id).map_or(0, |c| c.keys.len() + 1)
    }

    /// Cap on `beats` in one answer: collectors × providers.
    pub fn beats_cap(&self) -> usize {
        self.collectors.iter().map(|c| c.providers.len()).sum()
    }

    /// Cap on `unverified` in one answer: Σ providers × (keys + 1), the sum of the per-slot caps.
    pub fn unverified_total_cap(&self) -> usize {
        self.collectors.iter().map(|c| c.providers.len() * (c.keys.len() + 1)).sum()
    }
}

// ---------- verify ----------

/// Monotonic state per (collector, provider) at a seat, per (seat, collector, provider) at the guard.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Last {
    pub boot_id: u64,
    pub seq: u64,
    pub ts_ms: i64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    /// Seat ingest: an equal (boot_id, seq) is a triple-shot copy ([`Verdict::Dup`]).
    Ingest,
    /// Guard: an equal (boot_id, seq) is the same newest frame re-read between beats, still valid.
    Guard,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Verdict {
    /// Valid and newest. `logged_on = false` is stored too and makes the seat not able (spec N8).
    Valid(Beat),
    /// Ingest only: an exact copy of the stored frame; drop silently, state unchanged.
    Dup,
    Rejected(Reject),
}

/// Verify one frame. Pure: `now_ms` and `last` come in, the next state comes out (unchanged on reject or dup).
/// Clock steps (spec F2): an accepted ts is never more than [`MAX_SKEW_MS`] ahead of the verifier, so a stepped-back
/// collector is a regression for at most that long. A stale `last` needs no expiry: every accepted frame is newer.
pub fn verify_attest(frame: &[u8], roster: &Roster, now_ms: i64, last: Option<Last>, mode: Mode) -> (Verdict, Option<Last>) {
    let rej = |r| (Verdict::Rejected(r), last);
    let b = match parse(frame) {
        Ok(b) => b,
        Err(r) => return rej(r),
    };
    let Some(c) = roster.collector(b.collector_id) else {
        return rej(Reject::UnknownCollector);
    };
    let Some(k) = c.keys.iter().find(|k| k.key_id == b.key_id) else {
        return rej(Reject::UnknownKey);
    };
    if k.denied {
        return rej(Reject::Denied);
    }
    if b.ts_ms < k.not_before_ms || b.ts_ms >= k.not_after_ms {
        return rej(Reject::KeyWindow);
    }
    if !c.providers.contains(&b.provider_id) {
        return rej(Reject::Provider);
    }
    let sig = Signature::from_bytes(frame[SIG_OFF..].try_into().expect("64 bytes"));
    if k.pk.verify_strict(&signed_message(frame), &sig).is_err() {
        return rej(Reject::Signature);
    }
    let floor = now_ms.saturating_sub(BROKER_SESSION_TIMEOUT_MS);
    if b.ts_ms < floor {
        return rej(Reject::Stale);
    }
    if b.ts_ms > now_ms.saturating_add(MAX_SKEW_MS) {
        return rej(Reject::Future);
    }
    let next = Last {
        boot_id: b.boot_id,
        seq: b.seq,
        ts_ms: b.ts_ms,
    };
    // A `last` ahead of the verifier's own clock (its clock stepped back) would block every fresh frame until the clock
    // catches up: treat it as absent.
    let last = last.filter(|l| l.ts_ms <= now_ms.saturating_add(MAX_SKEW_MS));
    match last {
        Some(l) if l.boot_id == b.boot_id && l.seq == b.seq => match mode {
            Mode::Ingest => (Verdict::Dup, Some(l)),
            Mode::Guard => (Verdict::Valid(b), Some(l)),
        },
        Some(l) if l.boot_id == b.boot_id && (b.seq < l.seq || b.ts_ms < l.ts_ms) => (Verdict::Rejected(Reject::Regression), Some(l)),
        Some(l) if l.boot_id != b.boot_id && b.ts_ms <= l.ts_ms => (Verdict::Rejected(Reject::Regression), Some(l)),
        _ => (Verdict::Valid(b), Some(next)),
    }
}

// ---------- seat answer ----------

/// One served beat: the verbatim frame, the seat's path (metric only) and its arrival time.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Served {
    pub frame: [u8; FRAME_LEN],
    /// 0 = direct, 1 = relayed. Unverifiable: never part of a verdict.
    pub path: u8,
    pub rx_ms: i64,
}

fn sort_key(f: &[u8; FRAME_LEN]) -> (u16, u16, u16) {
    (u16_at(f, 16), u16_at(f, 20), u16_at(f, 18))
}

/// The bytes the seat attest key signs: domain ‖ u8 len(seat) ‖ seat ‖ u16 key_id ‖ i64 now_ms ‖ sha256(B) ‖ sha256(U).
/// B = beats sorted by (collector, provider, key_id, frame bytes): frame ‖ u8 path ‖ i64 rx_ms. U = unverified sorted by
/// (collector, provider, key_id, rx_ms, frame bytes): frame ‖ i64 rx_ms. Errors on a seat name over 255 bytes.
pub fn seat_message(seat: &str, key_id: u16, now_ms: i64, beats: &[Served], unverified: &[Served]) -> Result<Vec<u8>, String> {
    let n = u8::try_from(seat.len()).map_err(|_| "seat name over 255 bytes".to_string())?;
    let mut b: Vec<&Served> = beats.iter().collect();
    b.sort_by_key(|x| (sort_key(&x.frame), x.frame));
    let mut u: Vec<&Served> = unverified.iter().collect();
    u.sort_by_key(|x| (sort_key(&x.frame), x.rx_ms, x.frame));
    let mut hb = Sha256::new();
    for s in b {
        hb.update(s.frame);
        hb.update([s.path]);
        hb.update(s.rx_ms.to_le_bytes());
    }
    let mut hu = Sha256::new();
    for s in u {
        hu.update(s.frame);
        hu.update(s.rx_ms.to_le_bytes());
    }
    let mut m = Vec::with_capacity(SEAT_DOMAIN.len() + 1 + seat.len() + 2 + 8 + 64);
    m.extend_from_slice(SEAT_DOMAIN);
    m.push(n);
    m.extend_from_slice(seat.as_bytes());
    m.extend_from_slice(&key_id.to_le_bytes());
    m.extend_from_slice(&now_ms.to_le_bytes());
    m.extend_from_slice(&hb.finalize());
    m.extend_from_slice(&hu.finalize());
    Ok(m)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SeatReject {
    WrongSeat,
    UnknownKey,
    Denied,
    KeyWindow,
    Skew,
    Caps,
    Duplicate,
    Signature,
}

/// Guard-side check of one seat answer. `expected_seat` = the name this URL maps to (never the answer's own claim).
/// Checks caps and duplicates, key binding to THAT seat, skew against the guard clock, and the signature.
/// Frame validity is judged separately, per frame, with [`verify_attest`] in [`Mode::Guard`].
#[allow(clippy::too_many_arguments)]
pub fn verify_seat_answer(
    roster: &Roster,
    expected_seat: &str,
    seat: &str,
    key_id: u16,
    now_ms: i64,
    beats: &[Served],
    unverified: &[Served],
    sig: &[u8; 64],
    guard_now_ms: i64,
) -> Result<(), SeatReject> {
    if seat != expected_seat {
        return Err(SeatReject::WrongSeat);
    }
    let s = roster.seat(expected_seat).ok_or(SeatReject::UnknownKey)?;
    let k = s.keys.iter().find(|k| k.key_id == key_id).ok_or(SeatReject::UnknownKey)?;
    if k.denied {
        return Err(SeatReject::Denied);
    }
    if now_ms < k.not_before_ms || now_ms >= k.not_after_ms {
        return Err(SeatReject::KeyWindow);
    }
    if (now_ms - guard_now_ms).abs() > MAX_SKEW_MS {
        return Err(SeatReject::Skew);
    }
    // Absolute bounds only: a guard whose roster lags the seat's must not reject an honest answer (entries it does not
    // know are ignored per frame by verify_attest). Seats keep the roster-derived caps on their side.
    if beats.len() > MAX_ENTRIES || unverified.len() > MAX_ENTRIES {
        return Err(SeatReject::Caps);
    }
    let mut seen: Vec<(u16, u16)> = beats.iter().map(|s| (u16_at(&s.frame, 16), u16_at(&s.frame, 20))).collect();
    seen.sort_unstable();
    if seen.windows(2).any(|w| w[0] == w[1]) {
        return Err(SeatReject::Duplicate);
    }
    let mut seen: Vec<(u16, u16, u16)> = unverified.iter().map(|s| sort_key(&s.frame)).collect();
    seen.sort_unstable();
    if seen.windows(2).any(|w| w[0] == w[1]) {
        return Err(SeatReject::Duplicate);
    }
    let msg = seat_message(seat, key_id, now_ms, beats, unverified).map_err(|_| SeatReject::Caps)?;
    k.pk.verify_strict(&msg, &Signature::from_bytes(sig)).map_err(|_| SeatReject::Signature)
}

/// Sign a seat answer (seat side, once per second, served from cache).
pub fn sign_seat_answer(key: &SigningKey, seat: &str, key_id: u16, now_ms: i64, beats: &[Served], unverified: &[Served]) -> Result<[u8; 64], String> {
    Ok(key.sign(&seat_message(seat, key_id, now_ms, beats, unverified)?).to_bytes())
}

/// Preimage of the one-time seat key binding: domain ‖ u8 len(name) ‖ name ‖ u16 key_id ‖ 32 B pk ‖ i64 not_after_ms.
/// The quote signer signs keccak256(this) under EIP-191; verification (k256 recover) is the caller's.
pub fn binding_message(seat: &str, key_id: u16, pk: &VerifyingKey, not_after_ms: i64) -> Result<Vec<u8>, String> {
    let n = u8::try_from(seat.len()).map_err(|_| "seat name over 255 bytes".to_string())?;
    let mut m = Vec::with_capacity(BIND_DOMAIN.len() + 1 + seat.len() + 2 + 32 + 8);
    m.extend_from_slice(BIND_DOMAIN);
    m.push(n);
    m.extend_from_slice(seat.as_bytes());
    m.extend_from_slice(&key_id.to_le_bytes());
    m.extend_from_slice(pk.as_bytes());
    m.extend_from_slice(&not_after_ms.to_le_bytes());
    Ok(m)
}

/// Seat-side `unverified` insert for one (collector, provider) slot. Stored only for a ROSTERED collector and one of its
/// providers (spec R1, H2), so the answer always fits [`Roster::unverified_total_cap`]. One entry per key_id (newest by
/// rx_ms), at most [`Roster::unverified_cap`]; entries older than the timeout (against `item.rx_ms`) go first, and when
/// still full the NEWCOMER is dropped, so a flood of made-up key ids never displaces an established frame (m1). Never
/// touches the verdict slot or monotonic state.
// ponytail: a forger reusing the real new key_id with newer rx_ms can replace that entry (the seat cannot verify an
// unknown key); bounded to the rotation window where a seat's roster lags, which rotation step 2 makes ~0. Upgrade path:
// seats roll the roster before any key goes live, then drop the `unverified` path.
pub fn push_unverified(roster: &Roster, slot: &mut Vec<Served>, item: Served) {
    let (c, p, kid) = sort_key(&item.frame);
    if !roster.collector(c).is_some_and(|x| x.providers.contains(&p)) {
        return;
    }
    if let Some(i) = slot.iter().position(|s| u16_at(&s.frame, 18) == kid) {
        if slot[i].rx_ms <= item.rx_ms {
            slot[i] = item;
        }
        return;
    }
    slot.retain(|s| s.rx_ms >= item.rx_ms.saturating_sub(BROKER_SESSION_TIMEOUT_MS));
    if slot.len() < roster.unverified_cap(c) {
        slot.push(item);
    }
}

/// Exact-shape decode of one served frame from the JSON answer: exactly 256 hex chars.
pub fn frame_from_hex(h: &str) -> Option<[u8; FRAME_LEN]> {
    if h.len() != FRAME_LEN * 2 {
        return None;
    }
    hex::decode(h).ok()?.try_into().ok()
}

/// Absolute cap on `beats` and on `unverified` entries in one answer, whatever either side's roster says.
pub const MAX_ENTRIES: usize = 512;
/// Guards refuse a `/v1/broker/attest` body above this before parsing: 2 × [`MAX_ENTRIES`] entries of ≤ 320 B JSON each.
pub const MAX_ANSWER_BYTES: usize = 2 * MAX_ENTRIES * 320 + 4096;

#[cfg(test)]
mod tests {
    use super::*;

    const T0: i64 = 1_763_600_000_000;
    const DAY: i64 = 86_400_000;

    fn key(seed: u8) -> SigningKey {
        SigningKey::from_bytes(&[seed; 32])
    }

    fn pk_str(k: &SigningKey) -> String {
        format!("ed25519:{}", hex::encode(k.verifying_key().as_bytes()))
    }

    fn win(id: u16, seed: u8) -> KeyEntry {
        KeyEntry {
            key_id: id,
            pk: pk_str(&key(seed)),
            not_before: chrono::DateTime::from_timestamp_millis(T0 - DAY).unwrap(),
            not_after: chrono::DateTime::from_timestamp_millis(T0 + DAY).unwrap(),
        }
    }

    /// Collector 3 (key 1 = seed 7) on providers 41 + 42; seats nx-a and nx-c (key 2 = seed 8).
    fn roster_file() -> RosterFile {
        let seat = |n: &str| SeatEntry {
            name: n.into(),
            signer: format!("0x{}", "ab".repeat(20)),
            keys: vec![win(2, 8)],
            binding_sigs: vec!["0x00".into()],
        };
        RosterFile {
            collector: vec![CollectorEntry {
                id: 3,
                name: "nx-a-pepperstone".into(),
                providers: vec![41, 42],
                keys: vec![win(1, 7)],
            }],
            seat: vec![seat("nx-a"), seat("nx-c")],
            deny: vec![],
        }
    }

    fn roster() -> Roster {
        Roster::load(&roster_file()).unwrap()
    }

    fn beat() -> Beat {
        Beat {
            collector_id: 3,
            key_id: 1,
            provider_id: 41,
            logged_on: true,
            session_id: 0x1111,
            boot_id: 0x2222,
            ts_ms: T0,
            seq: 7,
        }
    }

    fn frame(b: &Beat) -> [u8; FRAME_LEN] {
        sign_frame(&key(7), [0xAA; 16], b)
    }

    fn served(b: &Beat, path: u8, rx: i64) -> Served {
        Served {
            frame: frame(b),
            path,
            rx_ms: rx,
        }
    }

    fn v(f: &[u8], now: i64, last: Option<Last>, m: Mode) -> (Verdict, Option<Last>) {
        verify_attest(f, &roster(), now, last, m)
    }

    // ---- golden vectors: keepers pins the same constants ----

    const GOLDEN_PK: &str = "ea4a6c63e29c520abef5507b132ec5f9954776aebebe7b92421eea691446d22c";
    const GOLDEN_FRAME: &str = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa0300010029000101111100000000000022220000000000000064c09e9a01000007000000000000000000000000000000a164abb2acaf49a432cb2f0feaf92d97cb3b5a9f386f8f3b6f53909be6a9c71aa1ee9a7e0d3040cd1c5b47ea95e592a92e686ddd8c81fb65f4f2a839dde0030c";
    const GOLDEN_FRAME_EDGE: &str = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa030001002a000100ffffffffffffffff01000000000000800164c09e9a01000000000000000000800000000000000000ea32d807cd981daf1dc9818c904a9000d68e2784333ce637a100d5d075912bc2141a8479da7aab250f779d520d87fa6a596b6f37b78e4e06b4c3821cd414920e";
    const GOLDEN_SEAT_PK: &str = "1398f62c6d1a457c51ba6a4b5f3dbd2f69fca93216218dc8997e416bd17d93ca";
    const GOLDEN_SEAT_SIG: &str =
        "26f0d9783002a8cac4b52fce9977e0ad4c3468ad1fb879ff3ba1947d9002288f261946f6b7b1446aadcf29aae1fa01bdfb051af964506f76fdbbc35fc2ad1e07";
    const GOLDEN_BIND_PREIMAGE: &str = "4e58522f736561742d6174746573742d62696e642f763100046e782d6102001398f62c6d1a457c51ba6a4b5f3dbd2f69fca93216218dc8997e416bd17d93ca0064c09e9a010000";

    /// logged_off, ts above 2^31 ms, seq with the high bit set, provider 42.
    fn edge_beat() -> Beat {
        Beat {
            logged_on: false,
            provider_id: 42,
            ts_ms: T0 + 1,
            seq: 1 << 63,
            session_id: u64::MAX,
            boot_id: 0x8000_0000_0000_0001,
            ..beat()
        }
    }

    #[test]
    fn golden_frames_sign_and_decode() {
        assert_eq!(hex::encode(key(7).verifying_key().as_bytes()), GOLDEN_PK);
        assert_eq!(hex::encode(frame(&beat())), GOLDEN_FRAME, "frame bytes changed: re-pin on BOTH sides");
        assert_eq!(hex::encode(frame(&edge_beat())), GOLDEN_FRAME_EDGE);
        // Decode side: the pinned bytes verify to the exact fields.
        let f = frame_from_hex(GOLDEN_FRAME).unwrap();
        assert_eq!(v(&f, T0, None, Mode::Guard).0, Verdict::Valid(beat()));
        let e = frame_from_hex(GOLDEN_FRAME_EDGE).unwrap();
        assert_eq!(v(&e, T0 + 1, None, Mode::Guard).0, Verdict::Valid(edge_beat()));
    }

    /// Several beats and unverified entries, shuffled, mixed paths: pins the sort order and both hash framings.
    fn golden_answer() -> (Vec<Served>, Vec<Served>) {
        let b2 = served(
            &Beat {
                provider_id: 42,
                seq: 9,
                ..beat()
            },
            0,
            T0 + 2,
        );
        let b1 = served(&beat(), 1, T0 + 3);
        let u_a = Served {
            frame: sign_frame(&key(9), [0; 16], &Beat { key_id: 2, ..beat() }),
            path: 1,
            rx_ms: T0 + 5,
        };
        let u_b = Served {
            frame: sign_frame(
                &key(9),
                [0; 16],
                &Beat {
                    key_id: 2,
                    provider_id: 42,
                    ..beat()
                },
            ),
            path: 0,
            rx_ms: T0 + 4,
        };
        (vec![b2, b1], vec![u_a, u_b])
    }

    #[test]
    fn golden_seat_answer_and_binding() {
        assert_eq!(hex::encode(key(8).verifying_key().as_bytes()), GOLDEN_SEAT_PK);
        let (b, u) = golden_answer();
        let sig = sign_seat_answer(&key(8), "nx-a", 2, T0 + 10, &b, &u).unwrap();
        assert_eq!(hex::encode(sig), GOLDEN_SEAT_SIG);
        let (mut rb, mut ru) = (b.clone(), u.clone());
        rb.reverse();
        ru.reverse();
        assert_eq!(sign_seat_answer(&key(8), "nx-a", 2, T0 + 10, &rb, &ru).unwrap(), sig, "order-independent");
        assert_eq!(verify_seat_answer(&roster(), "nx-a", "nx-a", 2, T0 + 10, &rb, &ru, &sig, T0 + 10), Ok(()));
        let bind = binding_message("nx-a", 2, &key(8).verifying_key(), T0).unwrap();
        assert_eq!(bind.len(), BIND_DOMAIN.len() + 1 + 4 + 2 + 32 + 8);
        assert_eq!(hex::encode(&bind), GOLDEN_BIND_PREIMAGE);
    }

    // ---- frame rules ----

    #[test]
    fn valid_frame_verifies_and_header_is_ignored() {
        let mut f = frame(&beat());
        f[..16].copy_from_slice(&[0x55; 16]);
        assert_eq!(v(&f, T0, None, Mode::Guard).0, Verdict::Valid(beat()));
    }

    #[test]
    fn every_static_rule_rejects() {
        let r = |b: Beat| v(&frame(&b), T0, None, Mode::Guard).0;
        assert_eq!(r(Beat { collector_id: 9, ..beat() }), Verdict::Rejected(Reject::UnknownCollector));
        assert_eq!(r(Beat { key_id: 9, ..beat() }), Verdict::Rejected(Reject::UnknownKey));
        assert_eq!(r(Beat { provider_id: 43, ..beat() }), Verdict::Rejected(Reject::Provider));
        for (ts, ok) in [(T0 + DAY, false), (T0 + DAY - 1, true), (T0 - DAY, true), (T0 - DAY - 1, false)] {
            let got = v(&frame(&Beat { ts_ms: ts, ..beat() }), ts, None, Mode::Guard).0;
            assert_eq!(matches!(got, Verdict::Valid(_)), ok, "key window at {ts}: {got:?}");
            if !ok {
                assert_eq!(got, Verdict::Rejected(Reject::KeyWindow));
            }
        }
        assert_eq!(v(&frame(&beat()), T0 + 20_001, None, Mode::Guard).0, Verdict::Rejected(Reject::Stale));
        assert!(matches!(v(&frame(&beat()), T0 + 20_000, None, Mode::Guard).0, Verdict::Valid(_)));
        assert_eq!(v(&frame(&beat()), T0 - 5_001, None, Mode::Guard).0, Verdict::Rejected(Reject::Future));
        assert!(matches!(v(&frame(&beat()), T0 - 5_000, None, Mode::Guard).0, Verdict::Valid(_)));
        assert_eq!(
            v(&frame(&beat()), i64::MIN, None, Mode::Guard).0,
            Verdict::Rejected(Reject::Future),
            "no overflow"
        );
        assert_eq!(
            v(&frame(&beat()), i64::MAX, None, Mode::Guard).0,
            Verdict::Rejected(Reject::Stale),
            "no overflow"
        );
        let mut f = frame(&beat());
        f[22] = 2;
        assert_eq!(v(&f, T0, None, Mode::Guard).0, Verdict::Rejected(Reject::Version));
        let mut f = frame(&beat());
        f[23] |= 2;
        assert_eq!(v(&f, T0, None, Mode::Guard).0, Verdict::Rejected(Reject::Reserved));
        let mut f = frame(&beat());
        f[60] = 1;
        assert_eq!(v(&f, T0, None, Mode::Guard).0, Verdict::Rejected(Reject::Reserved));
        let mut f = frame(&beat());
        f[30] ^= 1;
        assert_eq!(v(&f, T0, None, Mode::Guard).0, Verdict::Rejected(Reject::Signature));
        assert_eq!(v(&f[..127], T0, None, Mode::Guard).0, Verdict::Rejected(Reject::Length));
        let mut long = frame(&beat()).to_vec();
        long.push(0);
        assert_eq!(v(&long, T0, None, Mode::Guard).0, Verdict::Rejected(Reject::Length));
        let h = hex::encode(frame(&beat()));
        assert!(frame_from_hex(&h[..254]).is_none() && frame_from_hex(&format!("{h}00")).is_none() && frame_from_hex(&h[..255]).is_none());
    }

    #[test]
    fn non_canonical_s_is_rejected() {
        const L: [u8; 32] = [
            0xed, 0xd3, 0xf5, 0x5c, 0x1a, 0x63, 0x12, 0x58, 0xd6, 0x9c, 0xf7, 0xa2, 0xde, 0xf9, 0xde, 0x14, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0,
            0, 0x10,
        ];
        let mut f = frame(&beat());
        let mut carry = 0u16;
        for i in 0..32 {
            let s = u16::from(f[96 + i]) + u16::from(L[i]) + carry;
            f[96 + i] = s as u8;
            carry = s >> 8;
        }
        assert_eq!(v(&f, T0, None, Mode::Guard).0, Verdict::Rejected(Reject::Signature));
    }

    #[test]
    fn denied_key_rejected() {
        let mut rf = roster_file();
        rf.deny.push(DenyEntry {
            collector: Some(3),
            seat: None,
            key_id: 1,
        });
        let r = Roster::load(&rf).unwrap();
        assert_eq!(
            verify_attest(&frame(&beat()), &r, T0, None, Mode::Guard).0,
            Verdict::Rejected(Reject::Denied)
        );
    }

    #[test]
    fn roster_load_refuses_bad_input() {
        let bad = |f: &dyn Fn(&mut RosterFile), needle: &str| {
            let mut rf = roster_file();
            f(&mut rf);
            let e = Roster::load(&rf).unwrap_err();
            assert!(e.contains(needle), "{needle}: {e}");
        };
        let mut id = [0u8; 32];
        id[0] = 1;
        bad(&|rf| rf.collector[0].keys[0].pk = format!("ed25519:{}", hex::encode(id)), "weak");
        bad(&|rf| rf.collector[0].keys[0].pk = hex::encode([7u8; 32]), "prefix");
        bad(&|rf| rf.collector[0].keys[0].pk = "ed25519:abcd".into(), "32 bytes");
        bad(&|rf| rf.collector[0].keys[0].pk = "ed25519:PENDING".into(), "pk");
        bad(
            &|rf| {
                let d = rf.collector[0].keys[0].clone();
                rf.collector[0].keys.push(d)
            },
            "duplicate key_id",
        );
        bad(&|rf| rf.collector[0].keys[0].not_after = rf.collector[0].keys[0].not_before, "not_after");
        bad(&|rf| rf.collector[0].keys.clear(), "no keys");
        bad(&|rf| rf.collector[0].providers.clear(), "no providers");
        bad(&|rf| rf.collector[0].providers.push(41), "duplicate provider");
        bad(
            &|rf| {
                let d = rf.collector[0].clone();
                rf.collector.push(d)
            },
            "duplicate id",
        );
        bad(
            &|rf| {
                let d = rf.seat[0].clone();
                rf.seat.push(d)
            },
            "duplicate",
        );
        bad(&|rf| rf.seat[0].signer = "0xPENDING".into(), "0x address");
        bad(&|rf| rf.seat[0].binding_sigs.clear(), "binding_sig");
        bad(&|rf| rf.seat[0].name = "x".repeat(256), "255");
        bad(
            &|rf| {
                rf.deny.push(DenyEntry {
                    collector: Some(3),
                    seat: Some("nx-a".into()),
                    key_id: 1,
                })
            },
            "exactly one",
        );
        bad(
            &|rf| {
                rf.deny.push(DenyEntry {
                    collector: Some(3),
                    seat: None,
                    key_id: 5,
                })
            },
            "names no rostered key",
        );
    }

    #[test]
    fn roster_file_parses_toml_and_refuses_unknown_fields() {
        let ok = format!(
            "deny = []\n[[collector]]\nid = 3\nname = \"c\"\nproviders = [41]\nkeys = [{{ key_id = 1, pk = \"{}\", not_before = \"2026-11-21T00:00:00Z\", not_after = \"2027-05-21T00:00:00Z\" }}]\n",
            pk_str(&key(7))
        );
        let rf: RosterFile = toml::from_str(&ok).unwrap();
        Roster::load(&rf).unwrap();
        assert!(toml::from_str::<RosterFile>(&ok.replace("providers", "provider")).is_err());
        assert!(toml::from_str::<RosterFile>(&format!("{ok}extra = 1\n")).is_err());
        // Inside nx-rates, the shipped roster must parse and load.
        let shipped = std::fs::read_to_string(concat!(env!("CARGO_MANIFEST_DIR"), "/../../deploy/broker-attest-roster.toml"));
        if let Ok(t) = shipped {
            let rf: RosterFile = toml::from_str(&t).unwrap();
            Roster::load(&rf).expect("deploy/broker-attest-roster.toml must load");
        }
    }

    // ---- monotonic state ----

    #[test]
    fn monotonic_ingest_dedupes_guard_accepts_equal() {
        let f = frame(&beat());
        let (_, l) = v(&f, T0, None, Mode::Ingest);
        assert_eq!(v(&f, T0 + 1, l, Mode::Ingest).0, Verdict::Dup);
        assert!(matches!(v(&f, T0 + 1, l, Mode::Guard).0, Verdict::Valid(_)));
        let older = frame(&Beat { seq: 6, ..beat() });
        let (vd, l2) = v(&older, T0 + 1, l, Mode::Guard);
        assert_eq!(vd, Verdict::Rejected(Reject::Regression));
        assert_eq!(l2, l, "guard keeps its newest, never swaps back");
        let lower_ts = frame(&Beat {
            seq: 8,
            ts_ms: T0 - 1,
            ..beat()
        });
        assert_eq!(
            v(&lower_ts, T0, l, Mode::Guard).0,
            Verdict::Rejected(Reject::Regression),
            "same boot: ts never goes back"
        );
        let newer = frame(&Beat {
            seq: 8,
            ts_ms: T0 + 5_000,
            ..beat()
        });
        assert!(matches!(v(&newer, T0 + 5_000, l, Mode::Ingest).0, Verdict::Valid(_)));
    }

    #[test]
    fn new_boot_needs_a_later_ts_in_both_modes() {
        for m in [Mode::Ingest, Mode::Guard] {
            let (_, l) = v(&frame(&beat()), T0, None, m);
            let early = frame(&Beat {
                boot_id: 9,
                seq: 1,
                ts_ms: T0 - 1,
                ..beat()
            });
            assert_eq!(v(&early, T0, l, m).0, Verdict::Rejected(Reject::Regression));
            let later = frame(&Beat {
                boot_id: 9,
                seq: 1,
                ts_ms: T0 + 1,
                ..beat()
            });
            assert!(matches!(v(&later, T0 + 1, l, m).0, Verdict::Valid(_)));
        }
    }

    #[test]
    fn a_clock_step_blocks_at_most_the_skew_bound() {
        // A collector clock stepped ahead then back: the newest accepted ts is at most MAX_SKEW_MS ahead of the verifier
        // (the Future rule), so an honest new-boot frame is a regression only until the wall clock passes it.
        let stored = T0 + MAX_SKEW_MS;
        let (vd, ahead) = v(&frame(&Beat { ts_ms: stored, ..beat() }), T0, None, Mode::Ingest);
        assert!(matches!(vd, Verdict::Valid(_)));
        let back = |ts| {
            frame(&Beat {
                boot_id: 9,
                seq: 1,
                ts_ms: ts,
                ..beat()
            })
        };
        assert_eq!(v(&back(stored), stored, ahead, Mode::Ingest).0, Verdict::Rejected(Reject::Regression));
        assert!(matches!(v(&back(stored + 1), stored + 1, ahead, Mode::Ingest).0, Verdict::Valid(_)));
    }

    #[test]
    fn logged_off_frame_is_valid_and_newest() {
        let (_, l) = v(&frame(&beat()), T0, None, Mode::Guard);
        let off = frame(&Beat {
            logged_on: false,
            seq: 8,
            ts_ms: T0 + 1_000,
            ..beat()
        });
        match v(&off, T0 + 1_000, l, Mode::Guard) {
            (Verdict::Valid(b), Some(n)) => assert!(!b.logged_on && n.seq == 8),
            x => panic!("{x:?}"),
        }
    }

    // ---- seat answer ----

    #[test]
    fn seat_answer_binds_to_the_expected_seat() {
        let r = roster();
        let s = served(&beat(), 0, T0);
        let sig = sign_seat_answer(&key(8), "nx-a", 2, T0, std::slice::from_ref(&s), &[]).unwrap();
        let chk = |exp: &str, now: i64| verify_seat_answer(&r, exp, "nx-a", 2, T0, std::slice::from_ref(&s), &[], &sig, now);
        assert_eq!(chk("nx-a", T0), Ok(()));
        assert_eq!(chk("nx-c", T0), Err(SeatReject::WrongSeat), "nx-a's answer served at nx-c's URL");
        assert_eq!(chk("nx-a", T0 + 5_001), Err(SeatReject::Skew));
        assert_eq!(
            verify_seat_answer(&r, "nx-b", "nx-b", 2, T0, &[], &[], &sig, T0),
            Err(SeatReject::UnknownKey)
        );
        assert_eq!(
            verify_seat_answer(&r, "nx-a", "nx-a", 9, T0, &[], &[], &sig, T0),
            Err(SeatReject::UnknownKey)
        );
        assert_eq!(
            verify_seat_answer(&r, "nx-a", "nx-a", 2, T0 + DAY, &[], &[], &sig, T0 + DAY),
            Err(SeatReject::KeyWindow)
        );
        let t = Served { rx_ms: T0 + 1, ..s.clone() };
        assert_eq!(
            verify_seat_answer(&r, "nx-a", "nx-a", 2, T0, &[t], &[], &sig, T0),
            Err(SeatReject::Signature)
        );
        let u = Served {
            frame: sign_frame(&key(9), [0; 16], &Beat { key_id: 2, ..beat() }),
            path: 1,
            rx_ms: T0,
        };
        assert_eq!(
            verify_seat_answer(&r, "nx-a", "nx-a", 2, T0, std::slice::from_ref(&s), std::slice::from_ref(&u), &sig, T0),
            Err(SeatReject::Signature),
            "unverified is signed"
        );
    }

    #[test]
    fn seat_answer_caps_and_duplicates() {
        let r = roster(); // beats_cap 2, unverified_total_cap 4 (2 per provider)
        let chk = |b: &[Served], u: &[Served]| verify_seat_answer(&r, "nx-a", "nx-a", 2, T0, b, u, &[0; 64], T0);
        let a = served(&beat(), 0, T0);
        assert_eq!(chk(&[a.clone(), a.clone()], &[]), Err(SeatReject::Duplicate));
        assert_eq!(chk(&vec![a.clone(); MAX_ENTRIES + 1], &[]), Err(SeatReject::Caps), "absolute bound");
        let unk = |kid: u16, p: u16| Served {
            frame: sign_frame(
                &key(9),
                [0; 16],
                &Beat {
                    key_id: kid,
                    provider_id: p,
                    ..beat()
                },
            ),
            path: 1,
            rx_ms: T0,
        };
        assert_eq!(chk(&[], &[unk(2, 41), unk(2, 41)]), Err(SeatReject::Duplicate));
        // A guard whose roster lags the seat's (unknown collector, extra entries) still verifies the answer: only the
        // zero signature fails here, never Caps. Unknown frames are then ignored per frame by verify_attest.
        let stranger = Served {
            frame: sign_frame(&key(9), [0; 16], &Beat { collector_id: 77, ..beat() }),
            path: 1,
            rx_ms: T0,
        };
        let c = served(&Beat { provider_id: 42, ..beat() }, 0, T0);
        let lagging = served(&Beat { collector_id: 77, ..beat() }, 0, T0);
        assert_eq!(
            chk(&[a.clone(), c, lagging], &[stranger, unk(2, 41), unk(3, 41), unk(4, 41)]),
            Err(SeatReject::Signature)
        );
    }

    #[test]
    fn a_lagging_guard_accepts_an_honest_signed_answer() {
        let b = [served(&beat(), 0, T0), served(&Beat { collector_id: 77, ..beat() }, 1, T0)];
        let sig = sign_seat_answer(&key(8), "nx-a", 2, T0, &b, &[]).unwrap();
        assert_eq!(verify_seat_answer(&roster(), "nx-a", "nx-a", 2, T0, &b, &[], &sig, T0), Ok(()));
        assert_eq!(
            verify_attest(&b[1].frame, &roster(), T0, None, Mode::Guard).0,
            Verdict::Rejected(Reject::UnknownCollector)
        );
    }

    #[test]
    fn verifier_clock_stepped_back_does_not_block() {
        // `last` was stored when the verifier's clock read T0 + 1 h; it now reads T0. A fresh frame is accepted.
        let ahead = Some(Last {
            boot_id: 0x2222,
            seq: 9,
            ts_ms: T0 + 3_600_000,
        });
        assert!(matches!(v(&frame(&beat()), T0, ahead, Mode::Guard).0, Verdict::Valid(_)));
    }

    #[test]
    fn roster_refuses_more_than_one_answer_holds() {
        let mut rf = roster_file();
        rf.collector[0].providers = (1..=300).collect();
        assert!(Roster::load(&rf).unwrap_err().contains("too large"));
    }

    #[test]
    fn denied_seat_key_rejected() {
        let mut rf = roster_file();
        rf.deny.push(DenyEntry {
            collector: None,
            seat: Some("nx-a".into()),
            key_id: 2,
        });
        let r = Roster::load(&rf).unwrap();
        let sig = sign_seat_answer(&key(8), "nx-a", 2, T0, &[], &[]).unwrap();
        assert_eq!(verify_seat_answer(&r, "nx-a", "nx-a", 2, T0, &[], &[], &sig, T0), Err(SeatReject::Denied));
    }

    #[test]
    fn unverified_slot_resists_floods_and_strangers() {
        let r = roster(); // cap 2 per slot
        let real = Served {
            frame: sign_frame(&key(9), [0; 16], &Beat { key_id: 2, ..beat() }),
            path: 1,
            rx_ms: T0,
        };
        let mut slot = vec![];
        push_unverified(&r, &mut slot, real.clone());
        for (i, kid) in (100u16..110).enumerate() {
            let forged = Served {
                frame: sign_frame(&key(1), [0; 16], &Beat { key_id: kid, ..beat() }),
                path: 1,
                rx_ms: T0 + 1 + i as i64,
            };
            push_unverified(&r, &mut slot, forged);
        }
        assert!(slot.contains(&real), "newer-stamped forgeries never displace an established frame");
        assert_eq!(slot.len(), 2);
        let later = Served {
            rx_ms: T0 + 25_000,
            ..real.clone()
        };
        push_unverified(&r, &mut slot, later.clone());
        let fresh = Served {
            frame: sign_frame(&key(1), [0; 16], &Beat { key_id: 200, ..beat() }),
            path: 1,
            rx_ms: T0 + 25_001,
        };
        push_unverified(&r, &mut slot, fresh.clone());
        assert_eq!(slot, vec![later, fresh], "the stale squatter aged out");
        let mut other = vec![];
        let stranger = Served {
            frame: sign_frame(&key(1), [0; 16], &Beat { collector_id: 77, ..beat() }),
            path: 1,
            rx_ms: T0,
        };
        push_unverified(&r, &mut other, stranger);
        let wrong_provider = Served {
            frame: sign_frame(&key(1), [0; 16], &Beat { provider_id: 99, ..beat() }),
            path: 1,
            rx_ms: T0,
        };
        push_unverified(&r, &mut other, wrong_provider);
        assert!(other.is_empty(), "unrostered collectors and providers are never stored");
    }
}
