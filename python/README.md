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

## What you get

| API                          | Purpose                                                                |
|------------------------------|------------------------------------------------------------------------|
| `decode_idx_bytes(buf)`      | Bulk decode 56 B `IndexRecord` blob → NumPy structured array           |
| `decode_bar_bytes(buf)`      | Bulk decode 96 B `Bar` blob → NumPy structured array                   |
| `decode_tick_bytes(buf)`     | Bulk decode 32 B `Tick` blob → NumPy structured array                  |
| `encode_idx_record(dict)`    | Encode an `IndexRecord` dict → 56 B wire bytes                         |
| `encode_bar(dict)`           | Encode a `Bar` dict → 96 B wire bytes                                  |
| `IndexRecord` / `Bar` / `Tick` | PyClass wrappers around a single decoded sample                       |
| `MulticastSubscriber`        | Blocking + sync-iterable UDP multicast subscriber                      |
| `Client`                     | Blocking REST client (`/v1/idx`, `/v1/ohlc`, `/v1/bars`, ...)          |
| `resolve_ticker_id(sym)`     | Canonical symbol string → 64-bit MITCH ticker id                       |
| `resolve_ticker(id)`         | 64-bit ticker id → `(base, quote, instrument_type)`                    |
| `compute_synth_tick(...)`    | Off-line synth tick composition (multiplicative signed-leg paths)      |

## Quick start

### Real-time multicast subscribe

```python
import nxr_sdk

with nxr_sdk.MulticastSubscriber("239.0.42.1", 40006) as sub:
    for rec in sub:
        print(rec.ts_ms, rec.ticker, rec.mid, "conf=", rec.confidence)
```

### Historical REST batch

```python
import nxr_sdk

cli = nxr_sdk.Client("http://nxr.nxrates.com")

# .idx octet-stream -> NumPy structured array (zero-copy)
arr = cli.fetch_idx(sym="BTC/USDT", limit=1000)
print(arr.dtype.names)
# ('type_provider', 'mts_raw', 'count', 'flags', 'sequence',
#  'ticker', 'bid', 'ask', 'vbid', 'vask', 'ci', 'tick_count',
#  'confidence', 'accepted', 'rejected', 'flags_body')

# OHLC JSON -> NumPy
candles = cli.fetch_ohlc(sym="BTC/USDT", tf=60, limit=500)
print(candles[["ts", "open", "high", "low", "close"]][:5])
```

### Decode a `.idx` file directly

```python
import nxr_sdk

with open("/data/nxr/snapshots/btc_usdt.idx", "rb") as f:
    arr = nxr_sdk.decode_idx_bytes(f.read())

# arr is a 1-D structured numpy array. Convert mts_raw -> unix-ms:
import numpy as np
mts = arr["mts_raw"].view(np.uint8).reshape(-1, 6)
ms = nxr_sdk.EPOCH_MS_2010 + (
    mts[:, 0].astype(np.int64)
    | (mts[:, 1].astype(np.int64) << 8)
    | (mts[:, 2].astype(np.int64) << 16)
    | (mts[:, 3].astype(np.int64) << 24)
    | (mts[:, 4].astype(np.int64) << 32)
    | (mts[:, 5].astype(np.int64) << 40)
) * 16 // 1000
```

### Encode + round-trip

```python
import nxr_sdk

raw = nxr_sdk.encode_idx_record({
    "ts_ms": 1_700_000_000_000,
    "provider": 102,
    "ticker": 0xDEADBEEF,
    "bid": 50_000.0,
    "ask": 50_010.0,
    "vbid": 100,
    "vask": 110,
    "ci": 16,
    "tick_count": 42,
    "confidence": 3,
    "accepted": 3,
    "rejected": 0,
})
assert len(raw) == nxr_sdk.INDEX_RECORD_SIZE  # 56
arr = nxr_sdk.decode_idx_bytes(raw)
assert arr[0]["bid"] == 50_000.0
```

## Performance

- `decode_idx_bytes` reinterprets the input bytes object via `numpy.frombuffer`
  (zero-copy; NumPy holds the bytes as the array's base). Target: > 1 M
  records / s end-to-end for `len(buf) > 1 MB`.
- `MulticastSubscriber` runs a dedicated OS reader thread that pushes raw
  datagrams into an mpsc queue; the Python iterator pops + decodes only on
  consume. Target: < 10 us added jitter on top of the kernel UDP recv path.
- Build with `maturin develop --release` for production benchmarks; debug
  builds are ~10x slower.

## MITCH spec

Wire layout source-of-truth: `nx-rates/mitch/impl/rust/src/{header,index,bar,tick}.rs`.
This SDK mirrors those layouts byte-for-byte (see `index_record_dtype()` and
`bar_dtype()` for the NumPy view).

## License

MIT
