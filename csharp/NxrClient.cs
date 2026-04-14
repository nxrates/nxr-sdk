using System.Buffers.Binary;
using System.Net.Http.Json;
using System.Net.WebSockets;

namespace NxRates.Sdk;

// ---------------------------------------------------------------------------
//  REST response models
// ---------------------------------------------------------------------------

public readonly record struct TickerSnapshot(
    string Symbol,
    ulong  Ticker,
    double Bid,
    double Ask,
    double Mid,
    long   TimestampMs
);

// ---------------------------------------------------------------------------
//  WebSocket record structs (stride-aligned doubles)
// ---------------------------------------------------------------------------

/// <summary>Index WS frame row -- stride 9 doubles.</summary>
public readonly record struct WsIndex(
    double TsMs,
    double Ticker,
    double Mid,
    double Bid,
    double Ask,
    double CI,
    double Confidence,
    double Accepted,
    double Rejected
)
{
    public const int Stride = 9; // doubles per row
}

/// <summary>Tick WS frame row -- stride 6 doubles.</summary>
public readonly record struct WsTick(
    double TsMs,
    double Ticker,
    double ProviderId,
    double Bid,
    double Ask,
    double Accepted
)
{
    public const int Stride = 6; // doubles per row
}

// ---------------------------------------------------------------------------
//  NxrClient
// ---------------------------------------------------------------------------

public sealed class NxrClient : IDisposable
{
    private const int WsHeaderSize = 8;

    private readonly HttpClient _http;
    private readonly string _baseUrl;
    private readonly bool _ownsHttp;

    public NxrClient(string baseUrl) : this(baseUrl, new HttpClient(), ownsHttp: true) { }

    public NxrClient(string baseUrl, HttpClient http) : this(baseUrl, http, ownsHttp: false) { }

    private NxrClient(string baseUrl, HttpClient http, bool ownsHttp)
    {
        _baseUrl = baseUrl.TrimEnd('/');
        _http = http;
        _http.BaseAddress ??= new Uri(_baseUrl);
        _ownsHttp = ownsHttp;
    }

    // -----------------------------------------------------------------------
    //  REST
    // -----------------------------------------------------------------------

    /// <summary>GET /v1/symbols -- symbol name to ticker id mapping.</summary>
    public async Task<Dictionary<string, ulong>> SymbolsAsync(CancellationToken ct = default)
    {
        var result = await _http.GetFromJsonAsync<Dictionary<string, ulong>>(
            $"{_baseUrl}/v1/symbols", ct).ConfigureAwait(false);
        return result ?? new Dictionary<string, ulong>();
    }

    /// <summary>GET /v1/providers -- provider id to name mapping.</summary>
    public async Task<Dictionary<ushort, string>> ProvidersAsync(CancellationToken ct = default)
    {
        // JSON keys are strings; deserialize then convert.
        var raw = await _http.GetFromJsonAsync<Dictionary<string, string>>(
            $"{_baseUrl}/v1/providers", ct).ConfigureAwait(false);
        var dict = new Dictionary<ushort, string>();
        if (raw is not null)
        {
            foreach (var (k, v) in raw)
            {
                if (ushort.TryParse(k, out ushort id))
                    dict[id] = v;
            }
        }
        return dict;
    }

    /// <summary>GET /v1/tickers -- current ticker snapshots.</summary>
    public async Task<TickerSnapshot[]> TickersAsync(CancellationToken ct = default)
    {
        var result = await _http.GetFromJsonAsync<TickerSnapshot[]>(
            $"{_baseUrl}/v1/tickers", ct).ConfigureAwait(false);
        return result ?? [];
    }

    /// <summary>GET /health -- returns true if the service is healthy.</summary>
    public async Task<bool> IsHealthyAsync(CancellationToken ct = default)
    {
        try
        {
            using var resp = await _http.GetAsync($"{_baseUrl}/health", ct).ConfigureAwait(false);
            return resp.IsSuccessStatusCode;
        }
        catch
        {
            return false;
        }
    }

    // -----------------------------------------------------------------------
    //  WebSocket streaming
    // -----------------------------------------------------------------------

