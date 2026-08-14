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
| `c.providers()`                       | `GET /v1/providers`            | JSON             |
| `c.tickers()`                         | `GET /v1/tickers`              | JSON             |
| `c.tickers_detail()`                  | `GET /v1/tickers/detail`       | JSON, cached     |
| `c.price(sym, max_age_ms?)`           | `GET /v1/price/{ticker}`       | JSON             |
| `c.last(&[syms], max_age_ms?)`        | `GET /v1/last?symbols=...`     | JSON             |
| `c.freshness(sym)`                    | `GET /v1/freshness/{ticker}`   | JSON             |
| `c.idx(sym, opts)`                    | `GET /v1/idx/{sym}`            | MITCH 56B        |
| `c.bars(sym, kind, opts)`             | `GET /v1/bars/{sym}/{kind}`    | MITCH 96B        |
| `c.integrity(sym, kind?)`             | `GET /v1/integrity/{sym}`      | JSON             |
| `c.history(opts)` / chainable         | unified routing                | `HistoryData`    |
| `c.subscribe(&[syms])`                | `WS  /v1/stream`               | `WsStream`       |

`opts: RangeOpts` carries `from / to / limit / cursor` (all epoch ms). Idx + bars
default to `Accept: application/octet-stream` and decode via `bytemuck` (zero-copy
slice cast → owned `Vec<T>`). Metadata defaults to JSON.

## WS protocol

`ws[s]://<host>/v1/stream` ships an `IndexBatch` frame every 200 ms:

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

## Plan tiers at a glance

| Tier        | Price (USD/mo) | WS feeds   | Encodings        | History  |
| ----------- | -------------- | ---------- | ---------------- | -------- |
| Free        | 0              | —          | JSON             | 1 month  |
| Starter     | 20             | 10         | JSON, f64, MITCH | 3 months |
| Pro         | 100            | 50         | JSON, f64, MITCH | 1 year   |
| Enterprise  | 1,000          | 500        | JSON, f64, MITCH | full     |
| Colo        | 5,000          | unbounded  | JSON, f64, MITCH | full     |

Full matrix incl. min TFs, latency targets, and fair-use notes:
[`docs/api-plans.md`](../../docs/api-plans.md).

## Error handling

When a request exceeds a plan limit, the server emits HTTP 401 / 403 / 406 /
429 with a JSON body whose `error` field equals `"PLAN_LIMIT_EXCEEDED"`. The
SDK parses that shape into a typed `PlanLimitError` (`nxr_sdk::errors`) so
callers can branch on `code` rather than regexing English `message` strings.

| Code                       | HTTP | WS close | Resolution                |
| -------------------------- | ---- | -------- | ------------------------- |
| `PLAN_RATE_LIMIT_HTTP`     | 429  | —        | back off + retry          |
| `PLAN_RATE_LIMIT_WS`       | —    | 4029     | back off + retry          |
| `PLAN_WS_FEED_CAP`         | 403  | 4030     | upgrade plan / fewer subs |
| `PLAN_ENCODING_FORBIDDEN`  | 406  | —        | upgrade / fall back JSON  |
| `PLAN_TIMEFRAME_FORBIDDEN` | 403  | —        | upgrade / coarsen TF      |
| `PLAN_HISTORY_FORBIDDEN`   | 403  | —        | upgrade / shorten range   |
| `PLAN_AUTH_REQUIRED`       | 401  | 4401     | provide API key           |
| `PLAN_KEY_INVALID`         | 401  | 4401     | verify key                |
| `PLAN_KEY_REVOKED`         | 403  | 4403     | rotate / contact support  |

Full taxonomy + wire JSON samples in
[`docs/api-plans.md`](../../docs/api-plans.md#error-codes-and-sdk-handling).

### How to detect plan limits in code

```rust
use nxr_sdk::client::{NxrClient, RangeOpts};
use nxr_sdk::errors::PlanLimitError;

# async fn run() -> anyhow::Result<()> {
let c = NxrClient::default();

match c.idx("BTC/USDT", &RangeOpts { limit: Some(1000), ..Default::default() }).await {
    Ok(recs) => println!("{} records", recs.len()),
    Err(e) => {
        if let Some(plan_err) = e.downcast_ref::<PlanLimitError>() {
            eprintln!("{}: {}", plan_err.code.as_str(), plan_err.message);
            if plan_err.is_upgrade_needed() {
                eprintln!("Upgrade → {}", plan_err.upgrade_url);
            } else if plan_err.is_rate_limit() {
                // back off and retry
            } else if plan_err.is_auth_error() {
                // refresh / rotate API key
            }
        } else {
            return Err(e);
        }
    }
}
# Ok(()) }
```

Runnable example: [`examples/plan_aware.rs`](./examples/plan_aware.rs) —
`cargo run --release --example plan_aware`.

## Versioning

SemVer. The wire format (MITCH 56B / 96B) is stable across minor releases; new
fields are appended only at the end of fixed-width records. REST endpoints are
additive.

## License

MIT.
