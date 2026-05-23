//! Unified daily-shard storage layer (single source of truth).
//!
//! Replaces the duplicated shard I/O that previously lived in
//! `series-factory/src/sharding.rs`, the inline flat writer in
//! `core/src/aggregator.rs`, and the ad-hoc readers in the API and the
//! `*_from_idx` tools. Both live (aggregator) and historical (series-factory)
//! data now use the *same* on-disk format:
//!
//! ```text
//! <data>/indexes/<MITCH_ID>/<YYYY-MM-DD>.idx     56B IndexRecord, ts-ascending
//! <data>/indexes/<MITCH_ID>/manifest.json
//! <data>/bars/<MITCH_ID>/<YYYY-MM-DD>.s10         96B Bar (Kline)
//! <data>/bars/<MITCH_ID>/<YYYY-MM-DD>.renko       96B Bar (Renko)
//! <data>/bars/<MITCH_ID>/manifest.json
//! ```
//!
//! `<MITCH_ID>` is the decimal `u64` ticker id. Keying by id (not `BASE-QUOTE`)
//! removes the reverse-lookup ambiguity (spot vs futures vs options share a
//! plaintext pair but differ in `ticker_id`).
//!
//! ## Delta-only append
//!
//! [`IdxShardWriter`] only appends a record when the composite quote actually
//! moved (`bid != last.bid || ask != last.ask`), with two exceptions:
//! - the first record of each UTC day is always written (so every shard is
//!   self-contained and the day boundary is unambiguous), and
//! - a liveness *sentinel* is written every [`SENTINEL_INTERVAL_MS`] while the
//!   quote is unchanged, flagged via [`FLAG_HEARTBEAT_SENTINEL`] in
//!   `Index.flags`. This lets a gap-detection audit distinguish a *quiet*
//!   market (sentinels present) from a *producer outage* (no sentinels).

use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use bytemuck::Pod;
use chrono::NaiveDate;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::ipc::append_log::AppendLog;
use crate::ipc::record::IndexRecord;

/// `Index.flags` bit 0: this record is a liveness sentinel, not a real quote
/// change. Written every [`SENTINEL_INTERVAL_MS`] while the quote is unchanged.
pub const FLAG_HEARTBEAT_SENTINEL: u8 = 0b0000_0001;

/// Cadence at which the delta-gate writes a liveness sentinel while the quote
/// is unchanged. 60s bounds the "is this a gap or just quiet?" ambiguity.
pub const SENTINEL_INTERVAL_MS: i64 = 60_000;

// ─────────────────────────────────────────────────────────────────────────
// Date / path math
// ─────────────────────────────────────────────────────────────────────────

/// UTC date for an epoch-ms timestamp — the shard bucket key for ∀ artifacts.
#[inline]
pub fn ts_ms_to_utc_date(ts_ms: i64) -> NaiveDate {
    let secs = ts_ms.div_euclid(1000);
    chrono::DateTime::<chrono::Utc>::from_timestamp(secs, 0)
        .unwrap_or_else(|| chrono::DateTime::<chrono::Utc>::from_timestamp(0, 0).unwrap())
        .date_naive()
}

/// Format `YYYY-MM-DD` (spec-canonical shard filename stem).
#[inline]
pub fn date_stem(d: NaiveDate) -> String {
    d.format("%Y-%m-%d").to_string()
}

/// Per-ticker indexes directory, keyed by MITCH id: `<data>/indexes/<id>`.
pub fn idx_dir(data_root: &Path, ticker_id: u64) -> PathBuf {
    data_root.join("indexes").join(ticker_id.to_string())
}

/// Per-ticker bars directory, keyed by MITCH id: `<data>/bars/<id>`.
pub fn bars_dir(data_root: &Path, ticker_id: u64) -> PathBuf {
    data_root.join("bars").join(ticker_id.to_string())
}

