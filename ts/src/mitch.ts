/**
 * MITCH protocol constants + zero-dep DataView decoders.
 *
 * Mirrors `mitch::header`, `mitch::index`, `mitch::tick`, `mitch::bar` from the
 * canonical Rust impl. All wire layouts are little-endian, `#[repr(C, packed)]`.
 *
 * Sizes:
 *   header  16B   index   40B (body)   tick   32B (body)   bar   96B (body)
 *   IndexRecord = header + index = 56B (on-disk + UDP frame)
 *
 * Timestamp encoding: u48 LE = 16 us ticks since 2010-01-01T00:00:00Z.
 *   epoch_ms = (mts << 4) / 1000 + EPOCH_MS_2010
 *
 * CI encoding (Index.ci, Bar.avg_ci_ubp): sqrt-compressed u16.
 *   ci_ubp = (encoded / 16)^2
 *   ci_price = mid * ci_ubp / 1e8
 */

// caveman: ∀ sizes ← canonical mitch crate

/** MITCH message type ASCII codes. */
export const MessageType = {
  TRADE: 0x74, // 't'
  ORDER: 0x6f, // 'o'
  TICK: 0x73, // 's'
  INDEX: 0x69, // 'i'
  ORDER_BOOK: 0x62, // 'b'
  BAR: 0x6b, // 'k'
  HEARTBEAT: 0x68, // 'h'
} as const;

/** 4-bit wire codes (low nibble of MitchHeader.type_provider). */
export const WireCode = {
  TRADE: 1,
  ORDER: 2,
  TICK: 3,
  INDEX: 4,
  ORDER_BOOK: 5,
  BAR: 6,
  HEARTBEAT: 7,
} as const;

/** Wire sizes in bytes for each MITCH body type. */
export const SIZE_HEADER = 16;
export const SIZE_TRADE = 24;
export const SIZE_ORDER = 32;
export const SIZE_TICK = 32;
export const SIZE_INDEX = 40;
export const SIZE_BAR = 96;
export const SIZE_ORDER_BOOK = 2072;
export const SIZE_INDEX_RECORD = SIZE_HEADER + SIZE_INDEX; // 56

/** 2010-01-01T00:00:00Z as Unix epoch milliseconds. */
export const EPOCH_MS_2010 = 1_262_304_000_000;
/** 2010-01-01T00:00:00Z as Unix epoch microseconds. */
export const EPOCH_US_2010 = 1_262_304_000_000_000;
/** CI sqrt-encoding scale factor. Must match `mitch::common::CI_SCALE`. */
export const CI_SCALE = 16;

// ── Timestamp helpers ──────────────────────────────────────────────────────

/**
 * Read u48 LE from a DataView at offset. Returns bigint to preserve all 48 bits
 * losslessly even when above 2^53.
 */
export function readU48(dv: DataView, off: number): bigint {
  // caveman: u48 = lo32 | (hi16 << 32). assemble bigint.
  const lo = dv.getUint32(off, true);
  const hi = dv.getUint16(off + 4, true);
  return BigInt(lo) | (BigInt(hi) << 32n);
}

/**
 * Decode a 16us tick value (mts) to Unix epoch milliseconds.
 * mts is `bigint` to avoid the 2^53 ceiling. Output is `number` (ms fits easily).
 */
export function mtsToEpochMs(mts: bigint): number {
  // epoch_us = (mts << 4) + EPOCH_US_2010 ; epoch_ms = epoch_us / 1000
  const epochUs = (mts << 4n) + BigInt(EPOCH_US_2010);
  return Number(epochUs / 1000n);
}

/** Convert Unix epoch ms → MITCH 16us tick value (mts). */
export function epochMsToMts(epochMs: number): bigint {
  if (epochMs <= EPOCH_MS_2010) return 0n;
  const us = BigInt(epochMs - EPOCH_MS_2010) * 1000n;
  return us >> 4n;
}

// ── CI codec ──────────────────────────────────────────────────────────────

/** Decode sqrt-compressed u16 CI → micro-basis-points (ubp) of mid. */
export function ciToUbp(encoded: number): number {
  const x = encoded / CI_SCALE;
  return x * x;
}

/** Convert decoded ubp + mid → CI in price units. */
export function ciToPrice(ubp: number, mid: number): number {
  return (mid * ubp) / 1e8;
}

// ── MitchHeader ────────────────────────────────────────────────────────────

export interface MitchHeader {
  /** ASCII msg type ('t', 'o', 's', 'i', 'b', 'k', 'h'). */
  msgType: number;
  /** Provider id (12-bit, 0-4095). */
  providerId: number;
  /** 16us ticks since 2010-01-01 (u48). */
  mts: bigint;
  /** Number of body entries in batch (u8, 1-255). */
  count: number;
  /** Flags byte (low 2 bits = version). */
  flags: number;
  /** Per-stream sequence (u16) for gap detection. */
  sequence: number;
}

