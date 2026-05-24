"""Ergonomic SDK layer for ``nxr_sdk``.

Pure-Python wrapper over the pyo3-backed :class:`Client`. Adds smart
defaults and two equivalent fetch styles so integrators can pick whichever
reads best at the call-site:

Smart defaults applied automatically when omitted:

- ``instrument_type``  -> ``"spot"`` (only spots in the launch MITCH ids)
- ``quote``            -> ``"USDT"``
- ``kind``             -> ``"renko"`` (auto-calibrated per ticker)

The MITCH ticker_id is fully abstracted from the integrator -- they speak
in symbols ("BTC", "USDT") and pairs ("BTC/USDT"), and the SDK resolves
to the u64 wire identifier internally using the on-server
``/v1/tickers/detail`` inventory (cached on first use).

Object / options form
~~~~~~~~~~~~~~~~~~~~~
Single call, all parameters explicit::

    arr = nxr.history(base="BTC", kind="renko", limit=500)
    arr = nxr.history(ticker="ETH/USDC", kind="idx",
                      from_ms=now-3600_000, to_ms=now)

Chainable form
~~~~~~~~~~~~~~
Build up, then fetch::

    arr = nxr.get().history().base("BTC").quote("USDT").renko().limit(500).fetch()
    arr = nxr.get().history().pair("ETH/USDC").idx().fetch()

Both forms apply the same defaults; either is fine. The chainable form
flows when wrapping in conditionals; the options form is one line.

Real-time
~~~~~~~~~
Open a WebSocket subscriber. Requires the ``websockets`` library
(``pip install websockets``)::

    async with nxr.subscribe(["BTC/USDT", "ETH/USDT"]) as sub:
        async for rec in sub:
            print(rec.ts_ms, rec.ticker, rec.bid, rec.ask)

Returns
~~~~~~~
Historical fetch returns a NumPy structured array, decoded from the MITCH
wire format with field names matching the cross-SDK aligned dtype (see
:func:`nxr_sdk.decode_idx` / :func:`nxr_sdk.decode_bar`).
"""

from __future__ import annotations

import json
import struct
from dataclasses import dataclass, field
from typing import Any, Iterable, List, Optional


# Default endpoint for the public API.
DEFAULT_BASE_URL = "https://api.nxrates.com"

# Default values applied when the caller omits a parameter.
DEFAULT_QUOTE = "USDT"
DEFAULT_KIND = "renko"
DEFAULT_INSTRUMENT_TYPE = "spot"

# Valid data kinds. `kline` = s10 OHLC, `renko` = volatility-calibrated brick,
# `idx` = raw IndexRecord stream (the highest-fidelity tick aggregate we ship).
VALID_KINDS = ("idx", "kline", "renko")


# ── Typed view of /v1/tickers/detail ────────────────────────────────────


@dataclass
class ShardWindow:
    """Disk shard window: first/last ``YYYY-MM-DD`` filename for a kind."""

    first_date: Optional[str] = None
    last_date: Optional[str] = None
    count: int = 0

    @classmethod
    def from_dict(cls, d: dict[str, Any]) -> "ShardWindow":
        return cls(
            first_date=d.get("first_date"),
            last_date=d.get("last_date"),
            count=int(d.get("count", 0)),
        )


@dataclass
class KindSchema:
    """Per-data-kind schema + on-disk presence for a single ticker."""

    fields: List[str] = field(default_factory=list)
    stride_bytes: int = 0
    shards: ShardWindow = field(default_factory=ShardWindow)

    @classmethod
    def from_dict(cls, d: dict[str, Any]) -> "KindSchema":
        return cls(
            fields=list(d.get("fields", []) or []),
            stride_bytes=int(d.get("stride_bytes", 0)),
            shards=ShardWindow.from_dict(d.get("shards", {}) or {}),
        )