/// Shard file path for `(ticker_dir, date, ext)` — ext in {`idx`,`s10`,`renko`}.
pub fn shard_path(ticker_dir: &Path, date: NaiveDate, ext: &str) -> PathBuf {
    ticker_dir.join(format!("{}.{}", date_stem(date), ext))
}

/// Manifest path inside a ticker directory.
pub fn manifest_path(ticker_dir: &Path) -> PathBuf {
    ticker_dir.join("manifest.json")
}

/// List existing daily shards in a ticker dir by extension, sorted by date.
pub fn list_shards(ticker_dir: &Path, ext: &str) -> Result<Vec<(NaiveDate, PathBuf)>> {
    let mut out = Vec::new();
    if !ticker_dir.exists() {
        return Ok(out);
    }
    let suffix = format!(".{}", ext);
    for entry in fs::read_dir(ticker_dir)
        .with_context(|| format!("read_dir {}", ticker_dir.display()))?
    {
        let entry = entry?;
        let path = entry.path();
        let name = match path.file_name().and_then(|n| n.to_str()) {
            Some(n) => n,
            None => continue,
        };
        if !name.ends_with(&suffix) {
            continue;
        }
        let stem = &name[..name.len() - suffix.len()];
        if let Ok(d) = NaiveDate::parse_from_str(stem, "%Y-%m-%d") {
            out.push((d, path));
        }
    }
    out.sort_by_key(|(d, _)| *d);
    Ok(out)
}

// ─────────────────────────────────────────────────────────────────────────
// Record timestamp trait (generic shard readers / manifest builders)
// ─────────────────────────────────────────────────────────────────────────

/// A fixed-size Pod record that carries an epoch-ms observation timestamp.
/// Implemented for [`IndexRecord`] (56B) and [`mitch::bar::Bar`] (96B) so the
/// shard reader / manifest builder are generic over both.
pub trait ShardRecord: Pod {
    /// Observation timestamp in epoch milliseconds.
    fn shard_ts_ms(&self) -> i64;
}

impl ShardRecord for IndexRecord {
    #[inline]
    fn shard_ts_ms(&self) -> i64 {
        // Copy the header out of the packed record before calling a method on
        // it (no reference to a packed field).
        let header = self.header;
        crate::mitch::timestamp::to_epoch_ms(header.get_timestamp())
    }
}

impl ShardRecord for crate::mitch::bar::Bar {
    #[inline]
    fn shard_ts_ms(&self) -> i64 {
        self.ts_ms()
    }
}

/// Read a whole shard, healing a torn trailing write: any bytes beyond the last
/// whole `size_of::<T>()` boundary are dropped. Returns the records as a `Vec`.
///
/// Torn writes happen when a process is killed mid-append (the OS guarantees
/// atomicity only up to a page); the trailing partial record is unrecoverable
/// and must be discarded, not parsed.
pub fn read_shard_aligned<T: Pod>(path: &Path) -> Result<Vec<T>> {
    let bytes = fs::read(path).with_context(|| format!("read shard {}", path.display()))?;
    let stride = std::mem::size_of::<T>();
    if stride == 0 {
        return Ok(Vec::new());
    }
    let n = bytes.len() / stride;
    let aligned = &bytes[..n * stride];
    // T is `repr(C, packed)` (align 1) for our record types, so cast_slice
    // never fails the alignment check; bytes from `fs::read` are align-1 safe.
    Ok(bytemuck::cast_slice::<u8, T>(aligned).to_vec())
}

/// Number of whole records in a shard file (cheap: `len / stride`, no read).
pub fn shard_record_count<T>(path: &Path) -> Result<u64> {
    let len = fs::metadata(path)
        .with_context(|| format!("stat {}", path.display()))?
        .len();
    let stride = std::mem::size_of::<T>() as u64;
    Ok(if stride == 0 { 0 } else { len / stride })
}

// ─────────────────────────────────────────────────────────────────────────
// Manifest
// ─────────────────────────────────────────────────────────────────────────

