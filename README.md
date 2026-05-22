# nxr-sdk

Official multi-language SDK for [NX Rates](https://nxrates.com) market data.

Subscribe to real-time FX/crypto index prices via REST, WebSocket, or UDP multicast.

MITCH wire types (structs, pack/unpack, timestamps) live in the [mitch](https://github.com/nxrates/mitch) repo. This SDK provides **client/transport** code that depends on and re-exports those types.

## Languages

| Language | Path | Runtime / Deps | Install |
|----------|------|----------------|---------|
| **TypeScript** | [`ts/`](ts/) | Bun / Node 18+ | `bun add @nxrates/sdk` |
| **Python** | [`python/`](python/) | 3.11+, aiohttp, websockets | `pip install nxr-sdk` |
| **Go** | [`go/`](go/) | 1.22+, gorilla/websocket | `go get github.com/nxrates/nxr-sdk/go` |
| **Rust** | [`rust/`](rust/) | tokio, reqwest, tungstenite | `cargo add nxr-sdk` |
| **C#** | [`csharp/`](csharp/) | .NET 8+ | `dotnet add package NxrSdk` |
| **Java** | [`java/`](java/) | 17+, Maven | `<artifactId>nxr-sdk</artifactId>` |

C, C++, and Zig codec implementations are in [`mitch/impl/`](https://github.com/nxrates/mitch/tree/main/impl) (header-only, no client needed).

## Architecture

```
mitch repo (codec)          nxr-sdk repo (client)
├─ impl/rust/               ├─ rust/      → depends on mitch crate
├─ impl/go/                 ├─ go/        → imports mitch Go module
├─ impl/typescript/         ├─ ts/        → depends on @nxrates/mitch
├─ impl/python/             ├─ python/    → depends on nxr-mitch
├─ impl/java/               ├─ java/      → depends on io.mitch
├─ impl/csharp/             ├─ csharp/    → references NxrMitch
├─ impl/c/                  └─ (no client - codec-only)
├─ impl/cpp/
└─ impl/zig/
```

## Transports

| Transport | Latency | Format | Use case |
|-----------|---------|--------|----------|
| REST | ~10ms | JSON | Metadata, snapshots, health |
| WebSocket | ~1ms | Binary f64 frames | Real-time streaming over internet |
| UDP multicast | ~5us | Raw MITCH | Cross-host LAN |

### REST endpoints

Base URL: `https://api.nxrates.com`

```
GET /v1/tickers              -> JSON snapshot: [{ ticker, mid, bid, ask, ci, confidence }]
GET /v1/idx/{sym}            -> composite ticks (56B MITCH IndexRecord stream)
GET /v1/ohlc/{sym}?tf=<s>    -> uniform OHLC bars at timeframe <s> seconds
GET /v1/bars/{sym}/{kind}    -> kind = kline | renko (96B MITCH Bar stream)
GET /v1/synth/tick/{sym}     -> triangulated synthetic tick (e.g. ETH-BTC)
GET /v1/synth/ohlc/{sym}?tf= -> synthetic OHLC
GET /v1/integrity/{sym}      -> data-quality report
GET /health  /metrics
```

Range params on `idx` / `ohlc` / `bars`: `from`, `to` (epoch ms), `limit`, `cursor`.

**Symbol forms** — `{sym}` accepts any of three forms, resolved identically:

| Form | Example | Note |
|------|---------|------|
| Dash | `BTC-USDT` | **Preferred** — URL-safe, no encoding |
| Slash | `BTC%2FUSDT` | Must be percent-encoded |
| MITCH ticker_id | `435315775907037184` or `0x060A8D644C100000` | Numeric, machine-canonical |

**Content negotiation** via the `Accept` header:
- `application/octet-stream` -> raw fixed-stride MITCH records (56B IndexRecord / 96B Bar), zero-copy decodable
- `application/x-ndjson` -> newline-delimited JSON
- default -> JSON array (gzip/br compressed)

### WebSocket framing

Binary frames on `wss://api.nxrates.com/v1/stream`:

```
WS Header (8B)
  [0]    type     u8      1 (index) or 2 (tick)
  [1]    pad      u8
  [2..3] count    u16 LE  number of records
  [4..7] reserved 4B

Body: count x stride f64 values
  Index stride = 9: epoch_ms, ticker, mid, bid, ask, ci, confidence, accepted, rejected
  Tick  stride = 6: epoch_ms, ticker, provider_id, bid, ask, flags
```

## Quick start

The production trifecta — **Rust** (canonical), **Python** (pyo3 + NumPy), and
**TypeScript** (Node + browser) — all decode the identical MITCH wire format,
so a batch fetched in one language decodes bit-identically in the others. Each
ships a runnable demo against the live API in `examples/fetch_demo.*`.

**Python** (`pip install nxr-sdk`)
```python
import nxr_sdk
from nxr_sdk import Client

client = Client(base_url="https://api.nxrates.com")
arr = client.fetch_idx("BTC-USDT", limit=10_000)   # NumPy structured array (raw wire)
ohlc = client.fetch_ohlc("BTC-USDT", tf=60, limit=100)

# Cross-SDK-aligned decoded view (ts_ms / mid / ci_ubp) — same fields + values
# as the Rust and TS SDKs decoding the same /v1/idx octet-stream:
raw = open("snapshot.idx", "rb").read()
dec = nxr_sdk.decode_idx(raw)        # zero-Python-loop, NumPy-vectorized
```

**TypeScript** (`bun add @nxrates/sdk`)
```ts
import { NxrClient } from "@nxrates/sdk";
import { decodeIdxBatch } from "@nxrates/sdk/decode";

const nxr = new NxrClient({ baseUrl: "https://api.nxrates.com" });
const ticks = await nxr.idxBinary("BTC-USDT", { limit: 10_000 }); // IndexRecord[]
const ohlc  = await nxr.ohlc("BTC-USDT", 60, { limit: 100 });
```

**Rust** (`cargo add nxr-sdk`)
```rust
use nxr_sdk::ipc::record::IndexRecord;

// Zero-copy: bytemuck::cast_slice over the 56B fixed-stride octet-stream.
let buf: Vec<u8> = ureq::get("https://api.nxrates.com/v1/idx/BTC-USDT?limit=10000")
    .set("Accept", "application/octet-stream")
    .call()?.into_reader().bytes().collect::<Result<_,_>>()?;
let recs: &[IndexRecord] = bytemuck::cast_slice(&buf);
for r in recs { let _mid = (r.index.bid + r.index.ask) * 0.5; }
```

## License

MIT - see [LICENSE](LICENSE).
