/**
 * NxrClient — REST + WebSocket client for NX Rates.
 *
 * Transport-agnostic: works in browsers, Node.js, Deno, Bun.
 * Uses the Fetch API (universal) and WebSocket (universal).
 *
 * @example
 * ```ts
 * import { NxrClient } from '@nxr/sdk';
 *
 * const nxr = new NxrClient('http://nxr-svc:40004');
 *
 * // REST
 * const symbols = await nxr.symbols();
 * const tickers = await nxr.tickers();
 *
 * // WebSocket (zero-copy batches)
 * nxr.onIndex((batch) => {
 *   for (let i = 0; i < batch.count; i++) {
 *     if (batch.ticker(i) === btcId) {
 *       console.log(`BTC mid=${batch.mid(i)}`);
 *     }
 *   }
 * });
 * nxr.connect();
 * ```
 */

import {
  IndexBatch,
  TickBatch,
  WS_MSG_INDEX,
  WS_MSG_TICK,
  WS_HEADER_BYTES,
  type WsIndex,
  type WsTick,
} from './decode.js';

// ── REST response types ─────────────────────────────────────────────────────

export interface TickerResponse {
  ticker:     number;
  mid:        number;
  bid:        number;
  ask:        number;
  ci:         number;
  confidence: number;
}

// ── WebSocket state ─────────────────────────────────────────────────────────

export type WsState = 'disconnected' | 'connecting' | 'connected' | 'error';

// ── Client ──────────────────────────────────────────────────────────────────

export class NxrClient {
  private readonly restBase: string;
  private wsUrl: string | null = null;
  private ws: WebSocket | null = null;
  private reconnectTimer: ReturnType<typeof setTimeout> | null = null;
  private _wsState: WsState = 'disconnected';
  private reconnectMs = 3000;

  // Callbacks
  private indexCbs = new Set<(batch: IndexBatch) => void>();
  private tickCbs = new Set<(batch: TickBatch) => void>();
  private indexRecordCbs = new Set<(idx: WsIndex) => void>();
  private tickRecordCbs = new Set<(tick: WsTick) => void>();
  private stateCbs = new Set<(state: WsState) => void>();

  /**
   * @param restBase  REST endpoint, e.g. "http://nxr-svc:40004"
   * @param opts.reconnectMs  Reconnect delay in ms (default: 3000)
   */
  constructor(restBase: string, opts?: { reconnectMs?: number }) {
    this.restBase = restBase.replace(/\/$/, '');
    if (opts?.reconnectMs) this.reconnectMs = opts.reconnectMs;
  }

  // ── REST API ────────────────────────────────────────────────────────────

  /** Fetch symbol → ticker_id map. */
  async symbols(): Promise<Map<string, number>> {
    const data: Record<string, number> = await this.fetchJson('/v1/symbols');
    return new Map(Object.entries(data));
  }

  /** Fetch provider_id → name map. */
  async providers(): Promise<Map<number, string>> {
    const data: Record<string, string> = await this.fetchJson('/v1/providers');
    return new Map(Object.entries(data).map(([id, name]) => [Number(id), name]));
  }

  /** Fetch all active tickers. */
  async tickers(): Promise<TickerResponse[]> {
    return this.fetchJson('/v1/tickers');
  }

  /** Resolve unified symbol (e.g. "BTC/USDT") to ticker_id. */
  async resolve(symbol: string): Promise<number | undefined> {
    const map = await this.symbols();
    return map.get(symbol);
  }

  /** Health check — returns true if NXR is up. */
  async isHealthy(): Promise<boolean> {
    try {
      const r = await fetch(`${this.restBase}/health`);
      return r.ok;
    } catch {
      return false;
    }
  }

  private async fetchJson<T>(path: string): Promise<T> {
    const r = await fetch(`${this.restBase}${path}`);
    if (!r.ok) throw new Error(`NXR ${path}: ${r.status}`);
    return r.json();
  }

  // ── WebSocket ─────────────────────────────────────────────────────────

  /**
   * Connect to the WS binary stream.
   *
   * @param wsUrl  Optional override. Defaults to `ws://<restHost>/v1/stream`.
   */
  connect(wsUrl?: string): void {
    this.wsUrl = wsUrl ?? this.restBase.replace(/^http/, 'ws') + '/v1/stream';
    this.doConnect();
  }

  disconnect(): void {
    if (this.reconnectTimer) { clearTimeout(this.reconnectTimer); this.reconnectTimer = null; }
    if (this.ws) { try { this.ws.close(); } catch {} this.ws = null; }
    this.setState('disconnected');
  }

  get wsState(): WsState { return this._wsState; }

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

  /** Subscribe to materialized index records (convenience, allocates). */
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
    if (this.ws) { try { this.ws.close(); } catch {} this.ws = null; }
    if (this.reconnectTimer) { clearTimeout(this.reconnectTimer); this.reconnectTimer = null; }

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
    const type = new Uint8Array(buf)[0];
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
