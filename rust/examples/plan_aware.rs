//! NXR Rust SDK — plan-aware error handling demo.
//!
//! Demonstrates the typed `PlanLimitError` surface from `nxr_sdk::errors`:
//!   1. Reads NXR_API_KEY from env (Free / Starter+ depending on key)
//!   2. Catches `PlanLimitError` specifically via downcast — not generic anyhow
//!   3. Pretty-prints code / plan / limit / requested with an upgrade CTA
//!   4. Falls back gracefully (e.g. MITCH → JSON on Free, tf=10 → tf=60)
//!
//! Run:
//! ```sh
//! cd sdk/rust
//! NXR_API_KEY=<key> cargo run --release --example plan_aware
//! # Or anonymous (Free tier):
//! cargo run --release --example plan_aware
//! ```
//!
//! NB: today the `NxrClient` REST methods still return `anyhow::Error`
//! directly; the plan-error wire shape lands in this PR so SDK consumers can
//! pre-code against the taxonomy. Once the server-side enforcement wires
//! through, the same `downcast_ref::<PlanLimitError>()` call-site will start
//! catching real plan errors with zero further code change.

use std::env;
use std::time::Duration;

use nxr_sdk::client::{BarKindParam, NxrClient, RangeOpts};
use nxr_sdk::errors::{PlanErrorCode, PlanLimitError};

