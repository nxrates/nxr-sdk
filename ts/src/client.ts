/**
 * NxrClient — REST + WebSocket client for NX Rates.
 *
 * Universal: works in browsers, Node.js, Deno, Bun via `fetch` + `WebSocket`.
 *
 * Two equivalent call styles per method that supports it:
 *
 * 1) **Object form** — single call w/ all parameters explicit:
 *    ```ts
 *    const recs = await nxr.history({ ticker: 'BTC/USDT', kind: 'renko', limit: 500 });
 *    ```
 *
 * 2) **Chainable builder** — flows when wrapping conditionals:
 *    ```ts
 *    const recs = await nxr.get().history().pair('ETH/USDC').renko().limit(500).fetch();
 *    ```
 *
 * Smart defaults: missing quote → "USDT"; missing instrument → "spot";
 * missing kind → "renko". MITCH binary is the wire format on data endpoints
 * (`Accept: application/octet-stream`); metadata endpoints negotiate JSON.
 *
 * Real-time stream:
 * ```ts
 * const sub = nxr.subscribe(['BTC/USDT', 'ETH/USDT'], (rec) => {
 *   console.log(rec.ticker, rec.bid, rec.ask, rec.ts_ms);
 * });
 * // ... later: sub.close();
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
  DataKind,
  IndexRecord,
  Ohlc,
  ShardWindow,
  SnapshotResponse,
  Sym,
  SynthPath,
  SynthTick,
  TickerDetail,
  TickerSnapshot,
  TickersDetailResponse,
} from './types.js';

/** Default endpoint for the public API. */
export const DEFAULT_BASE_URL = 'https://api.nxrates.com';

// ── Defaults (operator-enforced) ──────────────────────────────────────────

export const DEFAULT_QUOTE = 'USDT';
export const DEFAULT_KIND: DataKind = 'renko';
export const DEFAULT_INSTRUMENT_TYPE = 'spot';

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
  /** Pagination cursor: ts in ms. Records with `ts < cursor` are skipped. */
  cursor?: number;
}

/** History request options (object form). */
export interface HistoryOpts extends RangeOpts {
  /** Pair string ("BTC/USDT", "BTC-USDT", or bare "BTC"). */
  ticker?: string;
  /** Base symbol (atomic). Required if `ticker` is omitted. */
  base?: string;
  /** Quote symbol (atomic). Defaults to `DEFAULT_QUOTE`. */
  quote?: string;
  /** Data kind. Defaults to `DEFAULT_KIND`. */
  kind?: DataKind;
  /** Instrument type. Defaults to "spot". */
  instrument_type?: string;
}

/** Return type discriminated union for `history()`. */
export type HistoryData =
  | { kind: 'idx'; records: IndexRecord[] }
  | { kind: 'kline' | 'renko'; bars: Bar[] };

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
  /**
   * REST/WS root. Defaults to {@link DEFAULT_BASE_URL}.
   * e.g. `https://api.nxrates.com` or `http://nxr-svc:40004`.
   */
  baseUrl?: string;
  /** WS reconnect delay (default 3000 ms). */
  reconnectMs?: number;
  /** Optional fetch override (test injection). */
  fetch?: typeof fetch;
  /** Optional WebSocket ctor override (test injection / Node ws-package). */
  WebSocket?: typeof WebSocket;
  /**
   * API key sent as `X-NXR-Key`. Paid plans unlock MITCH/f64 encodings and WS.
   * See https://nxrates.com/pricing
   */
  apiKey?: string;
}

/** Handle returned by {@link NxrClient.subscribe}. Idempotent close. */
export interface SubscriberHandle {
  /** Close the underlying WebSocket. */
  close(): void;
  /** Current connection state. */
  readonly state: WsState;
}

/**
 * Decoded WS record handed to the `subscribe` callback. Flat shape
 * mirroring {@link IndexRecord} so callers don't need to learn a second
 * type. `ticker` is a `number` here (full u64 precision exceeds JS
 * `Number.MAX_SAFE_INTEGER` only for synthetic ids; the WS encoder
 * round-trips through f64 server-side).
 */
export interface StreamIndexRecord {
  ts_ms: number;
  ticker: number;
  mid: number;
  bid: number;
  ask: number;
  ci_ubp: number;
  confidence: number;
  accepted: number;
  rejected: number;
}

/** HTTP + WebSocket client for the NXR v1 API. */
export class NxrClient {
  private readonly baseUrl: string;
  private readonly _fetch: typeof fetch;
  private readonly _WS: typeof WebSocket;
  private readonly apiKey: string | undefined;
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

