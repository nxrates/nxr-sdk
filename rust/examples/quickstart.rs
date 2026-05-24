//! NXR Rust SDK quickstart — covers the full client surface.
//!
//! Run: `cd sdk/rust && cargo run --release --example quickstart`
//!
//! This example hits the live API at `https://api.nxrates.com`. To point at a
//! local instance, set `NXR_BASE_URL=http://localhost:40004` in the env.

use std::env;
use std::time::Duration;

use nxr_sdk::client::{DataKind, HistoryData, HistoryOpts, NxrClient};

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let base = env::var("NXR_BASE_URL").unwrap_or_else(|_| nxr_sdk::client::DEFAULT_BASE_URL.into());
    let c = NxrClient::new(base).with_timeout(Duration::from_secs(15));

    println!("== Health ==");
    let h = c.health().await?;
    println!("{}", h);

    println!("\n== /v1/tickers/detail (typed + cached) ==");
    let detail = c.tickers_detail().await?;
    println!(
        "{} tickers; idx cadence = {} ms",
        detail.count, detail.idx_aggregation_ms
    );
    if let Some(btc) = detail.by_ticker("BTC/USDT") {
        let kinds: Vec<&String> = btc.kinds.keys().collect();
        println!(
            "BTC/USDT: id={} kinds={:?} idx-shards={}",
            btc.ticker_id,
            kinds,
            btc.kinds
                .get("idx")
                .map(|k| k.shards.count)
                .unwrap_or_default(),
        );
    }

    println!("\n== Object-form history (smart defaults) ==");
    let data = c
        .history(HistoryOpts {
            ticker: Some("BTC/USDT".into()),
            kind: Some(DataKind::Renko),
            limit: Some(5),
            ..Default::default()
        })
        .await?;
    match data {
        HistoryData::Bars { kind, bars } => {
            println!("kind={:?} n={}", kind, bars.len());
            for b in bars.iter().take(3) {
                let open = b.open;
                let close = b.close;
                println!("  open={open:.4} close={close:.4}");
            }
        }
        HistoryData::Idx(rs) => {
            println!("idx n={}", rs.len());
        }
    }

    println!("\n== Chainable form ==");
    let data = c
        .get()
        .history()
        .pair("ETH/USDT")
        .renko()
        .limit(3)
        .fetch()
        .await?;
    println!("got: {:?}", match data {
        HistoryData::Bars { bars, .. } => format!("{} bars", bars.len()),
        HistoryData::Idx(rs) => format!("{} idx", rs.len()),
    });

    Ok(())
}