/** Decode 16B MitchHeader from buffer at offset. */
export function readHeader(dv: DataView, off: number): MitchHeader {
  const tp = dv.getUint16(off, true);
  const code = tp & 0x0f;
  // caveman: code → ASCII via reverse table
  const codeToAscii = [
    0,
    MessageType.TRADE,
    MessageType.ORDER,
    MessageType.TICK,
    MessageType.INDEX,
    MessageType.ORDER_BOOK,
    MessageType.BAR,
    MessageType.HEARTBEAT,
  ];
  return {
    msgType: code < codeToAscii.length ? codeToAscii[code]! : 0,
    providerId: tp >> 4,
    mts: readU48(dv, off + 2),
    count: dv.getUint8(off + 8),
    flags: dv.getUint8(off + 9),
    sequence: dv.getUint16(off + 10, true),
  };
}

// ── Body: Index (40B) ─────────────────────────────────────────────────────

export interface Index {
  ticker: bigint; // u64
  bid: number; // f64
  ask: number; // f64
  vbid: number; // u32
  vask: number; // u32
  ci: number; // u16 sqrt-encoded
  tickCount: number; // u16
  confidence: number; // u8 raw freshness byte (percent 0-100 when FLAG_CONF_FRESHNESS set)
  confidence01: number; // confidence / 100 (freshness float ∈ [0,1])
  accepted: number; // u8
  rejected: number; // u8
  flags: number; // u8
}

/** Decode 40B Index body at offset. */
export function readIndex(dv: DataView, off: number): Index {
  return {
    ticker: dv.getBigUint64(off, true),
    bid: dv.getFloat64(off + 8, true),
    ask: dv.getFloat64(off + 16, true),
    vbid: dv.getUint32(off + 24, true),
    vask: dv.getUint32(off + 28, true),
    ci: dv.getUint16(off + 32, true),
    tickCount: dv.getUint16(off + 34, true),
    confidence: dv.getUint8(off + 36),
    confidence01: dv.getUint8(off + 36) / 100,
    accepted: dv.getUint8(off + 37),
    rejected: dv.getUint8(off + 38),
    flags: dv.getUint8(off + 39),
  };
}

// ── Body: Tick (32B) ──────────────────────────────────────────────────────

export interface Tick {
  ticker: bigint;
  bid: number;
  ask: number;
  vbid: number;
  vask: number;
}

/** Decode 32B Tick body at offset. */
export function readTick(dv: DataView, off: number): Tick {
  return {
    ticker: dv.getBigUint64(off, true),
    bid: dv.getFloat64(off + 8, true),
    ask: dv.getFloat64(off + 16, true),
    vbid: dv.getUint32(off + 24, true),
    vask: dv.getUint32(off + 28, true),
  };
}

// ── Body: Bar (96B) ───────────────────────────────────────────────────────

export interface Bar {
  /** Bar open ts as Unix epoch ms. */
  openMs: number;
  /** Bar close ts as Unix epoch ms. */
  closeMs: number;
  /** Raw u48 open mts. */
  openMts: bigint;
  /** Raw u48 close mts. */
  closeMts: bigint;
  open: number;
  high: number;
  low: number;
  close: number;
  vbid: number; // u32
  vask: number; // u32
  tickCount: number; // u32
  // microstructure (32B section)
  realizedVar: number; // f32
  bipowerVar: number; // f32
  drift: number; // f32
  volImbalance: number; // f32
  avgSpreadBps: number; // f32
  maxAbsReturn: number; // f32
  avgCiUbp: number; // u16
  rejectRate: number; // u16
  kind: number; // u8 (0=kline, 1=renko, 2=dib, 3=tib)
}

/** Decode 96B Bar at offset. */
export function readBar(dv: DataView, off: number): Bar {
  const openMts = readU48(dv, off);
  const closeMts = readU48(dv, off + 6);
  return {
    openMts,
    closeMts,
    openMs: mtsToEpochMs(openMts),
    closeMs: mtsToEpochMs(closeMts),
    open: dv.getFloat64(off + 12, true),
    high: dv.getFloat64(off + 20, true),
    low: dv.getFloat64(off + 28, true),
    close: dv.getFloat64(off + 36, true),
    vbid: dv.getUint32(off + 44, true),
    vask: dv.getUint32(off + 48, true),
    tickCount: dv.getUint32(off + 52, true),
    // _pad: 8 bytes @ 56..64
    realizedVar: dv.getFloat32(off + 64, true),
    bipowerVar: dv.getFloat32(off + 68, true),
    drift: dv.getFloat32(off + 72, true),
    volImbalance: dv.getFloat32(off + 76, true),
    avgSpreadBps: dv.getFloat32(off + 80, true),
    maxAbsReturn: dv.getFloat32(off + 84, true),
    avgCiUbp: dv.getUint16(off + 88, true),
    rejectRate: dv.getUint16(off + 90, true),
    kind: dv.getUint8(off + 92),
  };
}

// ── Derived helpers ────────────────────────────────────────────────────────

/** Mid price from bid/ask. */
export function mid(bid: number, ask: number): number {
  return (bid + ask) * 0.5;
}

/** Spread in micro basis points: (ask - bid) / mid * 1e6. */
export function spreadUbp(bid: number, ask: number): number {
  const m = mid(bid, ask);
  return m > 0 ? ((ask - bid) / m) * 1e6 : 0;
}

/** Spread in basis points: (ask - bid) / mid * 1e4. */
export function spreadBps(bid: number, ask: number): number {
  const m = mid(bid, ask);
  return m > 0 ? ((ask - bid) / m) * 1e4 : 0;
}
