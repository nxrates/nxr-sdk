# @nxrates/sdk

Official TypeScript SDK for **NX Rates** (NXR). MITCH binary decoders, REST + WebSocket + UDP multicast clients. Universal — runs in **Node 18+**, **Bun**, **Deno**, and modern browsers.

## Features

- Zero-dep MITCH binary decoders for the canonical wire types: **56 B `IndexRecord`** (header + body), **96 B `Bar`**, **32 B `Tick`**, and `Index`/`MitchHeader` primitives.
- **`NxrClient`** REST client: `/v1/tickers`, `/v1/idx`, `/v1/ohlc`, `/v1/bars`, `/v1/synth/*`, `/v1/integrity`, `/v1/symbols`, `/v1/providers`, `/health`.
- Octet-stream fast path: `idxBinary()` / `barsBinary()` fetch raw MITCH frames and decode in TS (or WASM if available).
- **WebSocket** zero-copy `Float64Array` batches over `/v1/stream`.
- **`MulticastSubscriber`** (Node only): UDP multicast subscriber for the raw 56 B IndexRecord stream on `239.0.42.1:40006` (channel A) / `239.0.42.2:40007` (channel B).
- Optional **WASM accelerator** (>10 M rec/s) compiled from the canonical Rust `mitch` crate; pure-TS fallback (~500 K rec/s) runs everywhere.
- Tree-shakable: `./node` and `./browser` entry points keep `dgram` out of browser bundles.

## Install

```sh
npm install @nxrates/sdk
# or
bun add @nxrates/sdk
```

## Quickstart — Browser

```ts
import { NxrClient } from '@nxrates/sdk/browser';

const nxr = new NxrClient({ baseUrl: 'https://nxr.nxrates.com' });

// REST
const tickers = await nxr.tickers();
const ohlc    = await nxr.ohlc('BTC/USDT', 60_000, { limit: 100 });

// Fast binary (server returns 56 B MITCH frames; SDK decodes)
const recs = await nxr.idxBinary('BTC/USDT', { limit: 1024 });
console.log(recs[0].mid, recs[0].ci_ubp);

// WebSocket — zero-copy batches
nxr.onIndex(batch => {
  for (let i = 0; i < batch.count; i++) {
    if (batch.confidence(i) >= 3) console.log(batch.ticker(i), batch.mid(i));
  }
});
nxr.connect();
```

## Quickstart — Node (multicast)

```ts
import { MulticastSubscriber } from '@nxrates/sdk/node';

const sub = new MulticastSubscriber({ group: '239.0.42.1', port: 40006 });

sub.on('record', rec => {
  console.log(rec.ticker, rec.mid, rec.confidence);
});

sub.on('batch', recs => {
  // Lower-overhead alternative when datagrams carry many frames.
});

sub.on('error', err => console.error('mcast error', err));

await sub.start();
// ... later ...
await sub.stop();
```

For both channels A and B (dedup at the app layer):

```ts
const a = new MulticastSubscriber({ group: '239.0.42.1', port: 40006 });
const b = new MulticastSubscriber({ group: '239.0.42.2', port: 40007 });
await Promise.all([a.start(), b.start()]);
```

## API surface

### Types ([`src/types.ts`](./src/types.ts))

```ts
interface IndexRecord {
  ts_ms: number;       // Unix epoch ms
  provider: number;    // u12 provider id
  ticker: bigint;      // u64 MITCH ticker id
  bid: number; ask: number; mid: number;
  ci_ubp: number;      // decoded confidence interval, micro basis points
  accepted: number; rejected: number; confidence: number;
  vbid: number; vask: number; tick_count: number; sequence: number;
}

interface Bar {
  open_ms: number; close_ms: number;
  open: number; high: number; low: number; close: number;
  vbid: number; vask: number; tick_count: number;
  realized_var: number; bipower_var: number; drift: number;
  vol_imbalance: number; avg_spread_bps: number; max_abs_return: number;
  avg_ci_ubp: number; reject_rate: number; kind: number;
}

interface Ohlc { ts_ms: number; open: number; ...; avg_ci_ubp: number; }
type Sym = string; // "BTC/USDT"
```

