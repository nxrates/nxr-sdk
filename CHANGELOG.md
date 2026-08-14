# Changelog

All notable changes per language live below. Wire format (MITCH 56 B / 96 B)
is stable across minor releases; new record fields are appended only at the
end of fixed-width structs. REST surface follows the same additive rule.

## 2026-08-14

Release tag: `sdk-v2026.08.14`

⚠ **Resolution changes for symbols that previously matched fuzzily.** An exact
ticker match now wins over a fuzzy one in every asset class, so a symbol that
used to land on a near-name now lands on itself: `BRK-B/USD` resolved to Brent
Crude and now resolves to Berkshire Hathaway. Indices load as `AssetClass::IP`
(10) instead of `IN` (9, Infrastructure), which changes their `ticker_id` bits.
A crypto-quoted base that is not itself crypto falls back through
`[FX, CM, IP]`; `EQ` is deliberately excluded. `BA` is Boeing again (BAE
Systems lost the bare alias). Anything that persisted or cached ticker_ids for
an affected symbol must re-resolve: the old and new ids differ, so shards
written under the old id will not be found under the new one.

- **Rust 0.3.0**: resolver precedence hardened as above. Registry grew by 108
  equity rows (12 US equities, 96 ETFs) and 11 crypto rows (Centrifuge, ENS,
  Falcon Finance, Gnosis, The Graph, Kamino, Meteora, Render, Venus, Lombard,
  Sanctum Infinity); two malformed rows repaired. `weights_schema.rs` gained
  `asset_markets: BTreeMap<String, Vec<AssetMarket>>` and `pipeline_config.rs`
  gained `CexsYml.pivot: PivotYml` (internal pipeline config, NOT client
  surface, deliberately not ported to the other bindings).

- **TS 0.4.0**: `resolve()` falls back to `GET /v1/price/{sym}` when a symbol
  is absent from `/v1/tickers/detail`, so any routable cross resolves instead
  of returning `undefined`. Response types re-derived field-for-field against
  the server DTOs, closing a class of silent omissions where the server always
  sent a field the interface did not declare: `SnapshotResponse` gained
  `flags`, `age_ms` and `status` (`age_ms` is PROVIDER observation age, not
  emit age: idle tickers heartbeat at 1 Hz, so emit age reads "fresh" through a
  dead-venue outage), `ShardWindow` gained `status`, `TickerDetail` gained
  `alias_of`, and both `IndexRecord` and `Bar` now carry `flags` off the binary
  path (`mitch.ts` read it and `decode.ts` dropped it: without it `confidence`
  is undecodable, and a carried-forward row or a composed bar reads exactly
  like a real observation). New `freshness()` + `FreshnessResponse` for
  `/v1/freshness/{ticker}`, the only route that distinguishes a quiet feed
  from a dead one: compare `provider_lag_ms` against `lag_ms`.
  Breaking: `Ohlc.ts_ms` renamed to `Ohlc.ts`: the server key is `ts`, so the
  old field always read `undefined`. `tickers()` returns `SnapshotResponse[]`
  (what `/v1/tickers` has always served) and `TickerSnapshot` is gone; its
  `symbol` / `ts_ms` / `ci_ubp` / `accepted` / `rejected` fields described a
  shape no route emits. `synthTick()`, `synthPaths()` and `synthOhlc()` removed
  with their `SynthTick` / `SynthPath` types: the server retired the
  `synth`-prefixed namespace so a URL cannot reveal whether a pair is primary
  or composed. Migrate to `price()`, `idx()`, and the `synth_legs` field on
  `tickersDetail()`. `bunx tsc` clean; 29/32 vitest pass (the 3 failures are a
  pre-existing mts-codec bug in `test/decode.test.ts`, untouched here).

- **Py 0.3.0**: new `try_resolve_ticker_id(symbol)` returning `int | None`.
  `resolve_ticker_id` is lenient by design: an unresolvable symbol gets an
  FNV1a-64 *phantom* id, which is unique but is not a bit-packed `TickerId`, so
  `resolve_ticker` reverses it to a hex base with an empty quote and any class
  or instrument-type bits read as hash noise. Callers that need to tell a real
  id from a phantom now can; the lenient function keeps its behaviour and
  carries the warning in its docstring. Exported at top level. The binding also
  inherits the whole resolver and registry change above through the Rust crate,
  including the `BRK-B/USD` and `BA` corrections and the 119 new rows. 25/25
  pytest pass.

- **Java**: no Java SDK. The empty `java/` directory was removed; nothing in
  this changelog or any README ever promised one.

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
