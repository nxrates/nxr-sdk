"""NX Rates async REST + WebSocket client."""

from __future__ import annotations

import struct
from dataclasses import dataclass
from typing import Any, Awaitable, Callable

import aiohttp
import websockets


# ── WebSocket frame types ────────────────────────────────────────────

_WS_HDR = "<BBH4s"  # type(u8), pad(u8), count(u16 LE), reserved(4B)
_WS_HDR_SIZE = struct.calcsize(_WS_HDR)
assert _WS_HDR_SIZE == 8

_INDEX_STRIDE = 9   # f64s per record
_TICK_STRIDE = 6    # f64s per record


@dataclass(slots=True)
class WsIndex:
    """One Index record from a WS binary frame (9 x f64)."""

    ts_ms: float
    ticker: float
    mid: float
    bid: float
    ask: float
    ci: float
    confidence: float
    accepted: float
    rejected: float


@dataclass(slots=True)
class WsTick:
    """One Tick record from a WS binary frame (6 x f64)."""

    ts_ms: float
    ticker: float
    provider_id: float
    bid: float
    ask: float
    accepted: float


def _parse_ws_frame(data: bytes) -> tuple[int, int, bytes]:
    """Parse WS header, return (type, count, payload)."""
    typ, _pad, count, _reserved = struct.unpack_from(_WS_HDR, data)
    return typ, count, data[_WS_HDR_SIZE:]


def _decode_index_records(payload: bytes, count: int) -> list[WsIndex]:
    """Decode *count* Index records (stride=9 f64s) from payload."""
    fmt = f"<{_INDEX_STRIDE}d"
    size = struct.calcsize(fmt)
    records: list[WsIndex] = []
    for i in range(count):
        vals = struct.unpack_from(fmt, payload, i * size)
        records.append(WsIndex(*vals))
    return records


def _decode_tick_records(payload: bytes, count: int) -> list[WsTick]:
    """Decode *count* Tick records (stride=6 f64s) from payload."""
    fmt = f"<{_TICK_STRIDE}d"
    size = struct.calcsize(fmt)
    records: list[WsTick] = []
    for i in range(count):
        vals = struct.unpack_from(fmt, payload, i * size)
        records.append(WsTick(*vals))
    return records


# ── Callback type aliases ────────────────────────────────────────────

OnIndex = Callable[[list[WsIndex]], Awaitable[None] | None]
OnTick = Callable[[list[WsTick]], Awaitable[None] | None]


# ── Client ───────────────────────────────────────────────────────────

class NxrClient:
    """Async client for the NX Rates REST API and WebSocket stream."""

    def __init__(self, base_url: str = "http://localhost:40000") -> None:
        self._base = base_url.rstrip("/")
        self._ws_url = self._base.replace("http", "ws", 1).replace(":40000", ":40004", 1) + "/v1/stream"

    # ── REST helpers ─────────────────────────────────────────────────

    async def _get_json(self, path: str) -> Any:
        async with aiohttp.ClientSession() as session:
            async with session.get(f"{self._base}{path}") as resp:
                resp.raise_for_status()
                return await resp.json()

    async def symbols(self) -> dict[str, int]:
        """GET /v1/symbols -> {symbol: ticker_id}."""
        return await self._get_json("/v1/symbols")

    async def providers(self) -> dict[int, str]:
        """GET /v1/providers -> {provider_id: name}."""
        raw: dict[str, str] = await self._get_json("/v1/providers")
        return {int(k): v for k, v in raw.items()}

    async def tickers(self) -> list[dict]:
        """GET /v1/tickers -> list of ticker snapshots."""
        return await self._get_json("/v1/tickers")

    async def is_healthy(self) -> bool:
        """GET /health -> True if service is up."""
        try:
            async with aiohttp.ClientSession() as session:
                async with session.get(f"{self._base}/health") as resp:
                    return resp.status == 200
        except aiohttp.ClientError:
            return False

    # ── WebSocket stream ─────────────────────────────────────────────

    async def stream(
        self,
        on_index: OnIndex | None = None,
        on_tick: OnTick | None = None,
    ) -> None:
        """Connect to the WS stream and dispatch binary frames to callbacks.

        Runs until the connection is closed or an exception is raised.
        """
        async with websockets.connect(self._ws_url) as ws:
            async for message in ws:
                if not isinstance(message, (bytes, bytearray)):
                    continue
                typ, count, payload = _parse_ws_frame(message)
                if typ == 1 and on_index is not None:
                    records = _decode_index_records(payload, count)
                    result = on_index(records)
                    if result is not None:
                        await result
                elif typ == 2 and on_tick is not None:
                    records = _decode_tick_records(payload, count)
                    result = on_tick(records)
                    if result is not None:
                        await result
