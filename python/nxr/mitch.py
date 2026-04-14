"""MITCH v2 wire format — pure-Python, struct-based encode/decode."""

from __future__ import annotations

import struct
from dataclasses import dataclass
from typing import Self

# ── Timestamp helpers ────────────────────────────────────────────────
# u48 LE, 16 us ticks since 2010-01-01T00:00:00Z
EPOCH_2010_US: int = 1_262_304_000_000_000
_TICK_US: int = 16


def from_epoch_us(epoch_us: int) -> int:
    """Convert Unix epoch microseconds to MITCH u48 timestamp."""
    return (epoch_us - EPOCH_2010_US) // _TICK_US


def to_epoch_us(ts: int) -> int:
    """Convert MITCH u48 timestamp to Unix epoch microseconds."""
    return ts * _TICK_US + EPOCH_2010_US


def from_epoch_ms(epoch_ms: int) -> int:
    """Convert Unix epoch milliseconds to MITCH u48 timestamp."""
    return from_epoch_us(epoch_ms * 1_000)


def to_epoch_ms(ts: int) -> int:
    """Convert MITCH u48 timestamp to Unix epoch milliseconds."""
    return to_epoch_us(ts) // 1_000


# ── Derived helpers ──────────────────────────────────────────────────

def mid(bid: float, ask: float) -> float:
    """Mid price."""
    return (bid + ask) / 2.0


def spread_bps(bid: float, ask: float) -> float:
    """Spread in basis points relative to mid."""
    m = mid(bid, ask)
    if m == 0.0:
        return 0.0
    return (ask - bid) / m * 10_000.0


def ci_to_price(ci: int, mid_price: float) -> float:
    """Decode composite-index value to a price."""
    return ci * mid_price / 10_000.0


# ── Wire code ↔ ASCII mapping ───────────────────────────────────────

_CODE_TO_CHAR: dict[int, str] = {
    1: "t",  # Trade
    2: "o",  # Order
    3: "s",  # Tick
    4: "i",  # Index
    5: "b",  # OrderBook
    6: "k",  # Bar
}
_CHAR_TO_CODE: dict[str, int] = {v: k for k, v in _CODE_TO_CHAR.items()}


# ── MitchHeader (16 bytes) ──────────────────────────────────────────

_HDR_FMT = "<H6sBBH4s"  # type_provider(u16), ts(6B), count(u8), flags(u8), seq(u16), _reserved(4B)
_HDR_SIZE = struct.calcsize(_HDR_FMT)
assert _HDR_SIZE == 16


def _pack_u48(val: int) -> bytes:
    """Pack an integer as 6 LE bytes (u48)."""
    return (val & 0xFFFF_FFFF_FFFF).to_bytes(6, "little")


def _unpack_u48(data: bytes) -> int:
    """Unpack 6 LE bytes into an integer (u48)."""
    return int.from_bytes(data[:6], "little")


@dataclass(slots=True)
class MitchHeader:
    """MITCH v2 message header (16 bytes)."""

    type_provider: int
    timestamp: int
    count: int
    flags: int
    sequence: int

    # ── Properties ───────────────────────────────────────────────────

    @property
    def msg_type(self) -> str:
        """Decode wire code (low 4 bits) to ASCII character."""
        code = self.type_provider & 0xF
        return _CODE_TO_CHAR.get(code, "?")

    @property
    def provider_id(self) -> int:
        """Provider id from bits [15:4]."""
        return (self.type_provider >> 4) & 0xFFF

    # ── Constructors ─────────────────────────────────────────────────

    @classmethod
    def make(
        cls,
        msg_char: str,
        provider_id: int,
        timestamp: int,
        count: int = 1,
        flags: int = 0,
        sequence: int = 0,
    ) -> Self:
        """Build a header from human-friendly values."""
        code = _CHAR_TO_CODE[msg_char]
        tp = (provider_id & 0xFFF) << 4 | (code & 0xF)
        return cls(
            type_provider=tp,
            timestamp=timestamp,
            count=count,
            flags=flags,
            sequence=sequence,
        )

    # ── Serialization ────────────────────────────────────────────────

    def pack(self) -> bytes:
        """Serialize to 16 LE bytes."""
        return struct.pack(
            _HDR_FMT,
            self.type_provider,
            _pack_u48(self.timestamp),
            self.count,
            self.flags,
            self.sequence,
            b"\x00" * 4,
        )

    @classmethod
    def unpack(cls, data: bytes) -> Self:
        """Deserialize from 16 LE bytes."""
        tp, ts_bytes, count, flags, seq, _ = struct.unpack_from(_HDR_FMT, data)
        return cls(
            type_provider=tp,
            timestamp=_unpack_u48(ts_bytes),
            count=count,
            flags=flags,
            sequence=seq,
        )


