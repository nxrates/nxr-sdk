/**
 * Zero-copy batch decoder for NXR WebSocket binary frames.
 *
 * The WS server encodes messages as:
 *   [8B header][count * stride f64 values]
 *
 * Index STRIDE=9:  epoch_ms, ticker, mid, bid, ask, ci, confidence, accepted, rejected
 * Tick  STRIDE=6:  epoch_ms, ticker, provider_id, bid, ask, accepted
 *
 * "Zero-copy" means the batch classes hold a Float64Array view into the
 * original ArrayBuffer -- no per-record allocation until you call get().
 *
 * Hot path: iterate with field accessors (ticker(i), bid(i), etc.)
 * Cold path: materialize with get(i) or [...batch]
 */

// ── WS frame constants ──────────────────────────────────────────────────────

export const WS_MSG_INDEX    = 1;
export const WS_MSG_TICK = 2;
export const WS_HEADER_BYTES = 8;

export const INDEX_STRIDE    = 9;
export const TICK_STRIDE = 6;

// ── WS record types (materialized) ─────────────────────────────────────────

export interface WsIndex {
  epoch_ms:      number;
  ticker:     number;
  mid:        number;
  bid:        number;
  ask:        number;
  ci:         number;
  confidence: number;
  accepted:   number;
  rejected:   number;
}

export interface WsTick {
  epoch_ms:       number;
  ticker:      number;
  provider_id: number;
  bid:         number;
  ask:         number;
  accepted:    boolean;
}

// ── Frame header ────────────────────────────────────────────────────────────

export interface WsFrameHeader {
  type:  number; // WS_MSG_INDEX | WS_MSG_TICK
  count: number; // u16 LE at offset 2
}

/** Parse WS frame header (first 8 bytes). */
export function readWsHeader(buf: ArrayBuffer): WsFrameHeader {
  const dv = new DataView(buf, 0, WS_HEADER_BYTES);
  return { type: dv.getUint8(0), count: dv.getUint16(2, true) };
}

// ── IndexBatch: zero-copy accessor over Float64Array ────────────────────────

/**
 * Zero-copy batch view over WS index records.
 *
 * The underlying Float64Array is a view into the original ArrayBuffer --
 * no data is copied. Field accessors are O(1) indexed reads.
 *
 * @example
 * ```ts
 * const batch = new IndexBatch(e.data);
 * for (let i = 0; i < batch.count; i++) {
 *   if (batch.ticker(i) === targetId) {
 *     console.log(`mid=${batch.mid(i)} ci=${batch.ci(i)}`);
 *   }
 * }
 * ```
 */
export class IndexBatch implements Iterable<WsIndex> {
  private readonly f64: Float64Array;
  readonly count: number;

  constructor(buf: ArrayBuffer) {
    const dv = new DataView(buf, 0, WS_HEADER_BYTES);
    this.count = dv.getUint16(2, true);
    this.f64 = this.count > 0
      ? new Float64Array(buf, WS_HEADER_BYTES, this.count * INDEX_STRIDE)
      : new Float64Array(0);
  }

  // ── Zero-copy field accessors (hot path) ──────────────────────────────

  ts(i: number): number         { return this.f64[i * INDEX_STRIDE]; }
  ticker(i: number): number     { return this.f64[i * INDEX_STRIDE + 1]; }
  mid(i: number): number        { return this.f64[i * INDEX_STRIDE + 2]; }
  bid(i: number): number        { return this.f64[i * INDEX_STRIDE + 3]; }
  ask(i: number): number        { return this.f64[i * INDEX_STRIDE + 4]; }
  ci(i: number): number         { return this.f64[i * INDEX_STRIDE + 5]; }
  confidence(i: number): number { return this.f64[i * INDEX_STRIDE + 6]; }
  accepted(i: number): number   { return this.f64[i * INDEX_STRIDE + 7]; }
  rejected(i: number): number   { return this.f64[i * INDEX_STRIDE + 8]; }

