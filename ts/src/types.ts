/**
 * Shared TypeScript types for NXR client + server contracts.
 *
 * Aligns with `nxr_sdk::IndexRecord`, `mitch::Bar`, `mitch::Tick`,
 * `nxr_sdk::ohlc::Ohlc` in the canonical Rust SDK.
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
 * - `confidence`: active provider count (u8, `<= accepted`).
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

/** Synth-tick result from `/v1/synth/tick`. */
export interface SynthTick {
  bid: number;
  ask: number;
  mid: number;
  confidence: number;
}

/** Bar kind for `/v1/bars` query. */
export type BarKind = 'kline' | 'renko';
