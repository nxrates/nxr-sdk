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
from typing import Any, Iterable, List, Optional, Sequence


# Default endpoint for the public API.
DEFAULT_BASE_URL = "https://api.nxrates.com"

# Default values applied when the caller omits a parameter.
DEFAULT_QUOTE = "USDT"
DEFAULT_KIND = "renko"
DEFAULT_INSTRUMENT_TYPE = "spot"

# Valid data kinds. `kline` = s10 OHLC, `renko` = volatility-calibrated brick,
# `idx` = raw IndexRecord stream (the highest-fidelity tick aggregate we ship).
VALID_KINDS = ("idx", "kline", "renko")

#: Server-side ceiling on ``/v1/tickers/detail?ids=|symbols=``. Over it the
#: server returns 400; it never truncates.
DETAIL_MAX_IDENTS = 1000


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


@dataclass
class Counts:
    """``/v1/counts`` -- structural cardinalities. Cheap enough to poll."""

    assets: int = 0
    #: Derived universe size: every ordered pair the resolver can serve.
    tickers: int = 0
    #: The registered subset that owns shards on disk.
    registered_tickers: int = 0
    venues: int = 0
    markets: int = 0
    aggregation_interval_ms: int = 0

    @classmethod
    def from_dict(cls, d: dict[str, Any]) -> "Counts":
        return cls(**{f: int(d.get(f, 0)) for f in cls.__dataclass_fields__})


@dataclass
class AssetMarket:
    """One scraped market of an asset, from ``/v1/assets/{ident}``."""

    venue: str = ""
    pair: str = ""
    volume_usd: float = 0.0
    #: True when the asset is the QUOTE side of ``pair``.
    inverted: bool = False

    @classmethod
    def from_dict(cls, d: dict[str, Any]) -> "AssetMarket":
        return cls(
            venue=str(d.get("venue", "")),
            pair=str(d.get("pair", "")),
            volume_usd=float(d.get("volume_usd", 0.0)),
            inverted=bool(d.get("inverted", False)),
        )


