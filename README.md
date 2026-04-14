# nxr-sdk

Official multi-language SDK for [NX Rates](https://nxrates.io) market data.

Subscribe to real-time FX/crypto index prices via REST, WebSocket, UDP multicast, or shared-memory ring buffers.

MITCH wire types (structs, pack/unpack, timestamps) live in the [mitch](https://github.com/nxrates/mitch) repo. This SDK provides **client/transport** code that depends on and re-exports those types.

## Languages

| Language | Path | Runtime / Deps | Install |
|----------|------|----------------|---------|
| **TypeScript** | [`ts/`](ts/) | Bun / Node 18+ | `bun add @nxr/sdk` |
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
├─ impl/c/                  └─ (no client — codec-only)
├─ impl/cpp/
└─ impl/zig/
```

## Transports

| Transport | Latency | Format | Use case |
|-----------|---------|--------|----------|
| REST | ~10ms | JSON | Metadata, snapshots, health |
| WebSocket | ~1ms | Binary f64 frames | Real-time streaming over internet |
| UDP multicast | ~5us | Raw MITCH | Cross-host LAN |
| MmapRing | ~100ns | Raw MITCH | Same-host shared memory |

### REST endpoints

```
GET /v1/symbols    -> { "BTC/USDT": 1, "ETH/USDT": 2, ... }
GET /v1/providers  -> { "1": "binance", "2": "coinbase", ... }
GET /v1/tickers    -> [{ ticker_id, bid, ask, ... }]
GET /health        -> 200 OK
```

### WebSocket framing

Binary frames on `wss://ws.nxrates.io/v1/stream`:

```
WS Header (8B)
  [0]    type     u8     — 1 (index) or 2 (tick)
  [1]    pad      u8
  [2..3] count    u16 LE — number of records
  [4..7] reserved 4B

Body: count x stride f64 values
  Index stride = 9: ts_ms, ticker, mid, bid, ask, ci, confidence, accepted, rejected
  Tick  stride = 6: ts_ms, ticker, provider_id, bid, ask, flags
```

## Quick start

**TypeScript**
```ts
import { NxrClient, IndexBatch } from "@nxr/sdk";

const nxr = new NxrClient("http://nxr-svc:40004");
const btc = await nxr.resolve("BTC/USDT");

nxr.onIndex((batch: IndexBatch) => {
  for (let i = 0; i < batch.count; i++) {
    console.log(`ticker=${batch.ticker(i)} mid=${batch.mid(i)}`);
  }
});
nxr.connect();
```

**Python**
```python
from nxr import NxrClient

client = NxrClient("http://localhost:40000")
symbols = await client.symbols()
await client.stream(on_index=lambda recs: print(recs))
```

**Go**
```go
client := nxr.NewClient("http://localhost:40000")
symbols, _ := client.Symbols(ctx)
client.Stream(ctx, func(idx []nxr.WsIndex) {
    fmt.Printf("ticker=%.0f mid=%.2f\n", idx[0].Ticker, idx[0].Mid)
}, nil)
```

**Rust**
```rust
let nxr = nxr_sdk::NxrClient::new("https://api.nxrates.io");
let symbols = nxr.symbols().await?;
let mut ws = nxr_sdk::WsStream::connect("wss://ws.nxrates.io/v1/stream").await?;
while let Some(msg) = ws.recv().await {
    println!("{:?}", msg);
}
```

## License

MIT — see [LICENSE](LICENSE).