/// Per-shard manifest entry.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ShardEntry {
    pub date: String,
    pub first_ts: i64,
    pub last_ts: i64,
    pub n_records: u64,
    pub size_bytes: u64,
    pub sha256: String,
}

/// Per-ticker shard inventory + integrity hashes.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Manifest {
    pub ticker: String,
    #[serde(default)]
    pub ticker_id: u64,
    #[serde(default)]
    pub kind: String, // "idx" | "s10" | "renko"
    pub shards: Vec<ShardEntry>,
    pub first_ts: i64,
    pub last_ts: i64,
    pub last_updated: i64,
    pub schema_version: u32,
}

impl Manifest {
    pub fn new(ticker: String, ticker_id: u64, kind: &str) -> Self {
        Self {
            ticker,
            ticker_id,
            kind: kind.to_string(),
            shards: Vec::new(),
            first_ts: i64::MAX,
            last_ts: i64::MIN,
            last_updated: 0,
            schema_version: 1,
        }
    }

    /// Insert or replace a shard entry by date; keeps shards sorted by date and
    /// recomputes the manifest-level first/last ts.
    pub fn upsert(&mut self, entry: ShardEntry) {
        if let Some(existing) = self.shards.iter_mut().find(|s| s.date == entry.date) {
            *existing = entry;
        } else {
            self.shards.push(entry);
        }
        self.shards.sort_by(|a, b| a.date.cmp(&b.date));
        if let Some(first) = self.shards.first() {
            self.first_ts = first.first_ts;
        }
        if let Some(last) = self.shards.last() {
            self.last_ts = last.last_ts;
        }
        self.last_updated = chrono::Utc::now().timestamp_millis();
    }
}

/// Read manifest if present; else `None`.
pub fn read_manifest(path: &Path) -> Result<Option<Manifest>> {
    if !path.exists() {
        return Ok(None);
    }
    let raw =
        fs::read_to_string(path).with_context(|| format!("read manifest {}", path.display()))?;
    let m: Manifest =
        serde_json::from_str(&raw).with_context(|| format!("parse manifest {}", path.display()))?;
    Ok(Some(m))
}

/// Atomic manifest write (`.tmp` then rename).
pub fn write_manifest(path: &Path, m: &Manifest) -> Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)
            .with_context(|| format!("create_dir_all {}", parent.display()))?;
    }
    let tmp = path.with_extension("json.tmp");
    let json = serde_json::to_string_pretty(m)?;
    {
        let mut f = fs::File::create(&tmp).with_context(|| format!("create {}", tmp.display()))?;
        f.write_all(json.as_bytes())?;
        let _ = f.sync_data();
    }
    fs::rename(&tmp, path)
        .with_context(|| format!("rename {} -> {}", tmp.display(), path.display()))?;
    Ok(())
}

/// Compute sha256 of a file (streamed, 1 MiB chunks).
pub fn sha256_file(path: &Path) -> Result<String> {
    use std::io::Read;
    let mut f =
        fs::File::open(path).with_context(|| format!("open for hash {}", path.display()))?;
    let mut hasher = Sha256::new();
    let mut buf = vec![0u8; 1 << 20];
    loop {
        let n = f.read(&mut buf)?;
        if n == 0 {
            break;
        }
        hasher.update(&buf[..n]);
    }
    Ok(hex::encode(hasher.finalize()))
}

/// Build a [`ShardEntry`] for a finished shard file by scanning it. Generic
/// over any [`ShardRecord`] (idx or bar).
pub fn shard_entry<T: ShardRecord>(date: NaiveDate, path: &Path) -> Result<ShardEntry> {
    let recs = read_shard_aligned::<T>(path)?;
    let first_ts = recs.first().map(|r| r.shard_ts_ms()).unwrap_or(0);
    let last_ts = recs.last().map(|r| r.shard_ts_ms()).unwrap_or(0);
    let size_bytes = fs::metadata(path).map(|m| m.len()).unwrap_or(0);
    Ok(ShardEntry {
        date: date_stem(date),
        first_ts,
        last_ts,
        n_records: recs.len() as u64,
        size_bytes,
        sha256: sha256_file(path)?,
    })
}