  /** Spread in micro basis points (derived). */
  spreadUbp(i: number): number {
    const m = this.mid(i);
    return m > 0 ? (this.ask(i) - this.bid(i)) / m * 1e6 : 0;
  }

  // ── Materialization (cold path) ───────────────────────────────────────

  /** Materialize record i as a WsIndex object. Allocates. */
  get(i: number): WsIndex {
    const b = i * INDEX_STRIDE;
    return {
      epoch_ms:      this.f64[b],
      ticker:     this.f64[b + 1],
      mid:        this.f64[b + 2],
      bid:        this.f64[b + 3],
      ask:        this.f64[b + 4],
      ci:         this.f64[b + 5],
      confidence: this.f64[b + 6],
      accepted:   this.f64[b + 7],
      rejected:   this.f64[b + 8],
    };
  }

  /** Iterate all records (materializes each). */
  *[Symbol.iterator](): Iterator<WsIndex> {
    for (let i = 0; i < this.count; i++) yield this.get(i);
  }

  /** Materialize all records to array. */
  toArray(): WsIndex[] {
    const out: WsIndex[] = new Array(this.count);
    for (let i = 0; i < this.count; i++) out[i] = this.get(i);
    return out;
  }

  /** Find first record matching predicate (zero-copy scan). */
  find(pred: (batch: IndexBatch, i: number) => boolean): WsIndex | undefined {
    for (let i = 0; i < this.count; i++) {
      if (pred(this, i)) return this.get(i);
    }
    return undefined;
  }
}

// ── TickBatch: zero-copy accessor over Float64Array ─────────────────────────

export class TickBatch implements Iterable<WsTick> {
  private readonly f64: Float64Array;
  readonly count: number;

  constructor(buf: ArrayBuffer) {
    const dv = new DataView(buf, 0, WS_HEADER_BYTES);
    this.count = dv.getUint16(2, true);
    this.f64 = this.count > 0
      ? new Float64Array(buf, WS_HEADER_BYTES, this.count * TICK_STRIDE)
      : new Float64Array(0);
  }

  // ── Zero-copy field accessors ─────────────────────────────────────────

  ts(i: number): number         { return this.f64[i * TICK_STRIDE]; }
  ticker(i: number): number     { return this.f64[i * TICK_STRIDE + 1]; }
  providerId(i: number): number { return this.f64[i * TICK_STRIDE + 2]; }
  bid(i: number): number        { return this.f64[i * TICK_STRIDE + 3]; }
  ask(i: number): number        { return this.f64[i * TICK_STRIDE + 4]; }
  accepted(i: number): boolean  { return this.f64[i * TICK_STRIDE + 5] === 1; }

  /** Spread in micro basis points (derived). */
  spreadUbp(i: number): number {
    const mid = (this.bid(i) + this.ask(i)) / 2;
    return mid > 0 ? (this.ask(i) - this.bid(i)) / mid * 1e6 : 0;
  }

  get(i: number): WsTick {
    const b = i * TICK_STRIDE;
    return {
      epoch_ms:       this.f64[b],
      ticker:      this.f64[b + 1],
      provider_id: this.f64[b + 2],
      bid:         this.f64[b + 3],
      ask:         this.f64[b + 4],
      accepted:    this.f64[b + 5] === 1,
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

// ── Dispatch decoder ────────────────────────────────────────────────────────

export type DecodedFrame =
  | { type: 'index';    batch: IndexBatch }
  | { type: 'tick'; batch: TickBatch }
  | null;

/**
 * Decode a raw WS binary frame into a typed batch.
 * Returns null for unknown frame types or empty frames.
 */
export function decodeFrame(buf: ArrayBuffer): DecodedFrame {
  if (buf.byteLength < WS_HEADER_BYTES) return null;
  const { type, count } = readWsHeader(buf);
  if (count === 0) return null;

  switch (type) {
    case WS_MSG_INDEX:    return { type: 'index',    batch: new IndexBatch(buf) };
    case WS_MSG_TICK: return { type: 'tick', batch: new TickBatch(buf) };
    default:              return null;
  }
}
