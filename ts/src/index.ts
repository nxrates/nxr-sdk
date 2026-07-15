/**
 * @nxrates/sdk - official TypeScript SDK for NX Rates.
 *
 * Cross-platform (Browser + Node + Bun + Deno) entry. Re-exports everything
 * that is safe in any runtime: MITCH binary decoders, the REST/WS client,
 * shared types.
 *
 * For Node-only features (UDP multicast subscriber) import from `@nxrates/sdk/node`.
 * For browser-tuned bundles import from `@nxrates/sdk/browser`.
 *
 * @example
 * ```ts
 * import { NxrClient } from '@nxrates/sdk';
 *
 * const nxr = new NxrClient(); // defaults to https://api.nxrates.com
 *
 * // Object form
 * const data = await nxr.history({ ticker: 'BTC/USDT', kind: 'renko', limit: 500 });
 * if (data.kind === 'renko') console.log(data.bars.length, 'bars');
 *
 * // Chainable form
 * const data2 = await nxr.get().history().pair('ETH/USDC').renko().limit(500).fetch();
 *
 * // Real-time stream
 * const sub = nxr.subscribe(['BTC/USDT'], (rec) => console.log(rec.ticker, rec.bid));
 * // sub.close() when done
 * ```
 */

// Types
export type {
  Sym,
  IndexRecord,
  Bar,
  Tick,
  Ohlc,
  TickerSnapshot,
  SnapshotResponse,
  SynthTick,
  SynthPath,
  SynthLeg,
  TickerDetail,
  TickersDetailResponse,
  KindSchema,
  ShardWindow,
  BarKind,
  DataKind,
} from './types.js';

// MITCH constants + low-level decoders
export {
  MessageType,
  WireCode,
  SIZE_HEADER,
  SIZE_TRADE,
  SIZE_ORDER,
  SIZE_TICK,
  SIZE_INDEX,
  SIZE_BAR,
  SIZE_INDEX_RECORD,
  SIZE_ORDER_BOOK,
  EPOCH_MS_2010,
  EPOCH_US_2010,
  CI_SCALE,
  readU48,
  readHeader,
  readIndex,
  readTick,
  readBar,
  mtsToEpochMs,
  epochMsToMts,
  ciToUbp,
  ciToPrice,
  mid,
  spreadUbp,
  spreadBps,
  type MitchHeader,
  type Index as MitchIndex,
  type Tick as MitchTick,
  type Bar as MitchBar,
} from './mitch.js';

// High-level decoders
export {
  decodeIdxRecord,
  decodeIdxBatch,
  decodeBar,
  decodeBarBatch,
  decodeTick,
  decodeFrame,
  // legacy WS batch decoders
  WS_MSG_INDEX,
  WS_MSG_TICK,
  WS_HEADER_BYTES,
  INDEX_STRIDE,
  TICK_STRIDE,
  IndexBatch,
  TickBatch,
  decodeWsFrame,
  readWsHeader,
  type WsIndex,
  type WsTick,
  type WsFrameHeader,
  type DecodedWsFrame,
} from './decode.js';

// Client
export {
  NxrClient,
  HistoryRoot,
  HistoryBuilder,
  DEFAULT_BASE_URL,
  DEFAULT_QUOTE,
  DEFAULT_KIND,
  DEFAULT_INSTRUMENT_TYPE,
  type NxrClientOpts,
  type RangeOpts,
  type HistoryOpts,
  type HistoryData,
  type WsState,
  type TickerResponse,
  type SubscriberHandle,
  type StreamIndexRecord,
} from './client.js';

// WASM accelerator (optional)
export { tryLoadWasm, decodeIdxBatchFast, type NxrWasmModule } from './wasm-loader.js';

// Plan-tier error types — typed handlers for HTTP 401/403/406/429 with the
// `PLAN_LIMIT_EXCEEDED` wire shape. See ../docs/api-plans.md §"Error codes".
export {
  PlanLimitError,
  parsePlanLimitError,
  planLimitErrorFromJson,
  PLAN_ERROR_DISCRIMINANT,
  type PlanErrorCode,
  type PlanLimitErrorBody,
} from './errors.js';
