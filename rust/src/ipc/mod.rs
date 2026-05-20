pub mod append_log;
pub mod record;

use std::io::Write;
use std::path::Path;

use anyhow::{Context, Result};
use bytemuck::Pod;

/// Atomically replace the file at `path` with `records` serialized as a
/// packed binary slice. Writes to `<path>.tmp` then `rename(2)`s into place.
///
/// POSIX guarantees that readers holding an open FD to the old file continue
/// reading the old inode; new readers see the new file. This prevents the
/// tail-a-batch-file corruption pattern where `File::create` would truncate
/// the same inode under an active reader's cursor.
///
/// Use this for **batch** writers (series-factory `.bars` output, etc.). For
/// incremental append, use [`append_log::AppendLog`] instead.
pub fn write_atomic<T: Pod>(path: impl AsRef<Path>, records: &[T]) -> Result<()> {
    let path = path.as_ref();
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("create_dir_all {:?}", parent))?;
    }
    let tmp = {
        let ext = path.extension().and_then(|s| s.to_str()).unwrap_or("");
        if ext.is_empty() {
            path.with_extension("tmp")
        } else {
            path.with_extension(format!("{ext}.tmp"))
        }
    };
    {
        let mut f = std::fs::File::create(&tmp)
            .with_context(|| format!("create {:?}", tmp))?;
        f.write_all(bytemuck::cast_slice(records))
            .with_context(|| format!("write {:?}", tmp))?;
        // Best-effort durability before rename; non-fatal on filesystems that
        // don't support it (tmpfs, etc.).
        let _ = f.sync_data();
    }
    std::fs::rename(&tmp, path)
        .with_context(|| format!("rename {:?} -> {:?}", tmp, path))?;
    Ok(())
}
