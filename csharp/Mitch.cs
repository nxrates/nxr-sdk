using System.Buffers.Binary;
using System.Runtime.InteropServices;

namespace NxRates.Sdk;

// ---------------------------------------------------------------------------
//  Constants
// ---------------------------------------------------------------------------

public static class MitchConstants
{
    public const int HeaderSize = 16;
    public const int TickSize = 32;
    public const int TradeSize = 24;
    public const int IndexSize = 40;

    // ASCII message-type bytes
    public const byte MsgTrade     = (byte)'t';
    public const byte MsgOrder     = (byte)'o';
    public const byte MsgTick      = (byte)'s';
    public const byte MsgIndex     = (byte)'i';
    public const byte MsgOrderBook = (byte)'b';
    public const byte MsgBar       = (byte)'k';

    // Wire code <-> ASCII lookup
    private static readonly byte[] WireToAscii = { 0, MsgTrade, MsgOrder, MsgTick, MsgIndex, MsgOrderBook, MsgBar };
    private static readonly byte[] AsciiToWire = new byte[256];

    static MitchConstants()
    {
        for (int i = 1; i < WireToAscii.Length; i++)
            AsciiToWire[WireToAscii[i]] = (byte)i;
    }

    public static byte WireCodeToAscii(int wireCode) =>
        wireCode >= 1 && wireCode < WireToAscii.Length ? WireToAscii[wireCode] : (byte)0;

    public static byte AsciiToWireCode(byte ascii) => AsciiToWire[ascii];
}

// ---------------------------------------------------------------------------
//  Timestamp helpers
// ---------------------------------------------------------------------------

public static class MitchTimestamp
{
    /// <summary>Microseconds from Unix epoch to 2010-01-01T00:00:00Z.</summary>
    public const long Epoch2010Us = 1_262_304_000_000_000L;

    private const long TicksPerMicrosecond = 16;

    /// <summary>Decode a 48-bit LE tick count to a DateTimeOffset (UTC).</summary>
    public static DateTimeOffset FromEpochUs(long ticks16us)
    {
        // Each tick = 16 us. Total microseconds since 2010 = ticks * 16.
        long us = ticks16us * TicksPerMicrosecond;
        long unixUs = us + Epoch2010Us;
        long dotnetTicks = unixUs * 10; // 1 us = 10 .NET ticks (100ns each)
        return new DateTimeOffset(dotnetTicks + DateTimeOffset.UnixEpoch.Ticks, TimeSpan.Zero);
    }

    /// <summary>Encode a DateTimeOffset to a 48-bit tick count (16 us resolution).</summary>
    public static long ToEpochUs(DateTimeOffset dt)
    {
        long dotnetTicks = dt.UtcTicks - DateTimeOffset.UnixEpoch.Ticks;
        long unixUs = dotnetTicks / 10;
        long usSince2010 = unixUs - Epoch2010Us;
        return usSince2010 / TicksPerMicrosecond;
    }

    internal static long ReadU48LE(ReadOnlySpan<byte> src)
    {
        // Read 6 bytes little-endian into a long.
        long v = src[0]
               | ((long)src[1] << 8)
               | ((long)src[2] << 16)
               | ((long)src[3] << 24)
               | ((long)src[4] << 32)
               | ((long)src[5] << 40);
        return v;
    }

    internal static void WriteU48LE(Span<byte> dst, long value)
    {
        dst[0] = (byte)(value);
        dst[1] = (byte)(value >> 8);
        dst[2] = (byte)(value >> 16);
        dst[3] = (byte)(value >> 24);
        dst[4] = (byte)(value >> 32);
        dst[5] = (byte)(value >> 40);
    }
}

// ---------------------------------------------------------------------------
//  MitchHeader (16 bytes)
// ---------------------------------------------------------------------------

[StructLayout(LayoutKind.Sequential, Pack = 1)]
public struct MitchHeader
{
    public const int Size = MitchConstants.HeaderSize;

    public ushort TypeProvider;  // [3:0]=wire code, [15:4]=provider_id
    public long   TimestampRaw; // only low 48 bits used on wire
    public byte   Count;
    public byte   Flags;
    public ushort Sequence;
    public uint   Reserved;

