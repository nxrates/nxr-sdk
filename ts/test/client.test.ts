import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';

import { NxrClient, HistoryBuilder } from '../src/client.js';
import { SIZE_INDEX_RECORD, SIZE_BAR } from '../src/mitch.js';

// ── Mock fetch ────────────────────────────────────────────────────────────

type FetchHandler = (
  url: string,
  init?: RequestInit,
) => Promise<Response> | Response;

function makeFetch(handler: FetchHandler): typeof fetch {
  return (async (input: RequestInfo | URL, init?: RequestInit) => {
    const url =
      typeof input === 'string'
        ? input
        : input instanceof URL
          ? input.toString()
          : input.url;
    return handler(url, init);
  }) as typeof fetch;
}

describe('NxrClient REST', () => {
  it('default baseUrl is https://api.nxrates.com', () => {
    const client = new NxrClient();
    expect(client).toBeDefined();
    // Internal check via a single fetch — verifies the URL is correct.
    const fetchMock = makeFetch(async (url) => {
      expect(url).toBe('https://api.nxrates.com/health');
      return new Response('{"status":"ok"}', { status: 200 });
    });
    const c = new NxrClient({ fetch: fetchMock });
    return c.isHealthy();
  });

  it('fetches and parses /v1/tickers', async () => {
    const sample = [
      {
        ticker: '12345',
        mid: 50000.5,
        bid: 50000,
        ask: 50001,
        ci: 4,
        confidence: 3,
        flags: 0x40,
        age_ms: 120,
        status: 'fresh',
      },
    ];
    const fetchMock = makeFetch(async (url) => {
      expect(url).toBe('http://nxr/v1/tickers');
      return new Response(JSON.stringify(sample), {
        status: 200,
        headers: { 'content-type': 'application/json' },
      });
    });
    const client = new NxrClient({ baseUrl: 'http://nxr', fetch: fetchMock });
    const tickers = await client.tickers();
    expect(tickers).toHaveLength(1);
    expect(tickers[0]!.ticker).toBe(12345n);
    expect(tickers[0]!.bid).toBe(50000);
    expect(tickers[0]!.status).toBe('fresh');
  });

  it('tickersDetail parses + caches', async () => {
    const sample = {
      idx_aggregation_ms: 100,
      count: 2,
      tickers: [
        {
          ticker_id: '435315551398526976',
          ticker: 'BTC/USDT',
          base: 'BTC',
          quote: 'USDT',
          base_class: 'CR',
          quote_class: 'CR',
          instrument_type: 'SPOT',
          native: true,
          kinds: {
            idx: {
              fields: ['ts', 'ticker'],
              stride_bytes: 56,
              shards: { first_date: '2025-01-01', last_date: '2025-01-31', count: 31 },
            },
          },
        },
        {
          ticker_id: 0,
          ticker: 'ETH-BTC',
          base: 'ETH',
          quote: 'BTC',
          base_class: '',
          quote_class: '',
          instrument_type: 'SPOT',
          native: false,
          synth_legs: [
            { sym: 'ETH/USDT', exp: 1 },
            { sym: 'BTC/USDT', exp: -1 },
          ],
          kinds: {},
        },
      ],
    };
    let calls = 0;
    const fetchMock = makeFetch(async (url) => {
      calls++;
      expect(url).toBe('http://nxr/v1/tickers/detail');
      return new Response(JSON.stringify(sample), { status: 200 });
    });
    const client = new NxrClient({ baseUrl: 'http://nxr', fetch: fetchMock });
    const r = await client.tickersDetail();
    expect(r.count).toBe(2);
    expect(r.tickers[0]!.ticker_id).toBe(435315551398526976n);
    expect(r.tickers[1]!.native).toBe(false);
    expect(r.tickers[1]!.synth_legs).toEqual([
      { sym: 'ETH/USDT', exp: 1 },
      { sym: 'BTC/USDT', exp: -1 },
    ]);
    // Second call hits the cache.
    await client.tickersDetail();
    expect(calls).toBe(1);
    // refresh forces re-fetch.
    await client.tickersDetail({ refresh: true });
    expect(calls).toBe(2);
  });

  it('price() resolves null when ticker is unknown', async () => {
    const fetchMock = makeFetch(async () => new Response('null', { status: 200 }));
    const client = new NxrClient({ baseUrl: 'http://nxr', fetch: fetchMock });
    expect(await client.price(123n)).toBeNull();
  });

  it('last() builds CSV query', async () => {
    const fetchMock = makeFetch(async (url) => {
      expect(url).toBe('http://nxr/v1/last?symbols=10%2C20%2C30');
      return new Response('[]', { status: 200 });
    });
    const client = new NxrClient({ baseUrl: 'http://nxr', fetch: fetchMock });
    expect(await client.last([10n, 20n, 30n])).toEqual([]);
  });

  it('builds the right /v1/ohlc query (tf in seconds)', async () => {
    const fetchMock = makeFetch(async (url) => {
      expect(url).toBe(
        'http://nxr/v1/ohlc/BTC-USDT?from=1700000000000&to=1700000060000&limit=10&tf=60',
      );
      return new Response('[]', { status: 200 });
    });
    const client = new NxrClient({ baseUrl: 'http://nxr/', fetch: fetchMock });
    await client.ohlc('BTC/USDT', 60, {
      from: 1_700_000_000_000,
      to: 1_700_000_060_000,
      limit: 10,
    });
  });

  it('idx() decodes octet-stream MITCH frames', async () => {
    // Build a single 56B record with bid=10, ask=11.
    const buf = new Uint8Array(SIZE_INDEX_RECORD);
    const dv = new DataView(buf.buffer);
    dv.setUint16(0, 4, true);
    dv.setBigUint64(16, 99n, true);
    dv.setFloat64(24, 10, true);
    dv.setFloat64(32, 11, true);

    const fetchMock = makeFetch(async (url, init) => {
      expect(url).toBe('http://nxr/v1/idx/BTC-USDT?limit=1');
      expect((init?.headers as Record<string, string>).Accept).toBe('application/octet-stream');
      return new Response(buf, {
        status: 200,
        headers: { 'content-type': 'application/octet-stream' },
      });
    });
    const client = new NxrClient({ baseUrl: 'http://nxr', fetch: fetchMock });
    const recs = await client.idx('BTC/USDT', { limit: 1 });
    expect(recs).toHaveLength(1);
    expect(recs[0]!.bid).toBe(10);
    expect(recs[0]!.ask).toBe(11);
    expect(recs[0]!.ticker).toBe(99n);
    expect(recs[0]!.mid).toBe(10.5);
  });

  it('bars() decodes octet-stream MITCH bars', async () => {
    const buf = new Uint8Array(SIZE_BAR);
    const dv = new DataView(buf.buffer);
    // open/high/low/close at offsets 12/20/28/36
    dv.setFloat64(12, 100, true);
    dv.setFloat64(20, 110, true);
    dv.setFloat64(28, 99, true);
    dv.setFloat64(36, 105, true);
    dv.setUint8(92, 1); // kind = renko

    const fetchMock = makeFetch(async (url, init) => {
      expect(url).toBe('http://nxr/v1/bars/BTC-USDT/renko?limit=1');
      expect((init?.headers as Record<string, string>).Accept).toBe('application/octet-stream');
      return new Response(buf, { status: 200 });
    });
    const client = new NxrClient({ baseUrl: 'http://nxr', fetch: fetchMock });
    const bars = await client.bars('BTC/USDT', 'renko', { limit: 1 });
    expect(bars).toHaveLength(1);
    expect(bars[0]!.open).toBe(100);
    expect(bars[0]!.close).toBe(105);
    expect(bars[0]!.kind).toBe(1);
  });

  it('history() chainable form mirrors object form', async () => {
    let calls = 0;
    const fetchMock = makeFetch(async (url) => {
      calls++;
      expect(url).toMatch(/^http:\/\/nxr\/v1\/bars\/BTC-USDT\/renko/);
      return new Response(new Uint8Array(0), { status: 200 });
    });
    const client = new NxrClient({ baseUrl: 'http://nxr', fetch: fetchMock });
    const a = await client.history({ ticker: 'BTC/USDT', kind: 'renko', limit: 10 });
    const b = await client.get().history().pair('BTC/USDT').renko().limit(10).fetch();
    expect(a.kind).toBe('renko');
    expect(b.kind).toBe('renko');
    expect(calls).toBe(2);
  });

  it('history() smart defaults: missing quote → USDT, missing kind → renko', async () => {
    const fetchMock = makeFetch(async (url) => {
      // Must include /BTC-USDT/renko — defaults applied.
      expect(url).toContain('/v1/bars/BTC-USDT/renko');
      return new Response(new Uint8Array(0), { status: 200 });
    });
    const client = new NxrClient({ baseUrl: 'http://nxr', fetch: fetchMock });
    const r = await client.history({ base: 'BTC' });
    expect(r.kind).toBe('renko');
  });

  it('history() with kind=idx returns IndexRecord[]', async () => {
    const buf = new Uint8Array(SIZE_INDEX_RECORD);
    const dv = new DataView(buf.buffer);
    dv.setUint16(0, 4, true);
    dv.setBigUint64(16, 1n, true);
    dv.setFloat64(24, 1.0, true);
    dv.setFloat64(32, 1.1, true);
    const fetchMock = makeFetch(
      async () =>
        new Response(buf, {
          status: 200,
          headers: { 'content-type': 'application/octet-stream' },
        }),
    );
    const client = new NxrClient({ baseUrl: 'http://nxr', fetch: fetchMock });
    const r = await client.history({ ticker: 'EUR/USD', kind: 'idx', limit: 1 });
    expect(r.kind).toBe('idx');
    if (r.kind === 'idx') {
      expect(r.records).toHaveLength(1);
      expect(r.records[0]!.bid).toBe(1.0);
    }
  });

  it('freshness() BigInt-converts the ticker and keeps both lag axes', async () => {
    const sample = {
      ticker: 435315775907037184,
      last_ms: 1_744_372_800_000,
      lag_ms: 900,
      status: 'fresh',
      provider_last_ms: 1_744_372_000_000,
      provider_lag_ms: 800_900,
      provider_status: 'dead',
    };
    const fetchMock = makeFetch(async (url) => {
      expect(url).toBe('http://nxr/v1/freshness/ETH-BTC');
      return new Response(JSON.stringify(sample), { status: 200 });
    });
    const client = new NxrClient({ baseUrl: 'http://nxr', fetch: fetchMock });
    const r = await client.freshness('ETH-BTC');
    expect(r.ticker).toBe(435315775907037184n);
    expect(r.status).toBe('fresh');
    expect(r.provider_status).toBe('dead');
  });

  it('isHealthy returns false on network error', async () => {
    const fetchMock = makeFetch(async () => {
      throw new Error('ECONNREFUSED');
    });
    const client = new NxrClient({ baseUrl: 'http://nxr', fetch: fetchMock });
    expect(await client.isHealthy()).toBe(false);
  });

  it('throws on non-2xx with body excerpt', async () => {
    const fetchMock = makeFetch(async () => new Response('boom', { status: 500 }));
    const client = new NxrClient({ baseUrl: 'http://nxr', fetch: fetchMock });
    await expect(client.tickers()).rejects.toThrow(/HTTP 500.*boom/);
  });

  it('builder is reusable for distinct queries', () => {
    const client = new NxrClient({ baseUrl: 'http://nxr' });
    const b = client.get().history();
    expect(b).toBeInstanceOf(HistoryBuilder);
  });
});