/// Atomic shard write: write `bytes` to `<path>.tmp` → fsync → rename → path.
/// For one-shot writers (migration / backfill); the live writer uses
/// [`IdxShardWriter`] which appends incrementally.
pub fn write_shard_atomic(path: &Path, bytes: &[u8]) -> Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)
            .with_context(|| format!("create_dir_all {}", parent.display()))?;
    }
    let ext = path.extension().and_then(|s| s.to_str()).unwrap_or("");
    let tmp = if ext.is_empty() {
        path.with_extension("tmp")
    } else {
        path.with_extension(format!("{ext}.tmp"))
    };
    {
        let mut f = fs::File::create(&tmp).with_context(|| format!("create {}", tmp.display()))?;
        f.write_all(bytes)?;
        let _ = f.sync_data();
    }
    fs::rename(&tmp, path)
        .with_context(|| format!("rename {} -> {}", tmp.display(), path.display()))?;
    Ok(())
}

// ─────────────────────────────────────────────────────────────────────────
// IdxShardWriter — live, delta-gated, self-rotating .idx writer
// ─────────────────────────────────────────────────────────────────────────

/// Append-only writer for the canonical `indexes/<id>/<date>.idx` layout.
///
/// - Self-rotates at UTC midnight (the shard is chosen by the record's ts).
/// - Optional delta-gate: with `gate` enabled, a record is only appended when
///   the quote moved, the day rolled, or a sentinel is due.
/// - Seeds `last_bid/last_ask` from the tail of the latest existing shard on
///   open, so a process restart neither drops nor duplicates the first
///   post-restart record.
pub struct IdxShardWriter {
    dir: PathBuf,
    cur_date: Option<NaiveDate>,
    log: Option<AppendLog<IndexRecord>>,
    gate: bool,
    have_last: bool,
    last_bid: f64,
    last_ask: f64,
    last_sentinel_ms: i64,
}

impl IdxShardWriter {
    /// Open the writer for `ticker_id` under `data_root`. `gate` toggles the
    /// delta-only-append behavior (off = append every record, for parity with
    /// the legacy writer during canary / migration).
    pub fn open(data_root: &Path, ticker_id: u64, gate: bool) -> Result<Self> {
        let dir = idx_dir(data_root, ticker_id);
        fs::create_dir_all(&dir)
            .with_context(|| format!("create_dir_all {}", dir.display()))?;
        let mut w = Self {
            dir,
            cur_date: None,
            log: None,
            gate,
            have_last: false,
            last_bid: 0.0,
            last_ask: 0.0,
            last_sentinel_ms: i64::MIN,
        };
        w.seed_from_tail()?;
        Ok(w)
    }

    /// Read the last record of the most-recent shard to seed the delta-gate
    /// baseline. Without this, a restart would re-write the first incoming
    /// record even if the quote was unchanged across the restart.
    fn seed_from_tail(&mut self) -> Result<()> {
        let shards = list_shards(&self.dir, "idx")?;
        if let Some((date, path)) = shards.last() {
            // Seed the rotation date from the latest shard so a same-day
            // restart does NOT trip the day-boundary "always write" exception.
            // If the latest shard predates today, the first record of today is
            // a genuine new-day write (handled by ensure_shard).
            self.cur_date = Some(*date);
            let recs = read_shard_aligned::<IndexRecord>(path)?;
            if let Some(last) = recs.last() {
                let idx = last.index;
                self.last_bid = idx.bid;
                self.last_ask = idx.ask;
                self.have_last = true;
                self.last_sentinel_ms = last.shard_ts_ms();
            }
        }
        Ok(())
    }

