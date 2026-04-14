"""Tests for nxr.mitch — pack/unpack roundtrips and timestamp helpers."""

import struct
import pytest
from nxr.mitch import (
    EPOCH_2010_US,
    MitchHeader,
    Tick,
    Trade,
    Index,
    from_epoch_us,
    to_epoch_us,
    from_epoch_ms,
    to_epoch_ms,
    mid,
    spread_bps,
    ci_to_price,
)


# ── MitchHeader ─────────────────────────────────────────────────────

class TestMitchHeader:
    def test_pack_unpack_roundtrip(self):
        hdr = MitchHeader.make("s", provider_id=42, timestamp=123456789, count=3, flags=0, sequence=7)
        data = hdr.pack()
        assert len(data) == 16
        hdr2 = MitchHeader.unpack(data)
        assert hdr2.msg_type == "s"
        assert hdr2.provider_id == 42
        assert hdr2.timestamp == 123456789
        assert hdr2.count == 3
        assert hdr2.sequence == 7

    def test_type_provider_encoding(self):
        hdr = MitchHeader.make("t", provider_id=0, timestamp=0)
        assert hdr.type_provider & 0xF == 1  # wire code for Trade
        assert hdr.msg_type == "t"

        hdr = MitchHeader.make("k", provider_id=4095, timestamp=0)
        assert hdr.type_provider & 0xF == 6  # wire code for Bar
        assert hdr.provider_id == 4095

    def test_all_wire_codes(self):
        for char, code in [("t", 1), ("o", 2), ("s", 3), ("i", 4), ("b", 5), ("k", 6)]:
            hdr = MitchHeader.make(char, provider_id=0, timestamp=0)
            assert hdr.msg_type == char
            assert hdr.type_provider & 0xF == code


# ── Tick ─────────────────────────────────────────────────────────────

class TestTick:
    def test_pack_unpack_roundtrip(self):
        tick = Tick(ticker=100, bid=1.1234, ask=1.1240, vbid=5000, vask=3000)
        data = tick.pack()
        assert len(data) == 32
        tick2 = Tick.unpack(data)
        assert tick2.ticker == tick.ticker
        assert tick2.bid == pytest.approx(tick.bid)
        assert tick2.ask == pytest.approx(tick.ask)
        assert tick2.vbid == tick.vbid
        assert tick2.vask == tick.vask


# ── Trade ────────────────────────────────────────────────────────────

class TestTrade:
    def test_pack_unpack_roundtrip(self):
        trade = Trade(ticker=200, price=50_000.5, qty=10, trade_id=0xABCDEF, side=1)
        data = trade.pack()
        assert len(data) == 24
        trade2 = Trade.unpack(data)
        assert trade2.ticker == trade.ticker
        assert trade2.price == pytest.approx(trade.price)
        assert trade2.qty == trade.qty
        assert trade2.trade_id == trade.trade_id
        assert trade2.side == trade.side

    def test_trade_id_u24_clamp(self):
        trade = Trade(ticker=0, price=0.0, qty=0, trade_id=0xFFFFFF, side=0)
        data = trade.pack()
        trade2 = Trade.unpack(data)
        assert trade2.trade_id == 0xFFFFFF


# ── Index ────────────────────────────────────────────────────────────

class TestIndex:
    def test_pack_unpack_roundtrip(self):
        idx = Index(
            ticker=300, bid=2.0, ask=2.01,
            vbid=1000, vask=2000,
            ci=500, tick_count=12,
            confidence=99, accepted=10, rejected=2,
        )
        data = idx.pack()
        assert len(data) == 40
        idx2 = Index.unpack(data)
        assert idx2.ticker == idx.ticker
        assert idx2.bid == pytest.approx(idx.bid)
        assert idx2.ask == pytest.approx(idx.ask)
        assert idx2.vbid == idx.vbid
        assert idx2.vask == idx.vask
        assert idx2.ci == idx.ci
        assert idx2.tick_count == idx.tick_count
        assert idx2.confidence == idx.confidence
        assert idx2.accepted == idx.accepted
        assert idx2.rejected == idx.rejected


# ── Timestamp helpers ────────────────────────────────────────────────

class TestTimestamp:
    def test_roundtrip_us(self):
        epoch_us = EPOCH_2010_US + 16 * 1_000_000  # 1M ticks
        ts = from_epoch_us(epoch_us)
        assert ts == 1_000_000
        assert to_epoch_us(ts) == epoch_us

    def test_roundtrip_ms(self):
        epoch_ms = EPOCH_2010_US // 1_000 + 16_000  # 1M ticks
        ts = from_epoch_ms(epoch_ms)
        assert ts == 1_000_000
        assert to_epoch_ms(ts) == epoch_ms

    def test_epoch_at_zero(self):
        assert to_epoch_us(0) == EPOCH_2010_US
        assert from_epoch_us(EPOCH_2010_US) == 0


# ── Derived helpers ──────────────────────────────────────────────────

class TestHelpers:
    def test_mid(self):
        assert mid(1.0, 2.0) == pytest.approx(1.5)

    def test_spread_bps(self):
        # spread = 0.01, mid = 1.005, bps = 0.01/1.005 * 10000 ~ 99.50
        assert spread_bps(1.0, 1.01) == pytest.approx(99.5024, rel=1e-3)

    def test_spread_bps_zero_mid(self):
        assert spread_bps(0.0, 0.0) == 0.0

    def test_ci_to_price(self):
        assert ci_to_price(5000, 2.0) == pytest.approx(1.0)
