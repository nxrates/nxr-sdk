package com.nxrates.sdk;

import org.junit.jupiter.api.Test;

import java.nio.ByteBuffer;
import java.nio.ByteOrder;
import java.time.Instant;

import static org.junit.jupiter.api.Assertions.*;

class MitchTest {

    // ── Timestamp ────────────────────────────────────────────────────────

    @Test
    void timestampRoundTrip() {
        // Known point: 2024-06-15T12:00:00Z
        long epochUs = Instant.parse("2024-06-15T12:00:00Z").toEpochMilli() * 1_000L;
        long ticks = Mitch.fromEpochUs(epochUs);
        long recovered = Mitch.toEpochUs(ticks);
        // 16us granularity means we lose at most 15 us
        assertEquals(epochUs, recovered, 15);
    }

    @Test
    void timestampEpoch2010IsZeroTick() {
        assertEquals(0L, Mitch.fromEpochUs(Mitch.EPOCH_2010_US));
        assertEquals(Mitch.EPOCH_2010_US, Mitch.toEpochUs(0L));
    }

    // ── Wire code mapping ────────────────────────────────────────────────

    @Test
    void wireCodeToAsciiMapping() {
        assertEquals((byte) 't', Mitch.wireCodeToAscii(1));
        assertEquals((byte) 'o', Mitch.wireCodeToAscii(2));
        assertEquals((byte) 's', Mitch.wireCodeToAscii(3));
        assertEquals((byte) 'i', Mitch.wireCodeToAscii(4));
        assertEquals((byte) 'b', Mitch.wireCodeToAscii(5));
        assertEquals((byte) 'k', Mitch.wireCodeToAscii(6));
    }

    @Test
    void asciiToWireCodeMapping() {
        assertEquals(1, Mitch.asciiToWireCode(Mitch.MSG_TRADE));
        assertEquals(2, Mitch.asciiToWireCode(Mitch.MSG_ORDER));
        assertEquals(3, Mitch.asciiToWireCode(Mitch.MSG_TICK));
        assertEquals(4, Mitch.asciiToWireCode(Mitch.MSG_INDEX));
        assertEquals(5, Mitch.asciiToWireCode(Mitch.MSG_ORDER_BOOK));
        assertEquals(6, Mitch.asciiToWireCode(Mitch.MSG_BAR));
    }

    @Test
    void wireCodeOutOfRangeThrows() {
        assertThrows(IllegalArgumentException.class, () -> Mitch.wireCodeToAscii(0));
        assertThrows(IllegalArgumentException.class, () -> Mitch.wireCodeToAscii(7));
    }

    // ── MitchHeader ──────────────────────────────────────────────────────

    @Test
    void headerPackUnpackRoundTrip() {
        short tp = Mitch.MitchHeader.packTypeProvider(Mitch.MSG_TICK, 42);
        long ts = Mitch.fromEpochUs(Instant.parse("2024-01-01T00:00:00Z").toEpochMilli() * 1000L);
        var header = new Mitch.MitchHeader(tp, ts, 5, 0, 1234, new byte[4]);

        byte[] packed = header.pack();
        assertEquals(Mitch.SIZE_HEADER, packed.length);

        var unpacked = Mitch.MitchHeader.unpack(packed);
        assertEquals(header.typeProvider(), unpacked.typeProvider());
        assertEquals(header.timestamp(), unpacked.timestamp());
        assertEquals(header.count(), unpacked.count());
        assertEquals(header.flags(), unpacked.flags());
        assertEquals(header.sequence(), unpacked.sequence());
    }

    @Test
    void headerTypeProviderEncoding() {
        // Tick = wire code 3, provider 42 -> (42 << 4) | 3 = 675
        short tp = Mitch.MitchHeader.packTypeProvider(Mitch.MSG_TICK, 42);
        var header = new Mitch.MitchHeader(tp, 0, 1, 0, 0, new byte[4]);
        assertEquals((byte) 's', header.msgType());
        assertEquals(42, header.providerId());
    }

    @Test
    void headerMaxProviderId() {
        short tp = Mitch.MitchHeader.packTypeProvider(Mitch.MSG_TRADE, 4095);
        var header = new Mitch.MitchHeader(tp, 0, 1, 0, 0, new byte[4]);
        assertEquals((byte) 't', header.msgType());
        assertEquals(4095, header.providerId());
    }

    @Test
    void headerTimestampU48Fidelity() {
        // Use a large tick value that exercises all 48 bits
        long bigTick = (1L << 47) - 1;
        short tp = Mitch.MitchHeader.packTypeProvider(Mitch.MSG_INDEX, 1);
        var header = new Mitch.MitchHeader(tp, bigTick, 1, 0, 0, new byte[4]);

        var unpacked = Mitch.MitchHeader.unpack(header.pack());
        assertEquals(bigTick, unpacked.timestamp());
    }

    // ── Tick ─────────────────────────────────────────────────────────────

