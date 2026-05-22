/**
 * Node-only UDP multicast subscriber for raw MITCH IndexRecord frames.
 *
 * Each datagram payload contains one or more contiguous 56B `IndexRecord`
 * frames (16B MitchHeader + 40B Index body). NXR publishes:
 *
 *   - channel A: 239.0.42.1:40006
 *   - channel B: 239.0.42.2:40007  (duplicate, for redundancy / arb)
 *
 * The subscriber decodes each frame in place using the pure-TS DataView
 * decoder. For higher throughput call `tryLoadWasm()` before `start()`.
 *
 * caveman: ! browser. dgram → Node only.
 */

import { decodeIdxBatch, decodeIdxRecord } from './decode.js';
import { SIZE_INDEX_RECORD } from './mitch.js';
import type { IndexRecord } from './types.js';

export interface MulticastOpts {
  /** Multicast group address (e.g. "239.0.42.1"). */
  group: string;
  /** UDP port (e.g. 40006). */
  port: number;
  /** Optional interface IP for IGMP join (default = system route). */
  iface?: string;
  /** Optional bind address (default 0.0.0.0). */
  bindAddress?: string;
  /** Reuse-address flag. Default true to allow multi-consumer on same host. */
  reuseAddr?: boolean;
}

type RecordCb = (rec: IndexRecord) => void;
type BatchCb = (recs: IndexRecord[]) => void;
type RawCb = (buf: Uint8Array) => void;
type ErrCb = (err: Error) => void;

type EventName = 'record' | 'batch' | 'raw' | 'error' | 'listening';

/**
 * UDP multicast subscriber. Uses `node:dgram` via a dynamic import so the
 * module can also be type-checked in browser builds (it'll throw at runtime
 * if instantiated there, but won't poison the bundle).
 */
export class MulticastSubscriber {
  private readonly opts: MulticastOpts;
  // typed as `any` to avoid hard-pinning node types in the browser bundle.
  // eslint-disable-next-line @typescript-eslint/no-explicit-any
  private socket: any = null;
  private recordCbs = new Set<RecordCb>();
  private batchCbs = new Set<BatchCb>();
  private rawCbs = new Set<RawCb>();
  private errCbs = new Set<ErrCb>();
  private listeningCbs = new Set<() => void>();

  constructor(opts: MulticastOpts) {
    this.opts = opts;
  }

  /** Subscribe to per-record decode events. */
  on(event: 'record', cb: RecordCb): () => void;
  /** Subscribe to per-datagram batch decode events (cheaper for big frames). */
  on(event: 'batch', cb: BatchCb): () => void;
  /** Subscribe to raw datagram payload (zero-cost; caller decodes). */
  on(event: 'raw', cb: RawCb): () => void;
  /** Subscribe to socket errors. */
  on(event: 'error', cb: ErrCb): () => void;
  /** Subscribe to socket-bound notification. */
  on(event: 'listening', cb: () => void): () => void;
  on(
    event: EventName,
    cb: RecordCb | BatchCb | RawCb | ErrCb | (() => void),
  ): () => void {
    switch (event) {
      case 'record':
        this.recordCbs.add(cb as RecordCb);
        return () => this.recordCbs.delete(cb as RecordCb);
      case 'batch':
        this.batchCbs.add(cb as BatchCb);
        return () => this.batchCbs.delete(cb as BatchCb);
      case 'raw':
        this.rawCbs.add(cb as RawCb);
        return () => this.rawCbs.delete(cb as RawCb);
      case 'error':
        this.errCbs.add(cb as ErrCb);
        return () => this.errCbs.delete(cb as ErrCb);
      case 'listening':
        this.listeningCbs.add(cb as () => void);
        return () => this.listeningCbs.delete(cb as () => void);
    }
  }

  /** Bind socket, join multicast group, start receiving. */
  async start(): Promise<void> {
    if (this.socket) return;
    // dynamic import so the browser bundle never includes 'node:dgram'.
    const dgram = await import('node:dgram');
    const sock = dgram.createSocket({
      type: 'udp4',
      reuseAddr: this.opts.reuseAddr ?? true,
    });
    this.socket = sock;

    sock.on('error', (err: Error) => {
      for (const cb of this.errCbs) cb(err);
    });

    sock.on('message', (msg: Buffer) => {
      // Adopt the Buffer as a Uint8Array (zero-copy view).
      const u8 = new Uint8Array(msg.buffer, msg.byteOffset, msg.byteLength);

      // Raw subscribers get the zero-cost path first.
      for (const cb of this.rawCbs) cb(u8);

      // Defensive: each datagram is N * 56B. Drop malformed frames.
      const nrecs = Math.floor(u8.byteLength / SIZE_INDEX_RECORD);
      if (nrecs === 0) return;

      // Batch subscribers: decode once.
      if (this.batchCbs.size > 0) {
        const recs = decodeIdxBatch(u8);
        for (const cb of this.batchCbs) cb(recs);
        if (this.recordCbs.size > 0) {
          for (const rec of recs) for (const cb of this.recordCbs) cb(rec);
        }
      } else if (this.recordCbs.size > 0) {
        for (let i = 0; i < nrecs; i++) {
          const rec = decodeIdxRecord(u8, i * SIZE_INDEX_RECORD);
          for (const cb of this.recordCbs) cb(rec);
        }
      }
    });

    sock.on('listening', () => {
      sock.setBroadcast(true);
      try {
        sock.addMembership(this.opts.group, this.opts.iface);
      } catch (err) {
        for (const cb of this.errCbs) cb(err as Error);
      }
      for (const cb of this.listeningCbs) cb();
    });

    await new Promise<void>((resolve, reject) => {
      try {
        sock.bind(
          { port: this.opts.port, address: this.opts.bindAddress ?? '0.0.0.0' },
          () => resolve(),
        );
      } catch (err) {
        reject(err);
      }
    });
  }

  /** Leave group + close socket. */
  async stop(): Promise<void> {
    if (!this.socket) return;
    const sock = this.socket;
    this.socket = null;
    try {
      sock.dropMembership(this.opts.group, this.opts.iface);
    } catch {
      // ignore - socket may already be closed
    }
    await new Promise<void>(resolve => {
      try {
        sock.close(() => resolve());
      } catch {
        resolve();
      }
    });
  }
}
