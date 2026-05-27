"""NX Rates Python SDK.

Thin Python facade re-exporting the pyo3-backed :mod:`nxr_sdk._native`
extension. The native module provides:

- :class:`IndexRecord`, :class:`Bar`, :class:`Tick` -- single-record wrappers
  over the canonical MITCH 56 B / 96 B / 32 B wire layouts.
- :func:`decode_idx_bytes`, :func:`decode_bar_bytes`, :func:`decode_tick_bytes`
  -- bulk decode raw octet streams into NumPy structured arrays (zero-copy,
  numpy holds the bytes buffer as the array's base).
- :func:`encode_idx_record`, :func:`encode_bar` -- inverse encoders.
- :func:`resolve_ticker_id` / :func:`resolve_ticker` -- MITCH 64-bit ticker
  encode/decode.
- :class:`MulticastSubscriber` -- UDP multicast subscriber with synchronous
  iterator and ``recv``/``recv_raw`` helpers.
- :class:`Client` -- blocking REST client wrapping ``/v1/idx``,
  ``/v1/ohlc``, ``/v1/bars``, ``/v1/tickers``, ``/v1/providers``.

Quick start -- decode + subscribe::

    import nxr_sdk

    # Bulk decode an .idx blob into a NumPy structured array
    arr = nxr_sdk.decode_idx_bytes(open("snapshot.idx", "rb").read())
    print(arr.shape, arr.dtype.names)

    # Live multicast subscription (blocking)
    with nxr_sdk.MulticastSubscriber("239.0.42.1", 40006) as sub:
        for rec in sub:
            print(rec.ticker, rec.mid, rec.confidence)

    # Historical REST batch
    cli = nxr_sdk.Client("http://nxr.nxrates.com")
    arr = cli.fetch_idx(sym="BTC/USDT", limit=1000)
"""

from __future__ import annotations

from nxr_sdk.ergo import (
    NxrClient,
    WsSubscriber,
    StreamIndexRecord,
    TickersDetailResponse,
    TickerDetail,
    KindSchema,
    ShardWindow,
    SynthLeg,
    DEFAULT_BASE_URL,
    DEFAULT_QUOTE,
    DEFAULT_KIND,
    DEFAULT_INSTRUMENT_TYPE,
    VALID_KINDS,
)
from nxr_sdk.errors import (
    PlanLimitError,
    parse_plan_limit_error,
    plan_limit_error_from_json,
    PLAN_ERROR_DISCRIMINANT,
    PLAN_RATE_LIMIT_HTTP,
    PLAN_RATE_LIMIT_WS,
    PLAN_WS_FEED_CAP,
    PLAN_ENCODING_FORBIDDEN,
    PLAN_TIMEFRAME_FORBIDDEN,
    PLAN_HISTORY_FORBIDDEN,
    PLAN_AUTH_REQUIRED,
    PLAN_KEY_INVALID,
    PLAN_KEY_REVOKED,
)
from nxr_sdk._native import (  # type: ignore[attr-defined]
    # constants
    INDEX_RECORD_SIZE,
    BAR_SIZE,
    HEADER_SIZE,
    INDEX_BODY_SIZE,
    TICK_SIZE,
    EPOCH_MS_2010,
    CI_SCALE,
    # classes
    IndexRecord,
    Bar,
    Tick,
    MulticastSubscriber,
    Client,
    # functions
    decode_idx_bytes,
    decode_bar_bytes,
    decode_tick_bytes,
    encode_idx_record,
    encode_bar,
    index_record_dtype,
    bar_dtype,
    tick_dtype,
    resolve_ticker_id,
    resolve_ticker,
    get_market_provider,
    compute_synth_tick,
)

__version__ = "0.2.0"

__all__ = [
    "__version__",
    # ergonomic layer
    "NxrClient",
    "WsSubscriber",
    "StreamIndexRecord",
    "TickersDetailResponse",
    "TickerDetail",
    "KindSchema",
    "ShardWindow",
    "SynthLeg",
    "DEFAULT_BASE_URL",
    "DEFAULT_QUOTE",
    "DEFAULT_KIND",
    "DEFAULT_INSTRUMENT_TYPE",
    "VALID_KINDS",
    # plan-tier errors
    "PlanLimitError",
    "parse_plan_limit_error",
    "plan_limit_error_from_json",
    "PLAN_ERROR_DISCRIMINANT",
    "PLAN_RATE_LIMIT_HTTP",
    "PLAN_RATE_LIMIT_WS",
    "PLAN_WS_FEED_CAP",
    "PLAN_ENCODING_FORBIDDEN",
    "PLAN_TIMEFRAME_FORBIDDEN",
    "PLAN_HISTORY_FORBIDDEN",
    "PLAN_AUTH_REQUIRED",
    "PLAN_KEY_INVALID",
    "PLAN_KEY_REVOKED",
    # constants
    "INDEX_RECORD_SIZE",
    "BAR_SIZE",
    "HEADER_SIZE",
    "INDEX_BODY_SIZE",
    "TICK_SIZE",
    "EPOCH_MS_2010",
    "CI_SCALE",
    # classes
    "IndexRecord",
    "Bar",
    "Tick",
    "MulticastSubscriber",
    "Client",
    # functions
    "decode_idx",
    "decode_bar",
    "decode_idx_bytes",
    "decode_bar_bytes",
    "decode_tick_bytes",
    "mts_to_ms",
    "encode_idx_record",
    "encode_bar",
    "index_record_dtype",
    "bar_dtype",
    "tick_dtype",
    "resolve_ticker_id",
    "resolve_ticker",
    "get_market_provider",
    "compute_synth_tick",
]


