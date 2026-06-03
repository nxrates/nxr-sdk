/**
 * Shared TypeScript types for NXR client + server contracts.
 *
 * Aligns with `nxr_sdk::IndexRecord`, `mitch::Bar`, `mitch::Tick`,
 * `nxr_sdk::ohlc::Ohlc` in the canonical Rust SDK.
 *
 * Naming convention (operator-enforced, matches MITCH):
 *   - `symbol`     = atomic identifier ("BTC", "USDT", "EUR")
 *   - `ticker`     = pair string ("BTC/USDT")
 *   - `ticker_id`  = u64 MITCH encoding (bigint at the boundary)
 */

/** Symbol type used at the API boundary. e.g. "BTC/USDT". */
export type Sym = string;

/**
 * Decoded NXR IndexRecord (56B on the wire: 16B MitchHeader + 40B Index body).
 *
 * Flat shape — bid/ask hoisted from the body, ts_ms from the header.
 * Use {@link mid} for the (bid+ask)/2 mid price.
 *
 * - `ts_ms`: Unix epoch ms, derived from MitchHeader u48 mts.
 * - `provider`: 12-bit MITCH provider id (0-4095).
 * - `ticker`: u64 MITCH ticker id (bigint to preserve full precision).
 * - `bid`, `ask`, `mid`: f64. `mid` is `(bid+ask)/2`, materialized for convenience.
 * - `ci_ubp`: confidence interval in micro basis points (decoded from the sqrt-u16).
 * - `accepted`: # accepted providers in this aggregation window (u8).
 * - `rejected`: # rejected providers (u8).
 * - `confidence`: raw u8 freshness byte. When `FLAG_CONF_FRESHNESS` (flags bit
 *   3) is set it is Q0.8 fixed-point freshness (use `confidence01 = byte/255`);
 *   when clear it is the legacy integer active-provider count.
 * - `confidence01`: `confidence / 255` — the Q0.8 freshness as a float ∈ [0,1].
 */
export interface IndexRecord {
  ts_ms: number;
  provider: number;
  ticker: bigint;
  bid: number;
  ask: number;
  mid: number;
  ci_ubp: number;
  accepted: number;
  rejected: number;
  confidence: number;
  /** Q0.8 freshness as float ∈ [0,1] (`confidence / 255`). */
  confidence01: number;
  /** Aggregated bid volume. */
  vbid: number;
  /** Aggregated ask volume. */
  vask: number;
  /** Raw tick count consumed by this aggregation. */
  tick_count: number;
  /** Per-stream sequence (u16) for gap detection. */
  sequence: number;
}

/** Decoded Bar — flat shape with timestamps hoisted to ms. */
export interface Bar {
  open_ms: number;
  close_ms: number;
  open: number;
  high: number;
  low: number;
  close: number;
  vbid: number;
  vask: number;
  tick_count: number;
  /** Σ(log(mid_t/mid_{t-1}))². */
  realized_var: number;
  /** Bipower variation (jump-robust). */
  bipower_var: number;
  /** OLS slope * duration / close. */
  drift: number;
  /** Signed order-flow imbalance. */
  vol_imbalance: number;
  /** Mean (ask-bid)/mid * 1e4. */
  avg_spread_bps: number;
  /** Largest |log return| in bar. */
  max_abs_return: number;
  /** Sqrt-compressed mean CI (u16). Decode via `ciToUbp`. */
  avg_ci_ubp: number;
  /** rejected / (accepted+rejected) * 65535. */
  reject_rate: number;
  /** 0=kline 1=renko 2=dib 3=tib. */
  kind: number;
}

/** Tick (raw bid/ask quote, NOT aggregated). */
export interface Tick {
  ticker: bigint;
  bid: number;
  ask: number;
  vbid: number;
  vask: number;
}

/**
 * Bucket-aligned OHLC candle.
 * Matches `nxr_sdk::ohlc::Ohlc` (see `sdk/rust/src/ohlc.rs`).
 */