    /// Ensure the open log targets `date`'s shard, rotating (and finalizing the
    /// previous day's manifest entry) at the boundary.
    fn ensure_shard(&mut self, date: NaiveDate) -> Result<()> {
        if self.cur_date == Some(date) && self.log.is_some() {
            return Ok(());
        }
        // Finalize the previous day's manifest entry only on a genuine date
        // change. A same-day open with no live log (seeded-from-tail restart)
        // just reopens today's shard in append mode — no finalize, no rotate.
        if let Some(prev) = self.cur_date {
            if prev != date {
                // Drop the old log first so its fdatasync runs and the file is
                // complete before we hash it.
                self.log = None;
                if let Err(e) = self.finalize_manifest(prev) {
                    tracing::warn!(err = %e, "shard manifest finalize failed");
                }
            }
        }
        let path = shard_path(&self.dir, date, "idx");
        self.log = Some(AppendLog::open(&path)?);
        self.cur_date = Some(date);
        Ok(())
    }

    /// Append `rec` subject to the delta-gate. Returns `true` if a real (non
    /// sentinel) record was written. The caller's broadcast / snapshot path is
    /// unaffected — only the *disk append* is gated.
    pub fn append(&mut self, rec: &IndexRecord) -> Result<bool> {
        let ts = rec.shard_ts_ms();
        let date = ts_ms_to_utc_date(ts);
        let new_day = self.cur_date != Some(date);
        // Copy bid/ask out of the packed body before comparing (no refs).
        let body = rec.index;
        let bid = body.bid;
        let ask = body.ask;
        let changed = !self.have_last || bid != self.last_bid || ask != self.last_ask;

        if !self.gate || changed || new_day {
            self.ensure_shard(date)?;
            self.log.as_mut().unwrap().append(rec)?;
            self.last_bid = bid;
            self.last_ask = ask;
            self.have_last = true;
            self.last_sentinel_ms = ts;
            return Ok(true);
        }

        // Gated out (quote unchanged): emit a liveness sentinel if one is due.
        if ts - self.last_sentinel_ms >= SENTINEL_INTERVAL_MS {
            self.ensure_shard(date)?;
            let mut sentinel = *rec;
            sentinel.index.flags |= FLAG_HEARTBEAT_SENTINEL;
            self.log.as_mut().unwrap().append(&sentinel)?;
            self.last_sentinel_ms = ts;
        }
        Ok(false)
    }

    /// Force durability of the current shard and refresh its manifest entry.
    pub fn flush(&mut self) -> Result<()> {
        if let Some(log) = self.log.as_mut() {
            log.flush()?;
        }
        if let Some(date) = self.cur_date {
            self.finalize_manifest(date)?;
        }
        Ok(())
    }

    /// Recompute and upsert the manifest entry for `date`'s shard.
    fn finalize_manifest(&self, date: NaiveDate) -> Result<()> {
        let path = shard_path(&self.dir, date, "idx");
        if !path.exists() {
            return Ok(());
        }
        let mpath = manifest_path(&self.dir);
        let ticker = self
            .dir
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("")
            .to_string();
        let ticker_id = ticker.parse::<u64>().unwrap_or(0);
        let mut m = read_manifest(&mpath)?
            .unwrap_or_else(|| Manifest::new(ticker.clone(), ticker_id, "idx"));
        m.upsert(shard_entry::<IndexRecord>(date, &path)?);
        write_manifest(&mpath, &m)
    }
}

