"""Tests for the ergonomic layer (`NxrClient`, `WsSubscriber`, typed detail).

These tests run entirely offline:
- `tickers_detail()` is exercised against a `urllib.request.urlopen` mock so no
  network call is made.
- WS decode round-trips through `_decode_index_frame` against a synthetic
  frame built byte-exact to the wire protocol.
- `NxrClient.history()` smart defaults / parsing are exercised against a stub
  `Client` that records each invocation.
"""

from __future__ import annotations

import io
import json
import struct
from typing import Any
from unittest import mock

import pytest

from nxr_sdk import ergo


# ── Helpers ─────────────────────────────────────────────────────────────


def _mock_urlopen_json(payload: dict[str, Any]):
    """Return a context-manager factory that yields a urlopen-like response."""
    body = json.dumps(payload).encode("utf-8")

    class _Resp:
        def __enter__(self_inner):
            return self_inner

        def __exit__(self_inner, *exc):
            return False

        def read(self_inner):
            return body

    def _factory(*_args, **_kwargs):
        return _Resp()

    return _factory


# ── tickers_detail() typed dataclass round-trip ──────────────────────────


def test_tickers_detail_parses_into_dataclass(monkeypatch):
    sample = {
        "idx_aggregation_ms": 100,
        "count": 2,
        "tickers": [
            {
                "ticker_id": 435315551398526976,
                "ticker": "BTC/USDT",
                "base": "BTC",
                "quote": "USDT",
                "base_class": "CR",
                "quote_class": "CR",
                "instrument_type": "SPOT",
                "native": True,
                "kinds": {
                    "idx": {
                        "fields": ["ts", "ticker"],
                        "stride_bytes": 56,
                        "shards": {
                            "first_date": "2025-01-01",
                            "last_date": "2025-01-31",
                            "count": 31,
                        },
                    }
                },
            },
            {
                "ticker_id": 0,
                "ticker": "ETH-BTC",
                "base": "ETH",
                "quote": "BTC",
                "base_class": "",
                "quote_class": "",
                "instrument_type": "SPOT",
                "native": False,
                "synth_legs": [
                    {"sym": "ETH/USDT", "exp": 1},
                    {"sym": "BTC/USDT", "exp": -1},
                ],
                "kinds": {},
            },
        ],
    }
    # We patch urllib at module-import boundary used by _fetch_detail.
    monkeypatch.setattr(
        "urllib.request.urlopen",
        _mock_urlopen_json(sample),
    )
    # Avoid hitting the pyo3 Client constructor by stubbing it.
    with mock.patch("nxr_sdk._native.Client") as mock_client:
        mock_client.return_value = mock.MagicMock()
        c = ergo.NxrClient("http://nxr")
        detail = c.tickers_detail()

    assert detail.idx_aggregation_ms == 100
    assert detail.count == 2
    assert len(detail.tickers) == 2
    t0 = detail.tickers[0]
    assert t0.ticker == "BTC/USDT"
    assert t0.ticker_id == 435315551398526976
    assert t0.base_class == "CR"
    assert t0.native is True
    assert t0.kinds["idx"].stride_bytes == 56
    assert t0.kinds["idx"].shards.first_date == "2025-01-01"
    assert t0.kinds["idx"].shards.count == 31

    t1 = detail.tickers[1]
    assert t1.native is False
    assert t1.synth_legs is not None
    assert t1.synth_legs[0].sym == "ETH/USDT"
    assert t1.synth_legs[0].exp == 1

    # by_ticker helper
    assert detail.by_ticker("BTC/USDT") is t0
    assert detail.by_ticker("ZZZ/USDT") is None

    # raw dict still accessible
    assert detail.raw == sample


# ── Single-ticker lookup + packed catalogue ──────────────────────────────


@pytest.mark.parametrize(
    "ident,segment",
    [
        ("435315556536549376", "435315556536549376"),
        ("BTC/USD", "BTC-USD"),
        ("BTC-USD", "BTC-USD"),
        ("CR:BTC/FX:USD", "CR%3ABTC-FX%3AUSD"),
    ],
)
def test_ticker_detail_spells_every_identifier_form(monkeypatch, ident, segment):
    """All three identifier forms must reach the server intact: a mangled path
    segment would silently look up a different pair (or 404)."""
    row = {
        "ticker_id": 435315556536549376,
        "ticker": "BTC/USD",
        "base": "BTC",
        "quote": "USD",
        "base_class": "CR",
        "quote_class": "FX",
        "instrument_type": "SPOT",
        "native": False,
        "synth_legs": [{"sym": "BTC/USDT", "exp": 1}],
    }
    seen: dict[str, Any] = {}

    def _factory(req, *_args, **_kwargs):
        seen["url"] = req.full_url
        return _mock_urlopen_json(row)()

    monkeypatch.setattr("urllib.request.urlopen", _factory)
    with mock.patch("nxr_sdk._native.Client") as mock_client:
        mock_client.return_value = mock.MagicMock()
        t = ergo.NxrClient("http://nxr").ticker_detail(ident)

    assert seen["url"] == f"http://nxr/v1/tickers/detail/{segment}"
    assert t.ticker_id == 435315556536549376
    assert t.native is False
    # A derived row owns no shards, so the server omits `kinds` entirely.
    assert t.kinds == {}
    assert t.synth_legs is not None and t.synth_legs[0].sym == "BTC/USDT"


def test_decode_packed_ids_round_trip():
    ids = [435315551398526976, 1, 2**64 - 1]
    buf = b"".join(struct.pack("<Q", i) for i in ids)
    assert ergo.decode_packed_ids(buf) == ids
    assert ergo.decode_packed_ids(b"") == []
    # A truncated body must fail loudly: a silently dropped tail is a catalogue
    # that quietly under-reports the universe.
    with pytest.raises(ValueError):
        ergo.decode_packed_ids(buf[:-1])


