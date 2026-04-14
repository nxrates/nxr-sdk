/// MITCH Protocol v2 — Zig 0.13+ implementation.
///
/// Wire format: all values little-endian, packed structs.
/// Zero-cost abstractions: extern structs map directly to wire layout.
const std = @import("std");
const mem = std.mem;
const testing = std.testing;

// ── Size constants ──────────────────────────────────────────────────────────

pub const size_header: usize = 16;
pub const size_trade: usize = 24;
pub const size_order: usize = 32;
pub const size_tick: usize = 32;
pub const size_index: usize = 40;
pub const size_bar: usize = 128;
pub const size_order_book: usize = 2072;

// ── Message type ASCII codes ────────────────────────────────────────────────

pub const msg_trade: u8 = 't';
pub const msg_order: u8 = 'o';
pub const msg_tick: u8 = 's';
pub const msg_index: u8 = 'i';
pub const msg_order_book: u8 = 'b';
pub const msg_bar: u8 = 'k';

// ── 4-bit wire codes ────────────────────────────────────────────────────────

pub const wire_trade: u8 = 1;
pub const wire_order: u8 = 2;
pub const wire_tick: u8 = 3;
pub const wire_index: u8 = 4;
pub const wire_order_book: u8 = 5;
pub const wire_bar: u8 = 6;

// ── Timestamp ───────────────────────────────────────────────────────────────

pub const epoch_2010_us: u64 = 1_262_304_000_000_000;

pub fn fromEpochUs(us: u64) u64 {
    return (us - epoch_2010_us) >> 4;
}

pub fn toEpochUs(ticks: u64) u64 {
    return (ticks << 4) + epoch_2010_us;
}

// ── Wire code mapping ───────────────────────────────────────────────────────

pub fn wireToAscii(code: u8) u8 {
    const tbl = [_]u8{ 0, 't', 'o', 's', 'i', 'b', 'k' };
    return if (code <= 6) tbl[code] else 0;
}

pub fn asciiToWire(ch: u8) u8 {
    return switch (ch) {
        't' => 1,
        'o' => 2,
        's' => 3,
        'i' => 4,
        'b' => 5,
        'k' => 6,
        else => 0,
    };
}

// ── MitchHeader v2 (16 bytes) ───────────────────────────────────────────────

/// Wire layout:
///   [0..1]   type_provider : u16 LE — [3:0]=wire_code, [15:4]=provider_id
///   [2..7]   timestamp     : u48 LE — 16µs ticks since 2010-01-01
///   [8]      count         : u8     — batch entry count (1-255)
///   [9]      flags         : u8     — [1:0]=version(0), [7:2]=reserved
///   [10..11] sequence      : u16 LE — per-stream gap detection
///   [12..15] _reserved     : 4 bytes
pub const Header = extern struct {
    type_provider: u16 align(1),
    timestamp: [6]u8,
    count: u8,
    flags: u8,
    sequence: u16 align(1),
    _reserved: [4]u8,

    pub fn msgType(self: Header) u8 {
        return wireToAscii(@as(u8, @truncate(self.type_provider & 0x0F)));
    }

    pub fn providerId(self: Header) u16 {
        return self.type_provider >> 4;
    }

    pub fn getTimestamp(self: Header) u64 {
        var buf: [8]u8 = .{ 0, 0, 0, 0, 0, 0, 0, 0 };
        @memcpy(buf[0..6], &self.timestamp);
        return mem.readInt(u64, &buf, .little);
    }

    pub fn setTimestamp(self: *Header, ts: u64) void {
        const bytes = mem.toBytes(mem.nativeToLittle(u64, ts));
        @memcpy(&self.timestamp, bytes[0..6]);
    }

    pub fn init(msg_type: u8, provider_id: u16, ts: u64, cnt: u8) Header {
        var h: Header = undefined;
        h.type_provider = (asciiToWire(msg_type) & 0x0F) | (provider_id << 4);
        h.setTimestamp(ts);
        h.count = cnt;
        h.flags = 0;
        h.sequence = 0;
        h._reserved = .{ 0, 0, 0, 0 };
        return h;
    }

    pub fn pack(self: Header) [size_header]u8 {
        return @as(*const [size_header]u8, @ptrCast(&self)).*;
    }

    pub fn unpack(data: []const u8) !Header {
        if (data.len < size_header) return error.BufferTooSmall;
        return @as(*const Header, @ptrCast(@alignCast(data.ptr))).*;
    }
};