  /** Lazy cache of `/v1/tickers/detail` (one-shot on first call). */
  private detailCache: TickersDetailResponse | null = null;
  /** Resolved ticker → id map, populated from `/v1/tickers/detail`. */
  private symbolToId = new Map<string, bigint>();

  constructor(opts: NxrClientOpts = {}) {
    this.baseUrl = (opts.baseUrl ?? DEFAULT_BASE_URL).replace(/\/$/, '');
    if (opts.reconnectMs) this.reconnectMs = opts.reconnectMs;
    this._fetch = opts.fetch ?? globalThis.fetch.bind(globalThis);
    this._WS = opts.WebSocket ?? (globalThis as { WebSocket: typeof WebSocket }).WebSocket;
    this.apiKey = opts.apiKey;
  }

  private authHeaders(extra?: Record<string, string>): Record<string, string> {
    const h: Record<string, string> = { ...(extra ?? {}) };
    if (this.apiKey) h['X-NXR-Key'] = this.apiKey;
    return h;
  }

  // ── REST: discovery ─────────────────────────────────────────────────────

  /** Health check. Returns `true` only on a 2xx status. */
  async isHealthy(): Promise<boolean> {
    try {
      const r = await this._fetch(`${this.baseUrl}/health`);
      return r.ok;
    } catch {
      return false;
    }
  }

  /** Raw `/health` JSON (status, uptime, forwarder liveness, snapshot count). */
  async health(): Promise<Record<string, unknown>> {
    return this.json<Record<string, unknown>>('/health');
  }

  /** Prometheus `/metrics` raw text body. */
  async metrics(): Promise<string> {
    const r = await this._fetch(`${this.baseUrl}/metrics`);
    if (!r.ok) throw new Error(`NXR /metrics: HTTP ${r.status}`);
    return await r.text();
  }

  /** Resolve unified symbol (e.g. "BTC/USDT") to ticker_id. */
  async resolve(symbol: Sym): Promise<bigint | undefined> {
    // /v1/tickers/detail is the sole id↔symbol source of truth; it populates
    // symbolToId. (The redundant /v1/symbols endpoint was retired 2026-07-14.)
    if (this.symbolToId.size === 0) await this.tickersDetail();
    return this.symbolToId.get(symbol);
  }

  /** Fetch provider_id → name from `/v1/providers`. */
  async providers(): Promise<Map<number, string>> {
    const data = await this.json<Record<string, string>>('/v1/providers');
    return new Map(Object.entries(data).map(([id, name]) => [Number(id), name]));
  }

  /**
   * Universal integrator inventory from `/v1/tickers/detail`.
   * Cached on the instance after first call. Pass `refresh: true` to force re-fetch.
   */
  async tickersDetail(opts: { refresh?: boolean } = {}): Promise<TickersDetailResponse> {
    if (!opts.refresh && this.detailCache) return this.detailCache;
    type RawRow = Omit<TickerDetail, 'ticker_id'> & { ticker_id: number | string };
    type Raw = Omit<TickersDetailResponse, 'tickers'> & { tickers: RawRow[] };
    const raw = await this.json<Raw>('/v1/tickers/detail');
    const tickers: TickerDetail[] = raw.tickers.map((r) => ({
      ...r,
      ticker_id: BigInt(r.ticker_id),
    }));
    const resolved: TickersDetailResponse = {
      idx_aggregation_ms: raw.idx_aggregation_ms,
      count: raw.count,
      tickers,
    };
    this.detailCache = resolved;
    // Populate the symbol→id cache so resolve() short-circuits.
    this.symbolToId.clear();
    for (const t of tickers) {
      if (t.ticker_id !== 0n) this.symbolToId.set(t.ticker, t.ticker_id);
    }
    return resolved;
  }

  // ── REST: market data ───────────────────────────────────────────────────

  /** All active ticker snapshots from `/v1/tickers`. */
  async tickers(): Promise<TickerSnapshot[]> {
    return this.json<TickerSnapshot[]>('/v1/tickers');
  }

  /**
   * Live snapshot for a single MITCH ticker_id from `/v1/price/{ticker_id}`.
   * Server route accepts numeric ticker_id (decimal or hex).
   */
  async price(tickerId: bigint | number): Promise<SnapshotResponse | null> {
    type Raw = (Omit<SnapshotResponse, 'ticker'> & { ticker: number | string }) | null;
    const data = await this.json<Raw>(`/v1/price/${tickerId.toString()}`);
    if (!data) return null;
    return { ...data, ticker: BigInt(data.ticker) };
  }

