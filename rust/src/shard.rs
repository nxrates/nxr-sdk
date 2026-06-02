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
use fs2::FileExt;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use tracing::warn;

use crate::ipc::append_log::AppendLog;
use crate::ipc::record::IndexRecord;

/// `Index.flags` bit 0: this record is a liveness sentinel, not a real quote
/// change. Written every [`SENTINEL_INTERVAL_MS`] while the quote is unchanged.
pub const FLAG_HEARTBEAT_SENTINEL: u8 = 0b0000_0001;

/// `Index.flags` bit 1: this record was produced by an offline backfill /
/// historical replay (merge-idx, migrate_to_sharded, ticks-to-idx), not by the
/// live aggregator. Lets a consumer tag synthetic vs real-time records when
/// joining backfill+live, and lets the audit understand why a "future ts"
/// guard might legitimately have to skip a backfilled record (R1 H12).
pub const FLAG_HISTORICAL_BACKFILL: u8 = 0b0000_0010;

/// `Bar.flags` bit 2: this renko brick is a synthetic continuation of a
/// multi-brick tick (i.e. one source tick crossed N >= 2 brick boundaries
/// and bricks 2..N were minted to maintain price-grid continuity). The
/// first brick of the sequence carries the full microstructure (tick_count,
/// realized_var, drift, …); synthetic bricks have `tick_count = 0` and zero
/// microstructure. Consumers training on tick-rate / OFI / spread should
/// filter `(flags & FLAG_RENKO_SYNTHETIC_BRICK) == 0`. Geometry-only
/// consumers can ignore the flag. Set by `nxr_sdk::renko::RenkoGenerator`.
pub const FLAG_RENKO_SYNTHETIC_BRICK: u8 = 0b0000_0100;

/// Cadence at which the delta-gate writes a liveness sentinel while the quote
/// is unchanged. 60s bounds the "is this a gap or just quiet?" ambiguity.
pub const SENTINEL_INTERVAL_MS: i64 = 60_000;

/// Milliseconds per UTC day. Shared by all retention / cutoff math.
pub const MS_PER_DAY: i64 = 86_400_000;

/// Milliseconds per hour. Shared time constant.
pub const MS_PER_HOUR: i64 = 3_600_000;

/// Milliseconds per minute. Shared time constant (1-min calibration buckets).
pub const MS_PER_MIN: i64 = 60_000;

/// Seconds per UTC day. Shared time constant.
pub const SECS_PER_DAY: u64 = 86_400;

/// Milliseconds per 30-minute bucket (offline Parkinson HLC + sigma window).
pub const MS_PER_30MIN: i64 = 1_800_000;

/// S10 bar window in milliseconds (the canonical s10 cadence). Shared by the
/// live s10 producer, the synth s10 producer, and the offline backfill so a
/// single edit changes them all.
pub const BAR_MS_S10: i64 = 10_000;

/// Shard-rotation stagger for s10 producers: feed the writer `ts - SHIFT` so
/// the daily rollover fsync lands at 00:00:02 UTC, off the idx 00:00:00 hot
/// path. Single source of truth.
pub const ROLLOVER_SHIFT_MS_S10: i64 = 2_000;

/// Shard-rotation stagger for renko producers: feed the writer `ts - SHIFT`
/// so the daily rollover fsync lands at 00:00:04 UTC, off both idx
/// (00:00:00) and s10 (00:00:02). Single source of truth.
pub const ROLLOVER_SHIFT_MS_RENKO: i64 = 4_000;

/// Canonical timeframe ladder in seconds — server-enforced whitelist for
/// `/v1/ohlc` and `/v1/synth/ohlc`, single source of truth for the BTR TF
/// ladder. Was hardcoded `TF_WHITELIST_S` at `core/src/server/rest.rs:31`
/// (phase 59.R3.C3.O2, 2026-05-30).
pub const CANONICAL_TFS_S: &[u32] = &[
    10, 20, 30, 60, 120, 300, 900, 1800,
    3600, 7200, 14400, 28800, 43200, 86400, 259200,
];

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

