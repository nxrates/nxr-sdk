/**
 * Node-only entry. Re-exports the universal API plus the UDP multicast subscriber.
 *
 * @example
 * ```ts
 * import { MulticastSubscriber } from '@nxrates/sdk/node';
 *
 * const sub = new MulticastSubscriber({ group: '239.0.42.1', port: 40006 });
 * sub.on('record', r => console.log(r.ticker, r.mid));
 * await sub.start();
 * ```
 */
export * from './index.js';
export { MulticastSubscriber, type MulticastOpts } from './multicast.js';