@dataclass
class SynthLeg:
    """Synth-path leg: ``sym`` and signed exponent (+1 forward / -1 inverse)."""

    sym: str
    exp: int

    @classmethod
    def from_dict(cls, d: dict[str, Any]) -> "SynthLeg":
        return cls(sym=str(d["sym"]), exp=int(d["exp"]))


@dataclass
class TickerDetail:
    """One row of the ``/v1/tickers/detail`` integrator inventory."""

    ticker_id: int
    ticker: str
    base: str
    quote: str
    base_class: str
    quote_class: str
    instrument_type: str
    native: bool
    synth_legs: Optional[List[SynthLeg]] = None
    kinds: dict[str, KindSchema] = field(default_factory=dict)

    @classmethod
    def from_dict(cls, d: dict[str, Any]) -> "TickerDetail":
        legs = d.get("synth_legs")
        return cls(
            ticker_id=int(d.get("ticker_id", 0)),
            ticker=str(d.get("ticker", "")),
            base=str(d.get("base", "")),
            quote=str(d.get("quote", "")),
            base_class=str(d.get("base_class", "")),
            quote_class=str(d.get("quote_class", "")),
            instrument_type=str(d.get("instrument_type", "")),
            native=bool(d.get("native", True)),
            synth_legs=[SynthLeg.from_dict(x) for x in legs] if legs else None,
            kinds={k: KindSchema.from_dict(v) for k, v in (d.get("kinds") or {}).items()},
        )


@dataclass
class TickersDetailResponse:
    """Typed wrapper around the ``/v1/tickers/detail`` payload."""

    idx_aggregation_ms: int
    count: int
    tickers: List[TickerDetail] = field(default_factory=list)
    #: Original parsed JSON, kept for power users who want the raw dict.
    raw: dict[str, Any] = field(default_factory=dict, repr=False)

    @classmethod
    def from_dict(cls, d: dict[str, Any]) -> "TickersDetailResponse":
        return cls(
            idx_aggregation_ms=int(d.get("idx_aggregation_ms", 0)),
            count=int(d.get("count", 0)),
            tickers=[TickerDetail.from_dict(t) for t in d.get("tickers", []) or []],
            raw=d,
        )

    def by_ticker(self, ticker: str) -> Optional[TickerDetail]:
        """Lookup a row by canonical "BASE/QUOTE" name. Returns None if absent."""
        for t in self.tickers:
            if t.ticker == ticker:
                return t
        return None


def _norm_pair(base: str, quote: str) -> str:
    """Normalize (base, quote) into the canonical "BASE/QUOTE" form."""
    return f"{base.upper()}/{quote.upper()}"


def _parse_ticker(s: str) -> tuple[str, str]:
    """Split a ticker string into (base, quote).

    Accepts ``BTC/USDT``, ``BTC-USDT``, or a bare base ``BTC``. The bare
    form defaults the quote to :data:`DEFAULT_QUOTE`.
    """
    s = s.upper().strip()
    for sep in ("/", "-", "_"):
        if sep in s:
            a, b = s.split(sep, 1)
            return a.strip(), b.strip()
    return s, DEFAULT_QUOTE


