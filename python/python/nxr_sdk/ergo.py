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

Returns
~~~~~~~
Always a NumPy structured array, decoded from the MITCH wire format with
field names matching the cross-SDK aligned dtype (see
:func:`nxr_sdk.decode_idx` / :func:`nxr_sdk.decode_bar`).
"""

from __future__ import annotations

from typing import Any, Optional, Sequence


# Default values applied when the caller omits a parameter.
DEFAULT_QUOTE = "USDT"
DEFAULT_KIND = "renko"
DEFAULT_INSTRUMENT_TYPE = "spot"

# Valid data kinds. `kline` = s10 OHLC, `renko` = volatility-calibrated brick,
# `idx` = raw IndexRecord stream (the highest-fidelity tick aggregate we ship).
VALID_KINDS = ("idx", "kline", "renko")


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
    >>> nxr = nxr_sdk.NxrClient("https://api.nxrates.com")
    >>> bars = nxr.history(base="BTC", kind="renko", limit=100)
    >>> # equivalent chainable form:
    >>> bars = nxr.get().history().base("BTC").renko().limit(100).fetch()
    """

    def __init__(self, base_url: str, timeout_s: float = 30.0):
        # Lazy import so circular module init stays clean.
        from nxr_sdk._native import Client as _Inner  # type: ignore

        self._inner = _Inner(base_url, timeout_s)
        # Lazy cache of /v1/tickers/detail (None until first use).
        self._detail_cache: Optional[dict[str, Any]] = None

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

    def tickers_detail(self, refresh: bool = False) -> dict[str, Any]:
        """Fetch the universal integrator inventory from
        ``/v1/tickers/detail`` (cached on the instance).

        Returns a dict with::

            { "idx_aggregation_ms": int,
              "count": int,
              "tickers": [ { "ticker_id": int, "ticker": "BTC/USDT",
                             "base": "BTC", "quote": "USDT",
                             "base_class": "CR", "quote_class": "CR",
                             "instrument_type": "SPOT",
                             "native": True,
                             "synth_legs": [ { "sym": "...", "exp": +/-1 }, ... ],
                             "kinds": { "idx": KindSchema, "kline": ..., "renko": ... } },
                           ... ] }

        Where ``KindSchema = { fields: [str], stride_bytes: int,
        shards: { first_date, last_date, count } }``.

        Set ``refresh=True`` to force a re-fetch (default = use cache).
        """
        if refresh or self._detail_cache is None:
            self._detail_cache = self._inner._get_json_path("/v1/tickers/detail") \
                if hasattr(self._inner, "_get_json_path") \
                else _fetch_detail_via_urllib(self._inner)
        return self._detail_cache

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
        return f"NxrClient({self._inner!r})"


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


def _fetch_detail_via_urllib(inner) -> dict[str, Any]:
    """Fallback /v1/tickers/detail fetcher when the pyo3 binding does not
    expose a generic ``_get_json_path``. Uses the inner Client's repr to
    extract the base URL and standard-library urllib for one shot.
    """
    import json
    import urllib.request

    repr_s = repr(inner)
    base = repr_s.split('base_url="', 1)[1].rsplit('"', 1)[0]
    req = urllib.request.Request(
        f"{base.rstrip('/')}/v1/tickers/detail",
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


__all__ = [
    "NxrClient",
    "DEFAULT_QUOTE",
    "DEFAULT_KIND",
    "DEFAULT_INSTRUMENT_TYPE",
    "VALID_KINDS",
]