/// UTC midnight (start-of-day) epoch-ms for a `NaiveDate`. Single source of
/// truth for the day-bucket alignment used across the series-factory bins
/// (mtf_sweep, renko_trailing_from_idx, synth_backfill_from_idx,
/// synth_sigma_benchmark). Was 4× verbatim local duplicates (phase
/// 59.R3.C5.C1, 2026-05-30).
#[inline]
pub fn day_start_ms(d: NaiveDate) -> i64 {
    use chrono::{NaiveDateTime, NaiveTime, TimeZone, Utc};
    let ndt = NaiveDateTime::new(d, NaiveTime::from_hms_opt(0, 0, 0).unwrap());
    Utc.from_utc_datetime(&ndt).timestamp_millis()
}

/// Parse a UTC date in `YYYY-MM-DD` format. Single source of truth for the
/// 5+ duplicate `parse_date` copies that previously lived in the offline bins.
#[inline]
pub fn parse_utc_date(s: &str) -> anyhow::Result<NaiveDate> {
    NaiveDate::parse_from_str(s, "%Y-%m-%d")
        .with_context(|| format!("parse date `{}`", s))
}

/// Parse a UTC date in `YYYY-MM-DD` format OR the literal "today" (current
/// UTC date). Thin wrapper around [`parse_utc_date`] for binaries that accept
/// a `today` alias on the CLI (e.g. `backfill_all`).
#[inline]
pub fn parse_utc_date_or_today(s: &str) -> anyhow::Result<NaiveDate> {
    if s.eq_ignore_ascii_case("today") {
        return Ok(chrono::Utc::now().date_naive());
    }
    parse_utc_date(s)
}

/// Inclusive set of UTC dates in `[from, to]` (ascending). Single source of
/// truth for the duplicate `day_range` copies in the offline bins.
pub fn day_range_inclusive(from: NaiveDate, to: NaiveDate) -> Vec<NaiveDate> {
    let mut out = Vec::new();
    let mut d = from;
    while d <= to {
        out.push(d);
        d = match d.succ_opt() {
            Some(n) => n,
            None => break,
        };
    }
    out
}

/// Per-ticker indexes directory, keyed by MITCH id: `<data>/indexes/<id>`.
pub fn idx_dir(data_root: &Path, ticker_id: u64) -> PathBuf {
    data_root.join("indexes").join(ticker_id.to_string())
}

/// Advisory lock-file path for a ticker's currently-open writer (R1 H9).
/// One per `idx_dir` — the live aggregator holds an exclusive flock here so
/// offline tools (`migrate_to_sharded`, `ticks_to_idx`) can detect contention
/// and skip the target ticker instead of producing torn writes.
pub(crate) fn writer_lock_path(ticker_dir: &Path) -> PathBuf {
    ticker_dir.join(".writer.lock")
}

/// Try to acquire an exclusive, non-blocking advisory lock on the per-ticker
/// writer-lock file. Returns the locked `File` handle (caller must hold it for
/// the writer's lifetime — `drop` releases the flock).
///
/// On lock contention, returns an error explicitly naming the path so callers
/// can surface "today's shard held by another writer (live aggregator likely)".
pub(crate) fn try_acquire_writer_lock(ticker_dir: &Path) -> Result<fs::File> {
    fs::create_dir_all(ticker_dir)
        .with_context(|| format!("create_dir_all {}", ticker_dir.display()))?;
    let lock_path = writer_lock_path(ticker_dir);
    let file = fs::OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(&lock_path)
        .with_context(|| format!("open writer-lock {}", lock_path.display()))?;
    file.try_lock_exclusive().map_err(|e| {
        anyhow::anyhow!(
            "writer-lock {} held by another writer (live aggregator likely): {}",
            lock_path.display(),
            e
        )
    })?;
    Ok(file)
}

/// Per-ticker bars directory, keyed by MITCH id: `<data>/bars/<id>`.
pub fn bars_dir(data_root: &Path, ticker_id: u64) -> PathBuf {
    data_root.join("bars").join(ticker_id.to_string())
}

/// Shard file path for `(ticker_dir, date, ext)` — ext in {`idx`,`s10`,`renko`}.
pub fn shard_path(ticker_dir: &Path, date: NaiveDate, ext: &str) -> PathBuf {
    ticker_dir.join(format!("{}.{}", date_stem(date), ext))
}

