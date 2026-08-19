/**
 * NXR signed-quote wire format: the canonical TypeScript codec.
 *
 * THE single source of truth for every TS consumer of `GET /v1/quote/signed`.
 * Mirrors `core/src/server/signed.rs` (the signer) and `ExternalOracle.sol`
 * (the on-chain verifier) byte for byte; the fixture in `test/signed.test.ts`
 * is the same `cast`-derived vector all three pin against. Nothing here is
 * consumer-specific: the oracle address and chainId arrive as arguments.
 *
 *   blob   = header(8) || record(22) * n            n >= 1
 *   header = version:u8 | sourceTs:u48 | reserved:u8      big-endian, reserved = 0
 *   record = tickerId:u64 | priceB64:u64 | sigma:u32 | conf:u16    big-endian
 *
 * The VERSION byte is read before anything else and it is the ONLY format
 * discriminator. Length cannot be one: a 4-record 22 B blob is 96 B, which is
 * also a valid 4-record 24 B blob under the retired stride, and a 4-record
 * 30 B blob is 120 B, a valid 5-record 24 B blob. The EIP-712 digest commits to
 * BYTES, not to a schema, so a blob misparsed under the wrong stride carries a
 * VALID quorum signature and misprices silently. That happened once; the
 * version byte is what makes it impossible.
 *
 * Crypto (`batchDigest`, `recoverAttester`) needs keccak + secp256k1, so this
 * module is reached through the `@nxrates/sdk/signed` subpath and declares
 * `@noble/hashes` + `@noble/curves` as PEER dependencies — the package's main
 * entry stays dependency-free.
 */

import { secp256k1 } from '@noble/curves/secp256k1.js';
import { keccak_256 } from '@noble/hashes/sha3.js';

// ─── Spec constants (FROZEN — mirror core/src/server/signed.rs) ─────────────

/** `version:u8 | sourceTs:u48 | reserved:u8`. */
export const HEADER_BYTES = 8;
/** `tickerId:u64 | priceB64:u64 | sigma:u32 | conf:u16`. */
export const RECORD_BYTES = 22;
/** Wire layout version, byte 0 of every blob. The only format discriminator. */
export const BLOB_VERSION = 1;
export const EIP712_DOMAIN_TYPE =
  'EIP712Domain(string name,string version,uint256 chainId,address verifyingContract)';
export const BATCH_TYPE = 'BatchQuote(bytes32 blobHash)';
export const DOMAIN_VERSION = '1';
/** B64 packed-price exponent bias. */
const EXPONENT_BIAS = 64;

const enc = new TextEncoder();
const keccak = (b: Uint8Array): Uint8Array => keccak_256(b);

// ─── hex ────────────────────────────────────────────────────────────────────

export function hexToBytes(h: string): Uint8Array {
  const s = h.replace(/^0x/i, '');
  if (s.length % 2 !== 0 || /[^0-9a-fA-F]/.test(s)) throw new Error('invalid hex');
  const out = new Uint8Array(s.length / 2);
  for (let i = 0; i < out.length; i++) out[i] = parseInt(s.slice(i * 2, i * 2 + 2), 16);
  return out;
}

export function bytesToHex(b: Uint8Array): string {
  let s = '';
  for (const x of b) s += x.toString(16).padStart(2, '0');
  return '0x' + s;
}

// ─── Types ──────────────────────────────────────────────────────────────────

export interface QuoteRecord {
  /**
   * MITCH instrument id: content-derived from both assets plus the instrument
   * type, so it is identical on every chain and every deployment. It replaced a
   * per-deployment array ordinal, which is why nothing may key on position.
   */
  tickerId: bigint;
  priceB64: bigint;
  /** `priceB64` decoded to a human number. */
  price: number;
  /** σ, PBPS (1e6 base). */
  sigma: number;
  /** Mark CI, bps. */
  conf: number;
}

export interface QuoteBlob {
  version: number;
  /** ONE per blob, ms: the signer refuses records whose source times differ. */
  sourceTsMs: number;
  records: QuoteRecord[];
}

/** The EIP-712 domain a signature is bound to. Never the label — the tuple. */
export interface Eip712Domain {
  name: string;
  chainId: bigint;
  /** 20-byte verifying contract. */
  oracle: Uint8Array;
}

// ─── B64 price decode (mirror signed.rs::decode_b64_human) ──────────────────

