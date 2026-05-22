/**
 * NxrClient - REST + WebSocket client for NX Rates.
 *
 * Universal: works in browsers, Node.js, Deno, Bun via `fetch` + `WebSocket`.
 *
 * @example
 * ```ts
 * import { NxrClient } from '@nxrates/sdk';
 *
 * const nxr = new NxrClient({ baseUrl: 'http://nxr.nxrates.com' });
 *
 * // JSON endpoints
 * const tickers = await nxr.tickers();
 * const ohlc    = await nxr.ohlc('BTC/USDT', 60_000, { limit: 100 });
 *
 * // Fast binary (octet-stream + MITCH decode)
 * const recs = await nxr.idxBinary('BTC/USDT', { limit: 1024 });
 *
 * // WS stream (zero-copy)
 * nxr.onIndex(batch => {
 *   for (let i = 0; i < batch.count; i++) console.log(batch.mid(i));
 * });
 * nxr.connect();
 * ```
 */

import {
  IndexBatch,
  TickBatch,
  WS_HEADER_BYTES,
  WS_MSG_INDEX,
  WS_MSG_TICK,
  decodeBarBatch,
  decodeIdxBatch,
  type WsIndex,
  type WsTick,
} from './decode.js';
import type {
  Bar,
  BarKind,
  IndexRecord,
  Ohlc,
  SynthTick,
  Sym,
  TickerSnapshot,
} from './types.js';

// ── WebSocket state ────────────────────────────────────────────────────────

export type WsState = 'disconnected' | 'connecting' | 'connected' | 'error';

/** Query options for time-bounded endpoints. */
export interface RangeOpts {
  /** Inclusive lower bound, Unix epoch ms. */
  from?: number;
  /** Exclusive upper bound, Unix epoch ms. */
  to?: number;
  /** Max rows to return. */
  limit?: number;
}

/** Re-export for back-compat. */
export interface TickerResponse {
  ticker: number;
  mid: number;
  bid: number;
  ask: number;
  ci: number;
  confidence: number;
}

export interface NxrClientOpts {
  /** REST/WS root. e.g. `http://nxr.nxrates.com` or `http://nxr-svc:40004`. */
  baseUrl: string;
  /** WS reconnect delay (default 3000 ms). */
  reconnectMs?: number;
  /** Optional fetch override (test injection). */
  fetch?: typeof fetch;
}

/** HTTP + WebSocket client for the NXR v1 API. */
export class NxrClient {
  private readonly baseUrl: string;
  private readonly _fetch: typeof fetch;
  private wsUrl: string | null = null;
  private ws: WebSocket | null = null;
  private reconnectTimer: ReturnType<typeof setTimeout> | null = null;
  private _wsState: WsState = 'disconnected';
  private reconnectMs = 3000;

  private indexCbs = new Set<(batch: IndexBatch) => void>();
  private tickCbs = new Set<(batch: TickBatch) => void>();
  private indexRecordCbs = new Set<(idx: WsIndex) => void>();
  private tickRecordCbs = new Set<(tick: WsTick) => void>();
  private stateCbs = new Set<(state: WsState) => void>();

  constructor(opts: NxrClientOpts) {
    this.baseUrl = opts.baseUrl.replace(/\/$/, '');
    if (opts.reconnectMs) this.reconnectMs = opts.reconnectMs;
    this._fetch = opts.fetch ?? globalThis.fetch.bind(globalThis);
  }

  // ── REST: discovery ─────────────────────────────────────────────────────

  /** Health check. */
  async isHealthy(): Promise<boolean> {
    try {
      const r = await this._fetch(`${this.baseUrl}/health`);
      return r.ok;
    } catch {
      return false;
    }
  }

  /** Fetch symbol → ticker_id map from `/v1/symbols`. */
  async symbols(): Promise<Map<Sym, bigint>> {
    const data = await this.json<Record<string, string | number>>('/v1/symbols');
    return new Map(Object.entries(data).map(([k, v]) => [k, BigInt(v)]));
  }

  /** Resolve unified symbol (e.g. "BTC/USDT") to ticker_id. */
  async resolve(symbol: Sym): Promise<bigint | undefined> {
    return (await this.symbols()).get(symbol);
  }