def test_tickers_detail_caches(monkeypatch):
    sample = {"idx_aggregation_ms": 50, "count": 0, "tickers": []}
    call_count = {"n": 0}

    def _factory(*_args, **_kwargs):
        call_count["n"] += 1

        class _R:
            def __enter__(self_inner):
                return self_inner

            def __exit__(self_inner, *exc):
                return False

            def read(self_inner):
                return json.dumps(sample).encode("utf-8")

        return _R()

    monkeypatch.setattr("urllib.request.urlopen", _factory)

    with mock.patch("nxr_sdk._native.Client") as mock_client:
        mock_client.return_value = mock.MagicMock()
        c = ergo.NxrClient("http://nxr")
        c.tickers_detail()
        c.tickers_detail()  # cached
        assert call_count["n"] == 1
        c.tickers_detail(refresh=True)
        assert call_count["n"] == 2


# ── history() smart defaults + chainable form ───────────────────────────


def test_history_smart_defaults():
    stub = mock.MagicMock()
    stub.fetch_bars.return_value = "ok"
    with mock.patch("nxr_sdk._native.Client", return_value=stub):
        c = ergo.NxrClient("http://nxr")
        out = c.history(base="BTC")  # quote→USDT, kind→renko
        stub.fetch_bars.assert_called_once_with(
            sym="BTC/USDT",
            kind="renko",
            from_ms=None,
            to_ms=None,
            limit=None,
        )
        assert out == "ok"


def test_history_chainable_matches_object_form():
    stub = mock.MagicMock()
    stub.fetch_idx.return_value = "idx-data"
    with mock.patch("nxr_sdk._native.Client", return_value=stub):
        c = ergo.NxrClient("http://nxr")
        # Object form
        a = c.history(ticker="ETH/USDC", kind="idx", limit=10)
        # Chainable
        b = c.get().history().pair("ETH/USDC").limit(10).idx()
        assert a == b == "idx-data"
        assert stub.fetch_idx.call_count == 2
        # Both calls used the same args.
        calls = stub.fetch_idx.call_args_list
        assert calls[0] == calls[1]


def test_history_ticker_overrides_implied_quote():
    stub = mock.MagicMock()
    stub.fetch_bars.return_value = None
    with mock.patch("nxr_sdk._native.Client", return_value=stub):
        c = ergo.NxrClient("http://nxr")
        # ticker="BTC/USDT" → (BTC, USDT); explicit quote="USDC" wins.
        c.history(ticker="BTC/USDT", quote="USDC", kind="renko")
        stub.fetch_bars.assert_called_once_with(
            sym="BTC/USDC",
            kind="renko",
            from_ms=None,
            to_ms=None,
            limit=None,
        )


def test_history_requires_ticker_or_base():
    stub = mock.MagicMock()
    with mock.patch("nxr_sdk._native.Client", return_value=stub):
        c = ergo.NxrClient("http://nxr")
        with pytest.raises(ValueError, match="ticker= or base="):
            c.history()


# ── WS frame decoder ────────────────────────────────────────────────────


def _build_ws_frame(records: list[tuple[float, ...]]) -> bytes:
    """Build a binary WS frame matching `core::server::ws::build_index_frame`."""
    # 8B header: msg_type=1, _pad=0, count_le u16, _pad u32
    header = bytes([1, 0]) + struct.pack("<H", len(records)) + bytes(4)
    body = b"".join(struct.pack("<9d", *r) for r in records)
    return header + body


def test_decode_index_frame_single():
    rec = (1_700_000_000_000.0, 42.0, 100.5, 100.0, 101.0, 5.0, 3.0, 10.0, 1.0)
    buf = _build_ws_frame([rec])
    out = ergo._decode_index_frame(buf)
    assert len(out) == 1
    r = out[0]
    assert r.ts_ms == 1_700_000_000_000
    assert r.ticker == 42
    assert r.mid == 100.5
    assert r.bid == 100.0
    assert r.ask == 101.0
    assert r.ci_ubp == 5.0
    assert r.confidence == 3
    assert r.accepted == 10
    assert r.rejected == 1


def test_decode_index_frame_batch():
    recs = [
        (
            1_700_000_000_000.0 + i,
            1000 + i,
            100.0 + i,
            99.5 + i,
            100.5 + i,
            1.0,
            5.0,
            10.0,
            0.0,
        )
        for i in range(8)
    ]
    buf = _build_ws_frame(recs)
    out = ergo._decode_index_frame(buf)
    assert len(out) == 8
    assert out[3].ticker == 1003
    assert out[7].mid == 107.0


def test_decode_index_frame_rejects_wrong_type():
    buf = bytes([99, 0]) + struct.pack("<H", 0) + bytes(4)
    assert ergo._decode_index_frame(buf) == []


def test_decode_index_frame_rejects_truncated():
    # Claims 5 records but body is empty.
    buf = bytes([1, 0]) + struct.pack("<H", 5) + bytes(4)
    assert ergo._decode_index_frame(buf) == []


# ── Public symbol re-exports ────────────────────────────────────────────


def test_top_level_re_exports():
    import nxr_sdk

    assert hasattr(nxr_sdk, "NxrClient")
    assert hasattr(nxr_sdk, "WsSubscriber")
    assert hasattr(nxr_sdk, "TickersDetailResponse")
    assert hasattr(nxr_sdk, "TickerDetail")
    assert hasattr(nxr_sdk, "KindSchema")
    assert hasattr(nxr_sdk, "ShardWindow")
    assert hasattr(nxr_sdk, "SynthLeg")
    assert nxr_sdk.__version__ == "0.2.0"
