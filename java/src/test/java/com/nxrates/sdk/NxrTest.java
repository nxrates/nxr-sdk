package com.nxrates.sdk;

import static org.junit.jupiter.api.Assertions.assertEquals;
import static org.junit.jupiter.api.Assertions.assertFalse;
import static org.junit.jupiter.api.Assertions.assertNotEquals;
import static org.junit.jupiter.api.Assertions.assertThrows;
import static org.junit.jupiter.api.Assertions.assertTrue;

import java.util.List;
import java.util.Optional;
import java.util.OptionalLong;
import org.junit.jupiter.api.Test;

class NxrTest {

    /** 2026-01-01T00:00:00Z. An even offset from the 2010 epoch, so the u48 mts round-trips exactly. */
    private static final long TS_MS = 1_767_225_600_000L;

    // ── Resolver ─────────────────────────────────────────────────────

    @Test
    void resolvesKnownSymbol() {
        OptionalLong id = Nxr.tryResolveTickerId("BTC/USDT");
        assertTrue(id.isPresent(), "BTC/USDT must resolve");
        assertEquals(id.getAsLong(), Nxr.resolveTickerId("BTC/USDT"),
                "lenient and strict must agree on a resolvable symbol");

        // Reverse yields asset NAMES, not the symbols that went in.
        Nxr.Ticker ticker = Nxr.resolveTicker(id.getAsLong());
        assertEquals("BITCOIN", ticker.base());
        assertEquals("TETHER", ticker.quote());
        assertEquals("SPOT", ticker.instrumentType());
    }

    @Test
    void strictResolveIsEmptyForUnresolvableSymbol() {
        String junk = "ZZZQQQ/XXXWWW";
        assertEquals(OptionalLong.empty(), Nxr.tryResolveTickerId(junk),
                "strict resolve must report failure, not a phantom");

        // The lenient call still yields an id: an FNV phantom, not a MITCH id.
        long phantom = Nxr.resolveTickerId(junk);
        assertNotEquals(0L, phantom);
        Nxr.Ticker reversed = Nxr.resolveTicker(phantom);
        assertTrue(reversed.base().startsWith("0x"), "phantom reverses to a hex base");
        assertEquals("", reversed.quote(), "phantom has no quote");
    }

    @Test
    void marketProviderLookup() {
        Optional<Nxr.MarketProvider> known = Nxr.getMarketProvider(101);
        assertTrue(known.isPresent(), "provider 101 must be known");
        assertFalse(known.get().name().isBlank());
        assertEquals(101, known.get().id());

        assertEquals(Optional.empty(), Nxr.getMarketProvider(65535));
    }

    // ── Codecs ───────────────────────────────────────────────────────

    @Test
    void indexRecordRoundTrip() {
        long ticker = Nxr.tryResolveTickerId("BTC/USDT").orElseThrow();
        Nxr.IndexRecord original = new Nxr.IndexRecord(
                TS_MS, ticker, 65_000.25, 65_001.75, 0.0,
                1_234, 5_678, 101, 42, 320, 17, 9, 7, 2, 0);

        byte[] wire = Nxr.encodeIdxRecord(original);
        assertEquals(Nxr.INDEX_RECORD_SIZE, wire.length);

        List<Nxr.IndexRecord> decoded = Nxr.decodeIdxBytes(wire);
        assertEquals(1, decoded.size());
        Nxr.IndexRecord r = decoded.get(0);

        assertEquals(original.tsMs(), r.tsMs());
        assertEquals(original.ticker(), r.ticker());
        assertEquals(original.bid(), r.bid());
        assertEquals(original.ask(), r.ask());
        assertEquals(original.vbid(), r.vbid());
        assertEquals(original.vask(), r.vask());
        assertEquals(original.provider(), r.provider());
        assertEquals(original.sequence(), r.sequence());
        assertEquals(original.ci(), r.ci());
        assertEquals(original.tickCount(), r.tickCount());
        assertEquals(original.confidence(), r.confidence());
        assertEquals(original.accepted(), r.accepted());
        assertEquals(original.rejected(), r.rejected());
        assertEquals(original.flags(), r.flags());

        assertEquals(65_001.0, r.mid());
        assertTrue(r.ciPrice() > 0, "ci_price is derived from the sqrt-compressed ci");
    }

    @Test
    void barRoundTrip() {
        Nxr.Bar original = new Nxr.Bar(
                TS_MS, TS_MS + 60_000L, 100.5, 110.25, 99.75, 108.0,
                11, 22, 33, 0.5f, 0.25f, -0.125f, 0.75f, 3.5f, 0.0625f,
                640, 1_234, 1, 4);

        byte[] wire = Nxr.encodeBar(original);
        assertEquals(Nxr.BAR_SIZE, wire.length);

        List<Nxr.Bar> decoded = Nxr.decodeBarBytes(wire);
        assertEquals(1, decoded.size());
        assertEquals(original, decoded.get(0), "every bar field must round-trip");
    }

    @Test
    void decodesMultipleRecords() {
        byte[] one = Nxr.encodeIdxRecord(Nxr.IndexRecord.of(TS_MS, 101, 7L, 1.5, 1.6));
        byte[] two = Nxr.encodeIdxRecord(Nxr.IndexRecord.of(TS_MS + 2, 102, 8L, 2.5, 2.6));
        byte[] slab = new byte[one.length + two.length];
        System.arraycopy(one, 0, slab, 0, one.length);
        System.arraycopy(two, 0, slab, one.length, two.length);

        List<Nxr.IndexRecord> decoded = Nxr.decodeIdxBytes(slab);
        assertEquals(2, decoded.size());
        assertEquals(7L, decoded.get(0).ticker());
        assertEquals(8L, decoded.get(1).ticker());
        assertEquals(102, decoded.get(1).provider());
    }

