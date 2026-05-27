//! Process-wide memory cap, shared by every binary that streams the
//! multi-exchange tick cache (series-factory, fetch-crypto-history,
//! generate-renko-from-ticks, optimize-renko-stats).
//!
//! `apply_safe_cap()` installs a hard resource limit AND a background RSS
//! watchdog so an allocation runaway is either translated into a clean
//! allocation failure or aborted by us before the kernel OOM-killer fires
//! against the user's foreground apps. Call it at the very top of `main()`
//! — before any large buffer is allocated.
//!
//! Why both:
//!   * `setrlimit(RLIMIT_DATA|RLIMIT_AS)` is ignored on stock macOS (returns
//!     EINVAL for any finite value), so we cannot rely on it there.
//!   * The watchdog polls our own RSS via `ps` / `/proc/self/status` every
//!     2 s and calls `std::process::abort()` if we cross 95% of the cap.
//!     On Linux the setrlimit actually sticks and usually traps the
//!     runaway first; the watchdog is belt-and-braces.
//!
//! Cap precedence:
//!   1. `NXR_MAX_MEM_GB` env var, if set and parseable.
//!   2. `min(60% physical RAM, 75% currently-available RAM)`. The available
//!      term is critical: on a 16 GiB host with only 3 GiB free, a 10 GiB
//!      cap would still let us push into swap and trigger the OOM-killer.
//!      Clamping to the current free pool keeps us within what the kernel
//!      will hand us without reclaiming the user's desktop.
//!
//! The limit applied is `RLIMIT_DATA` on macOS (RLIMIT_AS silently fails on
//! many macOS builds) and `RLIMIT_AS` on Linux (caps the whole address
//! space, catching mmap + malloc alike). setrlimit failures are logged with
//! the OS errno so an operator can see why the cap did not stick.

use tracing::{error, info, warn};

/// Fraction of physical RAM used when the available probe fails. 0.60 on a
/// 16 GiB host gives a 9.6 GiB cap, which reserves ~6.4 GiB for the desktop
/// / IDE / browser.
const DEFAULT_PHYSICAL_FRACTION: f64 = 0.60;

/// Fraction of currently-available RAM we will claim. The rest stays for
/// the user's foreground apps and the kernel's file cache. 0.75 of a 3 GiB
/// free pool = 2.25 GiB cap (much tighter than the 60% physical default
/// would give, and correctly reflective of actual pressure).
const DEFAULT_AVAILABLE_FRACTION: f64 = 0.75;

/// Absolute floor on the cap. Small enough that even a 4 GiB laptop with
/// little free RAM can still run the binaries for a smoke test; below this
/// the process would fail trivially on startup anyway.
const MIN_CAP_BYTES: u64 = 2 * 1024 * 1024 * 1024;

/// Fallback cap when RAM probing fails entirely (very unlikely).
const FALLBACK_CAP_BYTES: u64 = 4 * 1024 * 1024 * 1024;

const GIB: u64 = 1024 * 1024 * 1024;

/// Total physical RAM in bytes, or `None` if the platform is unsupported or
/// the syscall fails.
pub(crate) fn physical_memory_bytes() -> Option<u64> {
    #[cfg(target_os = "macos")]
    unsafe {
        let mut size: u64 = 0;
        let mut out_len = core::mem::size_of::<u64>();
        let name = b"hw.memsize\0";
        let rc = libc::sysctlbyname(
            name.as_ptr() as *const libc::c_char,
            &mut size as *mut u64 as *mut libc::c_void,
            &mut out_len,
            core::ptr::null_mut(),
            0,
        );
        if rc == 0 { Some(size) } else { None }
    }
    #[cfg(target_os = "linux")]
    unsafe {
        let mut si: libc::sysinfo = core::mem::zeroed();
        if libc::sysinfo(&mut si) == 0 {
            Some(si.totalram as u64 * si.mem_unit as u64)
        } else {
            None
        }
    }
    #[cfg(not(any(target_os = "macos", target_os = "linux")))]
    {
        None
    }
}

