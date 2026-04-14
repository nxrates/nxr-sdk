package com.nxrates.sdk;

import java.nio.ByteBuffer;
import java.nio.ByteOrder;

/**
 * MITCH wire format types and codec.
 * All multi-byte fields are little-endian. Java records for zero-overhead value types.
 */
public final class Mitch {

    private Mitch() {}

    // ── sizes ────────────────────────────────────────────────────────────
    public static final int SIZE_HEADER     = 16;
    public static final int SIZE_TRADE      = 24;
    public static final int SIZE_ORDER      = 32;
    public static final int SIZE_TICK       = 32;
    public static final int SIZE_INDEX      = 40;
    public static final int SIZE_BAR        = 128;
    public static final int SIZE_ORDER_BOOK = 2072;

    // ── message type ASCII codes ─────────────────────────────────────────
    public static final byte MSG_TRADE      = 't';
    public static final byte MSG_ORDER      = 'o';
    public static final byte MSG_TICK       = 's';
    public static final byte MSG_INDEX      = 'i';
    public static final byte MSG_BAR        = 'k';
    public static final byte MSG_ORDER_BOOK = 'b';

    // ── wire code <-> ASCII mapping ──────────────────────────────────────
    private static final byte[] WIRE_TO_ASCII = {0, MSG_TRADE, MSG_ORDER, MSG_TICK, MSG_INDEX, MSG_ORDER_BOOK, MSG_BAR};
    //                                           0      1          2          3         4            5           6

    /** Convert wire code (1-6) to ASCII message type. */
    public static byte wireCodeToAscii(int wireCode) {
        if (wireCode < 1 || wireCode > 6) throw new IllegalArgumentException("wire code out of range: " + wireCode);
        return WIRE_TO_ASCII[wireCode];
    }

    /** Convert ASCII message type to wire code (1-6). */
    public static int asciiToWireCode(byte ascii) {
        return switch (ascii) {
            case MSG_TRADE      -> 1;
            case MSG_ORDER      -> 2;
            case MSG_TICK       -> 3;
            case MSG_INDEX      -> 4;
            case MSG_ORDER_BOOK -> 5;
            case MSG_BAR        -> 6;
            default -> throw new IllegalArgumentException("unknown msg type: " + (char) ascii);
        };
    }

    // ── timestamp ────────────────────────────────────────────────────────
    /** Microseconds from Unix epoch to 2010-01-01T00:00:00Z. */
    public static final long EPOCH_2010_US = 1_262_304_000_000_000L;

    /** Encode a Unix-epoch microsecond timestamp to a u48 MITCH tick value (16 us granularity). */
    public static long fromEpochUs(long epochUs) {
        return (epochUs - EPOCH_2010_US) / 16;
    }

    /** Decode a u48 MITCH tick value back to Unix-epoch microseconds. */
    public static long toEpochUs(long ticks) {
        return ticks * 16 + EPOCH_2010_US;
    }

    /** Write a u48 LE value into buf at the current position (advances position by 6). */
    static void putU48(ByteBuffer buf, long value) {
        // write low 4 bytes then high 2 bytes
        buf.putInt((int) (value & 0xFFFFFFFFL));
        buf.putShort((short) ((value >>> 32) & 0xFFFFL));
    }

    /** Read a u48 LE value from buf at the current position (advances position by 6). */
    static long getU48(ByteBuffer buf) {
        long low  = Integer.toUnsignedLong(buf.getInt());
        long high = Short.toUnsignedLong(buf.getShort());
        return low | (high << 32);
    }

    // ── MitchHeader ──────────────────────────────────────────────────────
    /**
     * MITCH v2 header (16 bytes).
     *
     * @param typeProvider packed u16: bits [3:0] = wire code, bits [15:4] = provider_id
     * @param timestamp    u48 LE, 16us ticks since 2010-01-01
     * @param count        batch entry count (1-255)
     * @param flags        [1:0] = version, [7:2] = reserved
     * @param sequence     per-stream gap detection
     * @param reserved     4 reserved bytes
     */
    public record MitchHeader(
            short typeProvider,
            long  timestamp,
            int   count,
            int   flags,
            int   sequence,
            byte[] reserved
    ) {
        /** Decode the wire code to an ASCII message type char. */
        public byte msgType() {
            int wireCode = Short.toUnsignedInt(typeProvider) & 0x0F;
            return wireCodeToAscii(wireCode);
        }

        /** Extract the provider id (bits 4-15 of typeProvider). */
        public int providerId() {
            return (Short.toUnsignedInt(typeProvider) >>> 4) & 0x0FFF;
        }

        /** Build typeProvider from ASCII msg type and provider id. */
        public static short packTypeProvider(byte asciiType, int providerId) {
            int wireCode = asciiToWireCode(asciiType);
            return (short) ((wireCode & 0x0F) | ((providerId & 0x0FFF) << 4));
        }

        public byte[] pack() {
            var buf = ByteBuffer.allocate(SIZE_HEADER).order(ByteOrder.LITTLE_ENDIAN);
            buf.putShort(typeProvider);
            putU48(buf, timestamp);
            buf.put((byte) (count & 0xFF));
            buf.put((byte) (flags & 0xFF));
            buf.putShort((short) (sequence & 0xFFFF));
            byte[] res = reserved != null ? reserved : new byte[4];
            buf.put(res, 0, 4);
            return buf.array();
        }

        public static MitchHeader unpack(byte[] data) {
            return unpack(ByteBuffer.wrap(data).order(ByteOrder.LITTLE_ENDIAN));
        }

        public static MitchHeader unpack(ByteBuffer buf) {
            buf.order(ByteOrder.LITTLE_ENDIAN);
            short tp    = buf.getShort();
            long  ts    = getU48(buf);
            int   cnt   = Byte.toUnsignedInt(buf.get());
            int   fl    = Byte.toUnsignedInt(buf.get());
            int   seq   = Short.toUnsignedInt(buf.getShort());
            byte[] res  = new byte[4];
            buf.get(res);
            return new MitchHeader(tp, ts, cnt, fl, seq, res);
        }
    }

