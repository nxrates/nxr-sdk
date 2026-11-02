//! Generic 1 Hz mtime-poll file watcher, shared by every process that reloads
//! an on-disk config surface without a restart.
//!
//! Mechanism: 1 Hz mtime poll, not the `notify` crate. Saves ~3s build + 1.5MB
//! binary + the entire `notify` transitive dep tree vs an fsevents-based
//! watcher, and every watched file mutates at most once/hour (nxr-weights and
//! nxr-calibrate crons; a configmap edit), so a 1 s detection lag is irrelevant.
//!
//! A changed mtime with identical bytes is NOT a reload (content hash): the
//! nightly calibrate rewrote a 3.9 MB ticker-params.json hundreds of times with
//! mostly unchanged consumer fields, and every rewrite re-parsed the file in
//! every signer (scratch/hva/dark-rca-2026-11-02.md). Read + hash + parse run on
//! the blocking pool, never on a runtime worker.
//!
//! Failure mode is uniform: a missing or malformed file leaves the previous
//! `Arc` in place and emits a `warn!` from the caller's `parse`. A live process
//! never degrades because someone saved a broken file.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, SystemTime};

use tokio::sync::watch;
use tracing::{info, warn};

/// Read mtime of `path`. `None` if missing.
pub fn mtime(path: &Path) -> Option<SystemTime> {
    std::fs::metadata(path).ok().and_then(|m| m.modified().ok())
}

/// Spawn a 1 Hz mtime-poll watcher over `path`.
///
/// `parse` returns `None` for a missing/malformed file, in which case the
/// previous value is retained. `initial` is published when the first parse at
/// boot fails, so a consumer never blocks waiting for a valid file.
///
/// Returns the receiver half of a `watch` channel holding the current value.
/// Clone it to as many consumers as needed.
pub fn watch_file<T, F>(
    path: PathBuf,
    label: &'static str,
    parse: F,
    initial: Arc<T>,
) -> watch::Receiver<Arc<T>>
where
    T: Send + Sync + 'static,
    F: Fn(&Path) -> Option<Arc<T>> + Send + Sync + 'static,
{
    // mtime + hash BEFORE the boot parse: a write racing it is then seen as a change, never skipped.
    let mut last = mtime(&path);
    let mut last_hash = content_hash(&path);
    let first = parse(&path).unwrap_or(initial);
    info!(path = %path.display(), watcher = label, "hot_reload initial load (1Hz mtime poll)");
    let (tx, rx) = watch::channel(first);

    let parse = Arc::new(parse);
    tokio::spawn(async move {
        let mut tick = tokio::time::interval(Duration::from_secs(1));
        tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        loop {
            tick.tick().await;
            let cur = mtime(&path);
            if cur == last {
                continue;
            }
            last = cur;
            let (p, f, prev) = (path.clone(), Arc::clone(&parse), last_hash);
            let Ok((hash, v)) = tokio::task::spawn_blocking(move || {
                let h = content_hash(&p);
                (h, if h.is_some() && h == prev { None } else { f(&p) })
            })
            .await
            else {
                warn!(path = %path.display(), watcher = label, "hot_reload parse panicked; keeping the previous value");
                continue;
            };
            if hash.is_some() && hash == last_hash {
                continue; // touched, bytes unchanged
            }
            if let Some(v) = v {
                last_hash = hash;
                info!(path = %path.display(), watcher = label, "hot_reload reload");
                let _ = tx.send(v);
            }
        }
    });
    rx
}

/// Hash of the file's bytes; `None` if unreadable.
fn content_hash(path: &Path) -> Option<u64> {
    use std::hash::{Hash, Hasher};
    let bytes = std::fs::read(path).ok()?;
    let mut h = std::collections::hash_map::DefaultHasher::new();
    bytes.hash(&mut h);
    Some(h.finish())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};

    #[tokio::test(flavor = "multi_thread", worker_threads = 1)]
    async fn rewrite_with_identical_bytes_is_not_a_reload() {
        let dir = std::env::temp_dir().join(format!("hot-reload-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let p = dir.join("f.json");
        std::fs::write(&p, "a").unwrap();
        let parses = Arc::new(AtomicUsize::new(0));
        let n = Arc::clone(&parses);
        let mut rx = watch_file(p.clone(), "t", move |p| {
            n.fetch_add(1, Ordering::SeqCst);
            Some(Arc::new(std::fs::read_to_string(p).ok()?))
        }, Arc::new(String::new()));
        // same bytes, new mtime: no parse, no publish
        tokio::time::sleep(Duration::from_millis(1100)).await;
        std::fs::write(&p, "a").unwrap();
        filetime_bump(&p, 10);
        tokio::time::sleep(Duration::from_millis(2200)).await;
        assert_eq!(parses.load(Ordering::SeqCst), 1, "only the boot parse");
        assert!(!rx.has_changed().unwrap());
        // new bytes: parsed and published
        std::fs::write(&p, "b").unwrap();
        filetime_bump(&p, 20);
        tokio::time::timeout(Duration::from_secs(5), rx.changed()).await.unwrap().unwrap();
        assert_eq!(rx.borrow().as_str(), "b");
    }

    fn filetime_bump(p: &Path, secs: u64) {
        let t = SystemTime::now() + Duration::from_secs(secs);
        std::fs::File::options().write(true).open(p).unwrap().set_modified(t).unwrap();
    }
}
