/**
 * Browser-only entry. Re-exports the universal API. No Node-only modules.
 *
 * Multicast is unavailable in browsers; use REST + WebSocket via `NxrClient`.
 *
 * @example
 * ```ts
 * import { NxrClient } from '@nxrates/sdk/browser';
 *
 * const nxr = new NxrClient({ baseUrl: 'https://nxr.nxrates.com' });
 * nxr.onIndex(b => {
 *   for (let i = 0; i < b.count; i++) console.log(b.mid(i));
 * });
 * nxr.connect();
 * ```
 */
export * from './index.js';