    // Decoded properties -------------------------------------------------------

    /// <summary>ASCII message type ('t','o','s','i','b','k').</summary>
    public readonly byte MsgType => MitchConstants.WireCodeToAscii(TypeProvider & 0x0F);

    /// <summary>Provider id (0-4095).</summary>
    public readonly ushort ProviderId => (ushort)(TypeProvider >> 4);

    /// <summary>Decoded UTC timestamp.</summary>
    public readonly DateTimeOffset Timestamp => MitchTimestamp.FromEpochUs(TimestampRaw);

    // Wire I/O -----------------------------------------------------------------

    public readonly void WriteTo(Span<byte> dst)
    {
        BinaryPrimitives.WriteUInt16LittleEndian(dst, TypeProvider);
        MitchTimestamp.WriteU48LE(dst.Slice(2), TimestampRaw);
        dst[8] = Count;
        dst[9] = Flags;
        BinaryPrimitives.WriteUInt16LittleEndian(dst.Slice(10), Sequence);
        BinaryPrimitives.WriteUInt32LittleEndian(dst.Slice(12), Reserved);
    }

    public static MitchHeader ReadFrom(ReadOnlySpan<byte> src)
    {
        return new MitchHeader
        {
            TypeProvider = BinaryPrimitives.ReadUInt16LittleEndian(src),
            TimestampRaw = MitchTimestamp.ReadU48LE(src.Slice(2)),
            Count        = src[8],
            Flags        = src[9],
            Sequence     = BinaryPrimitives.ReadUInt16LittleEndian(src.Slice(10)),
            Reserved     = BinaryPrimitives.ReadUInt32LittleEndian(src.Slice(12)),
        };
    }

    // Factory helpers -----------------------------------------------------------

    public static ushort PackTypeProvider(byte asciiType, ushort providerId) =>
        (ushort)((providerId << 4) | MitchConstants.AsciiToWireCode(asciiType));
}

// ---------------------------------------------------------------------------
//  Tick (32 bytes)
// ---------------------------------------------------------------------------

[StructLayout(LayoutKind.Sequential, Pack = 1)]
public struct MitchTick
{
    public const int Size = MitchConstants.TickSize;

    public ulong  Ticker;  // @0
    public double Bid;     // @8
    public double Ask;     // @16
    public uint   VBid;    // @24
    public uint   VAsk;    // @28

    public readonly void WriteTo(Span<byte> dst)
    {
        BinaryPrimitives.WriteUInt64LittleEndian(dst, Ticker);
        BinaryPrimitives.WriteDoubleLittleEndian(dst.Slice(8), Bid);
        BinaryPrimitives.WriteDoubleLittleEndian(dst.Slice(16), Ask);
        BinaryPrimitives.WriteUInt32LittleEndian(dst.Slice(24), VBid);
        BinaryPrimitives.WriteUInt32LittleEndian(dst.Slice(28), VAsk);
    }

    public static MitchTick ReadFrom(ReadOnlySpan<byte> src) => new()
    {
        Ticker = BinaryPrimitives.ReadUInt64LittleEndian(src),
        Bid    = BinaryPrimitives.ReadDoubleLittleEndian(src.Slice(8)),
        Ask    = BinaryPrimitives.ReadDoubleLittleEndian(src.Slice(16)),
        VBid   = BinaryPrimitives.ReadUInt32LittleEndian(src.Slice(24)),
        VAsk   = BinaryPrimitives.ReadUInt32LittleEndian(src.Slice(28)),
    };
}

// ---------------------------------------------------------------------------
//  Trade (24 bytes)
// ---------------------------------------------------------------------------

[StructLayout(LayoutKind.Sequential, Pack = 1)]
public struct MitchTrade
{
    public const int Size = MitchConstants.TradeSize;

    public ulong  Ticker;   // @0
    public double Price;    // @8
    public uint   Qty;      // @16
    // TradeId is u24 LE @20 (3 bytes), Side is u8 @23
    public uint   TradeIdAndSide; // packing helper -- low 24 bits = TradeId, byte 3 = Side

