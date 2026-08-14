package com.nxrates.sdk;

import static java.lang.foreign.ValueLayout.ADDRESS;
import static java.lang.foreign.ValueLayout.JAVA_BYTE;
import static java.lang.foreign.ValueLayout.JAVA_DOUBLE;
import static java.lang.foreign.ValueLayout.JAVA_FLOAT;
import static java.lang.foreign.ValueLayout.JAVA_INT;
import static java.lang.foreign.ValueLayout.JAVA_LONG;
import static java.lang.foreign.ValueLayout.JAVA_SHORT;

import java.lang.foreign.Arena;
import java.lang.foreign.MemorySegment;
import java.util.ArrayList;
import java.util.List;
import java.util.Optional;
import java.util.OptionalLong;

/**
 * NX Rates SDK: MITCH ticker resolution, fixed-width record codecs, and synth
 * composition, bound to the Rust core over the FFM API.
 *
 * <p>Every method is static and thread-safe. Native memory is confined to a
 * per-call {@link Arena}, and owned native strings are released before the
 * call returns; {@link #liveStringCount()} exposes the outstanding count.
 */
public final class Nxr {

    private Nxr() {}

    /** Wire size of an {@link IndexRecord}. */
    public static final int INDEX_RECORD_SIZE = 56;
    /** Wire size of a {@link Bar}. */
    public static final int BAR_SIZE = 96;
    /** Wire size of a {@link Tick}. */
    public static final int TICK_SIZE = 32;

    // ── Records ──────────────────────────────────────────────────────

    /**
     * A 56-byte MITCH IndexRecord: 16-byte header plus 40-byte Index body.
     *
     * @param ciPrice derived from the sqrt-compressed {@code ci} by the native
     *                layer; ignored by {@link #encodeIdxRecord(IndexRecord)}.
     */
    public record IndexRecord(
            long tsMs, long ticker, double bid, double ask, double ciPrice,
            int vbid, int vask, int provider, int sequence, int ci, int tickCount,
            int confidence, int accepted, int rejected, int flags) {

        /** Minimal record with the same field defaults the Python binding applies. */
        public static IndexRecord of(long tsMs, int provider, long ticker, double bid, double ask) {
            return new IndexRecord(tsMs, ticker, bid, ask, 0.0, 0, 0, provider, 0, 0, 1, 1, 1, 0, 0);
        }

        public double mid() {
            return (bid + ask) / 2.0;
        }

        /** Spread in basis points, 0 for a non-positive mid (matches mitch). */
        public double spreadBps() {
            double mid = mid();
            return mid > 0 ? (ask - bid) / mid * 10_000.0 : 0.0;
        }
    }

    /** A 96-byte MITCH Bar: OHLCV plus microstructure. */
    public record Bar(
            long openMs, long closeMs, double open, double high, double low, double close,
            int vbid, int vask, int tickCount,
            float realizedVar, float bipowerVar, float drift, float volImbalance,
            float avgSpreadBps, float maxAbsReturn,
            int avgCiUbp, int rejectRate, int kind, int flags) {

        /** OHLC-only bar; microstructure fields zeroed. */
        public static Bar of(long openMs, long closeMs, double open, double high, double low, double close) {
            return new Bar(openMs, closeMs, open, high, low, close, 0, 0, 0,
                    0f, 0f, 0f, 0f, 0f, 0f, 0, 0, 0, 0);
        }
    }

    /** A 32-byte MITCH Tick body. */
    public record Tick(long ticker, double bid, double ask, int vbid, int vask) {
        public double mid() {
            return (bid + ask) / 2.0;
        }
    }

    /**
     * A ticker id reversed into its parts.
     *
     * <p>{@code base} and {@code quote} are upper-cased asset <i>names</i>, not
     * the symbols that resolved to the id: {@code BTC/USDT} reverses to
     * {@code BITCOIN} / {@code TETHER}. {@code instrumentType} is the enum name
     * ({@code SPOT}, {@code PERP}, ...).
     *
     * <p>For ids that did not come from the resolver (FNV phantoms), {@code base}
     * is {@code 0x}-prefixed hex and {@code quote} is empty.
     */
    public record Ticker(String base, String quote, String instrumentType) {}

    /** A market venue. */
    public record MarketProvider(int id, String name) {}

