//! Shared schema for `ticker-params.json` - the file produced by `nxr-weights`
//! and consumed by the aggregator's weights loader. Both sides derive from the
//! same struct so drift is impossible at compile time.

use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

/// Advisory **cross-process** lock on `ticker-params.json`.
///
/// The file has two independent writers in two separate processes:
/// `nxr-weights` (hourly, owns `generated_at`/`pair_volumes`/`exchanges`) and
/// `nxr-calibrate` (owns `renko_k*`/`calibration_status`/`last_run`). Both do a
/// read-modify-atomic-rename; the atomic rename only rules out torn reads, NOT
/// lost updates — whichever process read first silently reverts the other's
/// fields. An in-process mutex cannot help: the racing writers are different
/// processes. This is an `flock` on a `<path>.lock` sidecar, so it works across
/// processes (and containers sharing the volume).
///
/// Hold it around the short read-modify-rename ONLY, never across a fit.
/// Acquisition failure is an error, never a skipped write.
pub struct ParamsLock(std::fs::File);

impl ParamsLock {
    /// Blocking-with-timeout exclusive lock on `<path>.lock`.
    ///
    /// `flock` is per open-file-description, so a process that already holds this
    /// guard would block itself on a second `acquire`; callers must serialize
    /// their own threads first (see `nxr-calibrate`'s `ParamsStore`) and must not
    /// nest acquisitions. Times out rather than hanging a cron forever.
    pub fn acquire(path: &std::path::Path) -> std::io::Result<Self> {
        use fs2::FileExt;
        const TIMEOUT: std::time::Duration = std::time::Duration::from_secs(30);
        let mut lock_path = path.as_os_str().to_owned();
        lock_path.push(".lock");
        let lock_path = std::path::PathBuf::from(lock_path);
        if let Some(parent) = lock_path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let f = std::fs::OpenOptions::new()
            .create(true)
            .read(true)
            .write(true)
            .truncate(false)
            .open(&lock_path)?;
        let start = std::time::Instant::now();
        loop {
            match f.try_lock_exclusive() {
                Ok(()) => return Ok(Self(f)),
                Err(e) if start.elapsed() >= TIMEOUT => {
                    return Err(std::io::Error::new(
                        std::io::ErrorKind::WouldBlock,
                        format!("lock {} after {:?}: {}", lock_path.display(), TIMEOUT, e),
                    ))
                }
                Err(_) => std::thread::sleep(std::time::Duration::from_millis(50)),
            }
        }
    }
}

