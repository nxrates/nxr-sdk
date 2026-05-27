# nxr-sdk (Python)

Official Python SDK for [NX Rates](https://nxrates.com). Pyo3 wrapper over the
canonical Rust SDK at `sdk/rust`, sharing the exact same MITCH wire types so
quant clients can decode multicast frames + `.idx` files at native speed.

## Install

```bash
# From source (arm64 macOS / Linux)
pip install maturin
cd sdk/python
maturin develop --release

# From PyPI (once published)
pip install nxr-sdk
```

For the WebSocket subscriber, also install `websockets`:

```bash
pip install websockets
```

## 60 seconds to data

```python
import nxr_sdk

# Defaults to https://api.nxrates.com — pass a base_url to override.
# Optional: api_key= bypasses per-IP rate limits (see ../docs/api-plans.md).
nxr = nxr_sdk.NxrClient(api_key=os.environ.get("NXR_API_KEY"))

# Universal integrator inventory (typed dataclass + cached).
detail = nxr.tickers_detail()
print(detail.count, "tickers; idx cadence =", detail.idx_aggregation_ms, "ms")
btc = detail.by_ticker("BTC/USDT")
print(btc.ticker_id, btc.kinds["idx"].shards.first_date)

# Historical fetch — object form (smart defaults: quote=USDT, kind=renko).
bars = nxr.history(base="BTC", limit=500)  # → numpy structured array

# Chainable form.
bars = nxr.get().history().pair("ETH/USDC").renko().limit(500).fetch()

# Real-time stream (requires `pip install websockets`).
import asyncio

async def consume():
    async with nxr.subscribe(["BTC/USDT", "ETH/USDT"]) as sub:
        async for rec in sub:
            print(rec.ts_ms, rec.ticker, rec.bid, rec.ask, rec.ci_ubp)

asyncio.run(consume())
```

## What you get

### `NxrClient` (high-level)

| Method                              | Purpose                                                       |
| ----------------------------------- | ------------------------------------------------------------- |
| `nxr.tickers_detail(refresh=False)` | `/v1/tickers/detail` → `TickersDetailResponse` dataclass      |
| `nxr.resolve_ticker_id(sym)`        | "BTC/USDT" → MITCH ticker_id (cached)                         |
| `nxr.history(...)` / `nxr.get()...` | Unified historical fetch (idx / kline / renko)                |
| `nxr.subscribe([syms])`             | Async WebSocket subscriber over `/v1/stream`                  |
| `nxr.fetch_idx(sym, ...)`           | Raw pyo3 primitive — octet-stream → NumPy structured array    |
| `nxr.fetch_bars(sym, kind, ...)`    | Raw pyo3 primitive — 96 B Bars                                |
| `nxr.fetch_ohlc(sym, tf, ...)`      | Legacy JSON `/v1/ohlc`                                        |
| `nxr.fetch_tickers()`               | All-ticker JSON snapshot                                      |
| `nxr.fetch_providers()`             | provider_id → name                                            |
| `nxr.fetch_symbols()`               | symbol → ticker_id + synth paths                              |

### Typed `tickers_detail()` view

```python
@dataclass
class TickersDetailResponse:
    idx_aggregation_ms: int
    count: int
    tickers: list[TickerDetail]
    raw: dict           # original parsed JSON
    def by_ticker(s): ...

@dataclass
class TickerDetail:
    ticker_id: int      # 0 for synths
    ticker: str         # "BTC/USDT"
    base: str           # "BTC"
    quote: str          # "USDT"
    base_class: str     # "CR" | "FX" | ...
    quote_class: str
    instrument_type: str
    native: bool
    synth_legs: list[SynthLeg] | None
    kinds: dict[str, KindSchema]   # "idx" | "kline" | "renko"

@dataclass
class KindSchema:
    fields: list[str]
    stride_bytes: int
    shards: ShardWindow             # first_date / last_date / count
```

### Pyo3 primitives (raw / power-user)

| API                            | Purpose                                                                |
| ------------------------------ | ---------------------------------------------------------------------- |
| `decode_idx_bytes(buf)`        | Bulk decode 56 B `IndexRecord` blob → NumPy structured array           |
| `decode_bar_bytes(buf)`        | Bulk decode 96 B `Bar` blob → NumPy structured array                   |
| `decode_tick_bytes(buf)`       | Bulk decode 32 B `Tick` blob → NumPy structured array                  |
| `encode_idx_record(dict)`      | Encode an `IndexRecord` dict → 56 B wire bytes                         |
| `encode_bar(dict)`             | Encode a `Bar` dict → 96 B wire bytes                                  |
| `IndexRecord`/`Bar`/`Tick`     | PyClass wrappers around a single decoded sample                        |
| `MulticastSubscriber`          | Blocking + sync-iterable UDP multicast subscriber                      |
| `Client`                       | Blocking REST client wrapped by `NxrClient`                            |
| `resolve_ticker_id(sym)`       | Symbol → 64-bit MITCH ticker id                                        |
| `resolve_ticker(id)`           | 64-bit id → `(base, quote, instrument_type)`                           |
| `compute_synth_tick(...)`      | Off-line synth tick composition                                        |

## Real-time multicast (Node-LAN deployments)

```python
import nxr_sdk

with nxr_sdk.MulticastSubscriber("239.0.42.1", 40006) as sub:
    for rec in sub:
        print(rec.ts_ms, rec.ticker, rec.mid, "conf=", rec.confidence)
```

## Decode a `.idx` file directly

```python
import nxr_sdk

with open("/data/nxr/snapshots/btc_usdt.idx", "rb") as f:
    arr = nxr_sdk.decode_idx_bytes(f.read())

# Vectorized mts→ms (helper from the public surface):
ms = nxr_sdk._u48_le_to_ms(
    arr["mts_raw"].view("u1").reshape(-1, 6)
)
```

## Performance

- `decode_idx_bytes` reinterprets the input bytes object via `numpy.frombuffer`
  (zero-copy; NumPy holds the bytes as the array's base). Target: > 1 M
  records / s end-to-end for `len(buf) > 1 MB`.
- `MulticastSubscriber` runs a dedicated OS reader thread that pushes raw
  datagrams into an mpsc queue; the Python iterator pops + decodes only on
  consume. Target: < 10 µs added jitter on top of the kernel UDP recv path.
- `WsSubscriber` decodes binary frames in pure Python via `struct`; ~200 k
  records/s end-to-end (the 100 ms server flush keeps frame counts small).
- Build with `maturin develop --release` for production benchmarks; debug
  builds are ~10x slower.

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
SDK parses that shape into a typed `PlanLimitError` so callers can branch on
`code` rather than regexing English `message` strings.

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

```python
import nxr_sdk
from nxr_sdk.errors import PlanLimitError

nxr = nxr_sdk.NxrClient(base_url="https://api.nxrates.com")

try:
    arr = nxr.fetch_idx("BTC-USDT", limit=1000)
except PlanLimitError as e:
    print(f"{e.code}: {e.message}")
    if e.is_upgrade_needed():
        print(f"Upgrade → {e.upgrade_url}")
    elif e.is_rate_limit():
        pass  # back off and retry
    elif e.is_auth_error():
        pass  # refresh / rotate API key
```

Runnable example: [`examples/plan_aware.py`](./examples/plan_aware.py) —
`python examples/plan_aware.py`.

## MITCH spec

Wire layout source-of-truth: `nx-rates/mitch/impl/rust/src/{header,index,bar,tick}.rs`.
This SDK mirrors those layouts byte-for-byte (see `index_record_dtype()` and
`bar_dtype()` for the NumPy view).

## License

MIT
