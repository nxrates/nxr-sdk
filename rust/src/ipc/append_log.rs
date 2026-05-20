/// Append-only binary log backed by a regular file.
///
/// Single writer, multiple readers. Writer calls `append()`, readers open the
/// file independently and read `N = file_len / size_of::<T>()` records. Since
/// records are fixed-size and writes are atomic at the OS level for sizes <= 4096
/// bytes (Linux ext4/xfs), no locking is needed between writer and readers on
/// the same node.
///
/// # Durability
///
/// Writes go through `write_all` into the OS page cache, which is visible to
/// other processes on the same node but can be lost on a power failure or
/// kernel panic. To bound that loss window, `AppendLog` calls
/// [`File::sync_data`] (fdatasync) periodically during `append()` based on a
/// wall-clock interval (default [`DEFAULT_SYNC_INTERVAL`]), and once more from
/// `Drop` so graceful shutdowns are fully durable. Callers that need a barrier
/// between specific appends can call [`AppendLog::flush`] explicitly.
///
/// # Usage
/// ```rust,ignore
/// use nxr_sdk::{AppendLog, IndexRecord};
///
/// // Aggregator persists 56B self-dating rows (16B MitchHeader + 40B Index body)
/// // so every row carries its observation mts.
/// let mut log: AppendLog<IndexRecord> = AppendLog::open("/data/indexes/12345.idx").unwrap();
/// log.append(&record).unwrap();
/// ```
use std::fs::{File, OpenOptions};
use std::io::Write;
use std::marker::PhantomData;
use std::path::Path;
use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use bytemuck::Pod;
use tracing::warn;

/// Default `sync_data()` cadence. Bounds worst-case data loss on power failure
/// to ~5s per writer, while keeping per-append cost near zero (one
/// `Instant::now()` compare on the hot path).
pub const DEFAULT_SYNC_INTERVAL: Duration = Duration::from_secs(5);

pub struct AppendLog<T: Pod> {
    file: File,
    path: String,
    last_sync: Instant,
    sync_interval: Duration,
    _marker: PhantomData<T>,
}

impl<T: Pod> AppendLog<T> {
    /// Open or create the log file at `path` with the default sync interval.
    pub fn open(path: impl AsRef<Path>) -> Result<Self> {
        Self::open_with_sync_interval(path, DEFAULT_SYNC_INTERVAL)
    }

    /// Open or create the log file at `path` and set the periodic fdatasync
    /// cadence. Pass `Duration::ZERO` to fsync after every append (durable but
    /// slow), or `Duration::MAX` to disable periodic sync and rely solely on
    /// Drop / explicit `flush()`.
    pub fn open_with_sync_interval(
        path: impl AsRef<Path>,
        sync_interval: Duration,
    ) -> Result<Self> {
        let path = path.as_ref();
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)
                .with_context(|| format!("create_dir_all {:?}", parent))?;
        }
        let file = OpenOptions::new()
            .create(true)
            .append(true)
            .open(path)
            .with_context(|| format!("open {:?}", path))?;
        Ok(Self {
            file,
            path: path.to_string_lossy().into_owned(),
            last_sync: Instant::now(),
            sync_interval,
            _marker: PhantomData,
        })
    }

    /// Append one record. The write is atomic for `size_of::<T>() <= page_size`.
    ///
    /// Triggers an `fdatasync` if more than `sync_interval` has elapsed since
    /// the last sync. Sync failures are logged but do not propagate: the write
    /// itself succeeded, so the caller's record is in the page cache and
    /// readable by other processes; durability will be retried on the next
    /// periodic tick or at Drop.
    #[inline]
    pub fn append(&mut self, record: &T) -> Result<()> {
        self.file
            .write_all(bytemuck::bytes_of(record))
            .with_context(|| format!("append to {}", self.path))?;

        if self.last_sync.elapsed() >= self.sync_interval {
            if let Err(e) = self.file.sync_data() {
                warn!(path = %self.path, err = %e, "AppendLog periodic fdatasync failed");
            }
            self.last_sync = Instant::now();
        }
        Ok(())
    }

    /// Force an `fdatasync` now. Use before a process-level checkpoint or when
    /// a specific record must be durable before returning to the caller.
    pub fn flush(&mut self) -> Result<()> {
        self.file
            .sync_data()
            .with_context(|| format!("sync_data {}", self.path))?;
        self.last_sync = Instant::now();
        Ok(())
    }
}

impl<T: Pod> Drop for AppendLog<T> {
    /// Best-effort `fdatasync` on drop so graceful shutdowns do not lose
    /// buffered writes. Errors are swallowed (we can't return from Drop) but
    /// logged so they show up in process teardown traces.
    fn drop(&mut self) {
        if let Err(e) = self.file.sync_data() {
            warn!(path = %self.path, err = %e, "AppendLog drop fdatasync failed");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use bytemuck::{Pod, Zeroable};

    #[derive(Copy, Clone, Pod, Zeroable)]
    #[repr(C)]
    struct Row {
        v: u64,
    }

    #[test]
    fn append_and_reopen_sees_all_rows() {
        let tmp = std::env::temp_dir().join("nxr_sdk_append_log_basic.bin");
        let _ = std::fs::remove_file(&tmp);
        {
            let mut log: AppendLog<Row> = AppendLog::open(&tmp).unwrap();
            for i in 0..10u64 {
                log.append(&Row { v: i }).unwrap();
            }
        }
        let bytes = std::fs::read(&tmp).unwrap();
        assert_eq!(bytes.len(), 10 * std::mem::size_of::<Row>());
        std::fs::remove_file(&tmp).ok();
    }

    #[test]
    fn flush_triggers_sync() {
        let tmp = std::env::temp_dir().join("nxr_sdk_append_log_flush.bin");
        let _ = std::fs::remove_file(&tmp);
        let mut log: AppendLog<Row> =
            AppendLog::open_with_sync_interval(&tmp, Duration::MAX).unwrap();
        log.append(&Row { v: 7 }).unwrap();
        log.flush().unwrap();
        std::fs::remove_file(&tmp).ok();
    }

    #[test]
    fn zero_interval_syncs_each_append() {
        let tmp = std::env::temp_dir().join("nxr_sdk_append_log_zero.bin");
        let _ = std::fs::remove_file(&tmp);
        let mut log: AppendLog<Row> =
            AppendLog::open_with_sync_interval(&tmp, Duration::ZERO).unwrap();
        for i in 0..3u64 {
            log.append(&Row { v: i }).unwrap();
        }
        std::fs::remove_file(&tmp).ok();
    }
}
