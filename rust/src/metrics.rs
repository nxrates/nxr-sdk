//! Shared Prometheus metrics server.
//!
//! Installs the `metrics-exporter-prometheus` global recorder and spawns a
//! tiny axum HTTP server exposing `/metrics` (text exposition format) and
//! `/health` on the configured port. Every long-lived nxr binary uses this so
//! a single Vector scrape config reaches all of them.
//!
//! `/health` is the k8s liveness surface. A binary that owns a TRANSPORT
//! (a forwarder) registers a [`set_health_check`] probe; the probe MUST report
//! transport/connection sanity, never market-data freshness, so a healthy but
//! idle session (off-market, weekend, market close) stays `ok` and the pod is
//! not restarted. A probe absent means the binary has no transport and is
//! healthy by construction (core/weights).

use std::net::SocketAddr;
use std::sync::OnceLock;
use std::sync::atomic::{AtomicU64, Ordering};

use anyhow::{Context, Result};
use axum::Router;
use axum::extract::State;
use axum::http::StatusCode;
use axum::routing::get;
use metrics_exporter_prometheus::{PrometheusBuilder, PrometheusHandle};
use tokio::net::TcpListener;
use tracing::{info, warn};

/// Service health for k8s liveness. `ok` = keep the pod up.
#[derive(Debug, Clone)]
pub struct Health {
    ok: bool,
    reason: Option<String>,
}

impl Health {
    /// The pod is healthy. This is the DEFAULT and the only acceptable answer
    /// for an idle-but-connected transport (no market data during off-hours is
    /// not a fault).
    pub fn ok() -> Self {
        Health { ok: true, reason: None }
    }

    /// The pod must be restarted. `reason` is surfaced on `/health` so a
    /// liveness failure is diagnosable from the probe result alone.
    pub fn down(reason: impl Into<String>) -> Self {
        Health {
            ok: false,
            reason: Some(reason.into()),
        }
    }

    pub fn is_ok(&self) -> bool {
        self.ok
    }
}

/// Transport liveness for ONE forwarder provider/session: the wall-clock ms of
/// the last ANY inbound frame (market data OR heartbeat) plus the boot epoch.
///
/// A session that is CONNECTED but idle (weekend, market close) keeps sending
/// heartbeats, so [`mark_frame`](Self::mark_frame) keeps it fresh and the pod
/// stays up. A DEAD socket stops all frames — heartbeats too — and the probe
/// reports DOWN after `stall`, which is when k8s liveness restarts the pod.
/// Market-data silence alone never triggers down, which is what makes this
/// session-agnostic: no calendar, no catalogue.
pub struct TransportHealth {
    last_frame_ms: AtomicU64,
    boot_ms: AtomicU64,
}

impl Default for TransportHealth {
    fn default() -> Self {
        let now = crate::now_ms();
        TransportHealth {
            last_frame_ms: AtomicU64::new(now),
            boot_ms: AtomicU64::new(now),
        }
    }
}

impl TransportHealth {
    /// Call on every inbound transport frame, including heartbeats. Cheap.
    pub fn mark_frame(&self) {
        self.last_frame_ms.store(crate::now_ms(), Ordering::Relaxed);
    }

    /// ms since the last inbound frame (Refreshed each call).
    pub fn last_frame_age_ms(&self) -> u64 {
        crate::now_ms() - self.last_frame_ms.load(Ordering::Relaxed)
    }

    /// Liveness. `ok` while within `grace_ms` of boot (a slow first connect is
    /// not a fault), else while any frame arrived within `stall_ms`; `down`
    /// once a dead transport has produced no frame for `stall_ms`.
    pub fn health(&self, stall_ms: u64, grace_ms: u64) -> Health {
        let now = crate::now_ms();
        if now - self.boot_ms.load(Ordering::Relaxed) < grace_ms {
            return Health::ok();
        }
        let age = self.last_frame_age_ms();
        if age < stall_ms {
            Health::ok()
        } else {
            Health::down(format!(
                "no transport frame for {age}ms (stall {stall_ms}ms)"
            ))
        }
    }
}


