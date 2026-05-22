import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';

import { NxrClient } from '../src/client.js';
import { SIZE_INDEX_RECORD } from '../src/mitch.js';

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
  it('fetches and parses /v1/tickers', async () => {
    const sample = [
      {
        symbol: 'BTC/USDT',
        ticker: '12345',
        ts_ms: 1_700_000_000_000,
        bid: 50000,
        ask: 50001,
        mid: 50000.5,
        ci_ubp: 4,
        confidence: 3,
        accepted: 5,
        rejected: 0,
      },
    ];
    const fetchMock = makeFetch(async url => {
      expect(url).toBe('http://nxr/v1/tickers');
      return new Response(JSON.stringify(sample), {
        status: 200,
        headers: { 'content-type': 'application/json' },
      });
    });
    const client = new NxrClient({ baseUrl: 'http://nxr', fetch: fetchMock });
    const tickers = await client.tickers();
    expect(tickers).toHaveLength(1);
    expect(tickers[0]!.symbol).toBe('BTC/USDT');
    expect(tickers[0]!.bid).toBe(50000);
  });

  it('builds the right /v1/ohlc query', async () => {
    const fetchMock = makeFetch(async url => {
      expect(url).toBe(
        'http://nxr/v1/ohlc/BTC%2FUSDT?from=1700000000000&to=1700000060000&limit=10&tf=60000',
      );
      return new Response(JSON.stringify([]), { status: 200 });
    });
    const client = new NxrClient({ baseUrl: 'http://nxr/', fetch: fetchMock });
    await client.ohlc('BTC/USDT', 60_000, {
      from: 1_700_000_000_000,
      to: 1_700_000_060_000,
      limit: 10,
    });
  });

  it('idxBinary decodes octet-stream MITCH frames', async () => {
    // Build a single 56B record with bid=10, ask=11.
    const buf = new Uint8Array(SIZE_INDEX_RECORD);
    const dv = new DataView(buf.buffer);
    // type_provider: INDEX wire code 4, provider 0
    dv.setUint16(0, 4, true);
    // ticker @ offset 16
    dv.setBigUint64(16, 99n, true);
    dv.setFloat64(24, 10, true);
    dv.setFloat64(32, 11, true);

    const fetchMock = makeFetch(async (url, init) => {
      expect(url).toBe('http://nxr/v1/idx/BTC%2FUSDT?limit=1&fmt=binary');
      expect((init?.headers as Record<string, string>).Accept).toBe(
        'application/octet-stream',
      );
      return new Response(buf, {
        status: 200,
        headers: { 'content-type': 'application/octet-stream' },
      });
    });
    const client = new NxrClient({ baseUrl: 'http://nxr', fetch: fetchMock });
    const recs = await client.idxBinary('BTC/USDT', { limit: 1 });
    expect(recs).toHaveLength(1);
    expect(recs[0]!.bid).toBe(10);
    expect(recs[0]!.ask).toBe(11);
    expect(recs[0]!.ticker).toBe(99n);
    expect(recs[0]!.mid).toBe(10.5);
  });

  it('symbols() returns Map<string,bigint>', async () => {
    const fetchMock = makeFetch(
      async () =>
        new Response(JSON.stringify({ 'BTC/USDT': '0xabc', 'ETH/USDT': 42 }), {
          status: 200,
        }),
    );
    const client = new NxrClient({ baseUrl: 'http://nxr', fetch: fetchMock });
    const m = await client.symbols();
    expect(m.get('BTC/USDT')).toBe(0xabcn);
    expect(m.get('ETH/USDT')).toBe(42n);
  });

  it('isHealthy returns false on network error', async () => {
    const fetchMock = makeFetch(async () => {
      throw new Error('ECONNREFUSED');
    });
    const client = new NxrClient({ baseUrl: 'http://nxr', fetch: fetchMock });
    expect(await client.isHealthy()).toBe(false);
  });

  it('throws on non-2xx', async () => {
    const fetchMock = makeFetch(async () => new Response('boom', { status: 500 }));
    const client = new NxrClient({ baseUrl: 'http://nxr', fetch: fetchMock });
    await expect(client.tickers()).rejects.toThrow(/HTTP 500/);
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