    @Test
    void tickPackUnpackRoundTrip() {
        var tick = new Mitch.Tick(123456789L, 1.2345, 1.2347, 1000, 2000);

        byte[] packed = tick.pack();
        assertEquals(Mitch.SIZE_TICK, packed.length);

        var unpacked = Mitch.Tick.unpack(packed);
        assertEquals(tick.ticker(), unpacked.ticker());
        assertEquals(tick.bid(), unpacked.bid(), 1e-15);
        assertEquals(tick.ask(), unpacked.ask(), 1e-15);
        assertEquals(tick.vbid(), unpacked.vbid());
        assertEquals(tick.vask(), unpacked.vask());
    }

    @Test
    void tickUnpackFromByteBuffer() {
        var tick = new Mitch.Tick(42L, 100.5, 101.5, 500, 600);
        var buf = ByteBuffer.wrap(tick.pack()).order(ByteOrder.LITTLE_ENDIAN);
        var unpacked = Mitch.Tick.unpack(buf);
        assertEquals(tick, unpacked);
    }

    // ── Trade ────────────────────────────────────────────────────────────

    @Test
    void tradePackUnpackRoundTrip() {
        var trade = new Mitch.Trade(987654321L, 50000.25, 10, 12345, (byte) 1);

        byte[] packed = trade.pack();
        assertEquals(Mitch.SIZE_TRADE, packed.length);

        var unpacked = Mitch.Trade.unpack(packed);
        assertEquals(trade.ticker(), unpacked.ticker());
        assertEquals(trade.price(), unpacked.price(), 1e-15);
        assertEquals(trade.qty(), unpacked.qty());
        assertEquals(trade.tradeId(), unpacked.tradeId());
        assertEquals(trade.side(), unpacked.side());
    }

    @Test
    void tradeIdMaxValue() {
        // 24-bit max = 16_777_215
        var trade = new Mitch.Trade(1L, 1.0, 1, 16_777_215, (byte) 0);
        var unpacked = Mitch.Trade.unpack(trade.pack());
        assertEquals(16_777_215, unpacked.tradeId());
    }

    @Test
    void tradeIdByteBoundaries() {
        // 0x010203 = 66051  ->  bytes: 03, 02, 01 (LE)
        var trade = new Mitch.Trade(1L, 1.0, 1, 0x010203, (byte) 'B');
        byte[] packed = trade.pack();
        // tradeId starts at offset 20
        assertEquals((byte) 0x03, packed[20]);
        assertEquals((byte) 0x02, packed[21]);
        assertEquals((byte) 0x01, packed[22]);
        assertEquals((byte) 'B', packed[23]);

        var unpacked = Mitch.Trade.unpack(packed);
        assertEquals(0x010203, unpacked.tradeId());
    }

    // ── Index ────────────────────────────────────────────────────────────

    @Test
    void indexPackUnpackRoundTrip() {
        var idx = new Mitch.Index(
                111L, 2.5, 2.6, 300, 400,
                (short) 50, (short) 10,
                (byte) 95, (byte) 8, (byte) 2, (byte) 0
        );

        byte[] packed = idx.pack();
        assertEquals(Mitch.SIZE_INDEX, packed.length);

        var unpacked = Mitch.Index.unpack(packed);
        assertEquals(idx.ticker(), unpacked.ticker());
        assertEquals(idx.bid(), unpacked.bid(), 1e-15);
        assertEquals(idx.ask(), unpacked.ask(), 1e-15);
        assertEquals(idx.vbid(), unpacked.vbid());
        assertEquals(idx.vask(), unpacked.vask());
        assertEquals(idx.ci(), unpacked.ci());
        assertEquals(idx.tickCount(), unpacked.tickCount());
        assertEquals(idx.confidence(), unpacked.confidence());
        assertEquals(idx.accepted(), unpacked.accepted());
        assertEquals(idx.rejected(), unpacked.rejected());
        assertEquals(idx.pad(), unpacked.pad());
    }

    @Test
    void indexUnpackFromByteBuffer() {
        var idx = new Mitch.Index(
                999L, 1.1, 1.2, 10, 20,
                (short) -1, (short) 100,
                (byte) 127, (byte) 5, (byte) 3, (byte) 0
        );
        var buf = ByteBuffer.wrap(idx.pack()).order(ByteOrder.LITTLE_ENDIAN);
        var unpacked = Mitch.Index.unpack(buf);
        assertEquals(idx, unpacked);
    }

    // ── size constants sanity ────────────────────────────────────────────

    @Test
    void sizeConstants() {
        assertEquals(16, Mitch.SIZE_HEADER);
        assertEquals(24, Mitch.SIZE_TRADE);
        assertEquals(32, Mitch.SIZE_ORDER);
        assertEquals(32, Mitch.SIZE_TICK);
        assertEquals(40, Mitch.SIZE_INDEX);
        assertEquals(128, Mitch.SIZE_BAR);
        assertEquals(2072, Mitch.SIZE_ORDER_BOOK);
    }
}
