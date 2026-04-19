package com.nxrates.sdk;

import java.net.URI;
import java.net.http.HttpClient;
import java.net.http.HttpRequest;
import java.net.http.HttpResponse;
import java.net.http.WebSocket;
import java.nio.ByteBuffer;
import java.nio.ByteOrder;
import java.time.Duration;
import java.util.*;
import java.util.concurrent.CompletionStage;
import java.util.concurrent.CountDownLatch;
import java.util.function.Consumer;

/**
 * NX Rates REST + WebSocket client.
 * Uses only {@code java.net.http} - no external HTTP or JSON libraries.
 */
public class NxrClient implements AutoCloseable {

    private final String baseUrl;
    private final HttpClient http;

    public NxrClient(String baseUrl) {
        this.baseUrl = baseUrl.endsWith("/") ? baseUrl.substring(0, baseUrl.length() - 1) : baseUrl;
        this.http = HttpClient.newBuilder()
                .connectTimeout(Duration.ofSeconds(10))
                .build();
    }

    // ── REST helpers ─────────────────────────────────────────────────────

    private String get(String path) throws Exception {
        var req = HttpRequest.newBuilder()
                .uri(URI.create(baseUrl + path))
                .timeout(Duration.ofSeconds(30))
                .GET()
                .build();
        var resp = http.send(req, HttpResponse.BodyHandlers.ofString());
        if (resp.statusCode() / 100 != 2) {
            throw new RuntimeException("HTTP " + resp.statusCode() + " on " + path);
        }
        return resp.body();
    }

    // ── minimal JSON helpers (no external deps) ──────────────────────────

    /** Parse a flat JSON object whose values are numbers: {"KEY": 123, ...} */
    private static Map<String, Long> parseStringLongMap(String json) {
        var map = new LinkedHashMap<String, Long>();
        json = json.trim();
        if (json.startsWith("{")) json = json.substring(1);
        if (json.endsWith("}"))   json = json.substring(0, json.length() - 1);
        if (json.isBlank()) return map;
        for (var entry : splitJsonEntries(json)) {
            var kv = splitColon(entry);
            map.put(unquote(kv[0].trim()), Long.parseLong(kv[1].trim()));
        }
        return map;
    }

    /** Parse a flat JSON object whose keys are ints and values are strings: {"1":"foo",...} */
    private static Map<Integer, String> parseIntStringMap(String json) {
        var map = new LinkedHashMap<Integer, String>();
        json = json.trim();
        if (json.startsWith("{")) json = json.substring(1);
        if (json.endsWith("}"))   json = json.substring(0, json.length() - 1);
        if (json.isBlank()) return map;
        for (var entry : splitJsonEntries(json)) {
            var kv = splitColon(entry);
            map.put(Integer.parseInt(unquote(kv[0].trim())), unquote(kv[1].trim()));
        }
        return map;
    }

    /** Split top-level comma-separated entries, respecting braces/brackets. */
    private static List<String> splitJsonEntries(String s) {
        var entries = new ArrayList<String>();
        int depth = 0;
        int start = 0;
        for (int i = 0; i < s.length(); i++) {
            char c = s.charAt(i);
            if (c == '{' || c == '[') depth++;
            else if (c == '}' || c == ']') depth--;
            else if (c == ',' && depth == 0) {
                entries.add(s.substring(start, i));
                start = i + 1;
            }
        }
        if (start < s.length()) entries.add(s.substring(start));
        return entries;
    }

    /** Split on the first colon outside quotes. */
    private static String[] splitColon(String s) {
        boolean inQuote = false;
        for (int i = 0; i < s.length(); i++) {
            char c = s.charAt(i);
            if (c == '"') inQuote = !inQuote;
            else if (c == ':' && !inQuote) {
                return new String[]{s.substring(0, i), s.substring(i + 1)};
            }
        }
        throw new IllegalArgumentException("no colon found in: " + s);
    }

    private static String unquote(String s) {
        s = s.trim();
        if (s.startsWith("\"") && s.endsWith("\"")) return s.substring(1, s.length() - 1);
        return s;
    }

    // ── TickerSnapshot ───────────────────────────────────────────────────
    /**
     * Snapshot of a ticker from the REST tickers endpoint.
     * Parsed from a JSON array of objects.
     */
    public record TickerSnapshot(
            String symbol,
            long ticker,
            double bid,
            double ask,
            double last,
            long volume
    ) {}

    /** Parse a JSON array of ticker snapshot objects. */
    private static List<TickerSnapshot> parseTickerSnapshots(String json) {
        var list = new ArrayList<TickerSnapshot>();
        json = json.trim();
        if (json.startsWith("[")) json = json.substring(1);
        if (json.endsWith("]"))   json = json.substring(0, json.length() - 1);
        if (json.isBlank()) return list;
        for (var objStr : splitJsonEntries(json)) {
            objStr = objStr.trim();
            if (objStr.startsWith("{")) objStr = objStr.substring(1);
            if (objStr.endsWith("}"))   objStr = objStr.substring(0, objStr.length() - 1);
            var fields = new LinkedHashMap<String, String>();
            for (var field : splitJsonEntries(objStr)) {
                var kv = splitColon(field);
                fields.put(unquote(kv[0].trim()), kv[1].trim());
            }
            list.add(new TickerSnapshot(
                    unquote(fields.getOrDefault("symbol", "\"\"")),
                    Long.parseLong(fields.getOrDefault("ticker", "0")),
                    Double.parseDouble(fields.getOrDefault("bid", "0")),
                    Double.parseDouble(fields.getOrDefault("ask", "0")),
                    Double.parseDouble(fields.getOrDefault("last", "0")),
                    Long.parseLong(fields.getOrDefault("volume", "0"))
            ));
        }
        return list;
    }