  /** Fetch provider_id → name from `/v1/providers`. */
  async providers(): Promise<Map<number, string>> {
    const data = await this.json<Record<string, string>>('/v1/providers');
    return new Map(Object.entries(data).map(([id, name]) => [Number(id), name]));
  }

  // ── REST: market data ───────────────────────────────────────────────────

  /** All active ticker snapshots from `/v1/tickers`. */
  async tickers(): Promise<TickerSnapshot[]> {
    return this.json<TickerSnapshot[]>('/v1/tickers');
  }

  /**
   * IndexRecord rows from `/v1/idx` (JSON shape).
   *
   * For 10x faster decoding on large queries use {@link idxBinary}
   * which fetches `Accept: application/octet-stream` and runs the MITCH decoder.
   */
  async idx(sym: Sym, opts: RangeOpts = {}): Promise<IndexRecord[]> {
    const q = this.range(opts);
    return this.json<IndexRecord[]>(`/v1/idx/${encodeURIComponent(sym)}${q}`);
  }

  /**
   * IndexRecord rows via binary `/v1/idx?fmt=binary` endpoint.
   * Returns the same `IndexRecord[]` shape, decoded from the raw 56B MITCH frames.
   */
  async idxBinary(sym: Sym, opts: RangeOpts = {}): Promise<IndexRecord[]> {
    const q = this.range(opts, 'binary');
    const buf = await this.bytes(`/v1/idx/${encodeURIComponent(sym)}${q}`);
    return decodeIdxBatch(buf);
  }

  /** OHLC candles from `/v1/ohlc?tf=<tf_ms>`. */
  async ohlc(sym: Sym, tf_ms: number, opts: RangeOpts = {}): Promise<Ohlc[]> {
    const q = this.range({ ...opts, tf: tf_ms } as RangeOpts & { tf: number });
    return this.json<Ohlc[]>(`/v1/ohlc/${encodeURIComponent(sym)}${q}`);
  }

  /** Bars (kline or renko) from `/v1/bars`. Returns full microstructure-enriched bars. */
  async bars(sym: Sym, kind: BarKind, opts: RangeOpts = {}): Promise<Bar[]> {
    const q = this.range({ ...opts, kind } as RangeOpts & { kind: BarKind });
    return this.json<Bar[]>(`/v1/bars/${encodeURIComponent(sym)}${q}`);
  }

  /** Same as {@link bars} but using binary endpoint + MITCH decoder. */
  async barsBinary(sym: Sym, kind: BarKind, opts: RangeOpts = {}): Promise<Bar[]> {
    const q = this.range({ ...opts, kind, fmt: 'binary' } as Record<string, unknown>);
    const buf = await this.bytes(`/v1/bars/${encodeURIComponent(sym)}${q}`);
    return decodeBarBatch(buf);
  }

  /** Synthetic tick (triangulation result) from `/v1/synth/tick`. */
  async synthTick(sym: Sym): Promise<SynthTick> {
    return this.json<SynthTick>(`/v1/synth/tick/${encodeURIComponent(sym)}`);
  }

  /** Synthetic OHLC from `/v1/synth/ohlc`. */
  async synthOhlc(sym: Sym, tf_ms: number, opts: RangeOpts = {}): Promise<Ohlc[]> {
    const q = this.range({ ...opts, tf: tf_ms } as RangeOpts & { tf: number });
    return this.json<Ohlc[]>(`/v1/synth/ohlc/${encodeURIComponent(sym)}${q}`);
  }

  /** Integrity diagnostics from `/v1/integrity`. */
  async integrity(): Promise<unknown> {
    return this.json<unknown>('/v1/integrity');
  }

  // ── Low-level helpers ──────────────────────────────────────────────────

  private async json<T>(path: string): Promise<T> {
    const r = await this._fetch(`${this.baseUrl}${path}`);
    if (!r.ok) throw new Error(`NXR ${path}: HTTP ${r.status}`);
    return (await r.json()) as T;
  }

  private async bytes(path: string): Promise<Uint8Array> {
    const r = await this._fetch(`${this.baseUrl}${path}`, {
      headers: { Accept: 'application/octet-stream' },
    });
    if (!r.ok) throw new Error(`NXR ${path}: HTTP ${r.status}`);
    return new Uint8Array(await r.arrayBuffer());
  }

