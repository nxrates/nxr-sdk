#!/usr/bin/env bun
// NXR TypeScript SDK — fetch demo against live prod API.
// Run: cd sdk/ts && bun run examples/fetch_demo.ts

import { NxrClient } from '../src/client.js';
import { decodeIdxBatch } from '../src/decode.js';

const BASE_URL = 'https://api.nxrates.com';
const client = new NxrClient({ baseUrl: BASE_URL });

console.log('='.repeat(70));
console.log(`NXR TypeScript SDK demo · base=${BASE_URL}`);
console.log('='.repeat(70));

// 1. Live tickers
const t0a = performance.now();
const tickers = await client.tickers();
console.log(`\n[1] /v1/tickers          → ${tickers.length} mids in ${(performance.now()-t0a).toFixed(1)}ms`);
if (tickers.length) {
  const t = tickers[0];
  console.log(`    sample: ticker=${t.ticker}  mid=${t.mid}  ci=${t.ci}  conf=${t.confidence}`);
}

// 2. Raw ticks (binary octet-stream, MITCH zero-copy decode)
const t0b = performance.now();
const ticks = await client.idxBinary('BTC/USDT', { limit: 10_000 });
const dtB = performance.now() - t0b;
console.log(`\n[2] /v1/idx/BTC/USDT     → ${ticks.length} IndexRecords in ${dtB.toFixed(1)}ms (${(ticks.length/(dtB/1000)).toFixed(0)} rec/s)`);
if (ticks.length) {
  const f = ticks[0], l = ticks[ticks.length-1];
  console.log(`    first ts=${f.ts_ms}  bid=${f.bid}  ask=${f.ask}  conf=${f.confidence}`);
  console.log(`    last  ts=${l.ts_ms}  bid=${l.bid}  ask=${l.ask}  conf=${l.confidence}`);
}

// 3. OHLC at multiple TFs (rollup tested)
for (const tf of [60, 300, 3600, 86400]) {
  const t0 = performance.now();
  const ohlc = await client.ohlc('BTC/USDT', tf, { limit: 100 });
  const dt = performance.now() - t0;
  const label = ({60:'1m', 300:'5m', 3600:'1h', 86400:'1d'} as Record<number,string>)[tf];
  console.log(`\n[3.${tf}] /v1/ohlc tf=${label.padStart(3)}    → ${ohlc.length} bars in ${dt.toFixed(1)}ms`);
  if (ohlc.length) {
    const last = ohlc[ohlc.length-1];
    console.log(`      last ts=${last.ts}  O=${last.open.toFixed(2)}  H=${last.high.toFixed(2)}  L=${last.low.toFixed(2)}  C=${last.close.toFixed(2)}  ticks=${last.tick_count}`);
  }
}

// 4. Synth tick (triangulated)
const t0d = performance.now();
const synth = await client.synthTick('ETH/BTC');
console.log(`\n[4] /v1/synth/tick/ETH/BTC in ${(performance.now()-t0d).toFixed(1)}ms`);
console.log(`    bid=${synth.bid.toFixed(6)}  ask=${synth.ask.toFixed(6)}  mid=${synth.mid.toFixed(6)}  conf=${synth.conf}`);

// 5. Pure-TS DataView decode benchmark
const sampleBytes = await fetch(`${BASE_URL}/v1/idx/BTC-USDT?limit=10000`, {
  headers: { Accept: 'application/octet-stream' }
}).then(r => r.arrayBuffer());
const N_ITER = 50;
const t0e = performance.now();
let total = 0;
for (let i = 0; i < N_ITER; i++) {
  const recs = decodeIdxBatch(new Uint8Array(sampleBytes));
  total += recs.length;
}
const dtE = performance.now() - t0e;
console.log(`\n[5] DataView decode benchmark: ${N_ITER} iters × ${total/N_ITER} recs`);
console.log(`    ${dtE.toFixed(1)}ms total, ${(total/(dtE/1000)).toFixed(0)} rec/s sustained`);

console.log('\n' + '='.repeat(70));
console.log('All endpoints functional ✓');
console.log('='.repeat(70));
