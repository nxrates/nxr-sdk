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
use std::io::{Read, Write};
use std::marker::PhantomData;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use bytemuck::Pod;
use chrono::NaiveDate;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use tracing::warn;

use crate::ipc::append_log::AppendLog;
use crate::ipc::record::IndexRecord;

/// `Index.flags` bit 0: this record is a liveness sentinel, not a real quote
/// change. Written every [`SENTINEL_INTERVAL_MS`] while the quote is unchanged.
pub const FLAG_HEARTBEAT_SENTINEL: u8 = 0b0000_0001;

/// Cadence at which the delta-gate writes a liveness sentinel while the quote
/// is unchanged. 60s bounds the "is this a gap or just quiet?" ambiguity.
pub const SENTINEL_INTERVAL_MS: i64 = 60_000;

/// Milliseconds per UTC day. Shared by all retention / cutoff math.
pub const MS_PER_DAY: i64 = 86_400_000;

/// Milliseconds per 30-minute bucket (offline Parkinson HLC + sigma window).
pub const MS_PER_30MIN: i64 = 1_800_000;

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

/// Buffered streaming reader for any `[T: Pod]` shard file (4096 records per
/// refill). Used by the `*_from_idx` offline tools to avoid materializing whole
/// shards in memory. Returns `Ok(None)` at EOF; errors on misalignment.
///
/// Single source of truth for the previously duplicated `IdxStream` structs in
/// `s10_from_idx`, `renko_from_idx`, and `nxr_calibrate`.
pub struct ShardStream<T: Pod> {
    file: fs::File,
    buf: Vec<u8>,
    pos: usize,
    filled: usize,
    eof: bool,
    _marker: PhantomData<T>,
}

impl<T: Pod> ShardStream<T> {
    /// Open `path` for buffered streaming of `T` records.
    pub fn open(path: &Path) -> Result<Self> {
        let file = fs::File::open(path).with_context(|| format!("open {}", path.display()))?;
        let stride = core::mem::size_of::<T>().max(1);
        Ok(Self {
            file,
            buf: vec![0u8; 4096 * stride],
            pos: 0,
            filled: 0,
            eof: false,
            _marker: PhantomData,
        })
    }

    fn refill(&mut self) -> Result<()> {
        if self.eof {
            return Ok(());
        }
        self.filled = 0;
        self.pos = 0;
        while self.filled < self.buf.len() {
            match self.file.read(&mut self.buf[self.filled..])? {
                0 => {
                    self.eof = true;
                    break;
                }
                n => self.filled += n,
            }
        }
        let stride = core::mem::size_of::<T>();
        if stride > 0 && self.filled % stride != 0 {
            // Heal torn-trailing-write at EOF (mirrors `read_shard_aligned`):
            // the OS guarantees atomicity only up to a page, so an
            // aggregator killed mid-append can leave a partial trailing
            // record. Drop the unrecoverable bytes; do not bail — that
            // would block the whole calibrator + offline-bin fleet on any
            // single shard with a torn tail.
            let aligned = (self.filled / stride) * stride;
            warn!(
                dropped_bytes = self.filled - aligned,
                stride,
                "ShardStream: torn-trailing write at EOF — truncated"
            );
            self.filled = aligned;
        }
        Ok(())
    }

    /// Next record, or `Ok(None)` at EOF.
    pub fn next(&mut self) -> Result<Option<T>> {
        let stride = core::mem::size_of::<T>();
        if stride == 0 {
            return Ok(None);
        }
        if self.pos >= self.filled {
            self.refill()?;
            if self.pos >= self.filled {
                return Ok(None);
            }
        }
        let slice = &self.buf[self.pos..self.pos + stride];
        let rec: T = *bytemuck::from_bytes(slice);
        self.pos += stride;
        Ok(Some(rec))
    }
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

