"""Unit tests for the pyo3-backed nxr_sdk decoders, classes, and subscriber."""

from __future__ import annotations

import socket
import time

import numpy as np
import pytest

import nxr_sdk


# ──────────────────────────────────────────────────────────────────────
# IndexRecord encode/decode round-trip
# ──────────────────────────────────────────────────────────────────────

def test_index_record_size_constants():
    assert nxr_sdk.INDEX_RECORD_SIZE == 56
    assert nxr_sdk.HEADER_SIZE == 16
    assert nxr_sdk.INDEX_BODY_SIZE == 40
    assert nxr_sdk.BAR_SIZE == 96
    assert nxr_sdk.TICK_SIZE == 32


def test_index_record_dtype_layout():
    dt = nxr_sdk.index_record_dtype()
    assert dt.itemsize == 56
    # Spot check critical field offsets.
    assert dt.fields["ticker"][1] == 16
    assert dt.fields["bid"][1] == 24
    assert dt.fields["ask"][1] == 32
    assert dt.fields["ci"][1] == 48
    assert dt.fields["confidence"][1] == 52
    assert dt.fields["accepted"][1] == 53
    assert dt.fields["rejected"][1] == 54


def test_encode_decode_round_trip():
    rec = {
        "ts_ms": 1_700_000_000_000,
        "provider": 102,
        "ticker": 0xDEADBEEFCAFE,
        "bid": 50_000.0,
        "ask": 50_010.0,
        "vbid": 100,
        "vask": 110,
        "ci": 16,
        "tick_count": 42,
        "confidence": 3,
        "accepted": 3,
        "rejected": 0,
        "sequence": 7,
    }
    raw = nxr_sdk.encode_idx_record(rec)
    assert len(raw) == 56

    arr = nxr_sdk.decode_idx_bytes(raw)
    assert arr.shape == (1,)
    assert arr.dtype.itemsize == 56
    row = arr[0]
    assert int(row["ticker"]) == 0xDEADBEEFCAFE
    assert row["bid"] == 50_000.0
    assert row["ask"] == 50_010.0
    assert int(row["vbid"]) == 100
    assert int(row["vask"]) == 110
    assert int(row["ci"]) == 16
    assert int(row["tick_count"]) == 42
    assert int(row["confidence"]) == 3
    assert int(row["accepted"]) == 3
    assert int(row["rejected"]) == 0
    assert int(row["sequence"]) == 7
    # Provider id occupies bits 4..16 of type_provider.
    assert (int(row["type_provider"]) >> 4) == 102

    # Time round-trip via the helper.
    ms = nxr_sdk.mts_to_ms(bytes(row["mts_raw"]))
    # u48 mts encodes 16 us ticks; expect lossless ms.
    assert abs(ms - 1_700_000_000_000) <= 1


def test_decode_idx_batch_alignment():
    # Encode 100 records, decode all at once, verify per-row fields.
    blob = b""
    for i in range(100):
        blob += nxr_sdk.encode_idx_record({
            "ts_ms": 1_700_000_000_000 + i * 1_000,
            "provider": 1,
            "ticker": 1000 + i,
            "bid": 100.0 + i,
            "ask": 100.1 + i,
        })
    assert len(blob) == 100 * 56
    arr = nxr_sdk.decode_idx_bytes(blob)
    assert arr.shape == (100,)
    np.testing.assert_array_equal(arr["ticker"], np.arange(1000, 1100, dtype=np.uint64))
    np.testing.assert_allclose(arr["bid"], np.arange(100.0, 200.0))


def test_decode_idx_bad_length():
    with pytest.raises(ValueError, match="multiple"):
        nxr_sdk.decode_idx_bytes(b"\x00" * 55)


# ──────────────────────────────────────────────────────────────────────
# Bar encode/decode
# ──────────────────────────────────────────────────────────────────────

def test_bar_round_trip():
    raw = nxr_sdk.encode_bar({
        "open_ms": 1_700_000_000_000,
        "close_ms": 1_700_000_060_000,
        "open": 100.0,
        "high": 105.0,
        "low": 99.0,
        "close": 103.0,
        "vbid": 1000,
        "vask": 1200,
        "tick_count": 50,
    })
    assert len(raw) == 96
    arr = nxr_sdk.decode_bar_bytes(raw)
    assert arr.shape == (1,)
    row = arr[0]
    assert row["open"] == 100.0
    assert row["high"] == 105.0
    assert row["low"] == 99.0
    assert row["close"] == 103.0
    assert int(row["vbid"]) == 1000
    assert int(row["vask"]) == 1200
    assert int(row["tick_count"]) == 50


def test_bar_class_accessors():
    b = nxr_sdk.Bar(
        open_ms=1_700_000_000_000,
        close_ms=1_700_000_060_000,
        open=100.0,
        high=105.0,
        low=99.0,
        close=103.0,
        vbid=1000,
        vask=1200,
        tick_count=50,
    )
    assert b.open == 100.0
    assert b.close == 103.0
    assert b.tick_count == 50
    assert abs(b.open_ms - 1_700_000_000_000) <= 1
    assert abs(b.close_ms - 1_700_000_060_000) <= 1
    raw = b.to_bytes()
    assert len(raw) == 96


