/**
 * Decoders for NXR binary payloads.
 *
 * Two flavors:
 *
 * 1) **MITCH binary** (UDP multicast frames + HTTP octet-stream endpoints):
 *    - 56B `IndexRecord` (16B header + 40B Index body)
 *    - 96B `Bar`
 *    - 32B `Tick`
 *
 *    Pure-TS DataView decoders ~500K rec/s. Optional WASM accelerator for
 *    >10M rec/s (see {@link tryLoadWasm}).
 *
 * 2) **WS Float64Array frames** (legacy `/v1/stream` protocol):
 *    Server packs records as [8B header][count * stride f64]. Zero-copy
 *    accessor classes hold a `Float64Array` view into the buffer.
 */

import {
  type Bar,
  type IndexRecord,
  type Tick,
} from './types.js';
import {
  type Bar as MitchBar,
  type Index as MitchIndex,
  type MitchHeader,
  readBar as readMitchBar,
  readHeader,
  readIndex,
  readTick,
  SIZE_BAR,
  SIZE_HEADER,
  SIZE_INDEX,
  SIZE_INDEX_RECORD,
  SIZE_TICK,
  ciToUbp,
  mtsToEpochMs,
} from './mitch.js';

// ─── MITCH binary decoders ────────────────────────────────────────────────

/**
 * Decode a single 56B IndexRecord (header + index body) starting at `offset`.
 * Throws if buffer too small.
 */
export function decodeIdxRecord(buf: Uint8Array, offset = 0): IndexRecord {
  if (buf.byteLength - offset < SIZE_INDEX_RECORD) {
    throw new RangeError(
      `IndexRecord needs ${SIZE_INDEX_RECORD}B at offset ${offset}, buf has ${
        buf.byteLength - offset
      }B`,
    );
  }
  const dv = new DataView(buf.buffer, buf.byteOffset + offset, SIZE_INDEX_RECORD);
  const hdr: MitchHeader = readHeader(dv, 0);
  const idx: MitchIndex = readIndex(dv, SIZE_HEADER);
  const m = (idx.bid + idx.ask) * 0.5;
  return {
    ts_ms: mtsToEpochMs(hdr.mts),
    provider: hdr.providerId,
    ticker: idx.ticker,
    bid: idx.bid,
    ask: idx.ask,
    mid: m,
    ci_ubp: ciToUbp(idx.ci),
    accepted: idx.accepted,
    rejected: idx.rejected,
    confidence: idx.confidence,
    confidence01: idx.confidence / 255,
    vbid: idx.vbid,
    vask: idx.vask,
    tick_count: idx.tickCount,
    sequence: hdr.sequence,
    flags: idx.flags,
  };
}

/**
 * Decode a contiguous batch of 56B IndexRecords. Length must be multiple of 56.
 *
 * For UDP multicast frames: pass the entire datagram payload.
 * For HTTP octet-stream `/v1/idx` responses: pass `new Uint8Array(await r.arrayBuffer())`.
 */
export function decodeIdxBatch(buf: Uint8Array): IndexRecord[] {
  const n = Math.floor(buf.byteLength / SIZE_INDEX_RECORD);
  const out = new Array<IndexRecord>(n);
  for (let i = 0; i < n; i++) {
    out[i] = decodeIdxRecord(buf, i * SIZE_INDEX_RECORD);
  }
  return out;
}

/** Decode a single 96B Bar at `offset`. */
export function decodeBar(buf: Uint8Array, offset = 0): Bar {
  if (buf.byteLength - offset < SIZE_BAR) {
    throw new RangeError(
      `Bar needs ${SIZE_BAR}B at offset ${offset}, buf has ${buf.byteLength - offset}B`,
    );
  }
  const dv = new DataView(buf.buffer, buf.byteOffset + offset, SIZE_BAR);
  const b: MitchBar = readMitchBar(dv, 0);
  return {
    open_ms: b.openMs,
    close_ms: b.closeMs,
    open: b.open,
    high: b.high,
    low: b.low,
    close: b.close,
    vbid: b.vbid,
    vask: b.vask,
    tick_count: b.tickCount,
    realized_var: b.realizedVar,
    bipower_var: b.bipowerVar,
    drift: b.drift,
    vol_imbalance: b.volImbalance,
    avg_spread_bps: b.avgSpreadBps,
    max_abs_return: b.maxAbsReturn,
    avg_ci_ubp: b.avgCiUbp,
    reject_rate: b.rejectRate,
    kind: b.kind,
    flags: b.flags,
  };
}

/** Decode a contiguous batch of 96B bars (length must be multiple of 96). */
export function decodeBarBatch(buf: Uint8Array): Bar[] {
  const n = Math.floor(buf.byteLength / SIZE_BAR);
  const out = new Array<Bar>(n);
  for (let i = 0; i < n; i++) out[i] = decodeBar(buf, i * SIZE_BAR);
  return out;
}

