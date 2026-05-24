# nxr-sdk (Rust)

Official Rust SDK for **NX Rates** (NXR). Single crate covering:

- **`NxrClient`** — REST + WebSocket consumer for the v1 API.
- MITCH wire types (re-exported from the canonical `mitch` crate).
- IPC primitives: `AppendLog`, `IndexRecord` (56 B).
- Aggregation: `TickAccumulator`, `TDWAP`, OHLC rollup, microstructure bars.
- Daily-shard storage layer: `IdxShardWriter`, `BarShardWriter`, shard listing/healing.
- Synth composition: tick + OHLC + bar + correlation-aware variance.
- Multicast transport: `UdpMulticastSink`, `UdpMulticastSource`.

## Install

```toml
[dependencies]
nxr-sdk = { git = "https://github.com/nxrates/nxr-sdk", tag = "sdk-v2026.05.24" }
tokio = { version = "1", features = ["macros", "rt-multi-thread"] }
anyhow = "1"
```

## 60 seconds to data

```rust
use nxr_sdk::client::{NxrClient, HistoryOpts, DataKind, HistoryData};

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    // Defaults to https://api.nxrates.com. Optional: .with_api_key("…") to
    // bypass per-IP rate limits — see docs/api-plans.md.
    let c = NxrClient::default();

    // Discover the universe (cached after first call).
    let detail = c.tickers_detail().await?;
    println!("{} tickers; idx cadence = {} ms",
             detail.count, detail.idx_aggregation_ms);

    // Object form — smart defaults: quote=USDT, kind=renko, instrument=spot.
    let data = c.history(HistoryOpts {
        ticker: Some("BTC/USDT".into()),
        limit: Some(500),
        ..Default::default()
    }).await?;
    if let HistoryData::Bars { bars, .. } = data {
        println!("{} renko bars", bars.len());
    }

    // Chainable form.
    let data = c.get().history().pair("ETH/USDC").renko().limit(500).fetch().await?;

    // Real-time stream.
    let mut sub = c.subscribe(&["BTC/USDT".into()]).await?;
    while let Some(rec) = sub.next().await? {
        println!("{} {} {}", rec.epoch_ms, rec.ticker, rec.bid);
    }
    Ok(())
}
```

Run the included example:

```sh
cargo run --release --example quickstart
```

## REST surface

| Method                                | Endpoint                       | Wire             |
| ------------------------------------- | ------------------------------ | ---------------- |
| `c.health()`                          | `GET /health`                  | JSON value       |
| `c.symbols()`                         | `GET /v1/symbols`              | JSON             |
| `c.providers()`                       | `GET /v1/providers`            | JSON             |
| `c.tickers()`                         | `GET /v1/tickers`              | JSON             |
| `c.tickers_detail()`                  | `GET /v1/tickers/detail`       | JSON, cached     |
| `c.price(ticker_id)`                  | `GET /v1/price/{id}`           | JSON             |
| `c.last(&[ids])`                      | `GET /v1/last?symbols=...`     | JSON             |
| `c.idx(sym, opts)`                    | `GET /v1/idx/{sym}`            | MITCH 56B        |
| `c.bars(sym, kind, opts)`             | `GET /v1/bars/{sym}/{kind}`    | MITCH 96B        |
| `c.synth_paths()` / `c.synth_tick(s)` | `/v1/synth/*`                  | JSON             |
| `c.integrity(sym, kind?)`             | `GET /v1/integrity/{sym}`      | JSON             |
| `c.history(opts)` / chainable         | unified routing                | `HistoryData`    |
| `c.subscribe(&[syms])`                | `WS  /v1/stream`               | `WsStream`       |

`opts: RangeOpts` carries `from / to / limit / cursor` (all epoch ms). Idx + bars
default to `Accept: application/octet-stream` and decode via `bytemuck` (zero-copy
slice cast → owned `Vec<T>`). Metadata defaults to JSON.

## WS protocol

`ws[s]://<host>/v1/stream` ships an `IndexBatch` frame every 100 ms:

```text
[0]    u8   msg_type   (1 = index_batch)
[1]    u8   _pad
[2..4) u16  count      (LE)
[4..8) u32  _pad       (aligns payload to 8B boundary)
[8+]   count x 9 x f64 (LE) - epoch_ms, ticker, mid, bid, ask, ci_ubp,
                              confidence, accepted, rejected
```

`WsStream::next()` decodes frames into `WsIndex` records and applies the
client-side ticker filter (resolved from the cached `/v1/tickers/detail`).
Drop the handle or call `.close().await` to terminate.

## Aggregator-side use (no breaking changes)

The aggregator (`nx-rates/core`) depends on this crate via `path` and was tested
to compile cleanly with `nxr-sdk = "0.2"`. The new `client` module sits next to
the existing aggregation primitives; the `reqwest` + `tokio-tungstenite`
dependencies were already required by the consumer side of the SDK, so no new
transitive footprint lands on the producer binaries.

## Versioning

SemVer. The wire format (MITCH 56B / 96B) is stable across minor releases; new
fields are appended only at the end of fixed-width records. REST endpoints are
additive.

## License

MIT.
