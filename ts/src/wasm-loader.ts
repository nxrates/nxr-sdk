/**
 * Optional WASM accelerator loader.
 *
 * The companion `wasm/` crate exposes `decode_idx_batch(buf: &[u8]) -> JsValue`
 * built with `wasm-pack build --target web`. Output lives in `dist/wasm/`.
 *
 * In CI / dev where wasm-pack hasn't run, this loader silently returns null
 * and callers fall back to the pure-TS DataView decoders in `decode.ts`.
 *
 * caveman: wasm ? load → else fallback. ! error.
 */

import type { IndexRecord } from './types.js';

/** Shape of the (optional) WASM module produced by wasm-pack `--target web`. */
export interface NxrWasmModule {
  /** Initialize wasm-bindgen runtime. Call once before any decoder. */
  default: (input?: BufferSource | URL | string) => Promise<unknown>;
  /** Decode a contiguous buffer of 56B IndexRecords. */
  decode_idx_batch: (buf: Uint8Array) => unknown;
  /** Decode a single IndexRecord. */
  decode_idx_record: (buf: Uint8Array) => unknown;
  /** Decode a contiguous buffer of 96B Bars. */
  decode_bar_batch: (buf: Uint8Array) => unknown;
}

let _wasm: NxrWasmModule | null = null;
let _wasmInit: Promise<NxrWasmModule | null> | null = null;

/**
 * Attempt to load the optional WASM decoder. Idempotent.
 * Returns the module if available, or `null` if the WASM bundle is missing.
 *
 * Browser: pass an explicit URL to the `.wasm` file via `wasmUrl`.
 * Node/Bun: leave default; loader uses native `fs` via dynamic import.
 */
export async function tryLoadWasm(opts?: {
  wasmUrl?: string;
}): Promise<NxrWasmModule | null> {
  if (_wasm) return _wasm;
  if (_wasmInit) return _wasmInit;
  _wasmInit = (async () => {
    try {
      // Dynamic import so bundlers can tree-shake when WASM isn't used.
      // The path is relative to the built dist/ output; published packages
      // ship `dist/wasm/nxr_sdk_wasm.js` alongside `decode.js`.
      // @ts-expect-error - module may not exist at build time
      const mod = (await import('./wasm/nxr_sdk_wasm.js')) as NxrWasmModule;
      await mod.default(opts?.wasmUrl);
      _wasm = mod;
      return mod;
    } catch {
      _wasm = null;
      return null;
    }
  })();
  return _wasmInit;
}

/**
 * High-perf IndexRecord batch decoder. Uses WASM if loaded, else pure TS.
 *
 * Call {@link tryLoadWasm} once at startup to enable the fast path.
 */
export async function decodeIdxBatchFast(buf: Uint8Array): Promise<IndexRecord[]> {
  const wasm = await tryLoadWasm();
  if (wasm) {
    return wasm.decode_idx_batch(buf) as IndexRecord[];
  }
  const { decodeIdxBatch } = await import('./decode.js');
  return decodeIdxBatch(buf);
}