    /// Rescan all `<dir>/*.{kind_ext}` shards, drop stale entries for that
    /// extension, re-add fresh entries by scanning each file, and merge
    /// `kind_ext` into the comma-joined `kind` field (so a directory holding
    /// e.g. both `.s10` and `.renko` shards records both kinds).
    ///
    /// Replaces the duplicated manifest-merge loop in `s10_from_idx` and
    /// `renko_from_idx`.
    pub fn refresh_kind<T: ShardRecord>(&mut self, dir: &Path, kind_ext: &str) -> Result<()> {
        let existing = list_shards(dir, kind_ext)?;
        let existing_dates: Vec<String> = existing
            .iter()
            .map(|(d, _)| date_stem(*d))
            .collect();
        self.shards
            .retain(|s| !existing_dates.iter().any(|d| d == &s.date));
        for (date, path) in existing {
            self.upsert(shard_entry::<T>(date, &path)?);
        }
        if self.kind.is_empty() {
            self.kind = kind_ext.to_string();
        } else if !self.kind.split(',').any(|k| k == kind_ext) {
            self.kind = format!("{},{}", self.kind, kind_ext);
        }
        Ok(())
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
    /// Whether to refresh `manifest.json` on rotation/flush. The live aggregator
    /// keeps this on (one rotation per day per ticker — cheap). Bulk migration
    /// turns it OFF: rebuilding+sha256'ing+fsync'ing a growing manifest on every
    /// one of ~730 daily rotations per ticker is O(n²) fsync churn on DRBD. The
    /// API reader uses `list_shards`, not the manifest, so it is optional for
    /// serving; a manifest can be rebuilt cheaply in one pass afterward.
    manifest: bool,
    have_last: bool,
    // Full market-state tuple of the last written record. A tick is dropped
    // ONLY when ALL of these are identical to the previous (a truly redundant
    // observation) — so volume / confidence-interval changes are never lost.
    // This matters because bars (s10/renko, carrying realized_var, OFI,
    // vbid/vask, avg_ci) are rebuilt FROM the .idx by the *-from-idx tools;
    // gating on price alone would corrupt their microstructure aggregates.
    last_bid: f64,
    last_ask: f64,
    last_vbid: u32,
    last_vask: u32,
    last_ci: u16,
}

impl IdxShardWriter {
    /// Open the writer for `ticker_id` under `data_root`. `gate` toggles the
    /// delta-only-append behavior (off = append every record, for parity with
    /// the legacy writer during canary / migration). Manifest refresh is ON.
    pub fn open(data_root: &Path, ticker_id: u64, gate: bool) -> Result<Self> {
        Self::open_with(data_root, ticker_id, gate, true)
    }

