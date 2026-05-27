#!/usr/bin/env bun
// NXR TypeScript SDK — plan-aware error handling demo.
//
// Demonstrates the typed PlanLimitError surface from `@nxrates/sdk`:
//   1. Reads NXR_API_KEY from env (free/Starter+ depending on key)
//   2. Catches PlanLimitError specifically — not generic Error
//   3. Pretty-prints code / plan / limit / requested with an upgrade CTA
//   4. Falls back gracefully (e.g. MITCH → JSON on Free)
//
// Run: cd sdk/ts && NXR_API_KEY=<key> bun run examples/plan_aware.ts
// Run on Free (anonymous): bun run examples/plan_aware.ts

import { NxrClient } from '../src/client.js';
import { PlanLimitError } from '../src/errors.js';

const BASE_URL = process.env.NXR_BASE_URL ?? 'https://api.nxrates.com';
const API_KEY = process.env.NXR_API_KEY; // undefined → Free tier
const client = new NxrClient({ baseUrl: BASE_URL, apiKey: API_KEY });

const sep = '='.repeat(72);
console.log(sep);
console.log(`NXR TS SDK · plan-aware demo · key=${API_KEY ? 'set' : 'NONE (Free)'}`);
console.log(sep);

/**
 * Pretty-print a PlanLimitError with all wire fields + an actionable CTA.
 */
function reportPlanError(e: PlanLimitError, scenario: string): void {
  console.log(`\n[!] PlanLimitError in scenario: ${scenario}`);
  console.log(`    code        = ${e.code}`);
  console.log(`    plan        = ${e.plan}`);
  console.log(`    limit_name  = ${e.limitName}`);
  if (e.limitValue !== undefined) console.log(`    limit_value = ${e.limitValue}`);
  if (e.requested !== undefined) console.log(`    requested   = ${e.requested}`);
  console.log(`    http_status = ${e.httpStatus}`);
  console.log(`    message     = ${e.message}`);
  if (e.isUpgradeNeeded()) {
    console.log(`    -> action   : upgrade plan → ${e.upgradeUrl}`);
  } else if (e.isRateLimit()) {
    console.log(`    -> action   : back off + retry (rate-limit)`);
  } else if (e.isAuthError()) {
    console.log(`    -> action   : verify API key (auth error)`);
  }
}

// ── Scenario 1: tickers (cheap, JSON, allowed on every plan) ────────────────

try {
  const t0 = performance.now();
  const tickers = await client.tickers();
  console.log(
    `\n[1] /v1/tickers          → ${tickers.length} mids in ${(performance.now() - t0).toFixed(1)}ms (no plan limit hit)`,
  );
} catch (e) {
  if (e instanceof PlanLimitError) reportPlanError(e, 'tickers (unexpected)');
  else throw e;
}

// ── Scenario 2: MITCH binary on Free → expected PLAN_ENCODING_FORBIDDEN ─────
// Fallback: drop down to JSON when MITCH is gated.

console.log('\n[2] /v1/idx/BTC-USDT (MITCH binary)');
try {
  const recs = await client.idx('BTC/USDT', { limit: 100 });
  console.log(`    ok · ${recs.length} IndexRecords decoded (binary path)`);
} catch (e) {
  if (e instanceof PlanLimitError && e.code === 'PLAN_ENCODING_FORBIDDEN') {
    reportPlanError(e, 'MITCH binary on Free');
    // Graceful fallback — JSON path is allowed on every plan.
    console.log('    → falling back to JSON /v1/ohlc (allowed on Free)');
    const ohlc = await client.ohlc('BTC/USDT', 60, { limit: 100 });
    console.log(`    ok · ${ohlc.length} OHLC bars via JSON fallback`);
  } else if (e instanceof PlanLimitError) {
    reportPlanError(e, 'MITCH binary');
  } else {
    throw e;
  }
}

// ── Scenario 3: high-resolution TF → expected PLAN_TIMEFRAME_FORBIDDEN ──────
// (Once server enforcement lands; today this may pass on Free until the
// enforcement matrix wires through.)

console.log('\n[3] /v1/ohlc tf=10 (Pro-only)');
try {
  const ohlc = await client.ohlc('BTC/USDT', 10, { limit: 10 });
  console.log(`    ok · ${ohlc.length} bars at tf=10 (you must be on Pro+)`);
} catch (e) {
  if (e instanceof PlanLimitError && e.code === 'PLAN_TIMEFRAME_FORBIDDEN') {
    reportPlanError(e, 'tf=10s requested');
    console.log('    → falling back to tf=60 (allowed on Free)');
    const ohlc = await client.ohlc('BTC/USDT', 60, { limit: 10 });
    console.log(`    ok · ${ohlc.length} bars at tf=60 via fallback`);
  } else if (e instanceof PlanLimitError) {
    reportPlanError(e, 'tf=10 request');
  } else {
    throw e;
  }
}

// ── Scenario 4: WebSocket subscription → expected PLAN_WS_FEED_CAP on Free ──
// (Free has no WS at all — subscribing should yield PLAN_AUTH_REQUIRED first
// if no key, or PLAN_WS_FEED_CAP if too many.)

console.log('\n[4] /v1/stream (WebSocket)');
try {
  let n = 0;
  const sub = client.subscribe(['BTC/USDT', 'ETH/USDT'], (rec) => {
    n++;
    if (n === 1) console.log(`    first rec: ts=${rec.ts_ms} ${rec.ticker} bid=${rec.bid}`);
  });
  await new Promise((r) => setTimeout(r, 1500));
  sub.close();
  console.log(`    ok · received ${n} ticks in 1.5s`);
} catch (e) {
  if (e instanceof PlanLimitError) {
    reportPlanError(e, 'WS subscribe');
    console.log('    → WS not available; using REST snapshots instead');
    const snap = await client.tickers();
    console.log(`    ok · ${snap.length} snapshot rows via REST fallback`);
  } else if (e instanceof Error && /not implemented/i.test(e.message)) {
    console.log('    (WS subscribe surface not present in this SDK build, skipped)');
  } else {
    throw e;
  }
}

console.log('\n' + sep);
console.log('Plan-aware demo complete.');
console.log(sep);