  /**
   * Multi-ticker live snapshot from `/v1/last?symbols=<id>,<id>,...`.
   * Pass MITCH ticker_id values (the server accepts decimal ids in the CSV).
   */
  async last(tickerIds: Array<bigint | number>): Promise<SnapshotResponse[]> {
    if (tickerIds.length === 0) return [];
    const csv = tickerIds.map((x) => x.toString()).join(',');
    type Raw = Array<Omit<SnapshotResponse, 'ticker'> & { ticker: number | string }>;
    const data = await this.json<Raw>(`/v1/last?symbols=${encodeURIComponent(csv)}`);
    return data.map((d) => ({ ...d, ticker: BigInt(d.ticker) }));
  }

  /**
   * IndexRecord rows from `/v1/idx/{sym}`.
   * Defaults to `Accept: application/octet-stream` (MITCH binary, 10x faster decode).
   * Set `opts.json = true` to request JSON instead.
   */
  async idx(sym: Sym, opts: RangeOpts = {}): Promise<IndexRecord[]> {
    const buf = await this.bytes(`/v1/idx/${urlSym(sym)}${this.range(opts)}`);
    return decodeIdxBatch(buf);
  }

  /** OHLC candles from `/v1/ohlc/{sym}?tf=<tf_seconds>`. JSON only (legacy path). */
  async ohlc(sym: Sym, tf_s: number, opts: RangeOpts = {}): Promise<Ohlc[]> {
    const q = this.range({ ...opts, tf: tf_s } as RangeOpts & { tf: number });
    return this.json<Ohlc[]>(`/v1/ohlc/${urlSym(sym)}${q}`);
  }

  /**
   * Bars (kline or renko) from `/v1/bars/{sym}/{kind}`.
   * Defaults to MITCH binary; full 96B microstructure-enriched layout.
   */
  async bars(sym: Sym, kind: BarKind = 'renko', opts: RangeOpts = {}): Promise<Bar[]> {
    const buf = await this.bytes(`/v1/bars/${urlSym(sym)}/${kind}${this.range(opts)}`);
    return decodeBarBatch(buf);
  }

  /** Synthetic tick (triangulation result) from `/v1/synth/tick/{sym}`. */
  async synthTick(sym: Sym): Promise<SynthTick> {
    return this.json<SynthTick>(`/v1/synth/tick/${urlSym(sym)}`);
  }

  /** Static registry of synth paths from `/v1/synth/paths`. */
  async synthPaths(): Promise<SynthPath[]> {
    return this.json<SynthPath[]>('/v1/synth/paths');
  }

  /** Synthetic OHLC from `/v1/synth/ohlc/{sym}?tf=<tf_seconds>`. */
  async synthOhlc(sym: Sym, tf_s: number, opts: RangeOpts = {}): Promise<Ohlc[]> {
    const q = this.range({ ...opts, tf: tf_s } as RangeOpts & { tf: number });
    return this.json<Ohlc[]>(`/v1/synth/ohlc/${urlSym(sym)}${q}`);
  }

  /**
   * Integrity diagnostics from `/v1/integrity/{sym}`. Returns the parsed
   * JSON body; the server returns HTTP 503 with a JSON `{ warning, ... }`
   * envelope when shards are unhealthy — we surface that as a thrown error.
   */
  async integrity(sym: Sym, opts: { kind?: string } = {}): Promise<Record<string, unknown>> {
    const q = opts.kind ? `?kind=${encodeURIComponent(opts.kind)}` : '';
    return this.json<Record<string, unknown>>(`/v1/integrity/${urlSym(sym)}${q}`);
  }

  // ── Unified history (object + chainable) ────────────────────────────────

  /**
   * One-shot historical fetch with smart defaults.
   *
   * Returns a discriminated union — `idx` returns `IndexRecord[]`, `kline`
   * and `renko` return `Bar[]`. The data kind is also echoed back on the
   * envelope so callers can narrow without re-checking the input.
   */
  async history(opts: HistoryOpts = {}): Promise<HistoryData> {
    const { ticker, quote } = resolveBQ(opts);
    const kind: DataKind = (opts.kind ?? DEFAULT_KIND).toLowerCase() as DataKind;
    const instrument = (opts.instrument_type ?? DEFAULT_INSTRUMENT_TYPE).toLowerCase();
    if (instrument !== 'spot') {
      throw new Error(`instrument_type=${instrument} not supported (spot only)`);
    }
    void quote; // already folded into `ticker`
    const range: RangeOpts = {
      from: opts.from,
      to: opts.to,
      limit: opts.limit,
      cursor: opts.cursor,
    };
    if (kind === 'idx') {
      const records = await this.idx(ticker, range);
      return { kind: 'idx', records };
    }
    if (kind === 'kline' || kind === 'renko') {
      const bars = await this.bars(ticker, kind, range);
      return { kind, bars };
    }
    throw new Error(`unknown kind: ${kind}`);
  }

