/**
 * @nxr/sdk — NX Rates client SDK for TypeScript/JavaScript.
 *
 * Zero-copy binary WebSocket decoding, REST client, MITCH wire types.
 *
 * ## Quick start
 *
 * ```ts
 * import { NxrClient, IndexBatch } from '@nxr/sdk';
 *
 * const nxr = new NxrClient('http://nxr-svc:40004');
 * const btc = await nxr.resolve('BTC/USDT');
 *
 * // Zero-copy hot path
 * nxr.onIndex((batch: IndexBatch) => {
 *   for (let i = 0; i < batch.count; i++) {
 *     if (batch.ticker(i) === btc) {
 *       console.log(`BTC mid=${batch.mid(i)} ci=${batch.ci(i)}`);
 *     }
 *   }
 * });
 *
 * nxr.connect();
 * ```
 *
 * @see {@link https://github.com/nxrates/mitch MITCH Protocol Spec}
 */

// Re-export MITCH wire types from the codec package
export {
  // Type codes
  MessageType,
  // Wire codes
  WireCode,
  // Size constants
  SIZE_HEADER, SIZE_TRADE, SIZE_ORDER, SIZE_TICK, SIZE_INDEX, SIZE_BAR, SIZE_ORDER_BOOK,
  // Timestamp codec
  EPOCH_2010_US, fromEpochUs, toEpochUs, fromEpochMs, toEpochMs, readU48, writeU48,
  // Wire code mapping
  wireToAscii, asciiToWire,
  // Header
  packHeader, unpackHeader, headerMsgType, headerProviderId, createHeader,
  // Body readers
  readIndex, readTick, readTrade,
  // Derived helpers
  mid, spreadUbp, spreadBps, ciToPrice,
  // Types
  type MitchHeader, type Index, type Tick, type Trade,
} from '@nxrates/mitch';

// Zero-copy WS batch decoder
export {
  // Constants
  WS_MSG_INDEX, WS_MSG_TICK, WS_HEADER_BYTES, INDEX_STRIDE, TICK_STRIDE,
  // Batch classes
  IndexBatch, TickBatch,
  // Dispatch
  decodeFrame, readWsHeader,
  // Types
  type WsIndex, type WsTick, type WsFrameHeader, type DecodedFrame,
} from './decode.js';

// High-level client
export {
  NxrClient,
  type TickerResponse, type WsState,
} from './client.js';