    // ── Tick (32 bytes) ──────────────────────────────────────────────────
    /**
     * @param ticker encoded instrument id
     * @param bid    best bid price
     * @param ask    best ask price
     * @param vbid   bid volume
     * @param vask   ask volume
     */
    public record Tick(long ticker, double bid, double ask, int vbid, int vask) {

        public byte[] pack() {
            var buf = ByteBuffer.allocate(SIZE_TICK).order(ByteOrder.LITTLE_ENDIAN);
            buf.putLong(ticker);
            buf.putDouble(bid);
            buf.putDouble(ask);
            buf.putInt(vbid);
            buf.putInt(vask);
            return buf.array();
        }

        public static Tick unpack(byte[] data) {
            return unpack(ByteBuffer.wrap(data).order(ByteOrder.LITTLE_ENDIAN));
        }

        public static Tick unpack(ByteBuffer buf) {
            buf.order(ByteOrder.LITTLE_ENDIAN);
            return new Tick(buf.getLong(), buf.getDouble(), buf.getDouble(), buf.getInt(), buf.getInt());
        }
    }

    // ── Trade (24 bytes) ─────────────────────────────────────────────────
    /**
     * @param ticker  encoded instrument id
     * @param price   trade price
     * @param qty     trade quantity
     * @param tradeId 24-bit trade id (0 .. 16_777_215)
     * @param side    trade side byte
     */
    public record Trade(long ticker, double price, int qty, int tradeId, byte side) {

        public byte[] pack() {
            var buf = ByteBuffer.allocate(SIZE_TRADE).order(ByteOrder.LITTLE_ENDIAN);
            buf.putLong(ticker);
            buf.putDouble(price);
            buf.putInt(qty);
            // tradeId: 3 bytes LE at offset 20
            buf.put((byte) (tradeId & 0xFF));
            buf.put((byte) ((tradeId >>> 8) & 0xFF));
            buf.put((byte) ((tradeId >>> 16) & 0xFF));
            buf.put(side);
            return buf.array();
        }

        public static Trade unpack(byte[] data) {
            return unpack(ByteBuffer.wrap(data).order(ByteOrder.LITTLE_ENDIAN));
        }

        public static Trade unpack(ByteBuffer buf) {
            buf.order(ByteOrder.LITTLE_ENDIAN);
            long   tk  = buf.getLong();
            double px  = buf.getDouble();
            int    q   = buf.getInt();
            int    id  = Byte.toUnsignedInt(buf.get())
                       | (Byte.toUnsignedInt(buf.get()) << 8)
                       | (Byte.toUnsignedInt(buf.get()) << 16);
            byte   s   = buf.get();
            return new Trade(tk, px, q, id, s);
        }
    }

    // ── Index (40 bytes) ─────────────────────────────────────────────────
    /**
     * @param ticker     encoded instrument id
     * @param bid        index bid
     * @param ask        index ask
     * @param vbid       bid volume
     * @param vask       ask volume
     * @param ci         composite index value (short)
     * @param tickCount  number of ticks (short)
     * @param confidence confidence level (byte)
     * @param accepted   accepted count (byte)
     * @param rejected   rejected count (byte)
     * @param pad        padding byte
     */
    public record Index(
            long ticker, double bid, double ask,
            int vbid, int vask,
            short ci, short tickCount,
            byte confidence, byte accepted, byte rejected, byte pad
    ) {
        public byte[] pack() {
            var buf = ByteBuffer.allocate(SIZE_INDEX).order(ByteOrder.LITTLE_ENDIAN);
            buf.putLong(ticker);
            buf.putDouble(bid);
            buf.putDouble(ask);
            buf.putInt(vbid);
            buf.putInt(vask);
            buf.putShort(ci);
            buf.putShort(tickCount);
            buf.put(confidence);
            buf.put(accepted);
            buf.put(rejected);
            buf.put(pad);
            return buf.array();
        }

        public static Index unpack(byte[] data) {
            return unpack(ByteBuffer.wrap(data).order(ByteOrder.LITTLE_ENDIAN));
        }

        public static Index unpack(ByteBuffer buf) {
            buf.order(ByteOrder.LITTLE_ENDIAN);
            long   tk   = buf.getLong();
            double b    = buf.getDouble();
            double a    = buf.getDouble();
            int    vb   = buf.getInt();
            int    va   = buf.getInt();
            short  ci   = buf.getShort();
            short  tc   = buf.getShort();
            byte   conf = buf.get();
            byte   acc  = buf.get();
            byte   rej  = buf.get();
            byte   pad  = buf.get();
            return new Index(tk, b, a, vb, va, ci, tc, conf, acc, rej, pad);
        }
    }
}
