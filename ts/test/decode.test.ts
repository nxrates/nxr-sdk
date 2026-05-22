import { describe, expect, it } from 'vitest';

import {
  CI_SCALE,
  EPOCH_MS_2010,
  SIZE_BAR,
  SIZE_HEADER,
  SIZE_INDEX,
  SIZE_INDEX_RECORD,
  ciToUbp,
  decodeBar,
  decodeBarBatch,
  decodeIdxBatch,
  decodeIdxRecord,
  epochMsToMts,
  mtsToEpochMs,
  readU48,
  readHeader,
} from '../src/index.js';
import { WireCode } from '../src/mitch.js';

// ── Test fixture builders ─────────────────────────────────────────────────

function buildIndexRecord(params: {
  providerId: number;
  mts: bigint;
  count: number;
  sequence: number;
  ticker: bigint;
  bid: number;
  ask: number;
  vbid: number;
  vask: number;
  ci: number;
  tickCount: number;
  confidence: number;
  accepted: number;
  rejected: number;
}): Uint8Array {
  const buf = new Uint8Array(SIZE_INDEX_RECORD);
  const dv = new DataView(buf.buffer);
  // header
  const tp = (WireCode.INDEX & 0x0f) | (params.providerId << 4);
  dv.setUint16(0, tp, true);
  // u48 mts LE
  const lo = Number(params.mts & 0xffffffffn);
  const hi = Number((params.mts >> 32n) & 0xffffn);
  dv.setUint32(2, lo, true);
  dv.setUint16(6, hi, true);
  dv.setUint8(8, params.count);
  dv.setUint8(9, 0); // flags
  dv.setUint16(10, params.sequence, true);
  // reserved 12..15 = 0
  // body
  dv.setBigUint64(SIZE_HEADER + 0, params.ticker, true);
  dv.setFloat64(SIZE_HEADER + 8, params.bid, true);
  dv.setFloat64(SIZE_HEADER + 16, params.ask, true);
  dv.setUint32(SIZE_HEADER + 24, params.vbid, true);
  dv.setUint32(SIZE_HEADER + 28, params.vask, true);
  dv.setUint16(SIZE_HEADER + 32, params.ci, true);
  dv.setUint16(SIZE_HEADER + 34, params.tickCount, true);
  dv.setUint8(SIZE_HEADER + 36, params.confidence);
  dv.setUint8(SIZE_HEADER + 37, params.accepted);
  dv.setUint8(SIZE_HEADER + 38, params.rejected);
  dv.setUint8(SIZE_HEADER + 39, 0); // flags
  return buf;
}

function buildBar(params: {
  openMts: bigint;
  closeMts: bigint;
  open: number;
  high: number;
  low: number;
  close: number;
  vbid: number;
  vask: number;
  tickCount: number;
}): Uint8Array {
  const buf = new Uint8Array(SIZE_BAR);
  const dv = new DataView(buf.buffer);
  // u48 open_ts
  const ol = Number(params.openMts & 0xffffffffn);
  const oh = Number((params.openMts >> 32n) & 0xffffn);
  dv.setUint32(0, ol, true);
  dv.setUint16(4, oh, true);
  // u48 close_ts
  const cl = Number(params.closeMts & 0xffffffffn);
  const ch = Number((params.closeMts >> 32n) & 0xffffn);
  dv.setUint32(6, cl, true);
  dv.setUint16(10, ch, true);

  dv.setFloat64(12, params.open, true);
  dv.setFloat64(20, params.high, true);
  dv.setFloat64(28, params.low, true);
  dv.setFloat64(36, params.close, true);
  dv.setUint32(44, params.vbid, true);
  dv.setUint32(48, params.vask, true);
  dv.setUint32(52, params.tickCount, true);
  // microstructure left zero
  return buf;
}

// ── Constants sanity ──────────────────────────────────────────────────────

describe('MITCH wire sizes', () => {
  it('matches canonical Rust layout', () => {
    expect(SIZE_HEADER).toBe(16);
    expect(SIZE_INDEX).toBe(40);
    expect(SIZE_INDEX_RECORD).toBe(56);
    expect(SIZE_BAR).toBe(96);
  });
});

// ── Timestamp codec ───────────────────────────────────────────────────────