export interface Ohlc {
  /** Bucket-start epoch ms (UTC aligned). */
  ts_ms: number;
  open: number;
  high: number;
  low: number;
  close: number;
  vbid: number;
  vask: number;
  tick_count: number;
  /** Sqrt-encoded mean CI. */
  avg_ci_ubp: number;
}

/** REST `/v1/tickers` element. */
export interface TickerSnapshot {
  symbol: Sym;
  ticker: bigint;
  ts_ms: number;
  bid: number;
  ask: number;
  mid: number;
  ci_ubp: number;
  confidence: number;
  accepted: number;
  rejected: number;
}

/**
 * `/v1/price/{ticker_id}` / `/v1/last` snapshot — minimal live view.
 * Mirrors the server `SnapshotResponse` DTO.
 */
export interface SnapshotResponse {
  /** MITCH ticker_id (u64; bigint at the boundary). */
  ticker: bigint;
  mid: number;
  bid: number;
  ask: number;
  /** Confidence interval in micro basis points (relative to mid). */
  ci: number;
  /** Active provider count. */
  confidence: number;
}

/** Synth-tick result from `/v1/synth/tick/{sym}`. */
export interface SynthTick {
  sym: string;
  bid: number;
  ask: number;
  mid: number;
  /** Min-provider confidence across the legs. */
  conf: number;
}

/** Bar kind for `/v1/bars` query. */
export type BarKind = 'kline' | 'renko';

/** Data kind for the unified `history()` builder. `idx` = raw IndexRecord. */
export type DataKind = 'idx' | BarKind;

/** Synth-path leg ({ sym, exp: +1 | -1 }) returned by `/v1/synth/paths`. */
export interface SynthLeg {
  sym: string;
  exp: number;
}

/** Synth-path entry returned by `/v1/synth/paths` and inside `SymbolsResponse.synth`. */
export interface SynthPath {
  sym: string;
  legs: SynthLeg[];
}

/** Disk shard window: first/last `YYYY-MM-DD` filename for the kind. */
export interface ShardWindow {
  first_date: string | null;
  last_date: string | null;
  count: number;
}

/** Per-data-kind schema + on-disk presence for a single ticker. */
export interface KindSchema {
  /** Column names in the on-disk record (also the JSON keys). */
  fields: string[];
  /** Bytes per record on the binary (`Accept: application/octet-stream`) path. */
  stride_bytes: number;
  /** Daily-shard date range present on disk for this kind. */
  shards: ShardWindow;
}

/** One row of the `/v1/tickers/detail` integrator inventory. */
export interface TickerDetail {
  /** MITCH ticker_id (u64). Wire + disk identifier. `0` for synths. */
  ticker_id: bigint;
  /** Pair string "BASE/QUOTE" (the ticker). */
  ticker: string;
  /** Base symbol (e.g. "BTC"). */
  base: string;
  /** Quote symbol (e.g. "USDT"). */
  quote: string;
  /** Asset class of the base ("CR" | "FX" | ..). */
  base_class: string;
  /** Asset class of the quote. */
  quote_class: string;
  /** "SPOT" today. Other instrument types (PERP/FUT/OPT) added later. */
  instrument_type: string;
  /** `true` = published directly from a venue. `false` = reconstructed synth. */
  native: boolean;
  /** If `native == false`, the synth legs that compose this ticker. */
  synth_legs?: SynthLeg[];
  /** Per-kind schema + shard window. Keys: `idx`, `kline`, `renko`. */
  kinds: Record<string, KindSchema>;
}

/** `/v1/tickers/detail` response wrapper. */
export interface TickersDetailResponse {
  /** Aggregator cycle interval (ms). */
  idx_aggregation_ms: number;
  /** Total number of tickers (native + synth). */
  count: number;
  /** One row per ticker. */
  tickers: TickerDetail[];
}

/** `/v1/symbols` response: direct ticker -> id map + synth paths. */
export interface SymbolsResponse {
  direct: Map<string, bigint>;
  synth: SynthPath[];
}