    @Test
    void tickDecode() {
        // MITCH Tick body: u64 ticker | f64 bid | f64 ask | u32 vbid | u32 vask.
        java.nio.ByteBuffer buf = java.nio.ByteBuffer.allocate(Nxr.TICK_SIZE)
                .order(java.nio.ByteOrder.LITTLE_ENDIAN);
        buf.putLong(99L).putDouble(1.25).putDouble(1.75).putInt(5).putInt(6);

        List<Nxr.Tick> ticks = Nxr.decodeTickBytes(buf.array());
        assertEquals(1, ticks.size());
        assertEquals(new Nxr.Tick(99L, 1.25, 1.75, 5, 6), ticks.get(0));
        assertEquals(1.5, ticks.get(0).mid());
    }

    // ── Buffer validation ────────────────────────────────────────────

    @Test
    void shortBufferIsRejected() {
        // One byte short of a record: must fail, not read past the end.
        NxrException e = assertThrows(NxrException.class,
                () -> Nxr.decodeIdxBytes(new byte[Nxr.INDEX_RECORD_SIZE - 1]));
        assertEquals(-4, e.status());

        assertThrows(NxrException.class, () -> Nxr.decodeIdxBytes(new byte[1]));
        assertThrows(NxrException.class, () -> Nxr.decodeIdxBytes(new byte[0]));
        // Trailing partial record after a whole one.
        assertThrows(NxrException.class, () -> Nxr.decodeIdxBytes(new byte[Nxr.INDEX_RECORD_SIZE + 3]));

        assertThrows(NxrException.class, () -> Nxr.decodeBarBytes(new byte[Nxr.BAR_SIZE - 1]));
        assertThrows(NxrException.class, () -> Nxr.decodeTickBytes(new byte[Nxr.TICK_SIZE - 1]));
    }

    // ── Synth ────────────────────────────────────────────────────────

    @Test
    void synthComposesAndRejectsBadQuotes() {
        // ETH/BTC = (ETH/USDT) / (BTC/USDT).
        List<Nxr.SynthLeg> legs = List.of(
                Nxr.SynthLeg.of("ETH/USDT", 1, 3_000.0, 3_002.0),
                Nxr.SynthLeg.of("BTC/USDT", -1, 60_000.0, 60_020.0));

        Nxr.SynthTick tick = Nxr.computeSynthTick(legs).orElseThrow();
        assertTrue(tick.bid() > 0 && tick.ask() >= tick.bid());
        assertEquals(3_001.0 / 60_010.0, tick.mid(), 1e-9);

        // Identity path.
        Nxr.SynthTick identity = Nxr.computeSynthTick(List.of()).orElseThrow();
        assertEquals(1.0, identity.mid());
        assertEquals(10_000, identity.conf());

        // A non-positive leg quote yields no result rather than a garbage price.
        assertEquals(Optional.empty(),
                Nxr.computeSynthTick(List.of(Nxr.SynthLeg.of("ETH/USDT", 1, 0.0, 0.0))));

        assertThrows(IllegalArgumentException.class,
                () -> Nxr.computeSynthTick(List.of(new Nxr.SynthLeg("X/Y", 2, 1.0, 1.0, 1.0, 10_000))));
    }

    // ── Memory ───────────────────────────────────────────────────────

    /**
     * Every owned native string must come back. The native side counts strings
     * handed out minus strings freed, so a single missed free leaves a non-zero
     * residue: a deterministic proof a leak, unlike sampling RSS. At 200k
     * iterations x 4 strings a leak would also be plainly visible as native heap
     * growth.
     */
    @Test
    void ownedStringsDoNotLeak() {
        assertEquals(0L, Nxr.liveStringCount(), "must start clean");

        long ticker = Nxr.tryResolveTickerId("BTC/USDT").orElseThrow();
        for (int i = 0; i < 200_000; i++) {
            Nxr.Ticker t = Nxr.resolveTicker(ticker);
            if (t.base().isEmpty()) {
                throw new AssertionError("resolve returned nothing at iteration " + i);
            }
            Nxr.getMarketProvider(101).orElseThrow();
        }

        assertEquals(0L, Nxr.liveStringCount(), "every native string must have been freed");
    }

    /**
     * Guards the guard: proves {@link Nxr#liveStringCount()} actually tracks
     * un-freed strings, so the assertion above cannot pass vacuously.
     */
    @Test
    void liveStringCountDetectsAnUnfreedString() throws Throwable {
        long before = Nxr.liveStringCount();
        java.lang.foreign.MemorySegment[] held = new java.lang.foreign.MemorySegment[1_000];
        for (int i = 0; i < held.length; i++) {
            held[i] = (java.lang.foreign.MemorySegment)
                    NxrNative.MARKET_PROVIDER_NAME.invokeExact((short) 101);
        }
        assertEquals(before + held.length, Nxr.liveStringCount(), "counter must see un-freed strings");

        for (java.lang.foreign.MemorySegment p : held) {
            NxrNative.STRING_FREE.invokeExact(p);
        }
        assertEquals(before, Nxr.liveStringCount(), "and drop back once freed");
    }
}