describe('timestamp codec', () => {
  it('round-trips epoch_ms → mts → epoch_ms', () => {
    const ms = 1_744_372_800_000; // 2026-04-11T12:00:00Z
    const mts = epochMsToMts(ms);
    const back = mtsToEpochMs(mts);
    expect(Math.abs(back - ms)).toBeLessThanOrEqual(1);
  });

  it('saturates pre-2010 to 0', () => {
    expect(epochMsToMts(0)).toBe(0n);
    expect(epochMsToMts(EPOCH_MS_2010)).toBe(0n);
  });

  it('reads u48 LE losslessly above 2^32', () => {
    const buf = new Uint8Array(6);
    // 0xAA_BBCCDDEEFF (big endian: AA BB CC DD EE FF)
    // LE bytes: FF EE DD CC BB AA
    buf.set([0xff, 0xee, 0xdd, 0xcc, 0xbb, 0xaa]);
    const dv = new DataView(buf.buffer);
    const v = readU48(dv, 0);
    expect(v).toBe(0xaabbccddeeffn);
  });
});

// ── CI codec ──────────────────────────────────────────────────────────────

describe('CI codec', () => {
  it('inverts sqrt-encoding', () => {
    // ci_ubp = (encoded / 16)^2.  16 → 1.0 ubp.  256 → 256 ubp.
    expect(ciToUbp(16)).toBeCloseTo(1.0, 9);
    expect(ciToUbp(0)).toBe(0);
    expect(ciToUbp(160)).toBeCloseTo(100.0, 6);
    // Sanity: scale factor
    expect(CI_SCALE).toBe(16);
  });
});

// ── IndexRecord decoding ──────────────────────────────────────────────────

describe('decodeIdxRecord', () => {
  it('decodes a synthetic 56B IndexRecord', () => {
    const mts = epochMsToMts(1_744_372_800_000);
    const buf = buildIndexRecord({
      providerId: 101, // Binance
      mts,
      count: 1,
      sequence: 42,
      ticker: 0x1234_5678_9abc_def0n,
      bid: 50_000.5,
      ask: 50_001.25,
      vbid: 100,
      vask: 200,
      ci: 32, // (32/16)^2 = 4 ubp
      tickCount: 10,
      confidence: 3,
      accepted: 5,
      rejected: 1,
    });
    const rec = decodeIdxRecord(buf, 0);
    expect(rec.provider).toBe(101);
    expect(rec.ticker).toBe(0x1234_5678_9abc_def0n);
    expect(rec.bid).toBe(50_000.5);
    expect(rec.ask).toBe(50_001.25);
    expect(rec.mid).toBe((50_000.5 + 50_001.25) / 2);
    expect(rec.ci_ubp).toBeCloseTo(4.0, 9);
    expect(rec.accepted).toBe(5);
    expect(rec.rejected).toBe(1);
    expect(rec.confidence).toBe(3);
    expect(rec.vbid).toBe(100);
    expect(rec.vask).toBe(200);
    expect(rec.tick_count).toBe(10);
    expect(rec.sequence).toBe(42);
    expect(Math.abs(rec.ts_ms - 1_744_372_800_000)).toBeLessThanOrEqual(1);
  });

  it('throws on undersized buffer', () => {
    const buf = new Uint8Array(SIZE_INDEX_RECORD - 1);
    expect(() => decodeIdxRecord(buf, 0)).toThrow(RangeError);
  });

  it('respects byteOffset on a shared ArrayBuffer', () => {
    // Build a record at offset 100 in a larger buffer.
    const big = new Uint8Array(200);
    const slot = buildIndexRecord({
      providerId: 7,
      mts: 0n,
      count: 1,
      sequence: 0,
      ticker: 1n,
      bid: 1,
      ask: 2,
      vbid: 0,
      vask: 0,
      ci: 0,
      tickCount: 0,
      confidence: 0,
      accepted: 0,
      rejected: 0,
    });
    big.set(slot, 100);
    const view = new Uint8Array(big.buffer, 100, SIZE_INDEX_RECORD);
    const rec = decodeIdxRecord(view, 0);
    expect(rec.provider).toBe(7);
    expect(rec.bid).toBe(1);
    expect(rec.ask).toBe(2);
  });
});