class NxrClient:
    """High-level NXR client with smart defaults and dual call styles.

    Wraps the pyo3-backed :class:`nxr_sdk.Client` so existing primitives
    (``fetch_idx`` / ``fetch_bars`` / ``fetch_ohlc`` / ``fetch_tickers``)
    remain accessible while the new helpers handle defaults + ticker
    resolution.

    Examples
    --------
    >>> import nxr_sdk
    >>> nxr = nxr_sdk.NxrClient()  # defaults to https://api.nxrates.com
    >>> bars = nxr.history(base="BTC", kind="renko", limit=100)
    >>> # equivalent chainable form:
    >>> bars = nxr.get().history().base("BTC").renko().limit(100).fetch()
    """

    def __init__(self, base_url: str = DEFAULT_BASE_URL, timeout_s: float = 30.0):
        # Lazy import so circular module init stays clean.
        from nxr_sdk._native import Client as _Inner  # type: ignore

        self._base_url = base_url.rstrip("/")
        self._inner = _Inner(base_url, timeout_s)
        # Lazy caches.
        self._detail_cache: Optional[TickersDetailResponse] = None
        self._symbol_to_id: dict[str, int] = {}

    # ── Pass-through ─────────────────────────────────────────────────────
    # Existing pyo3 primitives stay available verbatim for power users.

    def fetch_idx(self, *args, **kwargs):
        return self._inner.fetch_idx(*args, **kwargs)

    def fetch_bars(self, *args, **kwargs):
        return self._inner.fetch_bars(*args, **kwargs)

    def fetch_ohlc(self, *args, **kwargs):
        return self._inner.fetch_ohlc(*args, **kwargs)

    def fetch_tickers(self):
        """Return the JSON list of (ticker_id, mid, bid, ask, ci, confidence)
        snapshots from ``/v1/tickers``.
        """
        return self._inner.fetch_tickers()

    def fetch_providers(self):
        return self._inner.fetch_providers()

    def fetch_symbols(self):
        return self._inner.fetch_symbols()

    # ── Integrator inventory ────────────────────────────────────────────

    def tickers_detail(self, refresh: bool = False) -> TickersDetailResponse:
        """Fetch the universal integrator inventory from
        ``/v1/tickers/detail`` (cached on the instance).

        Returns a :class:`TickersDetailResponse` dataclass for IDE autocomplete;
        the raw dict is still available via ``.raw`` for power users.

        Set ``refresh=True`` to force a re-fetch (default = use cache).
        """
        if refresh or self._detail_cache is None:
            raw = _fetch_detail(self._base_url)
            self._detail_cache = TickersDetailResponse.from_dict(raw)
            # Populate the symbol→id cache so resolve() short-circuits.
            self._symbol_to_id.clear()
            for t in self._detail_cache.tickers:
                if t.ticker_id != 0:
                    self._symbol_to_id[t.ticker] = t.ticker_id
        return self._detail_cache

    def resolve_ticker_id(self, ticker: str) -> Optional[int]:
        """Resolve a ticker string ("BTC/USDT") to its MITCH ticker_id.

        Populates the cache from ``/v1/tickers/detail`` if needed.
        """
        if not self._symbol_to_id:
            self.tickers_detail()
        return self._symbol_to_id.get(ticker)

    # ── Real-time stream ────────────────────────────────────────────────

    def subscribe(
        self,
        tickers: Optional[Iterable[str]] = None,
    ) -> "WsSubscriber":
        """Open a WebSocket subscriber over ``/v1/stream``.

        Returns a :class:`WsSubscriber` that supports ``async with`` for clean
        teardown and ``async for`` iteration. Optionally filter by ticker
        symbols (e.g. ``["BTC/USDT"]``); pass ``None`` to receive all records.

        Requires the ``websockets`` package (``pip install websockets``).
        """
        # Build ticker_id allowlist if filtering requested.
        allow_ids: Optional[set[int]] = None
        if tickers:
            ids: set[int] = set()
            # Populate the resolution cache if needed.
            if not self._symbol_to_id:
                try:
                    self.tickers_detail()
                except Exception:
                    pass
            for t in tickers:
                tid = self._symbol_to_id.get(t)
                if tid is not None:
                    ids.add(int(tid))
            allow_ids = ids if ids else None
        ws_url = self._base_url.replace("https://", "wss://").replace("http://", "ws://") + "/v1/stream"
        return WsSubscriber(ws_url, allow_ids)

    # ── Ergonomic entry points ──────────────────────────────────────────

    def get(self) -> "_Get":
        """Open a chainable builder root.

        ``client.get().history()...`` mirrors the natural-language reading
        order ``client get history of <ticker> as <kind>``. Returns a
        light proxy whose only method is :meth:`_Get.history`.
        """
        return _Get(self)

    def history(
        self,
        ticker: Optional[str] = None,
        *,
        base: Optional[str] = None,
        quote: Optional[str] = None,
        kind: Optional[str] = None,
        instrument_type: Optional[str] = None,
        from_ms: Optional[int] = None,
        to_ms: Optional[int] = None,
        limit: Optional[int] = None,
        tf: Optional[int] = None,
    ):
        """One-shot historical fetch with smart defaults.

        Parameters
        ----------
        ticker
            Pair form ``"BTC/USDT"`` (or ``"BTC-USDT"``, or bare ``"BTC"``
            which implies the default quote). Either ``ticker`` *or*
            ``base`` must be supplied.
        base, quote
            Atomic symbols. ``base`` is required if ``ticker`` is omitted;
            ``quote`` defaults to :data:`DEFAULT_QUOTE` (``"USDT"``).
        kind
            ``"idx"`` (raw IndexRecord stream, 56B / record), ``"kline"``
            (s10 OHLC microstructure-enriched Bar, 96B), or ``"renko"``
            (volatility-calibrated brick Bar, 96B). Defaults to
            :data:`DEFAULT_KIND` (``"renko"``).
        instrument_type
            Currently only ``"spot"`` (default).
        from_ms, to_ms, limit
            Standard server-side range + cap.
        tf
            Required only for ``kind="ohlc"`` (passthrough to
            ``/v1/ohlc``); the s10 microstructure-rich ``kline`` form is
            preferred for most uses.

        Returns
        -------
        numpy.ndarray
            Structured array; field names depend on ``kind``. See
            :func:`nxr_sdk.decode_idx` / :func:`nxr_sdk.decode_bar`.
        """
        b, q = _resolve_bq(ticker, base, quote)
        k = (kind or DEFAULT_KIND).lower()
        it = (instrument_type or DEFAULT_INSTRUMENT_TYPE).lower()
        if it != "spot":
            raise ValueError(
                f"instrument_type={it!r} not yet supported. "
                f"Launch MITCH ids are spot-only."
            )

        pair = _norm_pair(b, q)
        if k == "idx":
            return self._inner.fetch_idx(
                sym=pair, from_ms=from_ms, to_ms=to_ms, limit=limit
            )
        if k in ("kline", "renko"):
            return self._inner.fetch_bars(
                sym=pair, kind=k, from_ms=from_ms, to_ms=to_ms, limit=limit
            )
        if k == "ohlc":
            if tf is None:
                raise ValueError("kind='ohlc' requires tf (seconds)")
            return self._inner.fetch_ohlc(
                sym=pair, tf=tf, from_ms=from_ms, to_ms=to_ms, limit=limit
            )
        raise ValueError(
            f"kind={k!r} not in {VALID_KINDS} (or 'ohlc' with tf=...)"
        )

    def __repr__(self) -> str:
        return f"NxrClient({self._base_url!r})"


