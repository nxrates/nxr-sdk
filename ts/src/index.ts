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
 * import { NxrClient, decodeIdxBatch } from '@nxrates/sdk';
 *
 * const nxr = new NxrClient({ baseUrl: 'http://nxr.nxrates.com' });
 * const recs = await nxr.idxBinary('BTC/USDT', { limit: 1000 });
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
  SynthTick,
  BarKind,
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
  type NxrClientOpts,
  type RangeOpts,
  type WsState,
  type TickerResponse,
} from './client.js';

// WASM accelerator (optional)
export { tryLoadWasm, decodeIdxBatchFast, type NxrWasmModule } from './wasm-loader.js';