describe('decodeIdxBatch', () => {
  it('decodes contiguous batch of records', () => {
    const n = 5;
    const buf = new Uint8Array(n * SIZE_INDEX_RECORD);
    for (let i = 0; i < n; i++) {
      const slot = buildIndexRecord({
        providerId: 100 + i,
        mts: 0n,
        count: 1,
        sequence: i,
        ticker: BigInt(i + 1),
        bid: 100 + i,
        ask: 101 + i,
        vbid: 10,
        vask: 20,
        ci: 16,
        tickCount: 1,
        confidence: 1,
        accepted: 1,
        rejected: 0,
      });
      buf.set(slot, i * SIZE_INDEX_RECORD);
    }
    const recs = decodeIdxBatch(buf);
    expect(recs).toHaveLength(n);
    for (let i = 0; i < n; i++) {
      expect(recs[i]!.provider).toBe(100 + i);
      expect(recs[i]!.sequence).toBe(i);
      expect(recs[i]!.ticker).toBe(BigInt(i + 1));
      expect(recs[i]!.bid).toBe(100 + i);
    }
  });

  it('returns empty array for empty buffer', () => {
    expect(decodeIdxBatch(new Uint8Array(0))).toEqual([]);
  });

  it('ignores trailing partial record', () => {
    const buf = new Uint8Array(SIZE_INDEX_RECORD + 10);
    const slot = buildIndexRecord({
      providerId: 1,
      mts: 0n,
      count: 1,
      sequence: 0,
      ticker: 1n,
      bid: 1,
      ask: 2,
      vbid: 0,
      vask: 0,
      ci: 0,
      tickCount: 0,
      confidence: 0,
      accepted: 0,
      rejected: 0,
    });
    buf.set(slot, 0);
    const recs = decodeIdxBatch(buf);
    expect(recs).toHaveLength(1);
  });
});

// ── Header decoding ───────────────────────────────────────────────────────

describe('readHeader', () => {
  it('extracts msgType, providerId, mts, count, sequence', () => {
    const buf = buildIndexRecord({
      providerId: 0x123,
      mts: 0x1122_3344_5566n,
      count: 7,
      sequence: 0xbeef,
      ticker: 0n,
      bid: 0,
      ask: 0,
      vbid: 0,
      vask: 0,
      ci: 0,
      tickCount: 0,
      confidence: 0,
      accepted: 0,
      rejected: 0,
    });
    const dv = new DataView(buf.buffer);
    const h = readHeader(dv, 0);
    expect(h.providerId).toBe(0x123);
    expect(h.mts).toBe(0x1122_3344_5566n);
    expect(h.count).toBe(7);
    expect(h.sequence).toBe(0xbeef);
    // msgType ASCII for INDEX = 'i' = 105
    expect(h.msgType).toBe(0x69);
  });
});

// ── Bar decoding ──────────────────────────────────────────────────────────

describe('decodeBar', () => {
  it('decodes a synthetic 96B bar', () => {
    const openMts = epochMsToMts(1_744_372_800_000);
    const closeMts = epochMsToMts(1_744_372_860_000);
    const buf = buildBar({
      openMts,
      closeMts,
      open: 100.0,
      high: 105.0,
      low: 99.0,
      close: 103.0,
      vbid: 1000,
      vask: 1200,
      tickCount: 50,
    });
    const b = decodeBar(buf, 0);
    expect(b.open).toBe(100);
    expect(b.high).toBe(105);
    expect(b.low).toBe(99);
    expect(b.close).toBe(103);
    expect(b.vbid).toBe(1000);
    expect(b.vask).toBe(1200);
    expect(b.tick_count).toBe(50);
    expect(Math.abs(b.open_ms - 1_744_372_800_000)).toBeLessThanOrEqual(1);
    expect(Math.abs(b.close_ms - 1_744_372_860_000)).toBeLessThanOrEqual(1);
    expect(b.kind).toBe(0);
  });

  it('throws on undersized buffer', () => {
    expect(() => decodeBar(new Uint8Array(50), 0)).toThrow(RangeError);
  });
});

describe('decodeBarBatch', () => {
  it('decodes contiguous bars', () => {
    const n = 3;
    const buf = new Uint8Array(n * SIZE_BAR);
    for (let i = 0; i < n; i++) {
      const slot = buildBar({
        openMts: 0n,
        closeMts: 0n,
        open: 100 + i,
        high: 105 + i,
        low: 95 + i,
        close: 103 + i,
        vbid: 10 * i,
        vask: 20 * i,
        tickCount: i,
      });
      buf.set(slot, i * SIZE_BAR);
    }
    const bars = decodeBarBatch(buf);
    expect(bars).toHaveLength(n);
    for (let i = 0; i < n; i++) {
      expect(bars[i]!.open).toBe(100 + i);
      expect(bars[i]!.tick_count).toBe(i);
    }
  });
});