    /// Like [`open`](Self::open) but lets the caller disable per-rotation
    /// manifest refresh (bulk migration sets `manifest=false` for speed).
    pub fn open_with(data_root: &Path, ticker_id: u64, gate: bool, manifest: bool) -> Result<Self> {
        let dir = idx_dir(data_root, ticker_id);
        fs::create_dir_all(&dir)
            .with_context(|| format!("create_dir_all {}", dir.display()))?;
        let mut w = Self {
            dir,
            cur_date: None,
            log: None,
            gate,
            manifest,
            have_last: false,
            last_bid: 0.0,
            last_ask: 0.0,
            last_vbid: 0,
            last_vask: 0,
            last_ci: 0,
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
                self.last_vbid = idx.vbid;
                self.last_vask = idx.vask;
                self.last_ci = idx.ci;
                self.have_last = true;
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
                if self.manifest {
                    if let Err(e) = self.finalize_manifest(prev) {
                        tracing::warn!(err = %e, "shard manifest finalize failed");
                    }
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
        // Copy market fields out of the packed body before comparing (no refs).
        // A record is redundant ONLY if the WHOLE market observation repeats:
        // price (bid/ask) AND volume (vbid/vask) AND confidence interval (ci).
        // Any change is new information (it feeds downstream bar microstructure),
        // so it is kept. Per-cycle metadata (tick_count/confidence/accepted/
        // rejected) is intentionally NOT part of the identity — it churns every
        // cycle and is not a market observation.
        let body = rec.index;
        let bid = body.bid;
        let ask = body.ask;
        let vbid = body.vbid;
        let vask = body.vask;
        let ci = body.ci;
        let changed = !self.have_last
            || bid != self.last_bid
            || ask != self.last_ask
            || vbid != self.last_vbid
            || vask != self.last_vask
            || ci != self.last_ci;

        if !self.gate || changed || new_day {
            self.ensure_shard(date)?;
            self.log.as_mut().unwrap().append(rec)?;
            self.last_bid = bid;
            self.last_ask = ask;
            self.last_vbid = vbid;
            self.last_vask = vask;
            self.last_ci = ci;
            self.have_last = true;
            return Ok(true);
        }

        // Truly redundant observation (every market field identical): dropped.
        // No sentinel / no fill — the gate only ever REMOVES identical ticks,
        // never inserts synthetic ones.
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

// ─────────────────────────────────────────────────────────────────────────
// BarShardWriter — live, self-rotating .s10 / .renko writer
// ─────────────────────────────────────────────────────────────────────────

/// Append-only writer for the canonical `bars/<id>/<date>.<ext>` layout,
/// where `ext` is `"s10"` (10s time-bucketed klines) or `"renko"` (price
/// movement bricks). 96 B records (`mitch::Bar`), ts-ascending.
///
/// Compared to [`IdxShardWriter`] this writer is intentionally simpler:
/// - **No delta-gate.** Bars are emitted at a producer-decided cadence (s10
///   every 10 s wall-clock, renko on price-cross). Every produced bar is
///   appended — the cadence policy lives in the producer, not the writer.
/// - **No sentinel.** Empty s10 windows are filled by the producer via
///   `flat_bar(ts_ms, ref_price)` so the on-disk series is zero-gap; the
///   writer never inserts synthetic rows.
///
/// Rotation matches `IdxShardWriter`: the shard for a record is chosen from
/// the record's `ts_ms` (UTC date). Manifest refresh is ON by default — one
/// rotation per day per ticker, cheap.
///
/// Producers stagger the rotation point away from `00:00:00` UTC by feeding
/// the writer a shifted ts (e.g. s10 → `ts - 2_000` ms, renko → `ts - 4_000`
/// ms) so the fsync storm on midnight does not pile onto idx's rotation. The
/// shift is the producer's responsibility; the writer just routes on whatever
/// ts is passed in.
pub struct BarShardWriter {
    dir: PathBuf,
    ticker_id: u64,
    ext: &'static str,
    cur_date: Option<NaiveDate>,
    log: Option<AppendLog<crate::mitch::bar::Bar>>,
    /// Whether to refresh `manifest.json` on rotation/flush. ON by default.
    manifest: bool,
}

impl BarShardWriter {
    /// Open the writer for `ticker_id` under `data_root`. `ext` must be one
    /// of `"s10"` or `"renko"`. Manifest refresh is ON.
    pub fn open(data_root: &Path, ticker_id: u64, ext: &'static str) -> Result<Self> {
        Self::open_with(data_root, ticker_id, ext, true)
    }

    /// Like [`open`](Self::open) but lets the caller disable per-rotation
    /// manifest refresh.
    pub fn open_with(
        data_root: &Path,
        ticker_id: u64,
        ext: &'static str,
        manifest: bool,
    ) -> Result<Self> {
        let dir = bars_dir(data_root, ticker_id);
        fs::create_dir_all(&dir)
            .with_context(|| format!("create_dir_all {}", dir.display()))?;
        let mut w = Self {
            dir,
            ticker_id,
            ext,
            cur_date: None,
            log: None,
            manifest,
        };
        w.seed_from_tail()?;
        Ok(w)
    }

    /// Read the last record of the most-recent shard so a same-day restart
    /// reopens the existing shard in append mode (no rotation, no manifest
    /// finalize) instead of trying to start a fresh one.
    fn seed_from_tail(&mut self) -> Result<()> {
        let shards = list_shards(&self.dir, self.ext)?;
        if let Some((date, _path)) = shards.last() {
            self.cur_date = Some(*date);
        }
        Ok(())
    }

    /// Return the epoch-ms timestamp of the last bar in the most-recent shard,
    /// or `None` if no shards exist / the shard is empty. Producers use this
    /// at startup to skip emitting a bar for a bucket that already exists.
    pub fn last_ts_ms(&self) -> Result<Option<i64>> {
        let shards = list_shards(&self.dir, self.ext)?;
        if let Some((_date, path)) = shards.last() {
            let recs = read_shard_aligned::<crate::mitch::bar::Bar>(path)?;
            return Ok(recs.last().map(|b| b.ts_ms()));
        }
        Ok(None)
    }

    /// Return the close price of the last bar in the most-recent shard, or
    /// `None` if no shards exist / shard empty. Renko producer uses this on
    /// open to seed its `last_close` so a restart preserves brick state
    /// continuity across processes.
    pub fn last_close(&self) -> Result<Option<f64>> {
        let shards = list_shards(&self.dir, self.ext)?;
        if let Some((_date, path)) = shards.last() {
            let recs = read_shard_aligned::<crate::mitch::bar::Bar>(path)?;
            return Ok(recs.last().map(|b| b.close));
        }
        Ok(None)
    }

    /// Return the last bar in the most-recent shard, or `None` if no shards
    /// exist / shard empty. Renko producer uses this to warm-seed σ-EMA from
    /// the prior shard's `realized_var` on restart, eliminating cold-start
    /// brick storms (Sprint M2 — audit point #5(v), 2026-05-26).
    pub fn last_bar(&self) -> Result<Option<crate::mitch::bar::Bar>> {
        let shards = list_shards(&self.dir, self.ext)?;
        if let Some((_date, path)) = shards.last() {
            let recs = read_shard_aligned::<crate::mitch::bar::Bar>(path)?;
            return Ok(recs.last().copied());
        }
        Ok(None)
    }

    /// Ensure the open log targets `date`'s shard, rotating (and finalizing
    /// the previous day's manifest entry) at the boundary.
    fn ensure_shard(&mut self, date: NaiveDate) -> Result<()> {
        if self.cur_date == Some(date) && self.log.is_some() {
            return Ok(());
        }
        if let Some(prev) = self.cur_date {
            if prev != date {
                self.log = None;
                if self.manifest {
                    if let Err(e) = self.finalize_manifest(prev) {
                        tracing::warn!(err = %e, "bar shard manifest finalize failed");
                    }
                }
            }
        }
        let path = shard_path(&self.dir, date, self.ext);
        self.log = Some(AppendLog::open(&path)?);
        self.cur_date = Some(date);
        Ok(())
    }

    /// Append `bar` to the appropriate daily shard. The shard date is taken
    /// from `bar.ts_ms()`; producers that want to stagger the rollover should
    /// stamp the bar's close_ts accordingly before calling.
    pub fn append(&mut self, bar: &crate::mitch::bar::Bar) -> Result<()> {
        let date = ts_ms_to_utc_date(bar.ts_ms());
        self.ensure_shard(date)?;
        self.log.as_mut().unwrap().append(bar)?;
        Ok(())
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
        let path = shard_path(&self.dir, date, self.ext);
        if !path.exists() {
            return Ok(());
        }
        let mpath = manifest_path(&self.dir);
        let ticker = self.ticker_id.to_string();
        let mut m = read_manifest(&mpath)?
            .unwrap_or_else(|| Manifest::new(ticker.clone(), self.ticker_id, self.ext));
        m.upsert(shard_entry::<crate::mitch::bar::Bar>(date, &path)?);
        // Make sure the manifest's kind field reflects every ext that has
        // shards in this dir (it could legitimately hold both .s10 and
        // .renko side by side).
        if m.kind.is_empty() {
            m.kind = self.ext.to_string();
        } else if !m.kind.split(',').any(|k| k == self.ext) {
            m.kind = format!("{},{}", m.kind, self.ext);
        }
        write_manifest(&mpath, &m)
    }
}

impl Drop for BarShardWriter {
    fn drop(&mut self) {
        if let Some(date) = self.cur_date {
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
        rec_vol(ts_ms, bid, ask, 0, 0, 0)
    }

    fn rec_vol(ts_ms: i64, bid: f64, ask: f64, vbid: u32, vask: u32, ci: u16) -> IndexRecord {
        let mts = crate::mitch::timestamp::from_epoch_ms(ts_ms);
        let header = MitchHeader::new(message_type::INDEX, 0, mts, 1);
        let mut index = Index::new(1, bid, ask, ci, vbid, vask, 1, 1, 1, 0);
        index.ticker = 700;
        IndexRecord::new(header, index)
    }

    #[test]
    fn gate_keeps_volume_and_ci_changes_at_same_price() {
        // Microstructure guarantee: a tick with unchanged bid/ask but changed
        // volume or ci must NOT be dropped — bars (vbid/vask/OFI/avg_ci) are
        // rebuilt from the .idx, so dropping these would corrupt them.
        let root = std::env::temp_dir().join("nxr_shard_microstructure_test");
        let _ = fs::remove_dir_all(&root);
        let t0 = 1_716_249_600_000;
        {
            let mut w = IdxShardWriter::open(&root, 703, true).unwrap();
            assert!(w.append(&rec_vol(t0, 100.0, 101.0, 5, 5, 10)).unwrap()); // first
            assert!(w.append(&rec_vol(t0 + 100, 100.0, 101.0, 9, 5, 10)).unwrap()); // vbid Δ → kept
            assert!(w.append(&rec_vol(t0 + 200, 100.0, 101.0, 9, 9, 10)).unwrap()); // vask Δ → kept
            assert!(w.append(&rec_vol(t0 + 300, 100.0, 101.0, 9, 9, 22)).unwrap()); // ci Δ → kept
            assert!(!w.append(&rec_vol(t0 + 400, 100.0, 101.0, 9, 9, 22)).unwrap()); // identical → dropped
            w.flush().unwrap();
        }
        let dir = idx_dir(&root, 703);
        let recs = read_shard_aligned::<IndexRecord>(&list_shards(&dir, "idx").unwrap()[0].1).unwrap();
        assert_eq!(recs.len(), 4); // 4 distinct observations kept, 1 identical dropped
        let _ = fs::remove_dir_all(&root);
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
    fn shard_stream_round_trip() {
        // ShardStream<IndexRecord> must round-trip a multi-page shard 1:1.
        let root = std::env::temp_dir().join("nxr_shard_stream_test");
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(&root).unwrap();
        let p = root.join("2024-05-21.idx");
        let mut all = Vec::new();
        // 8200 records ⇒ exceeds the 4096-record refill page, exercises refill().
        for i in 0..8200i64 {
            let r = rec(1_716_249_600_000 + i * 1000, 100.0 + (i % 7) as f64, 101.0 + (i % 7) as f64);
            all.extend_from_slice(bytemuck::bytes_of(&r));
        }
        fs::write(&p, &all).unwrap();
        let mut stream = ShardStream::<IndexRecord>::open(&p).unwrap();
        let mut n = 0usize;
        while let Some(r) = stream.next().unwrap() {
            assert_eq!(r.shard_ts_ms(), 1_716_249_600_000 + (n as i64) * 1000);
            n += 1;
        }
        assert_eq!(n, 8200);
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn manifest_refresh_kind_rescans_and_merges() {
        // refresh_kind drops stale entries for the matching extension, re-adds
        // fresh ones from disk, and merges the kind into the comma-joined field.
        let root = std::env::temp_dir().join("nxr_manifest_refresh_test");
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(&root).unwrap();
        // Write two .idx shards.
        let d1 = NaiveDate::from_ymd_opt(2024, 5, 21).unwrap();
        let d2 = NaiveDate::from_ymd_opt(2024, 5, 22).unwrap();
        let r1 = rec(1_716_249_600_000, 100.0, 101.0);
        let r2 = rec(1_716_249_600_000 + 86_400_000, 102.0, 103.0);
        fs::write(shard_path(&root, d1, "idx"), bytemuck::bytes_of(&r1)).unwrap();
        fs::write(shard_path(&root, d2, "idx"), bytemuck::bytes_of(&r2)).unwrap();
        let mut m = Manifest::new("X".into(), 1, "");
        m.refresh_kind::<IndexRecord>(&root, "idx").unwrap();
        assert_eq!(m.shards.len(), 2);
        assert_eq!(m.kind, "idx");
        // Re-running should be idempotent.
        m.refresh_kind::<IndexRecord>(&root, "idx").unwrap();
        assert_eq!(m.shards.len(), 2);
        assert_eq!(m.kind, "idx");
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn bar_shard_writer_roundtrip() {
        // BarShardWriter must round-trip Bar records 1:1 through open / append /
        // reopen — and the seed_from_tail path must let a restart continue
        // writing into the same daily shard instead of starting fresh.
        use crate::mitch::bar::Bar;
        let root = std::env::temp_dir().join("nxr_bar_shard_writer_roundtrip");
        let _ = fs::remove_dir_all(&root);
        let t0 = 1_716_249_600_000; // 2024-05-21 00:00 UTC
        let mut bars: Vec<Bar> = Vec::new();
        {
            let mut w = BarShardWriter::open(&root, 900, "s10").unwrap();
            for i in 0..5i64 {
                let ts = t0 + i * 10_000; // 10 s buckets
                let mts = crate::mitch::timestamp::from_epoch_ms(ts);
                let b = Bar::new_ohlcv(
                    mts, mts,
                    100.0 + i as f64,
                    100.5 + i as f64,
                    99.5 + i as f64,
                    100.25 + i as f64,
                    1, 1, 1,
                );
                w.append(&b).unwrap();
                bars.push(b);
            }
            w.flush().unwrap();
        }
        // Reopen: shards should already exist, last_ts_ms reports the last bar.
        let w2 = BarShardWriter::open(&root, 900, "s10").unwrap();
        let last = w2.last_ts_ms().unwrap();
        assert_eq!(last, Some(t0 + 4 * 10_000));
        drop(w2);
        // Direct shard read must match the written sequence byte-for-byte.
        let dir = bars_dir(&root, 900);
        let shards = list_shards(&dir, "s10").unwrap();
        assert_eq!(shards.len(), 1);
        let recs = read_shard_aligned::<Bar>(&shards[0].1).unwrap();
        assert_eq!(recs.len(), 5);
        for (a, b) in recs.iter().zip(bars.iter()) {
            // Bar derives PartialEq, compare full struct (incl. enrichment).
            assert_eq!(a, b);
        }
        // Manifest must have a single entry for this date with n_records=5.
        let m = read_manifest(&manifest_path(&dir)).unwrap().unwrap();
        assert_eq!(m.shards.len(), 1);
        assert_eq!(m.shards[0].n_records, 5);
        assert!(m.kind.contains("s10"));
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