/// Currently-available RAM in bytes (free + reclaimable), or `None` if the
/// platform is unsupported or the probe fails. "Available" here mirrors
/// `vm_stat` free+inactive+speculative on macOS and `/proc/meminfo`
/// `MemAvailable` on Linux — both intended to reflect what a new process
/// can allocate without reclaim pressure.
pub(crate) fn available_memory_bytes() -> Option<u64> {
    #[cfg(target_os = "macos")]
    {
        // Shell out to vm_stat once at startup. The mach host_statistics64
        // API would work too, but its bindings are not in the libc crate
        // and vm_stat is always present on macOS. Parsing cost is ms-level.
        let out = std::process::Command::new("vm_stat").output().ok()?;
        let text = String::from_utf8_lossy(&out.stdout);
        let mut page_size: u64 = 16384;
        let mut free: u64 = 0;
        let mut inactive: u64 = 0;
        let mut speculative: u64 = 0;
        for line in text.lines() {
            if let Some(rest) = line.strip_prefix("Mach Virtual Memory Statistics: (page size of ") {
                if let Some(num) = rest.split_whitespace().next() {
                    if let Ok(n) = num.parse::<u64>() {
                        page_size = n;
                    }
                }
            } else if let Some(rest) = line.strip_prefix("Pages free:") {
                free = parse_pages(rest);
            } else if let Some(rest) = line.strip_prefix("Pages inactive:") {
                inactive = parse_pages(rest);
            } else if let Some(rest) = line.strip_prefix("Pages speculative:") {
                speculative = parse_pages(rest);
            }
        }
        let pages = free.saturating_add(inactive).saturating_add(speculative);
        if pages == 0 { None } else { Some(pages.saturating_mul(page_size)) }
    }
    #[cfg(target_os = "linux")]
    {
        let text = std::fs::read_to_string("/proc/meminfo").ok()?;
        for line in text.lines() {
            if let Some(rest) = line.strip_prefix("MemAvailable:") {
                if let Some(kb_str) = rest.split_whitespace().next() {
                    if let Ok(kb) = kb_str.parse::<u64>() {
                        return Some(kb * 1024);
                    }
                }
            }
        }
        None
    }
    #[cfg(not(any(target_os = "macos", target_os = "linux")))]
    {
        None
    }
}

#[cfg(target_os = "macos")]
fn parse_pages(s: &str) -> u64 {
    let t = s.trim().trim_end_matches('.');
    t.parse::<u64>().unwrap_or(0)
}

/// Resolve the cap in bytes without applying it. Exposed so callers can log
/// or validate before (or instead of) calling `apply_safe_cap`.
pub fn default_cap_bytes() -> u64 {
    if let Ok(s) = std::env::var("NXR_MAX_MEM_GB") {
        if let Ok(gb) = s.trim().parse::<u64>() {
            if gb > 0 {
                return gb * GIB;
            }
        }
        warn!("NXR_MAX_MEM_GB={:?} is not a positive integer, ignoring", s);
    }
    let physical_cap = physical_memory_bytes()
        .map(|total| (total as f64 * DEFAULT_PHYSICAL_FRACTION) as u64)
        .unwrap_or(FALLBACK_CAP_BYTES);
    let available_cap = available_memory_bytes()
        .map(|avail| (avail as f64 * DEFAULT_AVAILABLE_FRACTION) as u64);
    // The tighter of the two keeps us safe under current pressure without
    // ever exceeding the long-term share we expect of physical RAM.
    let cap = match available_cap {
        Some(a) => physical_cap.min(a),
        None => physical_cap,
    };
    cap.max(MIN_CAP_BYTES)
}

/// Install the hardest per-process RAM limit the platform supports. On
/// macOS that is `RLIMIT_DATA` (caps the heap; `RLIMIT_AS` silently fails
/// on many macOS builds). On Linux it is `RLIMIT_AS` (covers malloc + mmap
/// together). Only the soft limit is lowered — raising `rlim_max` requires
/// CAP_SYS_RESOURCE / root; we keep the existing hard ceiling intact so a
/// bins-only invocation does not need elevated privileges. No-op on
/// non-unix.
pub fn apply_rlimit_bytes(bytes: u64) {
    #[cfg(unix)]
    unsafe {
        #[cfg(target_os = "macos")]
        let resource = libc::RLIMIT_DATA;
        #[cfg(not(target_os = "macos"))]
        let resource = libc::RLIMIT_AS;

        // Read the current limits first so we never try to raise rlim_max.
        let mut rlim: libc::rlimit = core::mem::zeroed();
        if libc::getrlimit(resource, &mut rlim) != 0 {
            let err = std::io::Error::last_os_error();
            warn!(err = %err, "getrlimit failed; leaving process uncapped");
            return;
        }
        let requested = bytes as libc::rlim_t;
        let soft_cap = if rlim.rlim_max == libc::RLIM_INFINITY {
            requested
        } else {
            requested.min(rlim.rlim_max)
        };
        rlim.rlim_cur = soft_cap;
        if libc::setrlimit(resource, &rlim) != 0 {
            let err = std::io::Error::last_os_error();
            warn!(err = %err, soft_cap, "setrlimit failed; process is uncapped");
        }
    }
    #[cfg(not(unix))]
    {
        let _ = bytes;
    }
}