    public uint TradeId
    {
        readonly get => TradeIdAndSide & 0x00FF_FFFF;
        set => TradeIdAndSide = (TradeIdAndSide & 0xFF00_0000) | (value & 0x00FF_FFFF);
    }

    public byte Side
    {
        readonly get => (byte)(TradeIdAndSide >> 24);
        set => TradeIdAndSide = (TradeIdAndSide & 0x00FF_FFFF) | ((uint)value << 24);
    }

    public readonly void WriteTo(Span<byte> dst)
    {
        BinaryPrimitives.WriteUInt64LittleEndian(dst, Ticker);
        BinaryPrimitives.WriteDoubleLittleEndian(dst.Slice(8), Price);
        BinaryPrimitives.WriteUInt32LittleEndian(dst.Slice(16), Qty);
        // TradeId: 3 bytes LE @20
        uint tid = TradeId;
        dst[20] = (byte)(tid);
        dst[21] = (byte)(tid >> 8);
        dst[22] = (byte)(tid >> 16);
        dst[23] = Side;
    }

    public static MitchTrade ReadFrom(ReadOnlySpan<byte> src)
    {
        uint tid = (uint)(src[20] | (src[21] << 8) | (src[22] << 16));
        byte side = src[23];
        return new MitchTrade
        {
            Ticker         = BinaryPrimitives.ReadUInt64LittleEndian(src),
            Price          = BinaryPrimitives.ReadDoubleLittleEndian(src.Slice(8)),
            Qty            = BinaryPrimitives.ReadUInt32LittleEndian(src.Slice(16)),
            TradeIdAndSide = tid | ((uint)side << 24),
        };
    }
}

// ---------------------------------------------------------------------------
//  Index (40 bytes)
// ---------------------------------------------------------------------------

[StructLayout(LayoutKind.Sequential, Pack = 1)]
public struct MitchIndex
{
    public const int Size = MitchConstants.IndexSize;

    public ulong  Ticker;     // @0
    public double Bid;        // @8
    public double Ask;        // @16
    public uint   VBid;       // @24
    public uint   VAsk;       // @28
    public ushort CI;         // @32
    public ushort TickCount;  // @34
    public byte   Confidence; // @36
    public byte   Accepted;   // @37
    public byte   Rejected;   // @38
    public byte   Pad;        // @39

    public readonly void WriteTo(Span<byte> dst)
    {
        BinaryPrimitives.WriteUInt64LittleEndian(dst, Ticker);
        BinaryPrimitives.WriteDoubleLittleEndian(dst.Slice(8), Bid);
        BinaryPrimitives.WriteDoubleLittleEndian(dst.Slice(16), Ask);
        BinaryPrimitives.WriteUInt32LittleEndian(dst.Slice(24), VBid);
        BinaryPrimitives.WriteUInt32LittleEndian(dst.Slice(28), VAsk);
        BinaryPrimitives.WriteUInt16LittleEndian(dst.Slice(32), CI);
        BinaryPrimitives.WriteUInt16LittleEndian(dst.Slice(34), TickCount);
        dst[36] = Confidence;
        dst[37] = Accepted;
        dst[38] = Rejected;
        dst[39] = Pad;
    }

    public static MitchIndex ReadFrom(ReadOnlySpan<byte> src) => new()
    {
        Ticker     = BinaryPrimitives.ReadUInt64LittleEndian(src),
        Bid        = BinaryPrimitives.ReadDoubleLittleEndian(src.Slice(8)),
        Ask        = BinaryPrimitives.ReadDoubleLittleEndian(src.Slice(16)),
        VBid       = BinaryPrimitives.ReadUInt32LittleEndian(src.Slice(24)),
        VAsk       = BinaryPrimitives.ReadUInt32LittleEndian(src.Slice(28)),
        CI         = BinaryPrimitives.ReadUInt16LittleEndian(src.Slice(32)),
        TickCount  = BinaryPrimitives.ReadUInt16LittleEndian(src.Slice(34)),
        Confidence = src[36],
        Accepted   = src[37],
        Rejected   = src[38],
        Pad        = src[39],
    };
}
