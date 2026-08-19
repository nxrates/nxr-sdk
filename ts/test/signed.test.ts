/**
 * Signed-quote codec parity.
 *
 * The fixture is THE PRODUCER'S OWN, copied verbatim from
 * `nx-rates core/src/server/signed.rs` (FIX_BLOB_HEX / FIX_BLOB_HASH /
 * FIX_DIGEST / FIX_SIG) and independently re-derived with foundry `cast`.
 * The same hex is pinned by `btr keepers/src/oracle/signed.rs` and by
 * `btr dex/evm/test/unit/ExternalOracleSigned.t.sol`. Each repo pinning only
 * ITSELF is exactly how the 24 B / 30 B stride fork went unnoticed, so these
 * bytes are deliberately identical in all four places: change one, the rest fail.
 */

import { describe, expect, it } from 'vitest';
import {
  BLOB_VERSION,
  HEADER_BYTES,
  RECORD_BYTES,
  batchDigest,
  bytesToHex,
  decodeBlob,
  hexToBytes,
  recoverAttester,
} from '../src/signed.js';

const BLOB_HEX =
  '0x01019820abcdef000616a9648450000000112233445566770abc614e0019060a8d64845000008899aabbccddeeff00000001ffff';
const DIGEST = '0xb5ca4374e3998f63543b0c1023fd7589c65d7f4902f791f803f64720adbaca39';
const SIG =
  '0x7091a26900b63d09a458b831595c0eb1dbc2c79fc24f4fe02600077d4115049f0d46bac1e2be71eb3b5c007175884c13f1a0e7fc91f1371f43f717cffa4f7f0f1b';
/** anvil dev key 0 (public test key). */
const SIGNER = '0xf39fd6e51aad88f6f4ce6ab8827279cfffb92266';
const DOMAIN = {
  name: 'BTR ExternalOracle',
  chainId: 56n,
  oracle: hexToBytes('0x1111111111111111111111111111111111111111'),
};
/** Real MITCH spot ids, as `/v1/quote/signed/meta` publishes them. */
const T_ETH_USDC = 438_724_262_896_861_184n;
const T_BTC_USDC = 435_315_776_850_755_584n;

describe('decodeBlob', () => {
  it("decodes the producer's own fixture byte for byte", () => {
    const blob = hexToBytes(BLOB_HEX);
    expect(blob.length).toBe(HEADER_BYTES + 2 * RECORD_BYTES);
    const b = decodeBlob(blob);
    expect(b.version).toBe(BLOB_VERSION);
    expect(b.sourceTsMs).toBe(0x019820abcdef);
    expect(b.records).toHaveLength(2);
    expect(b.records[0].tickerId).toBe(T_ETH_USDC);
    expect(b.records[0].priceB64).toBe(0x0011223344556677n);
    expect(b.records[0].sigma).toBe(0x0abc614e);
    expect(b.records[0].conf).toBe(0x0019);
    expect(b.records[1].tickerId).toBe(T_BTC_USDC);
    expect(b.records[1].priceB64).toBe(0x8899aabbccddeeffn);
    expect(b.records[1].sigma).toBe(1);
    expect(b.records[1].conf).toBe(0xffff);
  });

  /**
   * THE anti-misparse gate. 4 records at the retired 24 B stride is 96 B, which
   * is also a valid header + 4 new records: length cannot tell the formats
   * apart, so only the version byte can. A real old blob's first byte is the
   * high byte of an ordinal `idx` (< 256), i.e. 0.
   */
  it('rejects a blob whose length is valid for the retired 24-byte stride', () => {
    const old24 = new Uint8Array(96);
    const dv = new DataView(old24.buffer);
    for (let i = 0; i < 4; i++) dv.setUint16(i * 24, i);
    expect((old24.length - HEADER_BYTES) % RECORD_BYTES).toBe(0);
    expect(() => decodeBlob(old24)).toThrow(/version/);
  });

  it('rejects a bad version, a non-zero reserved byte, a short and a ragged blob', () => {
    const ok = hexToBytes(BLOB_HEX);
    const badVer = Uint8Array.from(ok);
    badVer[0] = 2;
    expect(() => decodeBlob(badVer)).toThrow(/version/);
    const badRes = Uint8Array.from(ok);
    badRes[7] = 1;
    expect(() => decodeBlob(badRes)).toThrow(/reserved/);
    expect(() => decodeBlob(new Uint8Array(HEADER_BYTES))).toThrow();
    expect(() => decodeBlob(new Uint8Array([...ok, 0xff]))).toThrow(/records/);
  });
});

describe('batchDigest / recoverAttester', () => {
  it('reproduces the signer digest and recovers the attester', () => {
    const blob = hexToBytes(BLOB_HEX);
    expect(bytesToHex(batchDigest(blob, DOMAIN))).toBe(DIGEST);
    expect(recoverAttester(batchDigest(blob, DOMAIN), hexToBytes(SIG))).toBe(SIGNER);
  });

  it('is bound to chainId and to the verifying contract', () => {
    const blob = hexToBytes(BLOB_HEX);
    // Both are IN the separator on purpose: they are what stops one quorum
    // signature from being replayed against another chain or another oracle.
    expect(bytesToHex(batchDigest(blob, { ...DOMAIN, chainId: 1n }))).not.toBe(DIGEST);
    expect(
      bytesToHex(
        batchDigest(blob, { ...DOMAIN, oracle: hexToBytes('0x2222222222222222222222222222222222222222') }),
      ),
    ).not.toBe(DIGEST);
  });
});