    /** One leg of a synth path with its quote. {@code exp} is +1 or -1. */
    public record SynthLeg(String sym, int exp, double bid, double ask, double mid, int conf) {

        /** Leg with {@code mid} derived and full confidence, matching the Python defaults. */
        public static SynthLeg of(String sym, int exp, double bid, double ask) {
            return new SynthLeg(sym, exp, bid, ask, (bid + ask) / 2.0, 10_000);
        }
    }

    /** A composed synth quote. {@code conf} is in bps. */
    public record SynthTick(double bid, double ask, double mid, int conf) {}

    // ── Resolution ───────────────────────────────────────────────────

    /**
     * Resolve a symbol ("BTC/USDT", "EURUSD") to its 64-bit MITCH ticker id.
     *
     * <p><b>Lenient.</b> An unresolvable symbol yields an FNV1a-64 <i>phantom</i>
     * id rather than an error: unique, but not a bit-packed ticker id, so
     * {@link #resolveTicker(long)} reverses it to a hex base with an empty quote
     * and its class bits are hash noise. This is the same caveat the Python
     * binding carries. Prefer {@link #tryResolveTickerId(String)}.
     *
     * @return the ticker id, as raw u64 bits in a Java {@code long}
     */
    public static long resolveTickerId(String symbol) {
        return resolveId(symbol, NxrNative.RESOLVE_TICKER_ID, "nxr_resolve_ticker_id")
                .orElseThrow(() -> new NxrException("lenient resolve returned no id"));
    }

    /**
     * Strict resolution: empty when the symbol has no MITCH ticker id. No FNV
     * phantom fallback. This is the one callers should reach for.
     */
    public static OptionalLong tryResolveTickerId(String symbol) {
        return resolveId(symbol, NxrNative.TRY_RESOLVE_TICKER_ID, "nxr_try_resolve_ticker_id");
    }

    private static OptionalLong resolveId(String symbol, java.lang.invoke.MethodHandle handle, String call) {
        if (symbol == null) {
            throw new IllegalArgumentException("symbol must not be null");
        }
        try (Arena arena = Arena.ofConfined()) {
            MemorySegment sym = arena.allocateFrom(symbol);
            MemorySegment out = arena.allocate(JAVA_LONG);
            int status = (int) handle.invokeExact(sym, out);
            if (status == -3) {
                return OptionalLong.empty();
            }
            if (status != 0) {
                throw NxrNative.error(call, status);
            }
            return OptionalLong.of(out.get(JAVA_LONG, 0));
        } catch (NxrException | IllegalArgumentException e) {
            throw e;
        } catch (Throwable t) {
            throw new NxrException(call + " invocation failed", t);
        }
    }

    /** Reverse a ticker id into base, quote, and instrument type. */
    public static Ticker resolveTicker(long tickerId) {
        try (Arena arena = Arena.ofConfined()) {
            MemorySegment base = arena.allocate(ADDRESS);
            MemorySegment quote = arena.allocate(ADDRESS);
            MemorySegment instrument = arena.allocate(ADDRESS);
            int status = (int) NxrNative.RESOLVE_TICKER.invokeExact(tickerId, base, quote, instrument);
            if (status != 0) {
                throw NxrNative.error("nxr_resolve_ticker", status);
            }
            return new Ticker(
                    NxrNative.takeString(base.get(ADDRESS, 0)),
                    NxrNative.takeString(quote.get(ADDRESS, 0)),
                    NxrNative.takeString(instrument.get(ADDRESS, 0)));
        } catch (NxrException e) {
            throw e;
        } catch (Throwable t) {
            throw new NxrException("nxr_resolve_ticker invocation failed", t);
        }
    }

    /** Market provider metadata by numeric id, empty when unknown. */
    public static Optional<MarketProvider> getMarketProvider(int id) {
        try {
            MemorySegment ptr = (MemorySegment) NxrNative.MARKET_PROVIDER_NAME.invokeExact((short) id);
            String name = NxrNative.takeString(ptr);
            return name == null ? Optional.empty() : Optional.of(new MarketProvider(id, name));
        } catch (NxrException e) {
            throw e;
        } catch (Throwable t) {
            throw new NxrException("nxr_market_provider_name invocation failed", t);
        }
    }

    // ── Decoders ─────────────────────────────────────────────────────

