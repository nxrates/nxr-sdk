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

__version__ = "0.1.0"

__all__ = [
    "__version__",
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
    "decode_idx_bytes",
    "decode_bar_bytes",
    "decode_tick_bytes",
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