### Decoders ([`src/decode.ts`](./src/decode.ts))

```ts
decodeIdxRecord(buf: Uint8Array, offset?: number): IndexRecord
decodeIdxBatch(buf: Uint8Array): IndexRecord[]
decodeBar(buf: Uint8Array, offset?: number): Bar
decodeBarBatch(buf: Uint8Array): Bar[]
decodeTick(buf: Uint8Array, offset?: number): Tick
decodeFrame(buf: Uint8Array): { header, bodyOffset, bodyBytes } | null
```

### Client ([`src/client.ts`](./src/client.ts))

```ts
class NxrClient {
  constructor(opts: { baseUrl: string; reconnectMs?: number; fetch?: typeof fetch });

  // discovery
  isHealthy(): Promise<boolean>;
  symbols():   Promise<Map<string, bigint>>;
  providers(): Promise<Map<number, string>>;

  // market data — JSON
  tickers(): Promise<TickerSnapshot[]>;
  idx(sym: Sym, opts?: RangeOpts): Promise<IndexRecord[]>;
  ohlc(sym: Sym, tf_ms: number, opts?: RangeOpts): Promise<Ohlc[]>;
  bars(sym: Sym, kind: 'kline' | 'renko', opts?: RangeOpts): Promise<Bar[]>;
  synthTick(sym: Sym): Promise<SynthTick>;
  synthOhlc(sym: Sym, tf_ms: number, opts?: RangeOpts): Promise<Ohlc[]>;
  integrity(): Promise<unknown>;

  // market data — octet-stream (raw MITCH frames; decoded in SDK)
  idxBinary(sym: Sym, opts?: RangeOpts): Promise<IndexRecord[]>;
  barsBinary(sym: Sym, kind: 'kline' | 'renko', opts?: RangeOpts): Promise<Bar[]>;

  // websocket
  connect(wsUrl?: string): void;
  disconnect(): void;
  onIndex(cb: (batch: IndexBatch) => void): () => void;
  onTick(cb:  (batch: TickBatch) => void):  () => void;
  // ... see source for more
}
```

### Multicast (Node) ([`src/multicast.ts`](./src/multicast.ts))

```ts
class MulticastSubscriber {
  constructor(opts: { group: string; port: number; iface?: string; reuseAddr?: boolean });
  on(event: 'record', cb: (rec: IndexRecord) => void): () => void;
  on(event: 'batch',  cb: (recs: IndexRecord[]) => void): () => void;
  on(event: 'raw',    cb: (buf: Uint8Array) => void): () => void;
  on(event: 'error',  cb: (err: Error) => void): () => void;
  on(event: 'listening', cb: () => void): () => void;
  start(): Promise<void>;
  stop():  Promise<void>;
}
```

## Performance

| Path | Approx throughput | Notes |
|---|---|---|
| Pure-TS `decodeIdxRecord` | ~500 K rec/s | DataView, no allocations beyond the result object |
| Pure-TS `decodeIdxBatch`  | ~500 K rec/s | |
| WASM `decode_idx_batch`   | >10 M rec/s | Requires `wasm-pack` build (`npm run build:wasm`); auto-loaded by `decodeIdxBatchFast` when present |
| WS `IndexBatch.mid(i)`    | zero-copy | Float64Array view into the ArrayBuffer |

To opt-in to the WASM fast path:

```ts
import { tryLoadWasm, decodeIdxBatchFast } from '@nxrates/sdk';
await tryLoadWasm();                       // once at startup
const recs = await decodeIdxBatchFast(buf); // uses WASM if loaded
```

## Build

```sh
npm install
npm run build          # tsc + (optional) wasm-pack
npm test               # vitest
```

`wasm-pack` is optional. When absent, `npm run build` emits TypeScript-only artifacts and `decodeIdxBatchFast` falls back to the pure-TS decoder.

To build the WASM accelerator manually:

```sh
cargo install wasm-pack
npm run build:wasm
```

## License

MIT
