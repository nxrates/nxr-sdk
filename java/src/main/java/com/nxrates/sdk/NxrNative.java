package com.nxrates.sdk;

import static java.lang.foreign.ValueLayout.ADDRESS;
import static java.lang.foreign.ValueLayout.JAVA_BYTE;
import static java.lang.foreign.ValueLayout.JAVA_DOUBLE;
import static java.lang.foreign.ValueLayout.JAVA_FLOAT;
import static java.lang.foreign.ValueLayout.JAVA_INT;
import static java.lang.foreign.ValueLayout.JAVA_LONG;
import static java.lang.foreign.ValueLayout.JAVA_SHORT;

import java.lang.foreign.Arena;
import java.lang.foreign.FunctionDescriptor;
import java.lang.foreign.Linker;
import java.lang.foreign.MemoryLayout;
import java.lang.foreign.MemorySegment;
import java.lang.foreign.SymbolLookup;
import java.lang.invoke.MethodHandle;
import java.nio.file.Path;

/**
 * FFM plumbing for {@code libnxr_sdk_ffi}: library lookup, struct layouts,
 * downcall handles.
 *
 * <p>The layouts restate the {@code #[repr(C)]} mirrors in
 * {@code sdk/ffi/src/codec.rs}. Both sides assert their own sizes, so a field
 * added on one side without the other fails the build rather than shifting
 * offsets silently.
 */
final class NxrNative {

    private NxrNative() {}

    // ── Library ──────────────────────────────────────────────────────

    private static final Linker LINKER = Linker.nativeLinker();
    private static final SymbolLookup LOOKUP = lookup();

    /**
     * Resolution order: {@code -Dnxr.sdk.lib=<file>}, then {@code $NXR_SDK_LIB},
     * then the usual {@code java.library.path} search. The Gradle build sets the
     * system property to the freshly built cdylib so tests never pick up a stale
     * copy from elsewhere on the system.
     */
    private static SymbolLookup lookup() {
        String path = System.getProperty("nxr.sdk.lib");
        if (path == null || path.isBlank()) {
            path = System.getenv("NXR_SDK_LIB");
        }
        if (path != null && !path.isBlank()) {
            return SymbolLookup.libraryLookup(Path.of(path), Arena.global());
        }
        System.loadLibrary("nxr_sdk_ffi");
        return SymbolLookup.loaderLookup();
    }

    private static MethodHandle downcall(String name, FunctionDescriptor descriptor) {
        MemorySegment symbol = LOOKUP.find(name)
                .orElseThrow(() -> new NxrException("native symbol not found: " + name));
        return LINKER.downcallHandle(symbol, descriptor);
    }

    // ── Struct layouts ───────────────────────────────────────────────

    static final MemoryLayout IDX = MemoryLayout.structLayout(
            JAVA_LONG.withName("ts_ms"),
            JAVA_LONG.withName("ticker"),
            JAVA_DOUBLE.withName("bid"),
            JAVA_DOUBLE.withName("ask"),
            JAVA_DOUBLE.withName("ci_price"),
            JAVA_INT.withName("vbid"),
            JAVA_INT.withName("vask"),
            JAVA_SHORT.withName("provider"),
            JAVA_SHORT.withName("sequence"),
            JAVA_SHORT.withName("ci"),
            JAVA_SHORT.withName("tick_count"),
            JAVA_BYTE.withName("confidence"),
            JAVA_BYTE.withName("accepted"),
            JAVA_BYTE.withName("rejected"),
            JAVA_BYTE.withName("flags"),
            MemoryLayout.paddingLayout(4));

    static final MemoryLayout BAR = MemoryLayout.structLayout(
            JAVA_LONG.withName("open_ms"),
            JAVA_LONG.withName("close_ms"),
            JAVA_DOUBLE.withName("open"),
            JAVA_DOUBLE.withName("high"),
            JAVA_DOUBLE.withName("low"),
            JAVA_DOUBLE.withName("close"),
            JAVA_INT.withName("vbid"),
            JAVA_INT.withName("vask"),
            JAVA_INT.withName("tick_count"),
            JAVA_FLOAT.withName("realized_var"),
            JAVA_FLOAT.withName("bipower_var"),
            JAVA_FLOAT.withName("drift"),
            JAVA_FLOAT.withName("vol_imbalance"),
            JAVA_FLOAT.withName("avg_spread_bps"),
            JAVA_FLOAT.withName("max_abs_return"),
            JAVA_SHORT.withName("avg_ci_ubp"),
            JAVA_SHORT.withName("reject_rate"),
            JAVA_BYTE.withName("kind"),
            JAVA_BYTE.withName("flags"),
            MemoryLayout.paddingLayout(6));

