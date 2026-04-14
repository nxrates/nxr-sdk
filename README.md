# nxr-sdk

Official multi-language SDK for [NX Rates](https://nxrates.io) market data.

Subscribe to real-time FX/crypto index prices via REST, WebSocket, UDP multicast, or shared-memory ring buffers. All binary framing uses the [MITCH](https://github.com/nxrates/mitch) wire protocol (little-endian, packed structs).

## Languages

| Language | Path | Runtime / Deps | Install |
|----------|------|----------------|---------|
| **TypeScript** | [`ts/`](ts/) | Bun / Node 18+ | `bun add nxr-sdk` |
| **Python** | [`python/`](python/) | 3.11+, aiohttp, websockets | `pip install nxr-sdk` |
| **Go** | [`go/`](go/) | 1.22+, gorilla/websocket | `go get github.com/nxrates/nxr-sdk/go` |
| **Rust** | [`rust/`](rust/) | tokio, reqwest, tungstenite | `cargo add nxr-sdk` |
| **C#** | [`csharp/`](csharp/) | .NET 8+ | `dotnet add package NxrSdk` |
| **Java** | [`java/`](java/) | 17+, Maven | `<artifactId>nxr-sdk</artifactId>` |
| **C** | [`c/`](c/) | C99, header-only | `#include "mitch.h"` |
| **C++** | [`cpp/`](cpp/) | C++17, header-only | `#include "mitch.hpp"` |
| **Zig** | [`zig/`](zig/) | 0.13+ | `@import("mitch.zig")` |

## Wire protocol

All SDKs implement the same MITCH v2 binary format:

```
MitchHeader (16B)
  [0..1]  type_provider  u16 LE  — [3:0]=wire_code, [15:4]=provider_id
  [2..7]  timestamp      u48 LE  — 16us ticks since 2010-01-01
  [8]     count          u8      — batch entry count
  [9]     flags          u8      — [1:0]=version
  [10..11] sequence      u16 LE  — gap detection
  [12..15] reserved      4B
```

Body types: Trade (24B), Tick (32B), Order (32B), Index (40B), Bar (128B), OrderBook (2072B).

Timestamps encode as 48-bit ticks (16us resolution) offset from `2010-01-01T00:00:00Z` (epoch = 1,262,304,000,000,000 us).

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
  [0]    type     u8     — 'i' (index) or 's' (tick)
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
import { NxrClient } from "nxr-sdk";

const nxr = new NxrClient("https://api.nxrates.io");
const symbols = await nxr.symbols();
nxr.stream("wss://ws.nxrates.io/v1/stream", {
  onIndex: (rows) => console.log(rows),
});
```

**Python**
```python
from nxr import NxrClient

async with NxrClient("https://api.nxrates.io") as nxr:
    symbols = await nxr.symbols()
    async for msg in nxr.stream("wss://ws.nxrates.io/v1/stream"):
        print(msg)
```

**Go**
```go
client := nxr.NewClient("https://api.nxrates.io")
symbols, _ := client.Symbols()
client.Stream("wss://ws.nxrates.io/v1/stream", func(idx nxr.WsIndex) {
    fmt.Printf("ticker=%d mid=%.2f\n", idx.Ticker, idx.Mid)
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

## C / C++ / Zig

These are header-only MITCH codec libraries (no network client). Use them to pack/unpack binary frames in your own transport layer, or link against the Rust FFI shared library (`libmitch.so`).

```c
#include "mitch.h"

MitchHeader h;
mitch_header_init(&h, 'i', 101, ts, 1);
// send h + body over UDP / shared memory / etc.
```

## License

MIT — see [LICENSE](LICENSE).
