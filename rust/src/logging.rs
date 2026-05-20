//! Structured logging initialization for NXR binaries.
//!
//! Two output formats are supported:
//! - `compact` (default): human-readable single-line output for local dev.
//! - `json`: one-JSON-object-per-line, ingested by the Vector DaemonSet and
//!   queryable in OpenObserve (see `deploy/k8s/`).
//!
//! Select the format via `NXR_LOG_FORMAT=json|compact` (default `compact`).
//! The log level filter honours `RUST_LOG` first, falling back to the `level`
//! arg passed to [`init`].

use tracing_subscriber::{fmt, EnvFilter};

/// Initialize structured logging with the given level filter.
///
/// Respects `RUST_LOG` for per-target filtering and `NXR_LOG_FORMAT=json` to
/// switch to JSON output for multi-node log aggregation.
pub fn init(level: &str) {
    let filter = EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| EnvFilter::new(level));

    let format = std::env::var("NXR_LOG_FORMAT")
        .unwrap_or_else(|_| "compact".to_string());

    match format.as_str() {
        "json" => {
            fmt()
                .with_env_filter(filter)
                .with_target(true)
                .with_thread_ids(false)
                .with_file(false)
                .json()
                .with_current_span(true)
                .with_span_list(false)
                .flatten_event(true)
                .init();
        }
        _ => {
            fmt()
                .with_env_filter(filter)
                .with_target(true)
                .with_thread_ids(false)
                .with_file(false)
                .compact()
                .init();
        }
    }
}
