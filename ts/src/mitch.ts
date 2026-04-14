/**
 * MITCH wire format types and constants.
 *
 * Canonical spec: ../../mitch/model/overview.md
 * Reference impl: mitch crate (Rust)
 */

// ── Message type codes ───────────────────────────────────────────────────────

export const MSG_TRADE      = 0x74; // 't'
export const MSG_ORDER      = 0x6F; // 'o'
export const MSG_TICK       = 0x73; // 's'
export const MSG_INDEX      = 0x69; // 'i'
export const MSG_BAR        = 0x6B; // 'k'
export const MSG_ORDER_BOOK = 0x62; // 'b'

// ── Wire codes (v2 header type_provider field) ─────────────────────────────

export const WIRE_TRADE      = 1;
export const WIRE_ORDER      = 2;
export const WIRE_TICK       = 3;
export const WIRE_INDEX      = 4;
export const WIRE_ORDER_BOOK = 5;
export const WIRE_BAR        = 6;

// ── Body sizes (bytes) ──────────────────────────────────────────────────────

export const SIZE_HEADER     = 16;
export const SIZE_TRADE      = 24;
export const SIZE_ORDER      = 32;
export const SIZE_TICK       = 32;
export const SIZE_INDEX      = 40;
export const SIZE_BAR        = 128;
export const SIZE_ORDER_BOOK = 2072;

// ── Timestamp: u48 = 16us ticks since 2010-01-01T00:00:00Z ──────────────────

/** 2010-01-01T00:00:00Z in microseconds since Unix epoch. */
export const EPOCH_2010_US = 1_262_304_000_000_000n;

/** Encode Unix-epoch microseconds to u48 mts ticks. */
export function fromEpochUs(us: bigint): bigint {
  return (us - EPOCH_2010_US) >> 4n;
}

/** Decode u48 mts ticks to Unix-epoch microseconds. */
export function toEpochUs(ticks: bigint): bigint {
  return (ticks << 4n) + EPOCH_2010_US;
}

/** Encode Unix-epoch milliseconds to u48 mts ticks. */
export function fromEpochMs(ms: number): bigint {
  return fromEpochUs(BigInt(ms) * 1000n);
}

/** Decode u48 mts ticks to Unix-epoch milliseconds. */
export function toEpochMs(ticks: bigint): number {
  return Number(toEpochUs(ticks) / 1000n);
}

/** Read u48 LE from 6 bytes at offset in a DataView. */
export function readU48(dv: DataView, off: number): bigint {
  const lo = dv.getUint32(off, true);
  const hi = dv.getUint16(off + 4, true);
  return BigInt(lo) | (BigInt(hi) << 32n);
}

/** Write u48 LE to 6 bytes at offset in a DataView. */
export function writeU48(dv: DataView, off: number, val: bigint): void {
  dv.setUint32(off, Number(val & 0xFFFF_FFFFn), true);
  dv.setUint16(off + 4, Number((val >> 32n) & 0xFFFFn), true);
}

// ── Wire code ↔ ASCII msg type ─────────────────────────────────────────────

const wireToAscii: Record<number, number> = {
  [WIRE_TRADE]: MSG_TRADE, [WIRE_ORDER]: MSG_ORDER, [WIRE_TICK]: MSG_TICK,
  [WIRE_INDEX]: MSG_INDEX, [WIRE_ORDER_BOOK]: MSG_ORDER_BOOK, [WIRE_BAR]: MSG_BAR,
};
const asciiToWire: Record<number, number> = {};
for (const [w, a] of Object.entries(wireToAscii)) asciiToWire[a] = Number(w);

/** Map v2 wire code (1-6) → ASCII msg type (e.g. 0x74 't'). */
export function wireCodeToMsgType(code: number): number {
  return wireToAscii[code] ?? 0;
}

/** Map ASCII msg type → v2 wire code (1-6). */
export function msgTypeToWireCode(msgType: number): number {
  return asciiToWire[msgType] ?? 0;
}

// ── MitchHeader (16 bytes) ─────────────────────────────────────────────────

export interface MitchHeader {
  msgType:    number; // ASCII code derived from wire code
  providerId: number; // u12 (0-4095)
  timestamp:  bigint; // u48 mts ticks
  count:      number; // u8
  flags:      number; // u8
  sequence:   number; // u16
}