impl Drop for ParamsLock {
    fn drop(&mut self) {
        // Released on close anyway; explicit so the window is exactly the guard's.
        let _ = self.0.unlock();
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct WeightsFile {
    #[serde(default)]
    pub generated_at: String,
    /// cmc_slug -> { "BTC/USDT": volume_usd, ... }
    #[serde(default)]
    pub pair_volumes: BTreeMap<String, BTreeMap<String, f64>>,
    /// cmc_slug -> { mitch_id, name, weight }
    #[serde(default)]
    pub exchanges: BTreeMap<String, ExchangeMeta>,
    /// Per-CR-asset market ranking derived from `pair_volumes`. FACTS only: the
    /// conversion belongs to core, which has the bridges and the liveness view.
    #[serde(default)]
    pub asset_markets: BTreeMap<String, Vec<AssetMarket>>,
    /// Calibrated Renko `multiplier` per ticker (output of `nxr-calibrate`).
    /// Key = ticker_id as a decimal string (JSON object keys must be strings).
    /// Consumed by the aggregator's renko bar emitter at hot-reload time.
    /// Missing entries fall back to the per-ticker prior or the `config.yml`
    /// default multiplier — never panic.
    #[serde(default)]
    pub renko_k_per_ticker: BTreeMap<String, f64>,
    /// Unix-seconds timestamp of the last successful `nxr-calibrate` run.
    /// Optional so legacy weights files (pre-calibration era) still parse.
    #[serde(default)]
    pub calibrated_at: Option<u64>,
    /// IN-PROGRESS calibration results for the run identified by `staged_run_id`.
    /// Written incrementally, one entry per ticker as it finishes, so a run killed
    /// by `activeDeadlineSeconds` keeps its partial work and the next run resumes
    /// instead of restarting from zero.
    ///
    /// NEVER read by the renko engine. Promoted into `renko_k_per_ticker` as a
    /// WHOLE-SET REPLACE only when a pass completes — because the live map's
    /// contract is "only this run's Ok outcomes"; merging staged entries into it
    /// per-ticker would leave unreached/failed tickers on prior-run k, which is
    /// exactly the stale-k corruption the `no k fallback` policy forbids (see
    /// `nxr_calibrate.rs`, renko_k cohort 2026-06-01: 91% of base tickers were
    /// running on prior-run k).
    #[serde(default)]
    pub renko_k_staged: BTreeMap<String, f64>,
    /// Run that owns `renko_k_staged`. A run whose id differs discards the staged
    /// map rather than resuming into it (roster or window may have changed).
    #[serde(default)]
    pub staged_run_id: Option<String>,
    /// Per-ticker calibration health. Diagnostics only — never an input to the
    /// renko engine, so unlike `renko_k_*` these are safe to write incrementally.
    /// `consecutive_failures >= 3` is the hard-alert condition: 56 of 184 tickers
    /// silently carrying no k is the failure class this exists to make loud.
    #[serde(default)]
    pub calibration_status: BTreeMap<String, TickerCalStatus>,
    /// Outcome of the most recent `nxr-calibrate` invocation. Survives the
    /// `ttlSecondsAfterFinished` Job reaper, which is why a job that had not
    /// completed for cycles was invisible.
    #[serde(default)]
    pub last_run: Option<CalibrateRun>,
}

/// Per-ticker calibration health (unix seconds).
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct TickerCalStatus {
    #[serde(default)]
    pub last_attempt: Option<u64>,
    #[serde(default)]
    pub last_success: Option<u64>,
    /// Consecutive FAILED outcomes. Reset to 0 on success. Deliberately excludes
    /// `Skipped`: a pegged stable that cannot reach its target bars/day even at
    /// K_FLOOR is skipped *structurally* and forever, so counting skips here would
    /// leave the `>= 3` alert permanently red on assets for which renko is simply
    /// not offered — alert fatigue instead of signal. Skips are recorded in
    /// `last_skip_reason` and surfaced as coverage, not as failure.
    #[serde(default)]
    pub consecutive_failures: u32,
    /// Reason string from the last FAILED outcome. `None` after a success.
    #[serde(default)]
    pub last_error: Option<String>,
    /// Reason from the last `Skipped` outcome (structural: no shards, window too
    /// short, target unreachable at K_FLOOR). Not an error.
    #[serde(default)]
    pub last_skip_reason: Option<String>,
}

/// One `nxr-calibrate` invocation's outcome.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct CalibrateRun {
    pub run_id: String,
    pub started_at: u64,
    /// `None` while running — a `None` here on a stamp older than one cycle is
    /// itself the signal that the run was killed before it could finish.
    #[serde(default)]
    pub finished_at: Option<u64>,
    #[serde(default)]
    pub tickers_total: usize,
    #[serde(default)]
    pub tickers_attempted: usize,
    #[serde(default)]
    pub tickers_succeeded: usize,
    /// `completed` | `partial` (killed/aborted). Drives
    /// `nxr_calibrate_coverage_ratio` and the promote decision.
    #[serde(default)]
    pub exit_reason: String,
    /// Trailing window actually used (`--window-days` override or the YAML
    /// `calibration.rolling_window_days`). Recorded because k is only
    /// comparable across runs at equal window.
    #[serde(default)]
    pub window_days: Option<u32>,
}

/// One market an asset trades in, ranked by 24h USD volume.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct AssetMarket {
    /// cmc_slug: joins the `exchanges` map to a mitch_id.
    pub exchange: String,
    pub pair: String,
    /// The OTHER asset of `pair`.
    pub counter: String,
    /// True when the asset is the QUOTE side of `pair`.
    pub inverted: bool,
    pub volume_usd: f64,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ExchangeMeta {
    pub mitch_id: u16,
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub weight: f64,
}