def mts_to_ms(mts_raw: bytes) -> int:
    """Decode a 6-byte little-endian u48 mts header field to unix epoch ms.

    The on-wire timestamp is 16 microsecond ticks since 2010-01-01T00:00:00Z.
    The decode order is:: ``ms = EPOCH_MS_2010 + int.from_bytes(b, 'little') * 16 // 1000``.
    """
    if len(mts_raw) != 6:
        raise ValueError(f"mts_raw must be exactly 6 bytes, got {len(mts_raw)}")
    return EPOCH_MS_2010 + int.from_bytes(mts_raw, "little") * 16 // 1000


def _u48_le_to_ms(raw6):
    """Vectorized: (N, 6) uint8 little-endian u48 ticks -> (N,) int64 epoch ms."""
    import numpy as np

    m = raw6.astype(np.uint64)
    mts = (
        m[:, 0]
        | (m[:, 1] << np.uint64(8))
        | (m[:, 2] << np.uint64(16))
        | (m[:, 3] << np.uint64(24))
        | (m[:, 4] << np.uint64(32))
        | (m[:, 5] << np.uint64(40))
    )
    return (np.int64(EPOCH_MS_2010) + (mts * np.uint64(16) // np.uint64(1000)).astype(np.int64))


# Decoded IndexRecord dtype -- field names and units match the Rust SDK
# (`IndexRecord` accessors) and the TypeScript SDK (`decodeIdxRecord`). This is
# the cross-SDK-aligned view: any of the three SDKs decoding the same
# `/v1/idx` octet-stream yields identical field names and identical values.
_IDX_DECODED_DTYPE = [
    ("ts_ms", "<i8"),
    ("ticker", "<u8"),
    ("bid", "<f8"),
    ("ask", "<f8"),
    ("mid", "<f8"),
    ("vbid", "<u4"),
    ("vask", "<u4"),
    ("ci_ubp", "<u4"),
    ("confidence", "u1"),
    ("accepted", "u1"),
    ("rejected", "u1"),
]


def decode_idx(buf) -> "object":
    """Decode an `/v1/idx` octet-stream into a cross-SDK-aligned NumPy array.

    Unlike :func:`decode_idx_bytes` (raw zero-copy wire layout: ``mts_raw``,
    sqrt-encoded ``ci``), this returns the *decoded* view used by the Rust and
    TypeScript SDKs: ``ts_ms`` (epoch ms), ``mid``, and ``ci_ubp`` (CI in
    micro-basis-points, sqrt-decoded). Vectorized; no Python-level loop.
    """
    import numpy as np

    raw = decode_idx_bytes(buf)
    out = np.zeros(len(raw), dtype=_IDX_DECODED_DTYPE)
    out["ts_ms"] = _u48_le_to_ms(raw["mts_raw"])
    out["ticker"] = raw["ticker"]
    out["bid"] = raw["bid"]
    out["ask"] = raw["ask"]
    out["mid"] = (raw["bid"].astype(np.float64) + raw["ask"].astype(np.float64)) * 0.5
    out["vbid"] = raw["vbid"]
    out["vask"] = raw["vask"]
    # ci is sqrt-encoded: ubp = (ci / CI_SCALE) ** 2
    ci = raw["ci"].astype(np.float64) / float(CI_SCALE)
    out["ci_ubp"] = np.rint(ci * ci).astype(np.uint32)
    out["confidence"] = raw["confidence"]
    out["accepted"] = raw["accepted"]
    out["rejected"] = raw["rejected"]
    return out


def decode_bar(buf) -> "object":
    """Decode a `/v1/bars` octet-stream into a NumPy array with ``open_ms`` /
    ``close_ms`` epoch-ms timestamps (rather than the raw 6-byte u48 fields of
    :func:`decode_bar_bytes`). All microstructure fields are preserved as-is.
    """
    import numpy as np

    raw = decode_bar_bytes(buf)
    base = [("open_ms", "<i8"), ("close_ms", "<i8")]
    rest = [(n, raw.dtype[n].str) for n in raw.dtype.names if n not in ("open_ts", "close_ts")]
    out = np.zeros(len(raw), dtype=base + rest)
    out["open_ms"] = _u48_le_to_ms(raw["open_ts"])
    out["close_ms"] = _u48_le_to_ms(raw["close_ts"])
    for n, _ in rest:
        out[n] = raw[n]
    return out