impl Drop for IdxShardWriter {
    fn drop(&mut self) {
        // Best-effort durability + manifest finalize on graceful shutdown.
        if let Some(date) = self.cur_date {
            // Drop the log to fdatasync, then hash.
            self.log = None;
            let _ = self.finalize_manifest(date);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mitch::header::MitchHeader;
    use crate::mitch::common::message_type;
    use crate::mitch::index::Index;

    fn rec(ts_ms: i64, bid: f64, ask: f64) -> IndexRecord {
        let mts = crate::mitch::timestamp::from_epoch_ms(ts_ms);
        let header = MitchHeader::new(message_type::INDEX, 0, mts, 1);
        let mut index = Index::new(1, bid, ask, 0, 0, 0, 1, 1, 1, 0);
        index.ticker = 700;
        IndexRecord::new(header, index)
    }

    #[test]
    fn date_bucket_rolls_at_utc_midnight() {
        let d = ts_ms_to_utc_date(1_716_249_600_000); // 2024-05-21 00:00 UTC
        assert_eq!(date_stem(d), "2024-05-21");
        let d2 = ts_ms_to_utc_date(1_716_249_600_000 + 86_399_999);
        assert_eq!(date_stem(d2), "2024-05-21");
        let d3 = ts_ms_to_utc_date(1_716_249_600_000 + 86_400_000);
        assert_eq!(date_stem(d3), "2024-05-22");
    }

    #[test]
    fn delta_gate_drops_unchanged_quotes() {
        let root = std::env::temp_dir().join("nxr_shard_gate_test");
        let _ = fs::remove_dir_all(&root);
        let t0 = 1_716_249_600_000;
        {
            let mut w = IdxShardWriter::open(&root, 700, true).unwrap();
            assert!(w.append(&rec(t0, 100.0, 101.0)).unwrap()); // first: written
            assert!(!w.append(&rec(t0 + 100, 100.0, 101.0)).unwrap()); // same: gated
            assert!(w.append(&rec(t0 + 200, 100.5, 101.5)).unwrap()); // moved: written
            w.flush().unwrap();
        }
        let dir = idx_dir(&root, 700);
        let shards = list_shards(&dir, "idx").unwrap();
        assert_eq!(shards.len(), 1);
        let recs = read_shard_aligned::<IndexRecord>(&shards[0].1).unwrap();
        assert_eq!(recs.len(), 2); // the gated-out record is absent
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn day_boundary_always_writes() {
        let root = std::env::temp_dir().join("nxr_shard_dayboundary_test");
        let _ = fs::remove_dir_all(&root);
        let day1 = 1_716_249_600_000; // 2024-05-21
        let day2 = day1 + 86_400_000; // 2024-05-22
        {
            let mut w = IdxShardWriter::open(&root, 701, true).unwrap();
            assert!(w.append(&rec(day1, 100.0, 101.0)).unwrap());
            // Same quote but next day → still written (new shard).
            assert!(w.append(&rec(day2, 100.0, 101.0)).unwrap());
            w.flush().unwrap();
        }
        let dir = idx_dir(&root, 701);
        assert_eq!(list_shards(&dir, "idx").unwrap().len(), 2);
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn seed_from_tail_survives_restart() {
        let root = std::env::temp_dir().join("nxr_shard_restart_test");
        let _ = fs::remove_dir_all(&root);
        let t0 = 1_716_249_600_000;
        {
            let mut w = IdxShardWriter::open(&root, 702, true).unwrap();
            w.append(&rec(t0, 100.0, 101.0)).unwrap();
            w.flush().unwrap();
        }
        {
            // Restart: same quote should be gated (seeded from tail), not re-written.
            let mut w = IdxShardWriter::open(&root, 702, true).unwrap();
            assert!(!w.append(&rec(t0 + 100, 100.0, 101.0)).unwrap());
            w.flush().unwrap();
        }
        let dir = idx_dir(&root, 702);
        let recs = read_shard_aligned::<IndexRecord>(&list_shards(&dir, "idx").unwrap()[0].1).unwrap();
        assert_eq!(recs.len(), 1);
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn torn_trailing_record_is_healed() {
        let root = std::env::temp_dir().join("nxr_shard_torn_test");
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(&root).unwrap();
        let p = root.join("2024-05-21.idx");
        let good = rec(1_716_249_600_000, 100.0, 101.0);
        let mut bytes = bytemuck::bytes_of(&good).to_vec();
        bytes.extend_from_slice(&[0u8; 20]); // torn partial record
        fs::write(&p, &bytes).unwrap();
        let recs = read_shard_aligned::<IndexRecord>(&p).unwrap();
        assert_eq!(recs.len(), 1);
        let _ = fs::remove_dir_all(&root);
    }
}
