//! Zero-copy mmap binary bar format for instant data loading.
//! Each record is a 96-byte mitch::Bar (#[repr(C, packed)], Pod+Zeroable).
//! File is simply N * 96 bytes with no header - record count = file_size / 96.

use anyhow::{ensure, Result};
use memmap2::Mmap;
use std::path::Path;

use mitch::Bar;

const RECORD_SIZE: usize = std::mem::size_of::<Bar>();
const _: () = assert!(RECORD_SIZE == 96);

/// Memory-mapped bar file. Zero-copy access to records.
pub struct BarFile {
    _mmap: Mmap,
    records: *const Bar,
    len: usize,
}

// Safety: BarFile is read-only and Bar is Pod (plain old data)
unsafe impl Send for BarFile {}
unsafe impl Sync for BarFile {}

impl BarFile {
    /// Open a .bars file via mmap. Returns zero-copy slice of records.
    pub fn open(path: &Path) -> Result<Self> {
        let file = std::fs::File::open(path)?;
        let metadata = file.metadata()?;
        let file_size = metadata.len() as usize;

        ensure!(file_size > 0, "Empty file");
        ensure!(
            file_size.is_multiple_of(RECORD_SIZE),
            "File size {} not a multiple of record size {}",
            file_size,
            RECORD_SIZE
        );

        let mmap = unsafe { Mmap::map(&file)? };

        // Hint OS for sequential access
        #[cfg(unix)]
        mmap.advise(memmap2::Advice::Sequential).ok();

        let len = file_size / RECORD_SIZE;
        let records = mmap.as_ptr() as *const Bar;

        Ok(Self {
            _mmap: mmap,
            records,
            len,
        })
    }

    /// Number of records.
    #[inline]
    pub fn len(&self) -> usize {
        self.len
    }

    /// True if zero records.
    #[inline]
    pub fn is_empty(&self) -> bool {
        self.len == 0
    }

    /// Get record slice (zero-copy).
    #[inline]
    pub fn records(&self) -> &[Bar] {
        unsafe { std::slice::from_raw_parts(self.records, self.len) }
    }
}
