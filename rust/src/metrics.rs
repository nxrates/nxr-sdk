//! Shared Prometheus metrics server.
//!
//! Installs the `metrics-exporter-prometheus` global recorder and spawns a
//! tiny axum HTTP server exposing `/metrics` (text exposition format) and
//! `/health` on the configured port. Every long-lived nxr binary uses this so
//! a single Vector scrape config reaches all of them.

use std::net::SocketAddr;

use anyhow::{Context, Result};
use axum::Router;
use axum::extract::State;
use axum::routing::get;
use metrics_exporter_prometheus::{PrometheusBuilder, PrometheusHandle};
use tokio::net::TcpListener;
use tracing::{info, warn};

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
    axum::http::StatusCode,
    [(axum::http::HeaderName, &'static str); 1],
    String,
) {
    (
        axum::http::StatusCode::OK,
        [(
            axum::http::header::CONTENT_TYPE,
            "text/plain; version=0.0.4",
        )],
        handle.render(),
    )
}

async fn health() -> &'static str {
    "ok"
}

/// Resolve the metrics port from `NXR_METRICS_PORT`, falling back to `default`.
pub fn port_from_env(default: u16) -> u16 {
    std::env::var("NXR_METRICS_PORT")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(default)
}