const SEP: &str = "========================================================================";

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let base = env::var("NXR_BASE_URL")
        .unwrap_or_else(|_| nxr_sdk::client::DEFAULT_BASE_URL.into());
    let key = env::var("NXR_API_KEY").ok();
    let c = NxrClient::new(base.clone()).with_timeout(Duration::from_secs(15));

    println!("{SEP}");
    println!(
        "NXR Rust SDK · plan-aware demo · key={}",
        if key.is_some() { "set" } else { "NONE (Free)" }
    );
    println!("{SEP}");

    // ── Scenario 1: /v1/tickers (cheap, JSON, allowed everywhere) ──────────
    match c.tickers().await {
        Ok(rows) => println!("\n[1] /v1/tickers          → {} mids (no plan limit hit)", rows.len()),
        Err(e) => handle("tickers", e),
    }

    // ── Scenario 2: MITCH binary on Free → PLAN_ENCODING_FORBIDDEN ─────────
    println!("\n[2] /v1/idx/BTC-USDT (MITCH binary)");
    let opts = RangeOpts { limit: Some(100), ..Default::default() };
    match c.idx("BTC/USDT", &opts).await {
        Ok(recs) => println!("    ok · {} IndexRecords (binary path)", recs.len()),
        Err(e) => {
            if let Some(plan_err) = e.downcast_ref::<PlanLimitError>() {
                report_plan_error(plan_err, "MITCH binary on Free");
                if plan_err.code == PlanErrorCode::PlanEncodingForbidden {
                    println!("    → falling back to kline bars (JSON path absent in Rust SDK; uses MITCH 96B)");
                    let fb = c.bars("BTC/USDT", BarKindParam::Kline, &opts).await;
                    match fb {
                        Ok(bars) => println!("    ok · {} bars via fallback", bars.len()),
                        Err(e2) => println!("    fallback failed: {}", e2),
                    }
                }
            } else if e.to_string().contains("PLAN_LIMIT_EXCEEDED") {
                // Server is already emitting the wire shape; SDK layer
                // hasn't surfaced the typed downcast yet. Parse the string.
                println!("    [server returned PLAN_LIMIT_EXCEEDED but SDK layer has not")
                ; println!("     wired the downcast yet — parse the message manually]:");
                println!("    {}", e);
            } else {
                println!("    non-plan error: {}", e);
            }
        }
    }

    // ── Scenario 3: renko bars (Pro-tier brick size) → may surface gate ────
    // The Rust SDK does not expose tf=10 OHLC directly (the typed surface
    // covers MITCH bars + idx). We exercise the gate via the bars() path.
    println!("\n[3] /v1/bars/BTC-USDT/renko (Pro-tier brick density)");
    let opts10 = RangeOpts { limit: Some(10), ..Default::default() };
    match c.bars("BTC/USDT", BarKindParam::Renko, &opts10).await {
        Ok(bars) => println!("    ok · {} renko bricks", bars.len()),
        Err(e) => {
            if let Some(plan_err) = e.downcast_ref::<PlanLimitError>() {
                report_plan_error(plan_err, "renko bars");
                if plan_err.code == PlanErrorCode::PlanTimeframeForbidden {
                    println!("    → falling back to kline");
                    if let Ok(bars) = c.bars("BTC/USDT", BarKindParam::Kline, &opts10).await {
                        println!("    ok · {} kline bars via fallback", bars.len());
                    }
                }
            } else {
                println!("    error: {}", e);
            }
        }
    }

    // ── Scenario 4: WebSocket subscribe → PLAN_AUTH_REQUIRED on Free ───────
    println!("\n[4] /v1/stream (WebSocket)");
    match c.subscribe(&["BTC/USDT".to_string()]).await {
        Ok(mut sub) => {
            let deadline = std::time::Instant::now() + Duration::from_secs(2);
            let mut n = 0usize;
            while std::time::Instant::now() < deadline {
                match tokio::time::timeout(Duration::from_millis(500), sub.next()).await {
                    Ok(Ok(Some(rec))) => {
                        if n == 0 {
                            println!(
                                "    first rec: ts={} ticker={} bid={}",
                                rec.epoch_ms, rec.ticker, rec.bid,
                            );
                        }
                        n += 1;
                    }
                    Ok(Ok(None)) => break,
                    Ok(Err(e)) => {
                        if let Some(plan_err) = e.downcast_ref::<PlanLimitError>() {
                            report_plan_error(plan_err, "WS stream");
                        } else {
                            println!("    ws error: {}", e);
                        }
                        break;
                    }
                    Err(_) => continue, // timeout, keep polling
                }
            }
            println!("    received {} records in 2s", n);
        }
        Err(e) => {
            if let Some(plan_err) = e.downcast_ref::<PlanLimitError>() {
                report_plan_error(plan_err, "WS subscribe");
                println!("    → falling back to REST tickers snapshot");
                if let Ok(snap) = c.tickers().await {
                    println!("    ok · {} snapshot rows via REST fallback", snap.len());
                }
            } else {
                println!("    subscribe error: {}", e);
            }
        }
    }

    println!("\n{SEP}");
    println!("Plan-aware demo complete.");
    println!("{SEP}");
    Ok(())
}

/// Pretty-print a `PlanLimitError` with all wire fields + an actionable CTA.
fn report_plan_error(e: &PlanLimitError, scenario: &str) {
    println!("\n[!] PlanLimitError in scenario: {}", scenario);
    println!("    code        = {}", e.code.as_str());
    println!("    plan        = {}", e.plan);
    println!("    limit_name  = {}", e.limit_name);
    if let Some(v) = e.limit_value {
        println!("    limit_value = {}", v);
    }
    if let Some(r) = e.requested {
        println!("    requested   = {}", r);
    }
    println!("    http_status = {}", e.http_status);
    println!("    message     = {}", e.message);
    if e.is_upgrade_needed() {
        println!("    -> action   : upgrade plan → {}", e.upgrade_url);
    } else if e.is_rate_limit() {
        println!("    -> action   : back off + retry (rate-limit)");
    } else if e.is_auth_error() {
        println!("    -> action   : verify API key (auth error)");
    }
}

/// Generic handler that downcasts to `PlanLimitError` if possible.
fn handle(scenario: &str, e: anyhow::Error) {
    if let Some(plan_err) = e.downcast_ref::<PlanLimitError>() {
        report_plan_error(plan_err, scenario);
    } else {
        println!("    non-plan error in {}: {}", scenario, e);
    }
}