/**
 * Decode the packed `/v1/tickers/detail` catalogue: bare little-endian `u64`
 * MITCH ticker ids, 8 B a row, no header. `byteLength / 8` is the row count.
 *
 * A ticker id encodes instrument type and both asset class/id pairs, so the
 * 1.25 MB packed body replaces the 32 MB JSON one for callers that hold the
 * asset registry; fetch the rows they want from `/v1/tickers/detail/{ident}`.
 */
export function decodePackedIds(buf: Uint8Array): bigint[] {
  if (buf.byteLength % 8 !== 0) {
    throw new Error(`packed catalogue: ${buf.byteLength} bytes is not a multiple of 8`);
  }
  const dv = new DataView(buf.buffer, buf.byteOffset, buf.byteLength);
  const out = new Array<bigint>(buf.byteLength / 8);
  for (let i = 0; i < out.length; i++) out[i] = dv.getBigUint64(i * 8, true);
  return out;
}

/**
 * Decode a header + body MITCH frame (e.g. raw publisher frame).
 * Returns the (decoded header, body offset, body length).
 *
 * Caller dispatches on `header.msgType` to decode body slices.
 */
export function decodeFrame(
  buf: Uint8Array,
): { header: MitchHeader; bodyOffset: number; bodyBytes: number } | null {
  if (buf.byteLength < SIZE_HEADER) return null;
  const dv = new DataView(buf.buffer, buf.byteOffset, buf.byteLength);
  const header = readHeader(dv, 0);
  return {
    header,
    bodyOffset: SIZE_HEADER,
    bodyBytes: buf.byteLength - SIZE_HEADER,
  };
}

/** Decode a single 32B Tick body at `offset`. */
export function decodeTick(buf: Uint8Array, offset = 0): Tick {
  if (buf.byteLength - offset < SIZE_TICK) {
    throw new RangeError(
      `Tick needs ${SIZE_TICK}B at offset ${offset}, buf has ${buf.byteLength - offset}B`,
    );
  }
  const dv = new DataView(buf.buffer, buf.byteOffset + offset, SIZE_TICK);
  return readTick(dv, 0);
}

// ── Legacy WS Float64Array protocol ───────────────────────────────────────
//
// The /v1/stream WS server packs frames as [8B header][count * stride f64].
// Zero-copy: IndexBatch / TickBatch wrap the buffer with a Float64Array view.

export const WS_MSG_INDEX = 1;
export const WS_MSG_TICK = 2;
export const WS_HEADER_BYTES = 8;

export const INDEX_STRIDE = 9;
export const TICK_STRIDE = 6;

export interface WsIndex {
  epoch_ms: number;
  ticker: number;
  mid: number;
  bid: number;
  ask: number;
  ci: number;
  /**
   * Raw u8 liveness byte widened to f64. Meaning is FLAG-SELECTED on the source
   * record and changed at the 2026-07-25 cutover: FLAG_CONF_ACTIVE = packed
   * (bits 0..6 ticking-provider count, bit 7 fresh-weight-share OK);
   * FLAG_CONF_FRESHNESS = freshness fraction (byte/255); neither = legacy count.
   */
  confidence: number;
  /**
   * `confidence / 255`. ⚠ Only a freshness fraction ∈ [0,1] for legacy
   * FLAG_CONF_FRESHNESS records; on packed (FLAG_CONF_ACTIVE) records this is a
   * scaled count and is NOT a fraction. Computed unconditionally because the WS
   * frame carries no flags.
   */
  confidence01: number;
  accepted: number;
  rejected: number;
}

export interface WsTick {
  epoch_ms: number;
  ticker: number;
  provider_id: number;
  bid: number;
  ask: number;
  accepted: boolean;
}

export interface WsFrameHeader {
  type: number;
  count: number;
}

/** Parse WS frame header (first 8 bytes). */
export function readWsHeader(buf: ArrayBuffer): WsFrameHeader {
  const dv = new DataView(buf, 0, WS_HEADER_BYTES);
  return { type: dv.getUint8(0), count: dv.getUint16(2, true) };
}

/** Zero-copy batch view over WS index records. */
export class IndexBatch implements Iterable<WsIndex> {
  private readonly f64: Float64Array;
  readonly count: number;

  constructor(buf: ArrayBuffer) {
    const dv = new DataView(buf, 0, WS_HEADER_BYTES);
    this.count = dv.getUint16(2, true);
    this.f64 =
      this.count > 0
        ? new Float64Array(buf, WS_HEADER_BYTES, this.count * INDEX_STRIDE)
        : new Float64Array(0);
  }

