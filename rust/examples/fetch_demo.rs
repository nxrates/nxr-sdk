//! NXR Rust SDK — fetch demo against live prod API.
//!
//! Run: cd sdk/rust && cargo run --release --example fetch_demo

use std::time::Instant;

use bytemuck;
use nxr_sdk::ipc::record::IndexRecord;
use serde::Deserialize;

const BASE_URL: &str = "https://api.nxrates.com";

#[derive(Deserialize, Debug)]
#[allow(dead_code)]
struct TickerJson {
    ticker: u64,
    mid: f64,
    bid: f64,
    ask: f64,
    ci: u16,
    confidence: u8,
}

#[derive(Deserialize, Debug)]
#[allow(dead_code)]
struct SynthTick {
    sym: String,
    bid: f64,
    ask: f64,
    mid: f64,
    conf: u16,
}

#[derive(Deserialize, Debug)]
#[allow(dead_code)]
struct OhlcJson {
    ts: i64,
    open: f64,
    high: f64,
    low: f64,
    close: f64,
    vbid: u64,
    vask: u64,
    tick_count: u32,
    avg_ci_ubp: u16,
}

fn main() -> anyhow::Result<()> {
    println!("{}", "=".repeat(70));
    println!("NXR Rust SDK demo · base={}", BASE_URL);
    println!("{}", "=".repeat(70));

    let agent = ureq::AgentBuilder::new()
        .timeout_connect(std::time::Duration::from_secs(5))
        .timeout_read(std::time::Duration::from_secs(15))
        .build();

    // 1. Live tickers (JSON)
    let t0 = Instant::now();
    let body: Vec<TickerJson> = agent
        .get(&format!("{}/v1/tickers", BASE_URL))
        .call()?
        .into_json()?;
    println!(
        "\n[1] /v1/tickers          → {} mids in {:.1}ms",
        body.len(),
        t0.elapsed().as_secs_f64() * 1000.0
    );
    if let Some(t) = body.first() {
        println!(
            "    sample: ticker={:>20}  mid={:>12.6}  ci={}  conf={}",
            t.ticker, t.mid, t.ci, t.confidence
        );
    }

    // 2. Raw ticks (octet-stream, zero-copy bytemuck cast)
    let t0 = Instant::now();
    let mut buf = Vec::with_capacity(560_000);
    agent
        .get(&format!("{}/v1/idx/BTC-USDT?limit=10000", BASE_URL))
        .set("Accept", "application/octet-stream")
        .call()?
        .into_reader()
        .read_to_end(&mut buf)?;
    let dt = t0.elapsed().as_secs_f64() * 1000.0;
    let n = buf.len() / std::mem::size_of::<IndexRecord>();
    let recs: &[IndexRecord] = bytemuck::cast_slice(&buf[..n * std::mem::size_of::<IndexRecord>()]);
    println!(
        "\n[2] /v1/idx/BTC/USDT     → {} IndexRecords ({} bytes) in {:.1}ms ({:.0} rec/s)",
        recs.len(),
        buf.len(),
        dt,
        recs.len() as f64 / (dt / 1000.0).max(0.001)
    );
    if let (Some(f), Some(l)) = (recs.first(), recs.last()) {
        let (fb, fa, fc) = (f.index.bid, f.index.ask, f.index.confidence);
        let (lb, la, lc) = (l.index.bid, l.index.ask, l.index.confidence);
        println!("    first  bid={:>12.6}  ask={:>12.6}  conf={}", fb, fa, fc);
        println!("    last   bid={:>12.6}  ask={:>12.6}  conf={}", lb, la, lc);
    }

    // 3. OHLC at multiple TFs
    for (tf, label) in [(60, "1m"), (300, "5m"), (3600, "1h"), (86400, "1d")] {
        let t0 = Instant::now();
        let ohlc: Vec<OhlcJson> = agent
            .get(&format!(
                "{}/v1/ohlc/BTC-USDT?tf={}&limit=100",
                BASE_URL, tf
            ))
            .call()?
            .into_json()?;
        let dt = t0.elapsed().as_secs_f64() * 1000.0;
        println!(
            "\n[3.{}] /v1/ohlc tf={:>3}    → {} bars in {:.1}ms",
            tf,
            label,
            ohlc.len(),
            dt
        );
        if let Some(last) = ohlc.last() {
            println!(
                "      last ts={}  O={:.2}  H={:.2}  L={:.2}  C={:.2}  ticks={}",
                last.ts, last.open, last.high, last.low, last.close, last.tick_count
            );
        }
    }

    // 4. Synth tick
    let t0 = Instant::now();
    let synth: SynthTick = agent
        .get(&format!("{}/v1/synth/tick/ETH-BTC", BASE_URL))
        .call()?
        .into_json()?;
    println!(
        "\n[4] /v1/synth/tick/ETH/BTC in {:.1}ms",
        t0.elapsed().as_secs_f64() * 1000.0
    );
    println!(
        "    bid={:.6}  ask={:.6}  mid={:.6}  conf={}",
        synth.bid, synth.ask, synth.mid, synth.conf
    );

    // 5. Cast benchmark — bytemuck zero-copy MITCH decode
    const N_ITER: usize = 1000;
    let t0 = Instant::now();
    let mut sum_mid = 0.0_f64;
    for _ in 0..N_ITER {
        let recs: &[IndexRecord] = bytemuck::cast_slice(&buf[..n * 56]);
        for r in recs {
            sum_mid += (r.index.bid + r.index.ask) * 0.5;
        }
    }
    let dt = t0.elapsed().as_secs_f64() * 1000.0;
    let total = N_ITER * recs.len();
    println!(
        "\n[5] bytemuck zero-copy decode: {} iters × {} recs = {} total in {:.1}ms ({:.0} rec/s)",
        N_ITER,
        recs.len(),
        total,
        dt,
        total as f64 / (dt / 1000.0)
    );
    println!("    sum_mid={:.6} (sanity check, prevents dead code elim)", sum_mid);

    // 6. Symbol resolution (offline)
    let tid = nxr_sdk::resolve_ticker_id("BTC/USDT");
    println!("\n[6] resolve BTC/USDT → 0x{:016X}", tid);

    println!("\n{}", "=".repeat(70));
    println!("All endpoints functional ✓");
    println!("{}", "=".repeat(70));
    Ok(())
}
