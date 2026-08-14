# Changelog

All notable changes per language live below. Wire format (MITCH 56 B / 96 B)
is stable across minor releases; new record fields are appended only at the
end of fixed-width structs. REST surface follows the same additive rule.

## Unreleased

⚠ **BREAKING: `/v1/tickers/detail` no longer serves the whole universe.** The
unparameterised JSON body used to be the full DERIVED universe (156,656 rows,
~32 MB). That shape is REMOVED, not moved behind a flag: 32 MB per call should
never happen. The endpoint is now input-limited.

- `?ids=` (comma-separated decimal / `0x` hex MITCH ids) and/or `?symbols=`
  (comma-separated identifiers in any form the single lookup accepts, class pins
  included) return rich rows for exactly those, capped at **1000** entries. Over
  the cap the server answers `400` naming the cap and the count received. It
  never truncates: a short body that looks complete is the failure mode the cap
  exists to prevent.
- No arguments (and `?native=1`) serve the REGISTERED subset, ~4 MB, unchanged.
  Every SDK binding already pinned `?native=1` on its bulk inventory call, so no
  SDK caller changes behaviour.
- The bulk shape is `GET /v1/tickers/ids`: bare LE `u64` ids, 8 B a row, 1.25 MB
  for the whole universe. `?packed=1` on `/v1/tickers/detail` serves the same
  cached body. No CSV variant: 8 bytes an id beats ~20+ ASCII characters an id
  and needs no parsing, so CSV would be a larger body that costs a tokenizer.

**New asset-centric surface.** There are ~400 assets against 156k tickers, so
the human-scale unit gets its own endpoints. All read-only RAM, all composed on
read; nothing here is persisted.

- `GET /v1/counts`: assets, tickers, registered tickers, venues, markets,
  `aggregation_interval_ms`. ~110 B. What a dashboard polls instead of
  downloading a ticker list to measure its length.
- `GET /v1/assets`: the ~400 assets, one small row each (~60 KB): `asset`,
  `class`, `class_id`, `asset_id`, `storage_quote`, `market_count`,
  `venue_count`, `native_ticker`. Market lists are omitted here by design.
- `GET /v1/assets/{ident}`: one asset plus its markets and the tickers it bases
  (capped at 100, `ticker_count` carries the untruncated total). Accepts a bare
  symbol (`BTC`) or a class pin (`CR:BTC`), with the same FORCED class
  resolution as `/v1/tickers/detail/{ident}`: a mismatched pin is a 404.
- `GET /v1/assets/last?quote=`: last price per asset in its own
  `storage_quote`, or all re-denominated by `?quote=`. Rows carry the snapshot
  shape plus `asset` and `quote`.

Counts / assets / asset-detail carry strong ETags and honour `If-None-Match`.
`/v1/assets/last` deliberately does not: a cached body would serve a stale mid.

- **Rust**: `NxrClient::{counts, assets, asset, assets_last, tickers_detail_for}`,
  the `Counts` / `AssetRow` / `AssetMarket` / `AssetDetail` / `AssetLast` DTOs,
  and `DETAIL_MAX_IDENTS`. ⚠ `tickers_packed()` is RENAMED `tickers_ids()` and
  now reads `/v1/tickers/ids`.
- **TypeScript**: `client.{counts, assets, asset, assetsLast, tickersDetailFor}`,
  the `Counts` / `AssetRow` / `AssetMarket` / `AssetDetail` / `AssetLast` types,
  and the exported `DETAIL_MAX_IDENTS`. ⚠ `tickersPacked()` is RENAMED
  `tickersIds()`.
- **Python**: `NxrClient.{counts, assets, asset, assets_last, tickers_detail_for}`,
  the `Counts` / `Asset` / `AssetMarket` dataclasses, and `DETAIL_MAX_IDENTS`.
  ⚠ `tickers_packed()` is RENAMED `tickers_ids()`. `Asset.class` is spelled
  `cls_` because `class` is a Python keyword.
- **FFI / Java**: unchanged. Neither exposes an HTTP client (`sdk/ffi` is a
  codec/resolver shim by design, Java calls the REST surface with its stdlib),
  so there is no client surface to extend.

### Earlier in this cycle

Single-ticker lookup, in every binding: `GET /v1/tickers/detail/{ident}` serves
one row for a decimal MITCH id, a symbol (`BTC/USD` or `BTC-USD`), or a
class-pinned symbol (`CR:BTC/FX:USD`). A class pin FORCES that asset class and
404s rather than falling back, which is the ambiguity it exists to remove.

- **Rust**: `NxrClient::ticker_detail(ident)` and
  `client::decode_packed_ids(&[u8]) -> Vec<u64>`.
- **TypeScript**: `client.tickerDetail(ident)` and the exported
  `decodePackedIds(Uint8Array) -> bigint[]`. `TickerDetail.kinds` is now
  optional: a derived pair owns no shards, so the server omits the field rather
  than fabricating a shard window.
- **Python**: `NxrClient.ticker_detail(ident)` and the exported
  `decode_packed_ids(bytes) -> list[int]`. The ergo HTTP path now forwards
  `X-NXR-Key` where it previously dropped it.

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
  `client.rs` re-derived against the server DTOs, same closure as TS below:
  `SnapshotResponse` gained `flags`, `age_ms` and `status` (`age_ms` is
  PROVIDER observation age, not emit age), `ShardWindow` gained `status` with a
  new `ShardStatus` enum, `TickerDetail` gained `alias_of`, and new
  `freshness()` + `FreshnessResponse` cover `/v1/freshness/{ticker}`, the only
  route that separates a quiet feed from a dead one.
  Breaking: `price(ticker_id: u64)` is now `price(sym: &str, max_age_ms:
  Option<i64>)` returning `SnapshotResponse` rather than an `Option` that could
  never be `None` (no price is a 404, never a 200 with a null body); the server
  accepts ids and symbols alike and composes crosses on read. `last()` takes
  the same symbol forms plus `max_age_ms`; both opt into a 503 past the
  ceiling. `tickers()` returns `Vec<SnapshotResponse>` (what `/v1/tickers` has
  always served) and `TickerSnapshotJson` is gone: its `symbol` / `ts_ms` /
  `ci_ubp` / `accepted` / `rejected` fields described a shape no route emits.
  `synth_paths()` and `synth_tick()` removed with the client-side `SynthPath`
  and `SynthTickJson` types (`synth::SynthPath`, the composition engine's type,
  is untouched): the server retired the `synth`-prefixed namespace so a URL
  cannot reveal whether a pair is primary or composed. Migrate to `price()`,
  `idx()`, and the `synth_legs` field on `tickers_detail()`.

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