    private static final long I_TS = NxrNative.off(NxrNative.IDX, "ts_ms");
    private static final long I_TICKER = NxrNative.off(NxrNative.IDX, "ticker");
    private static final long I_BID = NxrNative.off(NxrNative.IDX, "bid");
    private static final long I_ASK = NxrNative.off(NxrNative.IDX, "ask");
    private static final long I_CI_PRICE = NxrNative.off(NxrNative.IDX, "ci_price");
    private static final long I_VBID = NxrNative.off(NxrNative.IDX, "vbid");
    private static final long I_VASK = NxrNative.off(NxrNative.IDX, "vask");
    private static final long I_PROVIDER = NxrNative.off(NxrNative.IDX, "provider");
    private static final long I_SEQUENCE = NxrNative.off(NxrNative.IDX, "sequence");
    private static final long I_CI = NxrNative.off(NxrNative.IDX, "ci");
    private static final long I_TICK_COUNT = NxrNative.off(NxrNative.IDX, "tick_count");
    private static final long I_CONFIDENCE = NxrNative.off(NxrNative.IDX, "confidence");
    private static final long I_ACCEPTED = NxrNative.off(NxrNative.IDX, "accepted");
    private static final long I_REJECTED = NxrNative.off(NxrNative.IDX, "rejected");
    private static final long I_FLAGS = NxrNative.off(NxrNative.IDX, "flags");

    private static final long B_OPEN_MS = NxrNative.off(NxrNative.BAR, "open_ms");
    private static final long B_CLOSE_MS = NxrNative.off(NxrNative.BAR, "close_ms");
    private static final long B_OPEN = NxrNative.off(NxrNative.BAR, "open");
    private static final long B_HIGH = NxrNative.off(NxrNative.BAR, "high");
    private static final long B_LOW = NxrNative.off(NxrNative.BAR, "low");
    private static final long B_CLOSE = NxrNative.off(NxrNative.BAR, "close");
    private static final long B_VBID = NxrNative.off(NxrNative.BAR, "vbid");
    private static final long B_VASK = NxrNative.off(NxrNative.BAR, "vask");
    private static final long B_TICK_COUNT = NxrNative.off(NxrNative.BAR, "tick_count");
    private static final long B_REALIZED = NxrNative.off(NxrNative.BAR, "realized_var");
    private static final long B_BIPOWER = NxrNative.off(NxrNative.BAR, "bipower_var");
    private static final long B_DRIFT = NxrNative.off(NxrNative.BAR, "drift");
    private static final long B_VOL_IMB = NxrNative.off(NxrNative.BAR, "vol_imbalance");
    private static final long B_AVG_SPREAD = NxrNative.off(NxrNative.BAR, "avg_spread_bps");
    private static final long B_MAX_ABS = NxrNative.off(NxrNative.BAR, "max_abs_return");
    private static final long B_AVG_CI = NxrNative.off(NxrNative.BAR, "avg_ci_ubp");
    private static final long B_REJECT = NxrNative.off(NxrNative.BAR, "reject_rate");
    private static final long B_KIND = NxrNative.off(NxrNative.BAR, "kind");
    private static final long B_FLAGS = NxrNative.off(NxrNative.BAR, "flags");

    private static final long T_TICKER = NxrNative.off(NxrNative.TICK, "ticker");
    private static final long T_BID = NxrNative.off(NxrNative.TICK, "bid");
    private static final long T_ASK = NxrNative.off(NxrNative.TICK, "ask");
    private static final long T_VBID = NxrNative.off(NxrNative.TICK, "vbid");
    private static final long T_VASK = NxrNative.off(NxrNative.TICK, "vask");