// ── State callbacks ──────────────────────────────────────────────────────

describe('NxrClient state', () => {
  beforeEach(() => {
    vi.useFakeTimers();
  });
  afterEach(() => {
    vi.useRealTimers();
  });

  it('starts disconnected', () => {
    const client = new NxrClient({ baseUrl: 'http://nxr' });
    expect(client.wsState).toBe('disconnected');
  });
});

// ── subscribe() handle behavior ──────────────────────────────────────────

describe('NxrClient.subscribe', () => {
  it('returns a handle with idempotent close', () => {
    // Mock minimal WebSocket
    let closeCount = 0;
    class FakeWS {
      binaryType = 'arraybuffer';
      onopen: (() => void) | null = null;
      onerror: (() => void) | null = null;
      onclose: (() => void) | null = null;
      onmessage: ((e: MessageEvent) => void) | null = null;
      constructor(public url: string) {}
      close(): void {
        closeCount++;
      }
    }
    const client = new NxrClient({
      baseUrl: 'http://nxr',
      WebSocket: FakeWS as unknown as typeof WebSocket,
    });
    const handle = client.subscribe([], () => {});
    handle.close();
    handle.close(); // second call should be a no-op
    expect(closeCount).toBe(1);
  });

  it('subscribes on open and honours the ack', () => {
    const sent: string[] = [];
    let sock: FakeSock | null = null;
    class FakeSock {
      binaryType = 'arraybuffer';
      onopen: (() => void) | null = null;
      onerror: (() => void) | null = null;
      onclose: (() => void) | null = null;
      onmessage: ((e: MessageEvent) => void) | null = null;
      constructor(public url: string) {
        sock = this;
      }
      send(d: string): void {
        sent.push(d);
      }
      close(): void {}
    }
    const client = new NxrClient({
      baseUrl: 'http://nxr',
      WebSocket: FakeSock as unknown as typeof WebSocket,
    });
    const rejected: { id: string; error: string }[] = [];
    client.subscribe(['BTC/ETH', 'ZZZ/QQQ'], () => {}, (r) => rejected.push(...r));
    sock!.onopen!();
    expect(JSON.parse(sent[0]!)).toEqual({
      type: 'sub',
      kind: 'idx',
      ids: ['BTC/ETH', 'ZZZ/QQQ'],
    });
    sock!.onmessage!({
      data: JSON.stringify({
        ok: true,
        subscribed: ['BTC/ETH'],
        rejected: [{ id: 'ZZZ/QQQ', error: 'unroutable' }],
      }),
    } as MessageEvent);
    expect(rejected).toEqual([{ id: 'ZZZ/QQQ', error: 'unroutable' }]);
  });

  it('sends no envelope without ids (broadcast stays the default)', () => {
    const sent: string[] = [];
    let sock: FakeQuiet | null = null;
    class FakeQuiet {
      binaryType = 'arraybuffer';
      onopen: (() => void) | null = null;
      onerror: (() => void) | null = null;
      onclose: (() => void) | null = null;
      onmessage: ((e: MessageEvent) => void) | null = null;
      constructor(public url: string) {
        sock = this;
      }
      send(d: string): void {
        sent.push(d);
      }
      close(): void {}
    }
    const client = new NxrClient({
      baseUrl: 'http://nxr',
      WebSocket: FakeQuiet as unknown as typeof WebSocket,
    });
    client.subscribe([], () => {});
    sock!.onopen!();
    expect(sent).toHaveLength(0);
  });
});

