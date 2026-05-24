# nxr-sdk

Official multi-language SDK for [NX Rates](https://nxrates.com) market data.

Subscribe to real-time FX/crypto index prices via REST, WebSocket, or UDP multicast. Three first-class clients (TypeScript / Python / Rust) share the same MITCH binary wire format, the same naming convention, and the same dual call style.

## Languages

| Language        | Path                  | Install                                                  | Version (2026-05-24) |
| --------------- | --------------------- | -------------------------------------------------------- | -------------------- |
| **TypeScript**  | [`ts/`](ts/)          | `npm install @nxrates/sdk` (or `bun add @nxrates/sdk`)   | `0.3.0`              |
| **Python**      | [`python/`](python/)  | `pip install nxr-sdk` (then `pip install websockets`)    | `0.2.0`              |
| **Rust**        | [`rust/`](rust/)      | `cargo add nxr-sdk`                                      | `0.2.0`              |

Default endpoint: `https://api.nxrates.com`.

## Authentication

The API is read-only and **anonymous by default**. The Free tier per-IP rate
limit (60 burst / 30 r/s) covers casual + dashboard use. To bypass the
per-IP throttle, pass `X-NXR-Key: <your-key>` on every REST + WebSocket
request:

```ts
// TS — set on the client; auto-attached to every call (REST + WS)
const nxr = new NxrClient({ baseUrl: 'https://api.nxrates.com', apiKey: '<key>' });
```

```python
# Python — same shape
nxr = NxrClient(base_url='https://api.nxrates.com', api_key='<key>')
```

```rust
// Rust
let nxr = NxrClient::new("https://api.nxrates.com").with_api_key("<key>");
```

Plans, limits, and key provisioning: see
[../docs/api-plans.md](../docs/api-plans.md).

## Design bar (operator-enforced)

- **Naming**: `symbol` = atomic ("BTC", "USDT"), `ticker` = pair ("BTC/USDT"), `ticker_id` = u64 MITCH encoding.
- **Two call styles**: object form + chainable builder, identical semantics.
- **Smart defaults**: missing quote → `USDT`, missing kind → `renko`, missing instrument → `spot`.
- **MITCH binary** is the default wire format on high-volume endpoints (`/v1/idx`, `/v1/bars`); JSON only on metadata.
- **`ticker_id` is abstracted** — users pass `"BTC/USDT"` and the SDK caches `/v1/tickers/detail` on first call to resolve.
- **Real-time** = WebSocket `/v1/stream`. **Historical** = REST `from`/`to`/`limit`/`cursor`.

## Quick start — side-by-side

### TypeScript

```ts
import { NxrClient } from '@nxrates/sdk';

const nxr = new NxrClient(); // defaults to https://api.nxrates.com

const detail = await nxr.tickersDetail();
console.log(detail.count, 'tickers');

// Object form
const data = await nxr.history({ ticker: 'BTC/USDT', kind: 'renko', limit: 500 });

// Chainable form
const data2 = await nxr.get().history().pair('ETH/USDC').renko().limit(500).fetch();

// Real-time
const sub = nxr.subscribe(['BTC/USDT'], (rec) => console.log(rec.ts_ms, rec.bid));
// sub.close() when done
```

### Python

```python
import nxr_sdk, asyncio

nxr = nxr_sdk.NxrClient()  # defaults to https://api.nxrates.com

detail = nxr.tickers_detail()
print(detail.count, "tickers")

# Object form
bars = nxr.history(ticker="BTC/USDT", kind="renko", limit=500)

# Chainable form
bars = nxr.get().history().pair("ETH/USDC").renko().limit(500).fetch()

async def stream():
    async with nxr.subscribe(["BTC/USDT"]) as sub:
        async for rec in sub:
            print(rec.ts_ms, rec.ticker, rec.bid)

asyncio.run(stream())
```

### Rust

```rust
use nxr_sdk::client::{NxrClient, HistoryOpts, DataKind, HistoryData};

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let c = NxrClient::default(); // https://api.nxrates.com

    let detail = c.tickers_detail().await?;
    println!("{} tickers", detail.count);

    // Object form
    let data = c.history(HistoryOpts {
        ticker: Some("BTC/USDT".into()),
        kind: Some(DataKind::Renko),
        limit: Some(500),
        ..Default::default()
    }).await?;

    // Chainable form
    let data2 = c.get().history().pair("ETH/USDC").renko().limit(500).fetch().await?;

    // Real-time
    let mut sub = c.subscribe(&["BTC/USDT".into()]).await?;
    while let Some(rec) = sub.next().await? {
        println!("{} {} {}", rec.epoch_ms, rec.ticker, rec.bid);
    }
    Ok(())
}
```

## REST surface (all three SDKs)

| Endpoint                              | Method               | Wire             | Notes                                |
| ------------------------------------- | -------------------- | ---------------- | ------------------------------------ |
| `GET /health`                         | `health()`           | JSON             | Liveness + forwarder ages            |
| `GET /metrics`                        | `metrics()`          | Prometheus text  | (TS only — `metrics()` raw body)     |
| `GET /v1/symbols`                     | `symbols()`          | JSON             | direct map + synth paths             |
| `GET /v1/providers`                   | `providers()`        | JSON             | provider_id → name                   |
| `GET /v1/tickers`                     | `tickers()`          | JSON             | live snapshot for every ticker       |
| `GET /v1/tickers/detail`              | `tickersDetail()`    | JSON, **cached** | universal integrator inventory       |
| `GET /v1/price/{ticker_id}`           | `price(id)`          | JSON             | single live snapshot                 |
| `GET /v1/last?symbols=...`            | `last([ids])`        | JSON             | multi-ticker snapshot                |
| `GET /v1/idx/{sym}`                   | `idx(sym, opts)`     | **MITCH 56B**    | raw IndexRecord stream               |
| `GET /v1/bars/{sym}/{kind}`           | `bars(sym, k, opts)` | **MITCH 96B**    | kline (S10 OHLC) or renko bars       |
| `GET /v1/ohlc/{sym}?tf=`              | `ohlc(sym, tf)`      | JSON (legacy)    | prefer `/v1/bars/{sym}/kline`        |
| `GET /v1/synth/paths`                 | `synthPaths()`       | JSON             | static synth registry                |
| `GET /v1/synth/tick/{sym}`            | `synthTick(sym)`     | JSON             | instantaneous synth tick             |
| `GET /v1/synth/ohlc/{sym}?tf=`        | `synthOhlc(...)`     | JSON             | synth OHLC reconstruction            |
| `GET /v1/integrity/{sym}?kind=`       | `integrity(sym, k)`  | JSON / 503       | shard-integrity diagnostics          |
| `WS  /v1/stream`                      | `subscribe(...)`     | binary frames    | live index broadcast (100 ms flush)  |

Range opts everywhere: `{ from?, to?, limit?, cursor? }` — all epoch ms. Symbol path
accepts dash form (`BTC-USDT`), slash form (`BTC%2FUSDT`), or numeric `ticker_id`.

## Wire schemas

### IndexRecord (56 B, little-endian, packed)

```text
[0..16)  MitchHeader (16 B):
   type_provider u16  (low 4b = msg type code, high 12b = provider_id)
   timestamp [u8;6]   (u48 LE mts, 16 us ticks since 2010-01-01)
   count u8
   flags u8
   sequence u16
   _reserved [u8;4]
[16..56) Index body (40 B):
   ticker u64 | bid f64 | ask f64 | vbid u32 | vask u32
   | ci u16 (sqrt-encoded; ubp = (ci/16)^2) | tick_count u16
   | confidence u8 | accepted u8 | rejected u8 | flags u8
```

`mts` = 16 us ticks since 2010-01-01 UTC.
`ms`  = EPOCH_MS_2010 (1262304000000) + mts*16/1000.

### Bar (96 B, little-endian, packed)

Bar has no embedded ticker - the file path identifies it. All timestamps
are u48 MITCH mts.

```text
open_ts [u8;6] | close_ts [u8;6]
| open f64 | high f64 | low f64 | close f64
| vbid u32 | vask u32 | tick_count u32 | _pad [u8;8]
| realized_var f32 | bipower_var f32 | drift f32
| vol_imbalance f32 | avg_spread_bps f32 | max_abs_return f32
| avg_ci_ubp u16 (sqrt-encoded) | reject_rate u16
| kind u8 (0=kline 1=renko 2=dib 3=tib) | reserved [u8;3]
```

## WebSocket protocol

```text
[0]     u8   msg_type   (1 = index_batch)
[1]     u8   _pad
[2..4)  u16  count      (little-endian)
[4..8)  u32  _pad       (aligns payload to 8B boundary)
[8+]    count x 9 x f64 (LE) - epoch_ms, ticker, mid, bid, ask,
                                ci_ubp, confidence, accepted, rejected
```

100 ms flush cadence. `count` is dedup-by-ticker within the window
(last-write-wins). Client `subscribe(tickers, cb)` filters records
client-side by ticker_id (resolved from the cached `/v1/tickers/detail`).

## Content-type negotiation (server-side)

| `Accept`                  | Response                                       |
| ------------------------- | ---------------------------------------------- |
| (default / unset)         | `application/octet-stream` — MITCH binary      |
| `application/json`        | JSON envelope                                  |
| `application/x-ndjson`    | newline-delimited JSON (for streaming clients) |

The SDKs send `Accept: application/octet-stream` on `/v1/idx`, `/v1/bars`,
`/v1/ohlc`, `/v1/synth/ohlc`. JSON on every other endpoint.

## Versioning

SemVer. The MITCH wire format (56 B / 96 B fixed-width records) is stable
across minor versions; new fields are appended only at the end of fixed-width
records. The REST surface follows the same rule — additive only.

## License

MIT — see [`LICENSE`](LICENSE).
