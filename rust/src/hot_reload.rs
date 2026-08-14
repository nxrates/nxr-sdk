//! Generic 1 Hz mtime-poll file watcher, shared by every process that reloads
//! an on-disk config surface without a restart.
//!
//! Mechanism: 1 Hz mtime poll, not the `notify` crate. Saves ~3s build + 1.5MB
//! binary + the entire `notify` transitive dep tree vs an fsevents-based
//! watcher, and every watched file mutates at most once/hour (nxr-weights and
//! nxr-calibrate crons; a configmap edit), so a 1 s detection lag is irrelevant.
//!
//! Failure mode is uniform: a missing or malformed file leaves the previous
//! `Arc` in place and emits a `warn!` from the caller's `parse`. A live process
//! never degrades because someone saved a broken file.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, SystemTime};

use tokio::sync::watch;
use tracing::info;

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
    F: Fn(&Path) -> Option<Arc<T>> + Send + 'static,
{
    let first = parse(&path).unwrap_or(initial);
    info!(path = %path.display(), watcher = label, "hot_reload initial load (1Hz mtime poll)");
    let (tx, rx) = watch::channel(first);

    let mut last = mtime(&path);
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
            if let Some(v) = parse(&path) {
                info!(path = %path.display(), watcher = label, "hot_reload reload");
                let _ = tx.send(v);
            }
        }
    });
    rx
}