    static final MemoryLayout TICK = MemoryLayout.structLayout(
            JAVA_LONG.withName("ticker"),
            JAVA_DOUBLE.withName("bid"),
            JAVA_DOUBLE.withName("ask"),
            JAVA_INT.withName("vbid"),
            JAVA_INT.withName("vask"));

    static final MemoryLayout SYNTH = MemoryLayout.structLayout(
            JAVA_DOUBLE.withName("bid"),
            JAVA_DOUBLE.withName("ask"),
            JAVA_DOUBLE.withName("mid"),
            JAVA_SHORT.withName("conf"),
            MemoryLayout.paddingLayout(6));

    static {
        assertSize(IDX, 64, "NxrIndexRecord");
        assertSize(BAR, 96, "NxrBar");
        assertSize(TICK, 32, "NxrTick");
        assertSize(SYNTH, 32, "NxrSynthTick");
    }

    private static void assertSize(MemoryLayout layout, int expected, String name) {
        if (layout.byteSize() != expected) {
            throw new NxrException(
                    "layout %s is %d B, native struct is %d B".formatted(name, layout.byteSize(), expected));
        }
    }

    static long off(MemoryLayout layout, String field) {
        return layout.byteOffset(MemoryLayout.PathElement.groupElement(field));
    }

    // ── Downcall handles ─────────────────────────────────────────────

    private static final FunctionDescriptor RESOLVE_ID =
            FunctionDescriptor.of(JAVA_INT, ADDRESS, ADDRESS);
    private static final FunctionDescriptor DECODE =
            FunctionDescriptor.of(JAVA_LONG, ADDRESS, JAVA_LONG, ADDRESS, JAVA_LONG);
    private static final FunctionDescriptor ENCODE =
            FunctionDescriptor.of(JAVA_INT, ADDRESS, ADDRESS, JAVA_LONG);

    static final MethodHandle RESOLVE_TICKER_ID = downcall("nxr_resolve_ticker_id", RESOLVE_ID);
    static final MethodHandle TRY_RESOLVE_TICKER_ID = downcall("nxr_try_resolve_ticker_id", RESOLVE_ID);
    static final MethodHandle RESOLVE_TICKER = downcall("nxr_resolve_ticker",
            FunctionDescriptor.of(JAVA_INT, JAVA_LONG, ADDRESS, ADDRESS, ADDRESS));
    static final MethodHandle MARKET_PROVIDER_NAME = downcall("nxr_market_provider_name",
            FunctionDescriptor.of(ADDRESS, JAVA_SHORT));
    static final MethodHandle DECODE_IDX = downcall("nxr_decode_idx", DECODE);
    static final MethodHandle DECODE_BAR = downcall("nxr_decode_bar", DECODE);
    static final MethodHandle DECODE_TICK = downcall("nxr_decode_tick", DECODE);
    static final MethodHandle ENCODE_IDX = downcall("nxr_encode_idx", ENCODE);
    static final MethodHandle ENCODE_BAR = downcall("nxr_encode_bar", ENCODE);
    static final MethodHandle COMPUTE_SYNTH_TICK = downcall("nxr_compute_synth_tick",
            FunctionDescriptor.of(JAVA_INT, ADDRESS, ADDRESS, JAVA_LONG, ADDRESS, ADDRESS, ADDRESS,
                    ADDRESS, ADDRESS));
    static final MethodHandle STRING_FREE = downcall("nxr_string_free",
            FunctionDescriptor.ofVoid(ADDRESS));
    static final MethodHandle STRING_LIVE_COUNT = downcall("nxr_string_live_count",
            FunctionDescriptor.of(JAVA_LONG));

    // ── Owned-string handling ────────────────────────────────────────

    /**
     * Copy an owned native string into a Java {@code String} and release it.
     * Always frees, including when the copy throws, so no path leaks.
     */
    static String takeString(MemorySegment ptr) {
        if (ptr == null || ptr.address() == 0) {
            return null;
        }
        try {
            return ptr.reinterpret(Long.MAX_VALUE).getString(0);
        } finally {
            try {
                STRING_FREE.invokeExact(ptr);
            } catch (Throwable t) {
                throw new NxrException("nxr_string_free failed", t);
            }
        }
    }

    /** Translate a negative native status into an exception. */
    static NxrException error(String call, int status) {
        String reason = switch (status) {
            case -1 -> "null pointer argument";
            case -2 -> "invalid UTF-8";
            case -3 -> "unresolvable symbol";
            case -4 -> "buffer length is zero or not a multiple of the record size";
            case -5 -> "output buffer too small";
            case -6 -> "no result";
            case -7 -> "argument out of range";
            default -> "unknown status";
        };
        return new NxrException("%s failed: %s (%d)".formatted(call, reason, status), status);
    }
}