  ts(i: number): number {
    return this.f64[i * INDEX_STRIDE]!;
  }
  ticker(i: number): number {
    return this.f64[i * INDEX_STRIDE + 1]!;
  }
  mid(i: number): number {
    return this.f64[i * INDEX_STRIDE + 2]!;
  }
  bid(i: number): number {
    return this.f64[i * INDEX_STRIDE + 3]!;
  }
  ask(i: number): number {
    return this.f64[i * INDEX_STRIDE + 4]!;
  }
  ci(i: number): number {
    return this.f64[i * INDEX_STRIDE + 5]!;
  }
  confidence(i: number): number {
    return this.f64[i * INDEX_STRIDE + 6]!;
  }
  /** Freshness float ∈ [0,1] (`confidence / 255`). */
  confidence01(i: number): number {
    return this.f64[i * INDEX_STRIDE + 6]! / 255;
  }
  accepted(i: number): number {
    return this.f64[i * INDEX_STRIDE + 7]!;
  }
  rejected(i: number): number {
    return this.f64[i * INDEX_STRIDE + 8]!;
  }
  spreadUbp(i: number): number {
    const m = this.mid(i);
    return m > 0 ? ((this.ask(i) - this.bid(i)) / m) * 1e6 : 0;
  }
  get(i: number): WsIndex {
    const b = i * INDEX_STRIDE;
    return {
      epoch_ms: this.f64[b]!,
      ticker: this.f64[b + 1]!,
      mid: this.f64[b + 2]!,
      bid: this.f64[b + 3]!,
      ask: this.f64[b + 4]!,
      ci: this.f64[b + 5]!,
      confidence: this.f64[b + 6]!,
      confidence01: this.f64[b + 6]! / 255,
      accepted: this.f64[b + 7]!,
      rejected: this.f64[b + 8]!,
    };
  }
  *[Symbol.iterator](): Iterator<WsIndex> {
    for (let i = 0; i < this.count; i++) yield this.get(i);
  }
  toArray(): WsIndex[] {
    const out: WsIndex[] = new Array(this.count);
    for (let i = 0; i < this.count; i++) out[i] = this.get(i);
    return out;
  }
}

/** Zero-copy batch view over WS tick records. */
export class TickBatch implements Iterable<WsTick> {
  private readonly f64: Float64Array;
  readonly count: number;

  constructor(buf: ArrayBuffer) {
    const dv = new DataView(buf, 0, WS_HEADER_BYTES);
    this.count = dv.getUint16(2, true);
    this.f64 =
      this.count > 0
        ? new Float64Array(buf, WS_HEADER_BYTES, this.count * TICK_STRIDE)
        : new Float64Array(0);
  }

  ts(i: number): number {
    return this.f64[i * TICK_STRIDE]!;
  }
  ticker(i: number): number {
    return this.f64[i * TICK_STRIDE + 1]!;
  }
  providerId(i: number): number {
    return this.f64[i * TICK_STRIDE + 2]!;
  }
  bid(i: number): number {
    return this.f64[i * TICK_STRIDE + 3]!;
  }
  ask(i: number): number {
    return this.f64[i * TICK_STRIDE + 4]!;
  }
  accepted(i: number): boolean {
    return this.f64[i * TICK_STRIDE + 5] === 1;
  }
  get(i: number): WsTick {
    const b = i * TICK_STRIDE;
    return {
      epoch_ms: this.f64[b]!,
      ticker: this.f64[b + 1]!,
      provider_id: this.f64[b + 2]!,
      bid: this.f64[b + 3]!,
      ask: this.f64[b + 4]!,
      accepted: this.f64[b + 5] === 1,
    };
  }
  *[Symbol.iterator](): Iterator<WsTick> {
    for (let i = 0; i < this.count; i++) yield this.get(i);
  }
  toArray(): WsTick[] {
    const out: WsTick[] = new Array(this.count);
    for (let i = 0; i < this.count; i++) out[i] = this.get(i);
    return out;
  }
}

export type DecodedWsFrame =
  | { type: 'index'; batch: IndexBatch }
  | { type: 'tick'; batch: TickBatch }
  | null;

/** Decode a raw WS binary frame into a typed batch. Returns null for unknown / empty. */
export function decodeWsFrame(buf: ArrayBuffer): DecodedWsFrame {
  if (buf.byteLength < WS_HEADER_BYTES) return null;
  const { type, count } = readWsHeader(buf);
  if (count === 0) return null;
  switch (type) {
    case WS_MSG_INDEX:
      return { type: 'index', batch: new IndexBatch(buf) };
    case WS_MSG_TICK:
      return { type: 'tick', batch: new TickBatch(buf) };
    default:
      return null;
  }
}