    // ── REST endpoints ───────────────────────────────────────────────────

    /** GET /v1/symbols -> Map<symbol, ticker_id> */
    public Map<String, Long> symbols() throws Exception {
        return parseStringLongMap(get("/v1/symbols"));
    }

    /** GET /v1/providers -> Map<provider_id, name> */
    public Map<Integer, String> providers() throws Exception {
        return parseIntStringMap(get("/v1/providers"));
    }

    /** GET /v1/tickers -> list of ticker snapshots */
    public List<TickerSnapshot> tickers() throws Exception {
        return parseTickerSnapshots(get("/v1/tickers"));
    }

    /** GET /health -> true if 2xx */
    public boolean isHealthy() {
        try {
            get("/health");
            return true;
        } catch (Exception e) {
            return false;
        }
    }

    // ── WebSocket types ──────────────────────────────────────────────────

    /**
     * WS Index frame row: stride = 9 doubles.
     */
    public record WsIndex(
            double tsMs, double ticker, double mid,
            double bid, double ask, double ci,
            double confidence, double accepted, double rejected
    ) {}

    /**
     * WS Tick frame row: stride = 6 doubles.
     */
    public record WsTick(
            double tsMs, double ticker, double providerId,
            double bid, double ask, double accepted
    ) {}

    // ── WebSocket streaming ──────────────────────────────────────────────

    private static final int WS_HEADER_SIZE = 8;
    private static final int WS_TYPE_INDEX  = 1;
    private static final int WS_TYPE_TICK   = 2;
    private static final int INDEX_STRIDE   = 9;
    private static final int TICK_STRIDE    = 6;

    /**
     * Connect to the WebSocket endpoint and dispatch decoded frames.
     * This method blocks until the WebSocket is closed or an error occurs.
     *
     * @param onIndex callback for index frames (list of rows per frame)
     * @param onTick  callback for tick frames (list of rows per frame)
     */
    public void stream(Consumer<List<WsIndex>> onIndex, Consumer<List<WsTick>> onTick) throws Exception {
        var wsUrl = baseUrl.replaceFirst("^http", "ws") + "/v1/stream";
        var latch = new CountDownLatch(1);

        http.newWebSocketBuilder()
                .connectTimeout(Duration.ofSeconds(10))
                .buildAsync(URI.create(wsUrl), new WebSocket.Listener() {

                    private final ByteBuffer accumulator = ByteBuffer.allocate(1 << 20).order(ByteOrder.LITTLE_ENDIAN);

                    @Override
                    public void onOpen(WebSocket webSocket) {
                        webSocket.request(1);
                    }

                    @Override
                    public CompletionStage<?> onBinary(WebSocket webSocket, ByteBuffer data, boolean last) {
                        accumulator.put(data);
                        if (last) {
                            accumulator.flip();
                            try {
                                processFrame(accumulator);
                            } catch (Exception e) {
                                System.err.println("nxr-sdk: frame decode error: " + e.getMessage());
                            }
                            accumulator.clear();
                        }
                        webSocket.request(1);
                        return null;
                    }

                    private void processFrame(ByteBuffer buf) {
                        if (buf.remaining() < WS_HEADER_SIZE) return;

                        int type     = Byte.toUnsignedInt(buf.get());
                        buf.get(); // padding
                        int count    = Short.toUnsignedInt(buf.getShort());
                        buf.getInt(); // reserved

                        if (type == WS_TYPE_INDEX) {
                            var rows = new ArrayList<WsIndex>(count);
                            for (int i = 0; i < count; i++) {
                                rows.add(new WsIndex(
                                        buf.getDouble(), buf.getDouble(), buf.getDouble(),
                                        buf.getDouble(), buf.getDouble(), buf.getDouble(),
                                        buf.getDouble(), buf.getDouble(), buf.getDouble()
                                ));
                            }
                            onIndex.accept(rows);
                        } else if (type == WS_TYPE_TICK) {
                            var rows = new ArrayList<WsTick>(count);
                            for (int i = 0; i < count; i++) {
                                rows.add(new WsTick(
                                        buf.getDouble(), buf.getDouble(), buf.getDouble(),
                                        buf.getDouble(), buf.getDouble(), buf.getDouble()
                                ));
                            }
                            onTick.accept(rows);
                        }
                    }

                    @Override
                    public CompletionStage<?> onClose(WebSocket webSocket, int statusCode, String reason) {
                        latch.countDown();
                        return null;
                    }

                    @Override
                    public void onError(WebSocket webSocket, Throwable error) {
                        System.err.println("nxr-sdk: ws error: " + error.getMessage());
                        latch.countDown();
                    }
                })
                .join(); // wait for connection

        latch.await(); // block until closed
    }

    @Override
    public void close() {
        // HttpClient does not need explicit close on Java 17; included for AutoCloseable contract
    }
}