def _resolve_bq(
    ticker: Optional[str], base: Optional[str], quote: Optional[str]
) -> tuple[str, str]:
    """Resolve ``(base, quote)`` from the three accepted shapes."""
    if ticker is not None:
        b, q = _parse_ticker(ticker)
        # An explicit `quote=` overrides anything implied by the ticker string,
        # which mirrors how `base=`/`quote=` win when both are passed.
        return b, (quote or q).upper()
    if base is None:
        raise ValueError("history() requires either ticker= or base=")
    return base.upper(), (quote or DEFAULT_QUOTE).upper()


def _fetch_detail(base_url: str) -> dict[str, Any]:
    """One-shot ``/v1/tickers/detail`` JSON fetcher (urllib, no extra deps)."""
    import urllib.request

    req = urllib.request.Request(
        f"{base_url.rstrip('/')}/v1/tickers/detail",
        headers={"Accept": "application/json"},
    )
    with urllib.request.urlopen(req, timeout=30) as resp:
        return json.loads(resp.read().decode("utf-8"))


# ── Chainable builder ───────────────────────────────────────────────────


class _Get:
    """Tiny proxy returned by :meth:`NxrClient.get`.

    Only purpose: read as ``client.get().history()...``. Holds a
    backreference to the parent client.
    """

    __slots__ = ("_client",)

    def __init__(self, client: NxrClient):
        self._client = client

    def history(self) -> "_HistoryBuilder":
        return _HistoryBuilder(self._client)


