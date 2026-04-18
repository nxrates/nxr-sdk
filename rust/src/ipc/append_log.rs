/// Append-only binary log backed by a regular file.
///
/// Single writer, multiple readers. Writer calls `append()`, readers open the
/// file independently and read `N = file_len / size_of::<T>()` records. Since
/// records are fixed-size and writes are atomic at the OS level for sizes <= 4096
/// bytes (Linux ext4/xfs), no locking is needed between writer and readers on
/// the same node.
///
/// # Usage
/// ```rust,ignore
/// use nxr_sdk::{AppendLog, IndexRecord};
///
/// // Aggregator persists 56B self-dating rows (16B MitchHeader + 40B Index body)
/// // so every row carries its observation mts.
/// let mut log: AppendLog<IndexRecord> = AppendLog::open("/data/index/12345.idx").unwrap();
/// log.append(&record).unwrap();
/// ```
use std::fs::{File, OpenOptions};
use std::io::Write;
use std::marker::PhantomData;
use std::path::Path;

use anyhow::{Context, Result};
use bytemuck::Pod;

pub struct AppendLog<T: Pod> {
    file: File,
    path: String,
    _marker: PhantomData<T>,
}

impl<T: Pod> AppendLog<T> {
    /// Open or create the log file at `path`.
    pub fn open(path: impl AsRef<Path>) -> Result<Self> {
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
            _marker: PhantomData,
        })
    }

    /// Append one record. The write is atomic for `size_of::<T>() <= page_size`.
    #[inline]
    pub fn append(&mut self, record: &T) -> Result<()> {
        self.file
            .write_all(bytemuck::bytes_of(record))
            .with_context(|| format!("append to {}", self.path))
    }

    /// Append a slice of records in a single write call.
    #[inline]
    pub fn append_all(&mut self, records: &[T]) -> Result<()> {
        if records.is_empty() {
            return Ok(());
        }
        self.file
            .write_all(bytemuck::cast_slice(records))
            .with_context(|| format!("append_all to {}", self.path))
    }

    /// Current number of records written (approximated from file metadata).
    pub fn len(&self) -> Result<u64> {
        let meta = self.file.metadata()?;
        Ok(meta.len() / std::mem::size_of::<T>() as u64)
    }

    /// Returns true if the log has no records.
    pub fn is_empty(&self) -> Result<bool> {
        Ok(self.len()? == 0)
    }

    pub fn path(&self) -> &str {
        &self.path
    }
}

/// Read-only view: open a log file and return all records as an owned `Vec<T>`.
/// Efficient for small-to-medium logs; for very large logs, mmap the file directly.
pub fn read_all<T: Pod>(path: impl AsRef<Path>) -> Result<Vec<T>> {
    let bytes = std::fs::read(path.as_ref())
        .with_context(|| format!("read {:?}", path.as_ref()))?;
    let record_size = std::mem::size_of::<T>();
    if bytes.len() % record_size != 0 {
        anyhow::bail!(
            "file {:?} size {} is not a multiple of record size {}",
            path.as_ref(),
            bytes.len(),
            record_size,
        );
    }
    // SAFETY: T is Pod (bytemuck), alignment is handled by cast_slice
    Ok(bytemuck::cast_slice::<u8, T>(&bytes).to_vec())
}