  /** Chainable builder root: `client.get().history()...`. */
  get(): HistoryRoot {
    return new HistoryRoot(this);
  }

  // ── Low-level helpers ──────────────────────────────────────────────────

  private async json<T>(path: string): Promise<T> {
    const r = await this._fetch(`${this.baseUrl}${path}`, {
      headers: this.authHeaders({ Accept: 'application/json' }),
    });
    if (!r.ok) {
      const body = await safeText(r);
      throw new Error(`NXR ${path}: HTTP ${r.status}${body ? ` — ${body.slice(0, 200)}` : ''}`);
    }
    return (await r.json()) as T;
  }

  private async bytes(path: string): Promise<Uint8Array> {
    const r = await this._fetch(`${this.baseUrl}${path}`, {
      headers: this.authHeaders({ Accept: 'application/octet-stream' }),
    });
    if (!r.ok) {
      const body = await safeText(r);
      throw new Error(`NXR ${path}: HTTP ${r.status}${body ? ` — ${body.slice(0, 200)}` : ''}`);
    }
    return new Uint8Array(await r.arrayBuffer());
  }

  private range(opts: object): string {
    const p = new URLSearchParams();
    for (const [k, v] of Object.entries(opts as Record<string, unknown>)) {
      if (v === undefined || v === null) continue;
      p.set(k, String(v));
    }
    const s = p.toString();
    return s ? `?${s}` : '';
  }

  // ── WebSocket ──────────────────────────────────────────────────────────

  /**
   * Subscribe to the live index stream from `/v1/stream`.
   *
   * The server broadcasts all tickers regardless of subscription state, so the
   * `tickers` argument is used only to filter records client-side (pass `[]`
   * or `undefined` to receive every record). Returns a {@link SubscriberHandle}
   * with an idempotent `close()`.
   *
   * Frame format (matches `core::server::ws`):
   * ```
   * [0]   u8  msg_type   1 = index_batch
   * [2-3] u16 count      (LE)
   * [8+]  count * 9 * f64 (LE) — epoch_ms, ticker, mid, bid, ask, ci_ubp,
   *                              confidence, accepted, rejected
   * ```
   */
  subscribe(
    tickers: string[] | undefined,
    cb: (rec: StreamIndexRecord) => void,
  ): SubscriberHandle {
    const wsUrl = `${this.baseUrl.replace(/^http/, 'ws')}/v1/stream`;
    if (!this._WS) throw new Error('No global WebSocket; pass opts.WebSocket (e.g. `ws` on Node)');
    const ws = this.apiKey
      ? new this._WS(wsUrl, { headers: { 'X-NXR-Key': this.apiKey } } as unknown as string)
      : new this._WS(wsUrl);
    (ws as { binaryType?: BinaryType }).binaryType = 'arraybuffer';
    const tickerSet = tickers && tickers.length > 0 ? new Set(tickers) : null;
    let resolvedIds: Set<number> | null = null;
    if (tickerSet) {
      // Best-effort resolve ticker→id from the detail cache. If the cache is
      // empty, the subscriber falls back to delivering every record (server
      // already broadcasts all tickers).
      resolvedIds = new Set<number>();
      for (const [sym, id] of this.symbolToId) {
        if (tickerSet.has(sym)) resolvedIds.add(Number(id));
      }
      if (resolvedIds.size === 0) resolvedIds = null;
    }
    let closed = false;
    let localState: WsState = 'connecting';
    const setLocalState = (s: WsState): void => {
      localState = s;
      this._wsState = s;
      for (const cb of this.stateCbs) cb(s);
    };
    ws.onopen = () => setLocalState('connected');
    ws.onerror = () => setLocalState('error');
    ws.onclose = () => setLocalState('disconnected');
    ws.onmessage = (e: MessageEvent<ArrayBuffer | Blob>) => {
      const data = e.data;
      if (!(data instanceof ArrayBuffer)) return; // Bun delivers ArrayBuffer when binaryType=='arraybuffer'
      if (data.byteLength < WS_HEADER_BYTES) return;
      const type = new Uint8Array(data)[0]!;
      if (type !== WS_MSG_INDEX) return;
      const batch = new IndexBatch(data);
      for (let i = 0; i < batch.count; i++) {
        const rec = batch.get(i);
        if (resolvedIds && !resolvedIds.has(rec.ticker)) continue;
        cb({
          ts_ms: rec.epoch_ms,
          ticker: rec.ticker,
          mid: rec.mid,
          bid: rec.bid,
          ask: rec.ask,
          ci_ubp: rec.ci,
          confidence: rec.confidence,
          accepted: rec.accepted,
          rejected: rec.rejected,
        });
      }
    };
    return {
      close: () => {
        if (closed) return;
        closed = true;
        try {
          ws.close();
        } catch {
          /* ignore */
        }
      },
      get state(): WsState {
        return localState;
      },
    };
  }