export function decodeB64Human(packed: bigint): number {
  const mant = Number(packed >> 12n);
  const dec = Number((packed >> 7n) & 0x1fn);
  const exp = Number(packed & 0x7fn) - EXPONENT_BIAS;
  return mant * Math.pow(10, exp - dec);
}

// ─── Blob decode ────────────────────────────────────────────────────────────

/** Decode `header(8) || record(22)*n`. Fails closed on version, reserved, length. */
export function decodeBlob(blob: Uint8Array): QuoteBlob {
  if (blob.length < HEADER_BYTES + RECORD_BYTES) {
    throw new Error(`blob length ${blob.length} shorter than one header + record`);
  }
  // Version FIRST, before any length arithmetic or record read.
  if (blob[0] !== BLOB_VERSION) {
    throw new Error(`blob version ${blob[0]} unsupported (expected ${BLOB_VERSION})`);
  }
  if (blob[7] !== 0) throw new Error(`blob header reserved byte ${blob[7]} must be 0`);
  const body = blob.length - HEADER_BYTES;
  if (body % RECORD_BYTES !== 0) {
    throw new Error(`blob body ${body} not a whole number of ${RECORD_BYTES} B records`);
  }

  const dv = new DataView(blob.buffer, blob.byteOffset, blob.byteLength);
  // u48 big-endian: there is no getBigUint48.
  let sourceTsMs = 0;
  for (let i = 1; i <= 6; i++) sourceTsMs = sourceTsMs * 256 + blob[i];

  const records: QuoteRecord[] = [];
  for (let o = HEADER_BYTES; o < blob.length; o += RECORD_BYTES) {
    const priceB64 = dv.getBigUint64(o + 8, false);
    records.push({
      tickerId: dv.getBigUint64(o, false),
      priceB64,
      price: decodeB64Human(priceB64),
      sigma: dv.getUint32(o + 16, false),
      conf: dv.getUint16(o + 20, false),
    });
  }
  return { version: blob[0], sourceTsMs, records };
}

// ─── EIP-712 digest (mirror signed.rs::Domain) ──────────────────────────────

/**
 * `keccak256(abi.encode(DOMAIN_TYPEHASH, nameHash, versionHash, chainId, oracle))`.
 *
 * chainId and the verifying contract are IN the separator on purpose: they are
 * what stops one quorum signature from being replayed against a different
 * oracle or a different chain. The signer is consumer-agnostic because it holds
 * an allow-list of domains and binds each signature to one of them, NOT because
 * the binding was removed.
 */
export function domainSeparator(d: Eip712Domain): Uint8Array {
  if (d.oracle.length !== 20) throw new Error('oracle must be 20 bytes');
  const b = new Uint8Array(160);
  b.set(keccak(enc.encode(EIP712_DOMAIN_TYPE)), 0);
  b.set(keccak(enc.encode(d.name)), 32);
  b.set(keccak(enc.encode(DOMAIN_VERSION)), 64);
  // chainId in the low 8 bytes of the [96..128) word (the signer writes a u64).
  new DataView(b.buffer).setBigUint64(120, d.chainId, false);
  b.set(d.oracle, 140); // address in the low 20 bytes of [128..160)
  return keccak(b);
}

/** `keccak(0x1901 || domainSep || keccak(abi.encode(BATCH_TYPEHASH, keccak(blob))))`. */
export function batchDigest(blob: Uint8Array, d: Eip712Domain): Uint8Array {
  const structEnc = new Uint8Array(64);
  structEnc.set(keccak(enc.encode(BATCH_TYPE)), 0);
  structEnc.set(keccak(blob), 32);
  const pre = new Uint8Array(66);
  pre[0] = 0x19;
  pre[1] = 0x01;
  pre.set(domainSeparator(d), 2);
  pre.set(keccak(structEnc), 34);
  return keccak(pre);
}

// ─── ecrecover ──────────────────────────────────────────────────────────────

/** Recover the attester address from a 65 B `r||s||v` signature over `digest`. */
export function recoverAttester(digest: Uint8Array, sig: Uint8Array): string {
  if (sig.length !== 65) throw new Error(`sig must be 65 B, got ${sig.length}`);
  const v = sig[64] - 27;
  if (v !== 0 && v !== 1) throw new Error(`bad recovery id ${sig[64]}`);
  const parsed = secp256k1.Signature.fromBytes(sig.slice(0, 64), 'compact').addRecoveryBit(v);
  const pub = parsed.recoverPublicKey(digest).toBytes(false); // 0x04 || X || Y
  return bytesToHex(keccak(pub.slice(1)).slice(12));
}