    /**
     * Decode a slab of 56-byte IndexRecords.
     *
     * @throws NxrException if the buffer is empty or its length is not a whole
     *                      multiple of 56. The native layer applies the same
     *                      check the core applies to every fixed-width slab, so
     *                      a short buffer is rejected rather than read past.
     */
    public static List<IndexRecord> decodeIdxBytes(byte[] buf) {
        return decode(buf, INDEX_RECORD_SIZE, NxrNative.IDX, NxrNative.DECODE_IDX, "nxr_decode_idx",
                (seg, base) -> new IndexRecord(
                        seg.get(JAVA_LONG, base + I_TS),
                        seg.get(JAVA_LONG, base + I_TICKER),
                        seg.get(JAVA_DOUBLE, base + I_BID),
                        seg.get(JAVA_DOUBLE, base + I_ASK),
                        seg.get(JAVA_DOUBLE, base + I_CI_PRICE),
                        seg.get(JAVA_INT, base + I_VBID),
                        seg.get(JAVA_INT, base + I_VASK),
                        Short.toUnsignedInt(seg.get(JAVA_SHORT, base + I_PROVIDER)),
                        Short.toUnsignedInt(seg.get(JAVA_SHORT, base + I_SEQUENCE)),
                        Short.toUnsignedInt(seg.get(JAVA_SHORT, base + I_CI)),
                        Short.toUnsignedInt(seg.get(JAVA_SHORT, base + I_TICK_COUNT)),
                        Byte.toUnsignedInt(seg.get(JAVA_BYTE, base + I_CONFIDENCE)),
                        Byte.toUnsignedInt(seg.get(JAVA_BYTE, base + I_ACCEPTED)),
                        Byte.toUnsignedInt(seg.get(JAVA_BYTE, base + I_REJECTED)),
                        Byte.toUnsignedInt(seg.get(JAVA_BYTE, base + I_FLAGS))));
    }

    /** Decode a slab of 96-byte Bars. Same length contract as {@link #decodeIdxBytes(byte[])}. */
    public static List<Bar> decodeBarBytes(byte[] buf) {
        return decode(buf, BAR_SIZE, NxrNative.BAR, NxrNative.DECODE_BAR, "nxr_decode_bar",
                (seg, base) -> new Bar(
                        seg.get(JAVA_LONG, base + B_OPEN_MS),
                        seg.get(JAVA_LONG, base + B_CLOSE_MS),
                        seg.get(JAVA_DOUBLE, base + B_OPEN),
                        seg.get(JAVA_DOUBLE, base + B_HIGH),
                        seg.get(JAVA_DOUBLE, base + B_LOW),
                        seg.get(JAVA_DOUBLE, base + B_CLOSE),
                        seg.get(JAVA_INT, base + B_VBID),
                        seg.get(JAVA_INT, base + B_VASK),
                        seg.get(JAVA_INT, base + B_TICK_COUNT),
                        seg.get(JAVA_FLOAT, base + B_REALIZED),
                        seg.get(JAVA_FLOAT, base + B_BIPOWER),
                        seg.get(JAVA_FLOAT, base + B_DRIFT),
                        seg.get(JAVA_FLOAT, base + B_VOL_IMB),
                        seg.get(JAVA_FLOAT, base + B_AVG_SPREAD),
                        seg.get(JAVA_FLOAT, base + B_MAX_ABS),
                        Short.toUnsignedInt(seg.get(JAVA_SHORT, base + B_AVG_CI)),
                        Short.toUnsignedInt(seg.get(JAVA_SHORT, base + B_REJECT)),
                        Byte.toUnsignedInt(seg.get(JAVA_BYTE, base + B_KIND)),
                        Byte.toUnsignedInt(seg.get(JAVA_BYTE, base + B_FLAGS))));
    }

    /** Decode a slab of 32-byte MITCH Ticks. Same length contract as {@link #decodeIdxBytes(byte[])}. */
    public static List<Tick> decodeTickBytes(byte[] buf) {
        return decode(buf, TICK_SIZE, NxrNative.TICK, NxrNative.DECODE_TICK, "nxr_decode_tick",
                (seg, base) -> new Tick(
                        seg.get(JAVA_LONG, base + T_TICKER),
                        seg.get(JAVA_DOUBLE, base + T_BID),
                        seg.get(JAVA_DOUBLE, base + T_ASK),
                        seg.get(JAVA_INT, base + T_VBID),
                        seg.get(JAVA_INT, base + T_VASK)));
    }

    @FunctionalInterface
    private interface Reader<T> {
        T read(MemorySegment seg, long base);
    }