class _HistoryBuilder:
    """Chainable builder produced by ``client.get().history()``.

    Each setter returns ``self`` so calls flow left-to-right. The terminal
    is :meth:`fetch`. Convenience terminals ``.idx()`` / ``.kline()`` /
    ``.renko()`` set the kind *and* execute the fetch in one step.
    """

    __slots__ = (
        "_client", "_base", "_quote", "_kind",
        "_from_ms", "_to_ms", "_limit", "_tf", "_instrument",
    )

    def __init__(self, client: NxrClient):
        self._client = client
        self._base: Optional[str] = None
        self._quote: Optional[str] = None
        self._kind: Optional[str] = None
        self._from_ms: Optional[int] = None
        self._to_ms: Optional[int] = None
        self._limit: Optional[int] = None
        self._tf: Optional[int] = None
        self._instrument: Optional[str] = None

    # ── Pair selection ──
    def base(self, sym: str) -> "_HistoryBuilder":
        """Set the base symbol (atomic). Required."""
        self._base = sym.upper()
        return self

    def quote(self, sym: str) -> "_HistoryBuilder":
        """Set the quote symbol. Defaults to ``USDT`` if omitted."""
        self._quote = sym.upper()
        return self

    def pair(self, ticker: str) -> "_HistoryBuilder":
        """Set base+quote from a pair string ``"BTC/USDT"`` (or ``"BTC-USDT"``,
        or bare ``"BTC"``).
        """
        b, q = _parse_ticker(ticker)
        self._base, self._quote = b, q
        return self

    def ticker(self, ticker: str) -> "_HistoryBuilder":
        """Alias for :meth:`pair`."""
        return self.pair(ticker)

    # ── Kind setters (non-terminal) ──
    def kind(self, k: str) -> "_HistoryBuilder":
        self._kind = k.lower()
        return self

    def spot(self) -> "_HistoryBuilder":
        """Set the instrument type to spot (the only launch type)."""
        self._instrument = "spot"
        return self

    # ── Range / limit setters ──
    def from_(self, ms: int) -> "_HistoryBuilder":
        """Set the inclusive lower-bound ts in epoch milliseconds.

        Underscore avoids the ``from`` keyword collision.
        """
        self._from_ms = int(ms)
        return self

    def to(self, ms: int) -> "_HistoryBuilder":
        self._to_ms = int(ms)
        return self

    def limit(self, n: int) -> "_HistoryBuilder":
        self._limit = int(n)
        return self

    def tf(self, seconds: int) -> "_HistoryBuilder":
        """OHLC-only: candle width in seconds. Server-validated against
        the TF whitelist; anything outside returns 400.
        """
        self._tf = int(seconds)
        return self

    # ── Terminals: set kind + execute in one call ──
    def idx(self):
        return self.kind("idx").fetch()

    def kline(self):
        return self.kind("kline").fetch()

    def renko(self):
        return self.kind("renko").fetch()

    def ohlc(self, tf: int):
        return self.kind("ohlc").tf(tf).fetch()

    def fetch(self):
        """Execute the request with the accumulated parameters and return
        the NumPy structured array.
        """
        return self._client.history(
            base=self._base,
            quote=self._quote,
            kind=self._kind,
            instrument_type=self._instrument,
            from_ms=self._from_ms,
            to_ms=self._to_ms,
            limit=self._limit,
            tf=self._tf,
        )


