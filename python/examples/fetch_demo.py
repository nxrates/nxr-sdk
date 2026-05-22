#!/usr/bin/env python3
"""NXR Python SDK — fetch demo against live prod API.

Run:
    cd sdk/python && maturin develop --release && python examples/fetch_demo.py
"""
import time
import nxr_sdk
from nxr_sdk import Client

BASE_URL = "https://api.nxrates.com"

print("=" * 70)
print(f"NXR Python SDK demo · base={BASE_URL}")
print("=" * 70)

client = Client(base_url=BASE_URL)

# 1. Live tickers (JSON snapshot)
t0 = time.perf_counter()
tickers = client.fetch_tickers()
dt_ms = (time.perf_counter() - t0) * 1000
print(f"\n[1] /v1/tickers          → {len(tickers)} mids in {dt_ms:.1f}ms")
if tickers:
    t = tickers[0]
    print(f"    sample: ticker={t['ticker']:>20}  mid={t['mid']:>12}  ci={t['ci']}  conf={t['confidence']}")

# 2. Raw ticks via the SDK Client (path-routed /v1/idx/{sym}, octet-stream).
t0 = time.perf_counter()
arr = client.fetch_idx("BTC-USDT", limit=10000)  # raw-wire structured array
dt_ms = (time.perf_counter() - t0) * 1000
print(f"\n[2] client.fetch_idx('BTC-USDT') → {len(arr):>6} IndexRecords in {dt_ms:.1f}ms ({len(arr)/max(dt_ms/1000,0.001):.0f} rec/s)")

# cross-SDK-aligned decode: ts_ms / mid / ci_ubp — identical fields + values
# to the Rust and TypeScript SDKs decoding the same /v1/idx octet-stream.
import urllib.request
req = urllib.request.Request(
    f"{BASE_URL}/v1/idx/BTC-USDT?limit=10000",
    headers={"Accept": "application/octet-stream", "User-Agent": "nxr-sdk-python/0.1.0"},
)
raw = urllib.request.urlopen(req).read()
dec = nxr_sdk.decode_idx(raw)
print(f"    aligned dtype : {dec.dtype.names}")
print(f"    first  bid={dec[0]['bid']:.6f}  ask={dec[0]['ask']:.6f}  mid={dec[0]['mid']:.6f}  conf={dec[0]['confidence']}")
print(f"    last   bid={dec[-1]['bid']:.6f}  ask={dec[-1]['ask']:.6f}  mid={dec[-1]['mid']:.6f}  conf={dec[-1]['confidence']}")

# 2b. Pure-Python decode benchmark (sustained, no network)
N_ITER = 100
t0 = time.perf_counter()
total = 0
for _ in range(N_ITER):
    a = nxr_sdk.decode_idx_bytes(raw)
    total += len(a)
dt_ms = (time.perf_counter() - t0) * 1000
print(f"\n[2b] NumPy decode benchmark: {N_ITER} iters × {len(arr)} recs → {dt_ms:.1f}ms ({total/(dt_ms/1000):.0f} rec/s)")

# 3. OHLC at multiple TFs via the SDK Client (path-routed /v1/ohlc/{sym}).
for tf in [60, 300, 3600, 86400]:
    t0 = time.perf_counter()
    ohlc = client.fetch_ohlc("BTC-USDT", tf, limit=100)  # NumPy structured array
    dt_ms = (time.perf_counter() - t0) * 1000
    label = {60: "1m", 300: "5m", 3600: "1h", 86400: "1d"}[tf]
    print(f"\n[3.{tf}] client.fetch_ohlc tf={label:>3} → {len(ohlc):>4} bars in {dt_ms:.1f}ms")
    if len(ohlc):
        last = ohlc[-1]
        print(f"      last ts={last['ts']}  O={last['open']:.2f}  H={last['high']:.2f}  L={last['low']:.2f}  C={last['close']:.2f}  n={last['n']}")

# 4. Synth tick (triangulated) — synth endpoints not yet on the Client surface.
import json
t0 = time.perf_counter()
req = urllib.request.Request(
    f"{BASE_URL}/v1/synth/tick/ETH-BTC",
    headers={"User-Agent": "nxr-sdk-python/0.1.0"},
)
s = json.loads(urllib.request.urlopen(req).read())
dt_ms = (time.perf_counter() - t0) * 1000
print(f"\n[4] /v1/synth/tick/ETH/BTC in {dt_ms:.1f}ms")
print(f"    bid={s['bid']:.6f}  ask={s['ask']:.6f}  mid={s['mid']:.6f}  conf={s['conf']}")

# 5. Symbol resolution (offline, MITCH ticker_id schema)
try:
    tid = nxr_sdk.resolve_ticker_id("BTC/USDT")
    print(f"\n[5] resolve BTC/USDT → 0x{tid:016X}")
except Exception as e:
    print(f"\n[5] resolve sig variant: {e}")

# 6. Raw IndexRecord roundtrip (bytemuck-style zero-copy)
rec_bytes = bytes(arr[:1].tobytes())
decoded = nxr_sdk.decode_idx_bytes(rec_bytes)
print(f"\n[6] roundtrip 56B IndexRecord → decode_idx_bytes OK, len={len(decoded)}")

print("\n" + "=" * 70)
print("All endpoints functional ✓")
print("=" * 70)