/// Resolve + apply the safe cap. Call from `main()` before any large
/// allocation. Returns the installed cap so callers can log it alongside
/// their own banner.
///
/// Installs two layers of protection:
///   1. `setrlimit` on the soft resource limit (RLIMIT_DATA on macOS,
///      RLIMIT_AS on Linux). On Linux this traps an overrun inside malloc.
///      On macOS this is typically ignored by the kernel (returns EINVAL),
///      which is why we also need layer 2.
///   2. A background watchdog thread that samples our own RSS every 2 s
///      and calls `std::process::abort` if we cross 95% of the cap. This
///      kills *us* before the kernel OOM-killer decides to kill the user's
///      IDE or browser. Abort (not exit) to avoid running destructors that
///      themselves allocate while we are already over the cap.
pub fn apply_safe_cap() -> u64 {
    let cap = default_cap_bytes();
    apply_rlimit_bytes(cap);
    let physical = physical_memory_bytes().unwrap_or(0);
    let available = available_memory_bytes().unwrap_or(0);
    info!(
        cap_gib = cap / GIB,
        physical_gib = physical / GIB,
        available_gib = available / GIB,
        "installed process RAM cap"
    );
    spawn_rss_watchdog(cap);
    cap
}

/// Spawn the RSS self-watchdog. Fire-and-forget; failure to start the
/// thread is logged but non-fatal (the setrlimit cap still stands where it
/// sticks).
///
/// Samples at 1 s and aborts at 80 % of the cap. 500 ms was previously used
/// to outrun a fast mmap'd-monthly RSS ramp, but after switching the file
/// reader to chunked `File::read` the RSS can no longer jump by > 1 GiB
/// between samples. 1 s halves the subprocess spawn rate (each sample runs
/// `ps`) while still beating the kernel OOM-killer's reaction time.
fn spawn_rss_watchdog(cap_bytes: u64) {
    let pid = std::process::id();
    let threshold = (cap_bytes as f64 * 0.80) as u64;
    let poll = std::time::Duration::from_millis(1000);
    let res = std::thread::Builder::new()
        .name("nxr-memguard".into())
        .spawn(move || loop {
            std::thread::sleep(poll);
            if let Some(rss) = process_rss_bytes(pid) {
                if rss > threshold {
                    error!(
                        rss_gib = rss / GIB,
                        cap_gib = cap_bytes / GIB,
                        "process RSS exceeded 80% of cap; aborting to protect host"
                    );
                    std::process::abort();
                }
            }
        });
    if let Err(e) = res {
        warn!(err = %e, "failed to spawn RSS watchdog; relying on setrlimit alone");
    }
}

/// Sample the given process's current RSS. Uses `/proc/<pid>/status` on
/// Linux and `ps -o rss=` elsewhere (macOS / BSDs). `None` on parse failure
/// — the watchdog will retry on its next tick rather than blowing up.
fn process_rss_bytes(pid: u32) -> Option<u64> {
    #[cfg(target_os = "linux")]
    {
        let status = std::fs::read_to_string(format!("/proc/{}/status", pid)).ok()?;
        for line in status.lines() {
            if let Some(rest) = line.strip_prefix("VmRSS:") {
                if let Some(kb_str) = rest.split_whitespace().next() {
                    if let Ok(kb) = kb_str.parse::<u64>() {
                        return Some(kb * 1024);
                    }
                }
            }
        }
        None
    }
    #[cfg(not(target_os = "linux"))]
    {
        // Explicit stdio null on stderr so a noisy ps build does not
        // pollute our structured log stream between samples.
        let out = std::process::Command::new("ps")
            .args(["-o", "rss=", "-p", &pid.to_string()])
            .stdin(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .output()
            .ok()?;
        let s = String::from_utf8_lossy(&out.stdout);
        s.trim().parse::<u64>().ok().map(|kb| kb * 1024)
    }
}