# ── WebSocket subscriber ────────────────────────────────────────────────


@dataclass
class StreamIndexRecord:
    """Decoded WS record. Flat shape mirroring the cross-SDK aligned dtype."""

    ts_ms: int
    ticker: int
    mid: float
    bid: float
    ask: float
    ci_ubp: float
    confidence: int
    accepted: int
    rejected: int


# WS frame layout — mirrors `core::server::ws::build_index_frame`.
_WS_HEADER_BYTES = 8
_WS_MSG_INDEX = 1
_WS_INDEX_STRIDE = 9  # 9 f64s per record


def _decode_index_frame(buf: bytes) -> list[StreamIndexRecord]:
    """Decode one binary WS frame into a list of :class:`StreamIndexRecord`."""
    if len(buf) < _WS_HEADER_BYTES:
        return []
    if buf[0] != _WS_MSG_INDEX:
        return []
    count = struct.unpack_from("<H", buf, 2)[0]
    body_off = _WS_HEADER_BYTES
    stride_bytes = _WS_INDEX_STRIDE * 8
    if len(buf) < body_off + count * stride_bytes:
        return []
    out: list[StreamIndexRecord] = []
    for i in range(count):
        off = body_off + i * stride_bytes
        lanes = struct.unpack_from(f"<{_WS_INDEX_STRIDE}d", buf, off)
        out.append(
            StreamIndexRecord(
                ts_ms=int(lanes[0]),
                ticker=int(lanes[1]),
                mid=lanes[2],
                bid=lanes[3],
                ask=lanes[4],
                ci_ubp=lanes[5],
                confidence=int(lanes[6]),
                accepted=int(lanes[7]),
                rejected=int(lanes[8]),
            )
        )
    return out


class WsSubscriber:
    """Async WebSocket subscriber over ``/v1/stream``.

    Use either as an async context manager + async iterator::

        async with c.subscribe(["BTC/USDT"]) as sub:
            async for rec in sub:
                print(rec.ticker, rec.bid, rec.ts_ms)

    or imperatively via :meth:`connect` / :meth:`close`. Requires the
    ``websockets`` package; raises :class:`RuntimeError` on first use if
    the import fails.
    """

    def __init__(self, ws_url: str, allow_ids: Optional[set[int]] = None):
        self._url = ws_url
        self._allow_ids = allow_ids
        self._ws = None  # populated on connect()
        self._buffer: list[StreamIndexRecord] = []

    async def __aenter__(self) -> "WsSubscriber":
        await self.connect()
        return self

    async def __aexit__(self, *_exc: Any) -> None:
        await self.close()

    async def connect(self) -> None:
        try:
            import websockets  # type: ignore[import-not-found]
        except ImportError as e:  # pragma: no cover
            raise RuntimeError(
                "WsSubscriber requires the `websockets` package. "
                "Install via `pip install websockets`."
            ) from e
        self._ws = await websockets.connect(self._url)

    async def close(self) -> None:
        if self._ws is not None:
            try:
                await self._ws.close()
            except Exception:
                pass
            self._ws = None

    def __aiter__(self) -> "WsSubscriber":
        return self

    async def __anext__(self) -> StreamIndexRecord:
        # Yield buffered records first.
        if self._buffer:
            return self._buffer.pop(0)
        if self._ws is None:
            await self.connect()
        assert self._ws is not None
        while True:
            try:
                msg = await self._ws.recv()
            except Exception as e:
                raise StopAsyncIteration from e
            if isinstance(msg, str):
                # Server may emit JSON error envelopes on bad sub frames; skip.
                continue
            recs = _decode_index_frame(msg)
            if self._allow_ids is not None:
                recs = [r for r in recs if r.ticker in self._allow_ids]
            if not recs:
                continue
            # Stash the rest; yield the head.
            head = recs[0]
            self._buffer.extend(recs[1:])
            return head


__all__ = [
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
]
