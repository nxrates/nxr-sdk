#!/usr/bin/env node
// Optional WASM build step.
//
// Runs `wasm-pack build wasm/ --target web --out-dir ../dist/wasm` when the
// wasm-pack binary is present. Silently skips with a notice otherwise so
// `npm install && npm run build` works on machines without wasm-pack.
//
// CI should install wasm-pack explicitly for release builds.

import { spawnSync } from 'node:child_process';
import { existsSync, mkdirSync } from 'node:fs';
import { dirname, join } from 'node:path';
import { fileURLToPath } from 'node:url';

const __dirname = dirname(fileURLToPath(import.meta.url));
const root = dirname(__dirname);
const wasmDir = join(root, 'wasm');
const outDir = join(root, 'dist', 'wasm');

function have(cmd) {
  const r = spawnSync(process.platform === 'win32' ? 'where' : 'which', [cmd], {
    stdio: 'ignore',
  });
  return r.status === 0;
}

if (!have('wasm-pack')) {
  console.log(
    '[nxr-sdk] wasm-pack not found — skipping WASM build. ' +
      'Install with `cargo install wasm-pack` for the fast-path decoder.',
  );
  // Ensure the dir exists so dist/ stays consistent
  mkdirSync(outDir, { recursive: true });
  process.exit(0);
}

if (!existsSync(wasmDir)) {
  console.log('[nxr-sdk] wasm/ source dir missing, skipping');
  process.exit(0);
}

console.log('[nxr-sdk] building wasm accelerator...');
const r = spawnSync(
  'wasm-pack',
  ['build', wasmDir, '--target', 'web', '--release', '--out-dir', outDir, '--out-name', 'nxr_sdk_wasm'],
  { stdio: 'inherit' },
);
if (r.status !== 0) {
  console.warn('[nxr-sdk] wasm-pack build failed (exit ' + r.status + '). Continuing without WASM.');
  process.exit(0);
}
console.log('[nxr-sdk] wasm built → ' + outDir);