# ──────────────────────────────────────────────────────────────────────
# IndexRecord pyclass
# ──────────────────────────────────────────────────────────────────────

def test_index_record_class():
    r = nxr_sdk.IndexRecord(
        ts_ms=1_700_000_000_000,
        provider=42,
        ticker=12345,
        bid=1.0,
        ask=1.1,
        ci=8,
        accepted=2,
        rejected=0,
        confidence=2,
    )
    assert r.ticker == 12345
    assert r.provider == 42
    assert r.bid == 1.0
    assert r.ask == 1.1
    assert abs(r.mid - 1.05) < 1e-12
    assert abs(r.ts_ms - 1_700_000_000_000) <= 1
    assert len(r.to_bytes()) == 56


# ──────────────────────────────────────────────────────────────────────
# Ticker resolve
# ──────────────────────────────────────────────────────────────────────

def test_resolve_ticker_id_roundtrip_crypto():
    tid = nxr_sdk.resolve_ticker_id("BTC/USDT")
    assert tid != 0
    base, quote, it = nxr_sdk.resolve_ticker(tid)
    # MITCH resolver may capitalise differently; just check non-empty.
    assert base
    assert quote
    assert it == "SPOT"


def test_try_resolve_ticker_id_strict_vs_lenient():
    assert nxr_sdk.try_resolve_ticker_id("BTC/USDT") == nxr_sdk.resolve_ticker_id("BTC/USDT")
    assert nxr_sdk.try_resolve_ticker_id("ZZZQQQ/NOPE") is None
    # lenient path still hands back an FNV phantom that reverses to hex/empty
    base, quote, _ = nxr_sdk.resolve_ticker(nxr_sdk.resolve_ticker_id("ZZZQQQ/NOPE"))
    assert base.startswith("0x") and quote == ""


def test_exact_ticker_beats_fuzzy():
    base, quote, _ = nxr_sdk.resolve_ticker(nxr_sdk.try_resolve_ticker_id("BRK-B/USD"))
    assert "BERKSHIRE" in base
    assert "DOLLAR" in quote


# ──────────────────────────────────────────────────────────────────────
# Synth tick composition
# ──────────────────────────────────────────────────────────────────────

def test_compute_synth_tick_two_leg_cross():
    # ETH/BTC = ETH/USDT * 1/(BTC/USDT) = (3000 / 60000) = 0.05
    out = nxr_sdk.compute_synth_tick(
        legs=[("ETH/USDT", 1), ("BTC/USDT", -1)],
        snapshots={
            "ETH/USDT": (2999.0, 3001.0, 3000.0, 10_000),
            "BTC/USDT": (59_990.0, 60_010.0, 60_000.0, 10_000),
        },
    )
    assert out is not None
    assert abs(out["mid"] - (3000.0 / 60_000.0)) < 1e-12
    # bid uses 1/ask for the inverted leg to keep bid <= ask.
    expected_bid = 2999.0 / 60_010.0
    expected_ask = 3001.0 / 59_990.0
    assert abs(out["bid"] - expected_bid) < 1e-12
    assert abs(out["ask"] - expected_ask) < 1e-12
    assert out["conf"] == 10_000


def test_compute_synth_tick_missing_leg_returns_none():
    out = nxr_sdk.compute_synth_tick(
        legs=[("ETH/USDT", 1)],
        snapshots={},
    )
    assert out is None


# ──────────────────────────────────────────────────────────────────────
# Multicast subscriber: loopback round-trip
# ──────────────────────────────────────────────────────────────────────

@pytest.mark.skipif(
    not hasattr(socket, "IP_MULTICAST_LOOP"),
    reason="multicast loopback unavailable",
)
def test_multicast_subscriber_loopback():
    """Spin up a local sender, subscribe on the same group, verify decode."""
    group = "239.7.7.7"
    port = 0  # let kernel pick

    # We need a fixed port both sides agree on. Bind a probe socket to discover.
    probe = socket.socket(socket.AF_INET, socket.SOCK_DGRAM)
    probe.bind(("0.0.0.0", 0))
    port = probe.getsockname()[1]
    probe.close()

    # Sender setup.
    snd = socket.socket(socket.AF_INET, socket.SOCK_DGRAM)
    snd.setsockopt(socket.IPPROTO_IP, socket.IP_MULTICAST_LOOP, 1)
    snd.setsockopt(socket.IPPROTO_IP, socket.IP_MULTICAST_TTL, 1)

    # Subscriber.
    sub = nxr_sdk.MulticastSubscriber(group, port)

    # Give the joined socket a moment to settle, then publish a single frame.
    time.sleep(0.05)

    payload = nxr_sdk.encode_idx_record({
        "ts_ms": 1_700_000_000_000,
        "provider": 99,
        "ticker": 424242,
        "bid": 12.34,
        "ask": 12.45,
        "accepted": 1,
        "rejected": 0,
        "confidence": 1,
    })
    snd.sendto(payload, (group, port))
    snd.close()

    rec = sub.recv(timeout=2.0)
    sub.close()

    assert rec is not None, "no frame received on multicast loopback"
    assert rec.ticker == 424242
    assert rec.provider == 99
    assert rec.bid == 12.34
    assert rec.ask == 12.45
