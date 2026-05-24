# Changelog

All notable changes per language live below. Wire format (MITCH 56 B / 96 B)
is stable across minor releases; new record fields are appended only at the
end of fixed-width structs. REST surface follows the same additive rule.

## 2026-05-24

Release tag: `sdk-v2026.05.24`

- **TS  0.3.0** — `NxrClient` covers 100% of the live `/v1` surface:
  `tickersDetail()` (typed + cached), `price()`, `last()`, `synthPaths()`,
  `synthTick()`, `synthOhlc()`, `integrity()`, `metrics()`, `health()`.
  `history(opts)` object form + `get().history()....fetch()` chainable, both
  returning a discriminated `{ kind, records|bars }` envelope. Smart defaults
  (quote=USDT, kind=renko, instrument=spot). MITCH binary is the wire default
  on idx/bars. `subscribe(tickers, cb)` returns a `SubscriberHandle` with an
  idempotent `close()`. New types exported: `SnapshotResponse`, `TickerDetail`,
  `TickersDetailResponse`, `KindSchema`, `ShardWindow`, `SymbolsResponse`,
  `SynthPath`, `SynthLeg`. `DEFAULT_BASE_URL = https://api.nxrates.com`.
  Tests: 34/34 pass (`bun test`); `bunx tsc --noEmit` clean.

- **Py  0.2.0** — `WsSubscriber` async-context-manager + async-iterator over
  `/v1/stream` (requires `websockets`). `tickers_detail()` now returns a
  `TickersDetailResponse` dataclass (`raw` dict still accessible). New types
  exported at top level: `TickersDetailResponse`, `TickerDetail`,
  `KindSchema`, `ShardWindow`, `SynthLeg`, `StreamIndexRecord`,
  `WsSubscriber`, `DEFAULT_BASE_URL`. `NxrClient()` defaults to
  `https://api.nxrates.com`. `resolve_ticker_id(sym)` returns the cached
  ticker_id (lazy-loads detail on first use). 23/23 pytest pass.

- **Rust 0.2.0** — New `client` module: `NxrClient` covering full `/v1`
  surface (`tickers_detail` typed + cached, `price`/`last`/`symbols`/
  `providers`/`synth_paths`/`synth_tick`/`integrity`, idx/bars octet-stream
  zero-copy decode via `bytemuck`). `history(opts)` object form +
  `get().history()....fetch().await` chainable. `subscribe(&[syms]) →
  WsStream` over `/v1/stream`, reusing `ws_client::WsClient`. Smart defaults
  (quote=USDT, kind=renko, instrument=spot). `DEFAULT_BASE_URL =
  https://api.nxrates.com`; `NxrClient::default()` works. New examples
  binary `examples/quickstart.rs`. 5 `wiremock` integration tests + 6 unit
  tests; aggregator (`core/`) re-tested clean — no breaking changes.