    /// <summary>
    /// Connect to the WebSocket endpoint and dispatch decoded frames until
    /// cancellation is requested or the server closes the connection.
    /// </summary>
    public async Task StreamAsync(
        Action<WsIndex[]>? onIndex,
        Action<WsTick[]>? onTick,
        CancellationToken ct = default)
    {
        using var ws = new ClientWebSocket();

        var wsUrl = _baseUrl
            .Replace("http://", "ws://")
            .Replace("https://", "wss://")
            + "/v1/stream";

        await ws.ConnectAsync(new Uri(wsUrl), ct).ConfigureAwait(false);

        // 64 KB receive buffer -- large enough for typical frames.
        var buf = new byte[65_536];

        while (ws.State == WebSocketState.Open && !ct.IsCancellationRequested)
        {
            int received = 0;
            WebSocketReceiveResult result;

            // Read full message (may arrive in fragments).
            do
            {
                result = await ws.ReceiveAsync(
                    new ArraySegment<byte>(buf, received, buf.Length - received), ct)
                    .ConfigureAwait(false);
                received += result.Count;

                // Grow buffer if needed.
                if (received == buf.Length && !result.EndOfMessage)
                {
                    Array.Resize(ref buf, buf.Length * 2);
                }
            }
            while (!result.EndOfMessage);

            if (result.MessageType == WebSocketMessageType.Close)
                break;

            if (result.MessageType != WebSocketMessageType.Binary || received < WsHeaderSize)
                continue;

            ReadOnlySpan<byte> frame = buf.AsSpan(0, received);
            ParseFrame(frame, onIndex, onTick);
        }

        if (ws.State == WebSocketState.Open)
        {
            await ws.CloseAsync(WebSocketCloseStatus.NormalClosure, null, CancellationToken.None)
                .ConfigureAwait(false);
        }
    }

    // -----------------------------------------------------------------------
    //  Frame parser
    // -----------------------------------------------------------------------

    private static void ParseFrame(
        ReadOnlySpan<byte> frame,
        Action<WsIndex[]>? onIndex,
        Action<WsTick[]>? onTick)
    {
        byte type = frame[0];
        // byte padding = frame[1];
        ushort count = BinaryPrimitives.ReadUInt16LittleEndian(frame.Slice(2));
        // 4 bytes reserved @4

        ReadOnlySpan<byte> body = frame.Slice(WsHeaderSize);

        switch (type)
        {
            case 1 when onIndex is not null:
            {
                int stride = WsIndex.Stride * sizeof(double); // 72 bytes
                var items = new WsIndex[count];
                for (int i = 0; i < count; i++)
                {
                    var row = body.Slice(i * stride);
                    items[i] = new WsIndex(
                        TsMs:       BinaryPrimitives.ReadDoubleLittleEndian(row),
                        Ticker:     BinaryPrimitives.ReadDoubleLittleEndian(row.Slice(8)),
                        Mid:        BinaryPrimitives.ReadDoubleLittleEndian(row.Slice(16)),
                        Bid:        BinaryPrimitives.ReadDoubleLittleEndian(row.Slice(24)),
                        Ask:        BinaryPrimitives.ReadDoubleLittleEndian(row.Slice(32)),
                        CI:         BinaryPrimitives.ReadDoubleLittleEndian(row.Slice(40)),
                        Confidence: BinaryPrimitives.ReadDoubleLittleEndian(row.Slice(48)),
                        Accepted:   BinaryPrimitives.ReadDoubleLittleEndian(row.Slice(56)),
                        Rejected:   BinaryPrimitives.ReadDoubleLittleEndian(row.Slice(64))
                    );
                }
                onIndex(items);
                break;
            }

            case 2 when onTick is not null:
            {
                int stride = WsTick.Stride * sizeof(double); // 48 bytes
                var items = new WsTick[count];
                for (int i = 0; i < count; i++)
                {
                    var row = body.Slice(i * stride);
                    items[i] = new WsTick(
                        TsMs:       BinaryPrimitives.ReadDoubleLittleEndian(row),
                        Ticker:     BinaryPrimitives.ReadDoubleLittleEndian(row.Slice(8)),
                        ProviderId: BinaryPrimitives.ReadDoubleLittleEndian(row.Slice(16)),
                        Bid:        BinaryPrimitives.ReadDoubleLittleEndian(row.Slice(24)),
                        Ask:        BinaryPrimitives.ReadDoubleLittleEndian(row.Slice(32)),
                        Accepted:   BinaryPrimitives.ReadDoubleLittleEndian(row.Slice(40))
                    );
                }
                onTick(items);
                break;
            }
        }
    }

    // -----------------------------------------------------------------------
    //  IDisposable
    // -----------------------------------------------------------------------

    public void Dispose()
    {
        if (_ownsHttp)
            _http.Dispose();
    }
}