    private static <T> List<T> decode(byte[] buf, int stride, java.lang.foreign.MemoryLayout layout,
            java.lang.invoke.MethodHandle handle, String call, Reader<T> reader) {
        if (buf == null) {
            throw new IllegalArgumentException("buffer must not be null");
        }
        // Native re-validates; this only sizes the output slab.
        long n = buf.length / stride;
        try (Arena arena = Arena.ofConfined()) {
            MemorySegment in = arena.allocateFrom(JAVA_BYTE, buf);
            MemorySegment out = arena.allocate(layout, n);
            long count = (long) handle.invokeExact(in, (long) buf.length, out, n);
            if (count < 0) {
                throw NxrNative.error(call, (int) count);
            }
            List<T> records = new ArrayList<>((int) count);
            for (long i = 0; i < count; i++) {
                records.add(reader.read(out, i * layout.byteSize()));
            }
            return records;
        } catch (NxrException | IllegalArgumentException e) {
            throw e;
        } catch (Throwable t) {
            throw new NxrException(call + " invocation failed", t);
        }
    }

    // ── Encoders ─────────────────────────────────────────────────────

    /** Encode one IndexRecord into 56 wire bytes. {@code ciPrice} is derived and ignored. */
    public static byte[] encodeIdxRecord(IndexRecord record) {
        if (record == null) {
            throw new IllegalArgumentException("record must not be null");
        }
        try (Arena arena = Arena.ofConfined()) {
            MemorySegment in = arena.allocate(NxrNative.IDX);
            in.set(JAVA_LONG, I_TS, record.tsMs());
            in.set(JAVA_LONG, I_TICKER, record.ticker());
            in.set(JAVA_DOUBLE, I_BID, record.bid());
            in.set(JAVA_DOUBLE, I_ASK, record.ask());
            in.set(JAVA_INT, I_VBID, record.vbid());
            in.set(JAVA_INT, I_VASK, record.vask());
            in.set(JAVA_SHORT, I_PROVIDER, (short) record.provider());
            in.set(JAVA_SHORT, I_SEQUENCE, (short) record.sequence());
            in.set(JAVA_SHORT, I_CI, (short) record.ci());
            in.set(JAVA_SHORT, I_TICK_COUNT, (short) record.tickCount());
            in.set(JAVA_BYTE, I_CONFIDENCE, (byte) record.confidence());
            in.set(JAVA_BYTE, I_ACCEPTED, (byte) record.accepted());
            in.set(JAVA_BYTE, I_REJECTED, (byte) record.rejected());
            in.set(JAVA_BYTE, I_FLAGS, (byte) record.flags());
            return encode(arena, in, INDEX_RECORD_SIZE, NxrNative.ENCODE_IDX, "nxr_encode_idx");
        }
    }

    /** Encode one Bar into 96 wire bytes. Lossless: every field round-trips. */
    public static byte[] encodeBar(Bar bar) {
        if (bar == null) {
            throw new IllegalArgumentException("bar must not be null");
        }
        try (Arena arena = Arena.ofConfined()) {
            MemorySegment in = arena.allocate(NxrNative.BAR);
            in.set(JAVA_LONG, B_OPEN_MS, bar.openMs());
            in.set(JAVA_LONG, B_CLOSE_MS, bar.closeMs());
            in.set(JAVA_DOUBLE, B_OPEN, bar.open());
            in.set(JAVA_DOUBLE, B_HIGH, bar.high());
            in.set(JAVA_DOUBLE, B_LOW, bar.low());
            in.set(JAVA_DOUBLE, B_CLOSE, bar.close());
            in.set(JAVA_INT, B_VBID, bar.vbid());
            in.set(JAVA_INT, B_VASK, bar.vask());
            in.set(JAVA_INT, B_TICK_COUNT, bar.tickCount());
            in.set(JAVA_FLOAT, B_REALIZED, bar.realizedVar());
            in.set(JAVA_FLOAT, B_BIPOWER, bar.bipowerVar());
            in.set(JAVA_FLOAT, B_DRIFT, bar.drift());
            in.set(JAVA_FLOAT, B_VOL_IMB, bar.volImbalance());
            in.set(JAVA_FLOAT, B_AVG_SPREAD, bar.avgSpreadBps());
            in.set(JAVA_FLOAT, B_MAX_ABS, bar.maxAbsReturn());
            in.set(JAVA_SHORT, B_AVG_CI, (short) bar.avgCiUbp());
            in.set(JAVA_SHORT, B_REJECT, (short) bar.rejectRate());
            in.set(JAVA_BYTE, B_KIND, (byte) bar.kind());
            in.set(JAVA_BYTE, B_FLAGS, (byte) bar.flags());
            return encode(arena, in, BAR_SIZE, NxrNative.ENCODE_BAR, "nxr_encode_bar");
        }
    }