@dataclass
class Asset:
    """One row of ``/v1/assets``; ``/v1/assets/{ident}`` fills the rest.

    ``markets`` / ``tickers`` are empty on the list endpoint by design: that is
    what keeps it small. ``ticker_count`` is the UNTRUNCATED total behind the
    capped ``tickers`` sample.
    """

    asset: str = ""
    #: Two-letter MITCH class alias ("CR" | "FX" | "EQ" | "IP" | ..).
    cls_: str = ""
    class_id: int = 0
    #: Packed 32-bit asset id (4-bit class + 16-bit class_id).
    asset_id: int = 0
    #: PUBLISHED denomination, fixed per asset. Never a counter denomination.
    storage_quote: str = ""
    market_count: int = 0
    venue_count: int = 0
    native_ticker: Optional[str] = None
    markets: List[AssetMarket] = field(default_factory=list)
    tickers: List[str] = field(default_factory=list)
    ticker_count: int = 0

    @classmethod
    def from_dict(cls, d: dict[str, Any]) -> "Asset":
        return cls(
            asset=str(d.get("asset", "")),
            # `class` is a Python keyword, so the field carries a trailing _.
            cls_=str(d.get("class", "")),
            class_id=int(d.get("class_id", 0)),
            asset_id=int(d.get("asset_id", 0)),
            storage_quote=str(d.get("storage_quote", "")),
            market_count=int(d.get("market_count", 0)),
            venue_count=int(d.get("venue_count", 0)),
            native_ticker=d.get("native_ticker"),
            markets=[AssetMarket.from_dict(m) for m in d.get("markets", []) or []],
            tickers=[str(t) for t in d.get("tickers", []) or []],
            ticker_count=int(d.get("ticker_count", 0)),
        )


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

    def __init__(self, base_url: str = DEFAULT_BASE_URL, timeout_s: float = 30.0, api_key: str | None = None):
        # Lazy import so circular module init stays clean.
        from nxr_sdk._native import Client as _Inner  # type: ignore

        self._base_url = base_url.rstrip("/")
        self._api_key = api_key
        self._inner = _Inner(base_url, timeout_s, api_key)
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

    # ── Integrator inventory ────────────────────────────────────────────

    def tickers_detail(self, refresh: bool = False) -> TickersDetailResponse:
        """Fetch the universal integrator inventory from
        ``/v1/tickers/detail`` (cached on the instance).

        Returns a :class:`TickersDetailResponse` dataclass for IDE autocomplete;
        the raw dict is still available via ``.raw`` for power users.

        Set ``refresh=True`` to force a re-fetch (default = use cache).
        """
        if refresh or self._detail_cache is None:
            # ``?native=1``: the registered subset carrying ``kinds`` + shard
            # status. It is also the unparameterised body; the derived universe
            # has no JSON shape at all (see ``tickers_ids``).
            raw = _fetch_json(self._base_url, "/v1/tickers/detail?native=1", self._api_key)
            self._detail_cache = TickersDetailResponse.from_dict(raw)
            # Populate the symbol→id cache so resolve() short-circuits.
            self._symbol_to_id.clear()
            for t in self._detail_cache.tickers:
                if t.ticker_id != 0:
                    self._symbol_to_id[t.ticker] = t.ticker_id
        return self._detail_cache

    def ticker_detail(self, ident: str) -> TickerDetail:
        """Fetch the rich row for ONE ticker from ``/v1/tickers/detail/{ident}``.

        ``ident`` is a decimal (or ``0x`` hex) MITCH id, a symbol in slash or
        dash form (``BTC/USD``, ``BTC-USD``), or a class-pinned symbol
        (``CR:BTC/FX:USD``). A pin FORCES that asset class: a leg absent from it
        is a 404, never a silent hit in another class. Not cached -- the universe
        is 156k pairs, which is why per-ticker richness lives behind a lookup.
        """
        path = f"/v1/tickers/detail/{_url_ident(ident)}"
        return TickerDetail.from_dict(_fetch_json(self._base_url, path, self._api_key))

    def tickers_detail_for(self, syms: Sequence[str]) -> TickersDetailResponse:
        """Rich rows for an EXPLICIT list, ``/v1/tickers/detail?symbols=``.

        Capped at :data:`DETAIL_MAX_IDENTS` server-side: over it the server
        returns 400 rather than truncating, so this raises instead of quietly
        returning a short body. Unresolvable entries are omitted from the reply.
        """
        if not syms:
            return TickersDetailResponse(idx_aggregation_ms=0, count=0)
        from urllib.parse import quote as _q

        csv = _q(",".join(s.replace("/", "-") for s in syms), safe="")
        path = f"/v1/tickers/detail?symbols={csv}"
        return TickersDetailResponse.from_dict(_fetch_json(self._base_url, path, self._api_key))

    def tickers_ids(self) -> List[int]:
        """The FULL servable universe as MITCH ticker ids, packed.

        ``/v1/tickers/ids``: 1.25 MB against the ~32 MB a JSON body would cost.
        Pair with :meth:`ticker_detail` for the rows actually wanted.
        """
        buf = _get(
            self._base_url,
            "/v1/tickers/ids",
            "application/vnd.nxr.u64",
            self._api_key,
        )
        return decode_packed_ids(buf)

    def counts(self) -> Counts:
        """``/v1/counts`` -- assets, tickers, venues, markets. The cheap poll."""
        return Counts.from_dict(_fetch_json(self._base_url, "/v1/counts", self._api_key))

    def assets(self) -> List[Asset]:
        """``/v1/assets`` -- the ~400 assets, one small row each."""
        raw = _fetch_json(self._base_url, "/v1/assets", self._api_key)
        return [Asset.from_dict(a) for a in raw or []]

    def asset(self, ident: str) -> Asset:
        """``/v1/assets/{ident}`` -- one asset, its markets, the tickers it bases.

        ``ident`` is a bare symbol (``BTC``) or class-pinned (``CR:BTC``). A pin
        FORCES that asset class: a mismatch is a 404, never a silent hit in
        another class.
        """
        from urllib.parse import quote as _q

        path = f"/v1/assets/{_q(ident, safe='')}"
        return Asset.from_dict(_fetch_json(self._base_url, path, self._api_key))

    def assets_last(self, quote: Optional[str] = None) -> List[dict[str, Any]]:
        """``/v1/assets/last`` -- last price per asset.

        Each row is denominated in its own ``storage_quote`` unless ``quote``
        re-denominates them all. Composed on read; nothing is persisted in the
        override basis. Rows carry the snapshot shape (mid/bid/ask/age_ms/
        status/flags) plus ``asset`` and ``quote``.
        """
        from urllib.parse import quote as _q

        path = "/v1/assets/last" + (f"?quote={_q(quote, safe='')}" if quote else "")
        return list(_fetch_json(self._base_url, path, self._api_key) or [])

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


def _get(base_url: str, path: str, accept: str, api_key: str | None = None) -> bytes:
    """One-shot GET (urllib, no extra deps). The only HTTP call site in ergo."""
    import urllib.request

    headers = {"Accept": accept}
    if api_key:
        headers["X-NXR-Key"] = api_key
    req = urllib.request.Request(f"{base_url.rstrip('/')}{path}", headers=headers)
    with urllib.request.urlopen(req, timeout=30) as resp:
        return resp.read()


def _fetch_json(base_url: str, path: str, api_key: str | None = None) -> Any:
    return json.loads(_get(base_url, path, "application/json", api_key).decode("utf-8"))


def _url_ident(ident: str) -> str:
    """Spell an identifier into one path segment.

    The server accepts the dash form natively, so ``BTC/USD`` and the
    class-pinned ``CR:BTC/FX:USD`` both travel without a raw ``/``.
    """
    from urllib.parse import quote

    return quote(ident.replace("/", "-"), safe="")


def decode_packed_ids(buf: bytes) -> List[int]:
    """Decode the packed ``/v1/tickers/detail`` catalogue.

    Bare little-endian ``u64`` MITCH ticker ids, 8 B a row, no header;
    ``len(buf) // 8`` is the row count. A ticker id encodes instrument type and
    both asset class/id pairs, so this 1.25 MB body replaces the ~32 MB JSON one
    for a caller holding the asset registry.
    """
    if len(buf) % 8:
        raise ValueError(f"packed catalogue: {len(buf)} bytes is not a multiple of 8")
    return list(struct.unpack(f"<{len(buf) // 8}Q", buf))


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