  private range(opts: object, fmt?: string): string {
    const p = new URLSearchParams();
    for (const [k, v] of Object.entries(opts as Record<string, unknown>)) {
      if (v === undefined || v === null) continue;
      p.set(k, String(v));
    }
    if (fmt) p.set('fmt', fmt);
    const s = p.toString();
    return s ? `?${s}` : '';
  }

  // ── WebSocket ──────────────────────────────────────────────────────────

  /**
   * Connect to the WS binary stream.
   * @param wsUrl Optional override (defaults to `ws(s)://<host>/v1/stream`).
   */
  connect(wsUrl?: string): void {
    this.wsUrl = wsUrl ?? `${this.baseUrl.replace(/^http/, 'ws')}/v1/stream`;
    this.doConnect();
  }

  disconnect(): void {
    if (this.reconnectTimer) {
      clearTimeout(this.reconnectTimer);
      this.reconnectTimer = null;
    }
    if (this.ws) {
      try {
        this.ws.close();
      } catch {
        // ignore
      }
      this.ws = null;
    }
    this.setState('disconnected');
  }

  get wsState(): WsState {
    return this._wsState;
  }

  /** Subscribe to zero-copy index batches (hot path). */
  onIndex(cb: (batch: IndexBatch) => void): () => void {
    this.indexCbs.add(cb);
    return () => this.indexCbs.delete(cb);
  }

  /** Subscribe to zero-copy tick batches. */
  onTick(cb: (batch: TickBatch) => void): () => void {
    this.tickCbs.add(cb);
    return () => this.tickCbs.delete(cb);
  }

  /** Subscribe to materialized index records (allocates per row). */
  onIndexRecord(cb: (idx: WsIndex) => void): () => void {
    this.indexRecordCbs.add(cb);
    return () => this.indexRecordCbs.delete(cb);
  }

  /** Subscribe to materialized tick records. */
  onTickRecord(cb: (tick: WsTick) => void): () => void {
    this.tickRecordCbs.add(cb);
    return () => this.tickRecordCbs.delete(cb);
  }

  /** Subscribe to connection state changes. */
  onStateChange(cb: (state: WsState) => void): () => void {
    this.stateCbs.add(cb);
    return () => this.stateCbs.delete(cb);
  }

  private doConnect(): void {
    if (!this.wsUrl) return;
    if (this.ws) {
      try {
        this.ws.close();
      } catch {
        // ignore
      }
      this.ws = null;
    }
    if (this.reconnectTimer) {
      clearTimeout(this.reconnectTimer);
      this.reconnectTimer = null;
    }

    this.setState('connecting');
    const ws = new WebSocket(this.wsUrl);
    ws.binaryType = 'arraybuffer';
    this.ws = ws;

    ws.onopen = () => this.setState('connected');
    ws.onerror = () => this.setState('error');
    ws.onclose = () => {
      this.ws = null;
      this.setState('error');
      this.reconnectTimer = setTimeout(() => this.doConnect(), this.reconnectMs);
    };
    ws.onmessage = (e: MessageEvent<ArrayBuffer>) => this.onFrame(e.data);
  }

  private onFrame(buf: ArrayBuffer): void {
    if (buf.byteLength < WS_HEADER_BYTES) return;
    const type = new Uint8Array(buf)[0]!;
    const count = new DataView(buf).getUint16(2, true);
    if (count === 0) return;

    if (type === WS_MSG_INDEX) {
      const batch = new IndexBatch(buf);
      for (const cb of this.indexCbs) cb(batch);
      if (this.indexRecordCbs.size > 0) {
        for (let i = 0; i < batch.count; i++) {
          const rec = batch.get(i);
          for (const cb of this.indexRecordCbs) cb(rec);
        }
      }
    } else if (type === WS_MSG_TICK) {
      const batch = new TickBatch(buf);
      for (const cb of this.tickCbs) cb(batch);
      if (this.tickRecordCbs.size > 0) {
        for (let i = 0; i < batch.count; i++) {
          const rec = batch.get(i);
          for (const cb of this.tickRecordCbs) cb(rec);
        }
      }
    }
  }

  private setState(s: WsState): void {
    this._wsState = s;
    for (const cb of this.stateCbs) cb(s);
  }
}