/// Process-global health probe. Default (nothing registered) = healthy.
/// Registered once at startup by a transport-owning binary.
static HEALTH: OnceLock<Box<dyn Fn() -> Health + Send + Sync>> = OnceLock::new();

/// Register the process health probe. Call once before `serve`.
///
/// The probe is evaluated on every `/health` request, so it reflects live
/// transport state rather than a boot snapshot. It must be transport-based:
/// report DOWN only when a connection is dead or the reconnect is wedged,
/// never merely because no market data is flowing.
pub fn set_health_check<F>(f: F)
where
    F: Fn() -> Health + Send + Sync + 'static,
{
    let _ = HEALTH.set(Box::new(f));
}

/// Install the Prometheus recorder and spawn an HTTP server on the given port.
///
/// Returns immediately once the listener is bound; the server runs on a
/// background tokio task. The recorder is process-global, so
/// `metrics::counter!`/`gauge!`/`histogram!` macros anywhere in the process
/// record into it without any handle threading.
pub async fn serve(port: u16) -> Result<()> {
    let handle: PrometheusHandle = PrometheusBuilder::new()
        .install_recorder()
        .context("install Prometheus recorder")?;

    let addr: SocketAddr = ([0, 0, 0, 0], port).into();
    let listener = TcpListener::bind(addr)
        .await
        .with_context(|| format!("bind metrics listener on {addr}"))?;

    let app = Router::new()
        .route("/metrics", get(render_metrics))
        .route("/health", get(health))
        .with_state(handle);

    tokio::spawn(async move {
        info!(%addr, "metrics server listening");
        if let Err(e) = axum::serve(listener, app).await {
            warn!(err = %e, "metrics server exited");
        }
    });
    Ok(())
}

async fn render_metrics(
    State(handle): State<PrometheusHandle>,
) -> (
    StatusCode,
    [(axum::http::HeaderName, &'static str); 1],
    String,
) {
    (
        StatusCode::OK,
        [(
            axum::http::header::CONTENT_TYPE,
            "text/plain; version=0.0.4",
        )],
        handle.render(),
    )
}

async fn health() -> (StatusCode, String) {
    let h = match HEALTH.get() {
        Some(probe) => probe(),
        None => Health::ok(),
    };
    if h.is_ok() {
        (StatusCode::OK, "ok".to_string())
    } else {
        (
            StatusCode::SERVICE_UNAVAILABLE,
            h.reason.unwrap_or_else(|| "unhealthy".to_string()),
        )
    }
}

/// Resolve the metrics port from `NXR_METRICS_PORT`, falling back to `default`.
pub fn port_from_env(default: u16) -> u16 {
    std::env::var("NXR_METRICS_PORT")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(default)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::Ordering;

    /// A transport that keeps ANY frame flowing (a heartbeat on a connected
    /// but idle/off-market session) stays `ok`; silence past the stall goes
    /// `down`. This is the whole HVA contract: market-data silence is NEVER a
    /// restart reason.
    #[test]
    fn transport_health_frames_keep_it_up_silence_does_not() {
        let h = TransportHealth::default();
        // Pin boot and last-frame to the far past so neither construction grace
        // nor a just-marked frame can mask the stall.
        h.boot_ms.store(1, Ordering::Relaxed);
        h.last_frame_ms.store(1, Ordering::Relaxed);
        // No frame for effectively forever -> down past the stall.
        assert!(!h.health(1000, 0).is_ok());
        // One inbound frame (a heartbeat) flips it back to healthy.
        h.mark_frame();
        assert!(h.health(1000, 0).is_ok());
    }

    /// A brand-new transport is `ok` during boot grace even before its first
    /// frame, so a slow first connect never triggers a startup restart.
    #[test]
    fn transport_health_boot_grace_keeps_pod_up_before_first_frame() {
        let h = TransportHealth::default();
        assert!(h.health(1, 60 * 60 * 1000).is_ok());
    }
}