    private static byte[] encode(Arena arena, MemorySegment in, int size,
            java.lang.invoke.MethodHandle handle, String call) {
        MemorySegment out = arena.allocate(size);
        try {
            int status = (int) handle.invokeExact(in, out, (long) size);
            if (status != 0) {
                throw NxrNative.error(call, status);
            }
            return out.toArray(JAVA_BYTE);
        } catch (NxrException e) {
            throw e;
        } catch (Throwable t) {
            throw new NxrException(call + " invocation failed", t);
        }
    }

    // ── Synth ────────────────────────────────────────────────────────

    private static final long S_BID = NxrNative.off(NxrNative.SYNTH, "bid");
    private static final long S_ASK = NxrNative.off(NxrNative.SYNTH, "ask");
    private static final long S_MID = NxrNative.off(NxrNative.SYNTH, "mid");
    private static final long S_CONF = NxrNative.off(NxrNative.SYNTH, "conf");

    /**
     * Compose a synthetic tick from signed legs.
     *
     * <p>Each leg carries its own quote, unlike the Python binding's separate
     * symbol-keyed snapshot map: the composer only looks a leg up by its own
     * symbol, so the map is redundant here. An empty list is the identity path
     * and yields {@code (1, 1, 1, conf=10000)}.
     *
     * @return empty when a leg quote is non-positive
     */
    public static Optional<SynthTick> computeSynthTick(List<SynthLeg> legs) {
        if (legs == null) {
            throw new IllegalArgumentException("legs must not be null");
        }
        int n = legs.size();
        try (Arena arena = Arena.ofConfined()) {
            MemorySegment syms = arena.allocate(ADDRESS, n);
            MemorySegment exps = arena.allocate(JAVA_BYTE, n);
            MemorySegment bids = arena.allocate(JAVA_DOUBLE, n);
            MemorySegment asks = arena.allocate(JAVA_DOUBLE, n);
            MemorySegment mids = arena.allocate(JAVA_DOUBLE, n);
            MemorySegment confs = arena.allocate(JAVA_SHORT, n);
            for (int i = 0; i < n; i++) {
                SynthLeg leg = legs.get(i);
                if (leg.exp() != 1 && leg.exp() != -1) {
                    throw new IllegalArgumentException("leg exp must be +1 or -1, got " + leg.exp());
                }
                syms.setAtIndex(ADDRESS, i, arena.allocateFrom(leg.sym()));
                exps.setAtIndex(JAVA_BYTE, i, (byte) leg.exp());
                bids.setAtIndex(JAVA_DOUBLE, i, leg.bid());
                asks.setAtIndex(JAVA_DOUBLE, i, leg.ask());
                mids.setAtIndex(JAVA_DOUBLE, i, leg.mid());
                confs.setAtIndex(JAVA_SHORT, i, (short) leg.conf());
            }
            MemorySegment out = arena.allocate(NxrNative.SYNTH);
            int status = (int) NxrNative.COMPUTE_SYNTH_TICK.invokeExact(
                    syms, exps, (long) n, bids, asks, mids, confs, out);
            if (status == -6) {
                return Optional.empty();
            }
            if (status != 0) {
                throw NxrNative.error("nxr_compute_synth_tick", status);
            }
            return Optional.of(new SynthTick(
                    out.get(JAVA_DOUBLE, S_BID),
                    out.get(JAVA_DOUBLE, S_ASK),
                    out.get(JAVA_DOUBLE, S_MID),
                    Short.toUnsignedInt(out.get(JAVA_SHORT, S_CONF))));
        } catch (NxrException | IllegalArgumentException e) {
            throw e;
        } catch (Throwable t) {
            throw new NxrException("nxr_compute_synth_tick invocation failed", t);
        }
    }

    // ── Diagnostics ──────────────────────────────────────────────────

    /**
     * Native strings handed out and not yet freed. Every method above frees
     * before returning, so this is 0 at rest; the test suite asserts it.
     */
    public static long liveStringCount() {
        try {
            return (long) NxrNative.STRING_LIVE_COUNT.invokeExact();
        } catch (Throwable t) {
            throw new NxrException("nxr_string_live_count invocation failed", t);
        }
    }
}