# ── Body types ──────────────────────────────────────────────────────

# Tick (32 bytes)
_TICK_FMT = "<QddII"
_TICK_SIZE = struct.calcsize(_TICK_FMT)
assert _TICK_SIZE == 32


@dataclass(slots=True)
class Tick:
    """MITCH Tick body (32 bytes)."""

    ticker: int
    bid: float
    ask: float
    vbid: int
    vask: int

    def pack(self) -> bytes:
        return struct.pack(_TICK_FMT, self.ticker, self.bid, self.ask, self.vbid, self.vask)

    @classmethod
    def unpack(cls, data: bytes) -> Self:
        ticker, bid, ask, vbid, vask = struct.unpack_from(_TICK_FMT, data)
        return cls(ticker=ticker, bid=bid, ask=ask, vbid=vbid, vask=vask)


# Trade (24 bytes)
_TRADE_FMT_PREFIX = "<QdI"  # ticker(u64), price(f64), qty(u32) = 20 bytes
_TRADE_SIZE = 24


@dataclass(slots=True)
class Trade:
    """MITCH Trade body (24 bytes)."""

    ticker: int
    price: float
    qty: int
    trade_id: int  # u24
    side: int      # u8

    def pack(self) -> bytes:
        buf = struct.pack(_TRADE_FMT_PREFIX, self.ticker, self.price, self.qty)
        # trade_id as 3 LE bytes + side as 1 byte
        buf += (self.trade_id & 0xFFFFFF).to_bytes(3, "little")
        buf += self.side.to_bytes(1, "little")
        return buf

    @classmethod
    def unpack(cls, data: bytes) -> Self:
        ticker, price, qty = struct.unpack_from(_TRADE_FMT_PREFIX, data)
        trade_id = int.from_bytes(data[20:23], "little")
        side = data[23]
        return cls(ticker=ticker, price=price, qty=qty, trade_id=trade_id, side=side)


# Index (40 bytes)
_INDEX_FMT = "<QddIIHHBBBB"
_INDEX_SIZE = struct.calcsize(_INDEX_FMT)
assert _INDEX_SIZE == 40


@dataclass(slots=True)
class Index:
    """MITCH Index body (40 bytes)."""

    ticker: int
    bid: float
    ask: float
    vbid: int
    vask: int
    ci: int          # u16
    tick_count: int   # u16
    confidence: int   # u8
    accepted: int     # u8
    rejected: int     # u8

    def pack(self) -> bytes:
        return struct.pack(
            _INDEX_FMT,
            self.ticker, self.bid, self.ask,
            self.vbid, self.vask,
            self.ci, self.tick_count,
            self.confidence, self.accepted, self.rejected,
            0,  # _pad
        )

    @classmethod
    def unpack(cls, data: bytes) -> Self:
        (
            ticker, bid, ask, vbid, vask,
            ci, tick_count, confidence, accepted, rejected, _pad,
        ) = struct.unpack_from(_INDEX_FMT, data)
        return cls(
            ticker=ticker, bid=bid, ask=ask,
            vbid=vbid, vask=vask,
            ci=ci, tick_count=tick_count,
            confidence=confidence, accepted=accepted, rejected=rejected,
        )