export function readHeader(buf: ArrayBuffer, off = 0): MitchHeader {
  const dv = new DataView(buf, off, SIZE_HEADER);
  const tp = dv.getUint16(0, true);
  return {
    msgType:    wireCodeToMsgType(tp & 0xF),
    providerId: tp >>> 4,
    timestamp:  readU48(dv, 2),
    count:      dv.getUint8(8),
    flags:      dv.getUint8(9),
    sequence:   dv.getUint16(10, true),
  };
}

export function writeHeader(dv: DataView, off: number, h: MitchHeader): void {
  const wire = msgTypeToWireCode(h.msgType);
  dv.setUint16(off, (h.providerId << 4) | (wire & 0xF), true);
  writeU48(dv, off + 2, h.timestamp);
  dv.setUint8(off + 8, h.count);
  dv.setUint8(off + 9, h.flags);
  dv.setUint16(off + 10, h.sequence, true);
  // bytes 12-15 reserved, zero them
  dv.setUint32(off + 12, 0, true);
}

// ── Index (40 bytes body) ───────────────────────────────────────────────────

export interface Index {
  ticker:     number; // u64 (safe as f64 for IDs < 2^53)
  bid:        number; // f64
  ask:        number; // f64
  vbid:       number; // u32
  vask:       number; // u32
  ci:         number; // u16 micro basis points
  tickCount:  number; // u16
  confidence: number; // u8
  accepted:   number; // u8
  rejected:   number; // u8
}

/** Read Index body from bytes at offset. */
export function readIndex(dv: DataView, off: number): Index {
  return {
    ticker:     Number(dv.getBigUint64(off, true)),
    bid:        dv.getFloat64(off + 8, true),
    ask:        dv.getFloat64(off + 16, true),
    vbid:       dv.getUint32(off + 24, true),
    vask:       dv.getUint32(off + 28, true),
    ci:         dv.getUint16(off + 32, true),
    tickCount:  dv.getUint16(off + 34, true),
    confidence: dv.getUint8(off + 36),
    accepted:   dv.getUint8(off + 37),
    rejected:   dv.getUint8(off + 38),
  };
}

// ── Tick (32 bytes body) ────────────────────────────────────────────────────

export interface Tick {
  ticker: number; // u64
  bid:    number; // f64
  ask:    number; // f64
  vbid:   number; // u32
  vask:   number; // u32
}

export function readTick(dv: DataView, off: number): Tick {
  return {
    ticker: Number(dv.getBigUint64(off, true)),
    bid:    dv.getFloat64(off + 8, true),
    ask:    dv.getFloat64(off + 16, true),
    vbid:   dv.getUint32(off + 24, true),
    vask:   dv.getUint32(off + 28, true),
  };
}

// ── Trade (24 bytes body) ───────────────────────────────────────────────────

export interface Trade {
  ticker:  number; // u64
  price:   number; // f64
  volume:  number; // u32
  tradeId: number; // u24
  side:    number; // u8 (0=Buy, 1=Sell)
}

export function readTrade(dv: DataView, off: number): Trade {
  return {
    ticker:  Number(dv.getBigUint64(off, true)),
    price:   dv.getFloat64(off + 8, true),
    volume:  dv.getUint32(off + 16, true),
    tradeId: dv.getUint8(off + 20) | (dv.getUint8(off + 21) << 8) | (dv.getUint8(off + 22) << 16),
    side:    dv.getUint8(off + 23),
  };
}

// ── Derived helpers ─────────────────────────────────────────────────────────

/** Mid price: (bid + ask) / 2. */
export function mid(bid: number, ask: number): number {
  return (bid + ask) / 2;
}

/** Spread in micro basis points: (ask - bid) / mid * 1e6. */
export function spreadUbp(bid: number, ask: number): number {
  const m = (bid + ask) / 2;
  return m > 0 ? (ask - bid) / m * 1e6 : 0;
}

/** Spread in basis points: (ask - bid) / mid * 1e4. */
export function spreadBps(bid: number, ask: number): number {
  const m = (bid + ask) / 2;
  return m > 0 ? (ask - bid) / m * 1e4 : 0;
}

/** Decode CI (u16 UBP) to price units given mid. */
export function ciToPrice(ci: number, midPrice: number): number {
  return (ci / 1e8) * midPrice;
}
