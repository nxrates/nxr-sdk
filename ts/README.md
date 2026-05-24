# @nxrates/sdk

Official TypeScript SDK for **NX Rates** (NXR). MITCH binary decoders, REST + WebSocket + UDP multicast clients. Universal — runs in **Node 18+**, **Bun**, **Deno**, and modern browsers.

## Features

- Zero-dep MITCH binary decoders for the canonical wire types: **56 B `IndexRecord`** (header + body), **96 B `Bar`**, **32 B `Tick`**, plus `Index`/`MitchHeader` primitives.
- **`NxrClient`** REST client covering 100% of the live `/v1` surface:
  - Metadata: `/health`, `/metrics`, `/v1/symbols`, `/v1/providers`, `/v1/tickers`, `/v1/tickers/detail`, `/v1/synth/paths`.
  - Live snapshots: `/v1/price/{ticker_id}`, `/v1/last`.
  - History: `/v1/idx/{sym}`, `/v1/bars/{sym}/{kind}`, `/v1/ohlc/{sym}`, `/v1/synth/ohlc/{sym}`, `/v1/synth/tick/{sym}`.
  - Diagnostics: `/v1/integrity/{sym}`.
- Two equivalent call styles per fetch — **object form** (single call) and **chainable builder** — w/ smart defaults (quote=USDT, kind=renko, instrument=spot).
- **MITCH binary is the wire default** on idx/bars (`Accept: application/octet-stream`); JSON only on metadata.
- **Real-time WebSocket** subscriber (`subscribe(tickers, cb)`) returning a closeable handle.
- **`MulticastSubscriber`** (Node only): UDP multicast for the raw 56 B IndexRecord stream.
- Optional **WASM accelerator** (>10 M rec/s); pure-TS fallback (~500 K rec/s) runs everywhere.
- Tree-shakable: `./node` and `./browser` entry points keep `dgram` out of browser bundles.

## Install

```sh
npm install @nxrates/sdk
# or
bun add @nxrates/sdk
```

## Quickstart — 60 seconds to data

```ts
import { NxrClient } from '@nxrates/sdk';

// Defaults to https://api.nxrates.com — pass baseUrl to override.
// Optional: pass apiKey to bypass per-IP rate limits (see plans doc).
const nxr = new NxrClient({ apiKey: process.env.NXR_API_KEY });

// 1. Discover the universe (cached after first call).
const detail = await nxr.tickersDetail();
console.log(detail.count, 'tickers; idx cadence =', detail.idx_aggregation_ms, 'ms');

// 2. Object form — one call, smart defaults (quote=USDT, kind=renko).
const data = await nxr.history({ ticker: 'BTC/USDT', limit: 500 });
if (data.kind === 'renko') console.log(data.bars[0]);

// 3. Chainable form — same result, flows in conditionals.
const idx = await nxr.get().history().pair('BTC/USDT').idx().limit(1000).fetch();
if (idx.kind === 'idx') console.log(idx.records[0]);

// 4. Real-time stream.
const sub = nxr.subscribe(['BTC/USDT', 'ETH/USDT'], (rec) => {
  console.log(rec.ts_ms, rec.ticker, rec.bid, rec.ask, rec.ci_ubp);
});
// ...
sub.close();
```

## REST surface reference

| Method                                      | Endpoint                          | Wire             |
| ------------------------------------------- | --------------------------------- | ---------------- |
| `client.health()` / `client.isHealthy()`    | `GET /health`                     | JSON / boolean   |
| `client.metrics()`                          | `GET /metrics`                    | Prometheus text  |
| `client.symbols()`                          | `GET /v1/symbols`                 | JSON             |
| `client.providers()`                        | `GET /v1/providers`               | JSON             |
| `client.tickers()`                          | `GET /v1/tickers`                 | JSON             |
| `client.tickersDetail({ refresh? })`        | `GET /v1/tickers/detail`          | JSON, cached     |
| `client.price(tickerId)`                    | `GET /v1/price/{ticker_id}`       | JSON             |
| `client.last([tickerId, ...])`              | `GET /v1/last?symbols=...`        | JSON             |
| `client.idx(sym, opts)`                     | `GET /v1/idx/{sym}`               | MITCH binary 56B |
| `client.bars(sym, kind, opts)`              | `GET /v1/bars/{sym}/{kind}`       | MITCH binary 96B |
| `client.ohlc(sym, tf_s, opts)`              | `GET /v1/ohlc/{sym}?tf=`          | JSON (legacy)    |
| `client.synthPaths()`                       | `GET /v1/synth/paths`             | JSON             |
| `client.synthTick(sym)`                     | `GET /v1/synth/tick/{sym}`        | JSON             |
| `client.synthOhlc(sym, tf_s, opts)`         | `GET /v1/synth/ohlc/{sym}?tf=`    | JSON             |
| `client.integrity(sym, { kind? })`          | `GET /v1/integrity/{sym}`         | JSON             |
| `client.history(opts)`                      | unified routing                   | typed envelope   |
| `client.get().history()....fetch()`         | unified routing                   | typed envelope   |
| `client.subscribe(tickers, cb)`             | `WS  /v1/stream`                  | binary frames    |

Range opts: `{ from?, to?, limit?, cursor? }` — all epoch ms.

## Wire schemas

### IndexRecord (56 B, little-endian, packed)

```
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
`ms`  = EPOCH_MS_2010 + mts*16/1000.

### Bar (96 B, little-endian, packed)

Bar has no embedded ticker - the file path identifies it. Timestamps are
u48 MITCH mts, not epoch ms.

```
open_ts [u8;6] | close_ts [u8;6]
| open f64 | high f64 | low f64 | close f64
| vbid u32 | vask u32 | tick_count u32 | _pad [u8;8]
| realized_var f32 | bipower_var f32 | drift f32
| vol_imbalance f32 | avg_spread_bps f32 | max_abs_return f32
| avg_ci_ubp u16 (sqrt-encoded) | reject_rate u16
| kind u8 (0=kline, 1=renko, 2=dib, 3=tib) | reserved [u8;3]
```

## WebSocket protocol

`ws://<host>/v1/stream` ships an `IndexBatch` frame every 100 ms:

```
[0]     u8   msg_type   (1 = index_batch)
[1]     u8   _pad
[2..4)  u16  count      (little-endian)
[4..8)  u32  _pad       (aligns payload to 8B boundary)
[8+]    count x 9 x f64 (LE) - epoch_ms, ticker, mid, bid, ask,
                                ci_ubp, confidence, accepted, rejected
```

The `subscribe(tickers, cb)` API decodes frames and dispatches per record;
records for unsubscribed tickers (when `tickers` is non-empty) are filtered
client-side after the WS detail cache resolves them to `ticker_id`s.

## Node multicast subscriber

```ts
import { MulticastSubscriber } from '@nxrates/sdk/node';

const sub = new MulticastSubscriber({ group: '239.0.42.1', port: 40006 });
sub.on('record', (r) => console.log(r.ticker, r.mid));
await sub.start();
```

## Versioning

Semantic versioning. The wire format (MITCH 56B/96B) is stable across minor
versions; new fields are appended only at the end of fixed-width records.
The REST surface follows the same rule — additive only.

## License

MIT.