comptime {
    if (@sizeOf(Header) != size_header) @compileError("Header must be 16 bytes");
}

// ── Tick (32 bytes) ─────────────────────────────────────────────────────────

pub const Tick = extern struct {
    ticker: u64 align(1),
    bid: f64 align(1),
    ask: f64 align(1),
    vbid: u32 align(1),
    vask: u32 align(1),

    pub fn mid(self: Tick) f64 {
        return (self.bid + self.ask) * 0.5;
    }

    pub fn spread(self: Tick) f64 {
        return self.ask - self.bid;
    }

    pub fn pack(self: Tick) [size_tick]u8 {
        return @as(*const [size_tick]u8, @ptrCast(&self)).*;
    }

    pub fn unpack(data: []const u8) !Tick {
        if (data.len < size_tick) return error.BufferTooSmall;
        return @as(*const Tick, @ptrCast(@alignCast(data.ptr))).*;
    }
};

comptime {
    if (@sizeOf(Tick) != size_tick) @compileError("Tick must be 32 bytes");
}

// ── Trade (24 bytes) ────────────────────────────────────────────────────────

pub const Trade = extern struct {
    ticker: u64 align(1),
    price: f64 align(1),
    qty: u32 align(1),
    trade_id: [3]u8, // u24 LE
    side: u8, // 0=Buy, 1=Sell

    pub fn getTradeId(self: Trade) u32 {
        return @as(u32, self.trade_id[0]) |
            (@as(u32, self.trade_id[1]) << 8) |
            (@as(u32, self.trade_id[2]) << 16);
    }

    pub fn pack(self: Trade) [size_trade]u8 {
        return @as(*const [size_trade]u8, @ptrCast(&self)).*;
    }

    pub fn unpack(data: []const u8) !Trade {
        if (data.len < size_trade) return error.BufferTooSmall;
        return @as(*const Trade, @ptrCast(@alignCast(data.ptr))).*;
    }
};

comptime {
    if (@sizeOf(Trade) != size_trade) @compileError("Trade must be 24 bytes");
}

// ── Index (40 bytes) ────────────────────────────────────────────────────────

pub const Index = extern struct {
    ticker: u64 align(1),
    bid: f64 align(1),
    ask: f64 align(1),
    vbid: u32 align(1),
    vask: u32 align(1),
    ci: u16 align(1), // micro basis points
    tick_count: u16 align(1),
    confidence: u8,
    accepted: u8,
    rejected: u8,
    _pad: u8,

    pub fn mid(self: Index) f64 {
        return (self.bid + self.ask) * 0.5;
    }

    pub fn spreadBps(self: Index) f64 {
        const m = self.mid();
        return if (m > 0) (self.ask - self.bid) / m * 1e4 else 0;
    }

    pub fn ciPrice(self: Index) f64 {
        return self.mid() * @as(f64, @floatFromInt(self.ci)) / 1e8;
    }

    pub fn pack(self: Index) [size_index]u8 {
        return @as(*const [size_index]u8, @ptrCast(&self)).*;
    }

    pub fn unpack(data: []const u8) !Index {
        if (data.len < size_index) return error.BufferTooSmall;
        return @as(*const Index, @ptrCast(@alignCast(data.ptr))).*;
    }
};

comptime {
    if (@sizeOf(Index) != size_index) @compileError("Index must be 40 bytes");
}

// ── Tests ───────────────────────────────────────────────────────────────────

test "header roundtrip" {
    const h = Header.init('i', 101, 12345, 5);
    try testing.expectEqual(h.msgType(), 'i');
    try testing.expectEqual(h.providerId(), 101);
    try testing.expectEqual(h.getTimestamp(), 12345);
    try testing.expectEqual(h.count, 5);

    const packed = h.pack();
    const h2 = try Header.unpack(&packed);
    try testing.expectEqual(h2.msgType(), 'i');
    try testing.expectEqual(h2.providerId(), 101);
}

test "wire code mapping" {
    try testing.expectEqual(wireToAscii(1), 't');
    try testing.expectEqual(wireToAscii(4), 'i');
    try testing.expectEqual(asciiToWire('s'), 3);
    try testing.expectEqual(asciiToWire('k'), 6);
}

test "timestamp encode decode" {
    const us: u64 = 1_700_000_000_000_000;
    const ticks = fromEpochUs(us);
    const back = toEpochUs(ticks);
    // Within 16µs resolution
    try testing.expect(back >= us - 16 and back <= us + 16);
}