  // ── Legacy callback-based WS API (kept for back-compat) ────────────────

  /**
   * Connect to the WS binary stream and dispatch decoded batches to any
   * registered `onIndex` / `onTick` callbacks.
   *
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
    if (!this._WS) throw new Error('No global WebSocket; pass opts.WebSocket (e.g. `ws` on Node)');
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
    const ws = new this._WS(this.wsUrl);
    (ws as { binaryType?: BinaryType }).binaryType = 'arraybuffer';
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

// ── Chainable builder ────────────────────────────────────────────────────

/** Builder root produced by {@link NxrClient.get}. Hold for fluent chains. */
export class HistoryRoot {
  constructor(private readonly client: NxrClient) {}
  /** Begin a history fetch builder. */
  history(): HistoryBuilder {
    return new HistoryBuilder(this.client);
  }
}

/** Chainable history-fetch builder. Terminal: `.fetch()`. */
export class HistoryBuilder {
  private opts: HistoryOpts = {};
  constructor(private readonly client: NxrClient) {}

  /** Set the pair string (accepts "BTC/USDT", "BTC-USDT", or bare "BTC"). */
  pair(ticker: string): this {
    this.opts.ticker = ticker;
    return this;
  }
  /** Alias for {@link pair}. */
  ticker(ticker: string): this {
    return this.pair(ticker);
  }
  /** Set the base symbol (required if `pair` not used). */
  base(sym: string): this {
    this.opts.base = sym;
    return this;
  }
  /** Set the quote symbol (defaults to USDT). */
  quote(sym: string): this {
    this.opts.quote = sym;
    return this;
  }
  /** Set the data kind explicitly. */
  kind(k: DataKind): this {
    this.opts.kind = k;
    return this;
  }
  /** Convenience: set kind=idx. */
  idx(): this {
    return this.kind('idx');
  }
  /** Convenience: set kind=kline. */
  kline(): this {
    return this.kind('kline');
  }
  /** Convenience: set kind=renko. */
  renko(): this {
    return this.kind('renko');
  }
  /** Set the inclusive lower-bound timestamp (epoch ms). */
  from(ms: number): this {
    this.opts.from = ms;
    return this;
  }
  /** Set the exclusive upper-bound timestamp (epoch ms). */
  to(ms: number): this {
    this.opts.to = ms;
    return this;
  }
  /** Set the row cap. */
  limit(n: number): this {
    this.opts.limit = n;
    return this;
  }
  /** Set the pagination cursor (epoch ms). */
  cursor(ms: number): this {
    this.opts.cursor = ms;
    return this;
  }
  /** Execute the request. */
  fetch(): Promise<HistoryData> {
    return this.client.history(this.opts);
  }
}

// ── Helpers ──────────────────────────────────────────────────────────────

/**
 * URL-encode a symbol for the path segment. Prefers dash form because the
 * server's `resolve_sym` accepts both dash and slash, and dash avoids `%2F`.
 */
function urlSym(sym: Sym): string {
  return encodeURIComponent(sym.replace('/', '-'));
}

function parseTicker(s: string): [string, string] {
  const t = s.toUpperCase().trim();
  for (const sep of ['/', '-', '_']) {
    const i = t.indexOf(sep);
    if (i >= 0) return [t.slice(0, i).trim(), t.slice(i + 1).trim()];
  }
  return [t, DEFAULT_QUOTE];
}

function resolveBQ(opts: HistoryOpts): { ticker: string; quote: string } {
  if (opts.ticker) {
    const [b, q] = parseTicker(opts.ticker);
    const quote = (opts.quote ?? q).toUpperCase();
    return { ticker: `${b}/${quote}`, quote };
  }
  if (!opts.base) throw new Error('history() requires either ticker or base');
  const b = opts.base.toUpperCase();
  const q = (opts.quote ?? DEFAULT_QUOTE).toUpperCase();
  return { ticker: `${b}/${q}`, quote: q };
}

async function safeText(r: Response): Promise<string> {
  try {
    return await r.text();
  } catch {
    return '';
  }
}