/// Per-ticker volatility (`.vol`) directory, keyed by MITCH id: `<data>/vol`.
/// Files are id-keyed (`<id>.vol`) so the LIVE renko producer can prime its
/// in-process Parkinson ring from the same EMA-smoothed σ bins the offline
/// backfill wrote — the seam-glue path. Mirrors `idx_dir` / `bars_dir`.
pub fn vol_dir(data_root: &Path) -> PathBuf {
    data_root.join("vol")
}

/// Id-keyed persistent `.vol` path: `<data>/vol/<id>.vol`. The offline
/// backfill writes this and the live producer primes from it.
pub fn vol_path_for_id(data_root: &Path, ticker_id: u64) -> PathBuf {
    vol_dir(data_root).join(format!("{}.vol", ticker_id))
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
        self.last_updated = crate::now_ms() as i64;
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
    /// R1 H9: advisory writer-lock. Kept open for the writer's lifetime;
    /// dropping the field releases the flock automatically. `None` only for
    /// the unlocked-open variant used by offline tools that legitimately do
    /// not contend with the live aggregator (e.g. read-back paths constructed
    /// just to inspect `last_bar`).
    _writer_lock: Option<fs::File>,
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
    /// ts_ms of the last record we actually wrote (sentinel or otherwise).
    /// Used by the sentinel rule: if the quote has not changed AND the gap
    /// since the last write reaches [`SENTINEL_INTERVAL_MS`], emit a sentinel
    /// row so a downstream gap-detection audit can distinguish "quiet market"
    /// from "producer outage". (R1 C2)
    last_written_ts: i64,
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
    ///
    /// Acquires an exclusive advisory flock on `<ticker_dir>/.writer.lock`
    /// (R1 H9). On contention, returns an error naming the path so the
    /// caller can decide to skip the ticker (offline tools) vs hard-fail
    /// (live aggregator — a second instance starting up means a deploy bug).
    pub fn open_with(data_root: &Path, ticker_id: u64, gate: bool, manifest: bool) -> Result<Self> {
        let dir = idx_dir(data_root, ticker_id);
        fs::create_dir_all(&dir)
            .with_context(|| format!("create_dir_all {}", dir.display()))?;
        let lock = try_acquire_writer_lock(&dir)?;
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
            last_written_ts: 0,
            _writer_lock: Some(lock),
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
                // Seed the sentinel-gap clock from the tail's ts so a same-
                // session restart does not emit a spurious immediate sentinel
                // (or hide an outage by resetting to 0).
                self.last_written_ts = last.shard_ts_ms();
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

    /// Append `rec` subject to the delta-gate. Returns `true` if a record
    /// (real OR sentinel) was written. The caller's broadcast / snapshot path
    /// is unaffected — only the *disk append* is gated.
    ///
    /// Sentinel rule (R1 C2): when the quote is unchanged but the gap since
    /// the last on-disk write reaches [`SENTINEL_INTERVAL_MS`], a copy of the
    /// record with [`FLAG_HEARTBEAT_SENTINEL`] set is written. This lets the
    /// gap-detection audit distinguish a *quiet* market (sentinels present)
    /// from a *producer outage* (no sentinels for > sentinel interval).
    ///
    /// Bounds-check (R1 H1): rejects ts > now+60s and ts < now-2y for any
    /// record whose `Index.flags` does NOT carry [`FLAG_HISTORICAL_BACKFILL`].
    /// Offline merge / migrate paths set that bit before calling `append`
    /// (single source of truth for "this row is from offline replay"); the
    /// live aggregator never sets it, so a bad clock or stale ws frame still
    /// fails loudly there.
    pub fn append(&mut self, rec: &IndexRecord) -> Result<bool> {
        // Default path: ask the OS for now_ms once. Hot callers (live
        // aggregator) should use `append_with_now` and supply a cycle-scoped
        // `now_ms` so they avoid one vDSO syscall per appended record.
        let now_ms = crate::now_ms() as i64;
        self.append_with_now(rec, now_ms)
    }

    /// Like [`append`] but takes a caller-supplied `now_ms` for the ts sanity
    /// bounds. Lets a tight loop (e.g. the aggregator cycle) capture the
    /// wall-clock once and amortise it across many `append` calls.
    pub fn append_with_now(&mut self, rec: &IndexRecord, now_ms: i64) -> Result<bool> {
        let ts = rec.shard_ts_ms();
        // ts sanity: a future ts mis-routes the shard date (creates a "future"
        // shard) and a >2y-old ts is almost certainly a bug for LIVE writes.
        // Historical-backfill records bypass the guard because legitimate
        // 5y backfills land timestamps far older than `now-2y`.
        let body_flags = rec.index.flags;
        let is_historical = body_flags & FLAG_HISTORICAL_BACKFILL != 0;
        if ts > now_ms + MS_PER_MIN {
            return Err(anyhow::anyhow!(
                "IdxShardWriter::append: future ts {} > now+60s ({})",
                ts,
                now_ms + MS_PER_MIN
            ));
        }
        if !is_historical && ts < now_ms - 730 * MS_PER_DAY {
            return Err(anyhow::anyhow!(
                "IdxShardWriter::append: ancient ts {} < now-2y ({}) without FLAG_HISTORICAL_BACKFILL",
                ts,
                now_ms - 730 * MS_PER_DAY
            ));
        }
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
            self.last_written_ts = ts;
            return Ok(true);
        }

        // Quote unchanged. If enough wall-clock has elapsed since the last
        // on-disk write, emit a liveness sentinel so the audit can tell quiet
        // markets apart from producer outages.
        if self.have_last && ts - self.last_written_ts >= SENTINEL_INTERVAL_MS {
            let mut sentinel = *rec;
            sentinel.index.flags |= FLAG_HEARTBEAT_SENTINEL;
            self.ensure_shard(date)?;
            self.log.as_mut().unwrap().append(&sentinel)?;
            // Do NOT update last_{bid,ask,...} — those track the last real
            // market state, and a sentinel by definition carries the same
            // state. Only the wall-clock cursor advances.
            self.last_written_ts = ts;
            return Ok(true);
        }

        // Truly redundant observation: dropped (no real quote change, and the
        // sentinel cadence has not yet elapsed).
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
        // R1 H8: respect manifest=false — bulk migration disables manifest
        // refresh per rotation for O(n²) fsync churn reasons; honouring that
        // ON DROP too means a cleanly-finishing migration does not pay the
        // O(n*shards) hash bill it explicitly opted out of.
        if let Some(date) = self.cur_date {
            // Drop the log to fdatasync, then optionally hash.
            self.log = None;
            if self.manifest {
                let _ = self.finalize_manifest(date);
            }
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
    /// R1 H9: per-(ticker, ext) writer-lock. The s10 + renko producers and the
    /// offline `*_from_idx` replays both target `bars/<id>/<date>.<ext>` — flock
    /// lets the offline tool detect "live producer holds this stream" and skip.
    _writer_lock: Option<fs::File>,
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
        // R1 H9: per-stream lock — `<bars_dir>/.writer.<ext>.lock`. Separate
        // from .idx lock and per-ext so s10 and renko producers can both run
        // concurrently on the same ticker.
        let lock_path = dir.join(format!(".writer.{}.lock", ext));
        let lock_file = fs::OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(&lock_path)
            .with_context(|| format!("open bar writer-lock {}", lock_path.display()))?;
        lock_file.try_lock_exclusive().map_err(|e| {
            anyhow::anyhow!(
                "bar writer-lock {} held by another writer (live producer likely): {}",
                lock_path.display(),
                e
            )
        })?;
        let mut w = Self {
            dir,
            ticker_id,
            ext,
            cur_date: None,
            log: None,
            manifest,
            _writer_lock: Some(lock_file),
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
        // R1 H8: same manifest=false honouring as IdxShardWriter::Drop.
        if let Some(date) = self.cur_date {
            self.log = None;
            if self.manifest {
                let _ = self.finalize_manifest(date);
            }
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

    /// UTC midnight of yesterday (in epoch-ms) — always within the writer's
    /// `[now-2y, now+60s]` accept window. Tests that exercise `append()` use
    /// this instead of a hard-coded ts so they don't bit-rot as the wall
    /// clock advances past the guard (R1 H1).
    fn t_recent_day_start() -> i64 {
        let now = crate::now_ms() as i64;
        let d = ts_ms_to_utc_date(now - MS_PER_DAY);
        d.and_hms_opt(0, 0, 0)
            .unwrap()
            .and_utc()
            .timestamp_millis()
    }

    #[test]
    fn gate_keeps_volume_and_ci_changes_at_same_price() {
        // Microstructure guarantee: a tick with unchanged bid/ask but changed
        // volume or ci must NOT be dropped — bars (vbid/vask/OFI/avg_ci) are
        // rebuilt from the .idx, so dropping these would corrupt them.
        let root = std::env::temp_dir().join("nxr_shard_microstructure_test");
        let _ = fs::remove_dir_all(&root);
        let t0 = t_recent_day_start();
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
        let t0 = t_recent_day_start();
        {
            let mut w = IdxShardWriter::open(&root, 700, true).unwrap();
            assert!(w.append(&rec(t0, 100.0, 101.0)).unwrap()); // first: written
            assert!(!w.append(&rec(t0 + 100, 100.0, 101.0)).unwrap()); // same: gated (sentinel not yet due)
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
        // Use yesterday + day-before-yesterday so both ts pass the H1 guard.
        let day1 = t_recent_day_start() - MS_PER_DAY;
        let day2 = day1 + 86_400_000;
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
        let t0 = t_recent_day_start();
        {
            let mut w = IdxShardWriter::open(&root, 702, true).unwrap();
            w.append(&rec(t0, 100.0, 101.0)).unwrap();
            w.flush().unwrap();
        }
        {
            // Restart: same quote should be gated (seeded from tail), not re-written.
            // +100ms keeps us well under SENTINEL_INTERVAL_MS so no sentinel fires.
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
    fn sentinel_emitted_after_interval_when_quote_unchanged() {
        // R1 C2: a quiet market (no quote change) must still emit a sentinel
        // row every SENTINEL_INTERVAL_MS so the audit can distinguish quiet
        // from outage. Caller's record is NOT mutated — a copy is written
        // with FLAG_HEARTBEAT_SENTINEL set.
        let root = std::env::temp_dir().join("nxr_shard_sentinel_test");
        let _ = fs::remove_dir_all(&root);
        let t0 = t_recent_day_start();
        {
            let mut w = IdxShardWriter::open(&root, 704, true).unwrap();
            assert!(w.append(&rec(t0, 100.0, 101.0)).unwrap()); // first: written
            // Half-interval later, unchanged quote → still gated, no sentinel.
            assert!(!w
                .append(&rec(t0 + SENTINEL_INTERVAL_MS / 2, 100.0, 101.0))
                .unwrap());
            // Full-interval later, unchanged quote → sentinel emitted.
            assert!(w
                .append(&rec(t0 + SENTINEL_INTERVAL_MS, 100.0, 101.0))
                .unwrap());
            w.flush().unwrap();
        }
        let dir = idx_dir(&root, 704);
        let recs =
            read_shard_aligned::<IndexRecord>(&list_shards(&dir, "idx").unwrap()[0].1).unwrap();
        assert_eq!(recs.len(), 2);
        // First record: real, no flag.
        let f0 = recs[0].index.flags;
        assert_eq!(f0 & FLAG_HEARTBEAT_SENTINEL, 0);
        // Second record: sentinel.
        let f1 = recs[1].index.flags;
        assert_eq!(f1 & FLAG_HEARTBEAT_SENTINEL, FLAG_HEARTBEAT_SENTINEL);
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn future_ts_rejected() {
        // R1 H1: future ts > now+60s is a hard error (mis-routes shard date).
        let root = std::env::temp_dir().join("nxr_shard_future_ts_test");
        let _ = fs::remove_dir_all(&root);
        let mut w = IdxShardWriter::open(&root, 705, true).unwrap();
        let future = crate::now_ms() as i64 + 5 * MS_PER_MIN; // +5min
        let r = rec(future, 100.0, 101.0);
        assert!(w.append(&r).is_err());
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
        let t0 = t_recent_day_start();
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
