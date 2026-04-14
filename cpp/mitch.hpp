/**
 * MITCH Protocol v2 — C++17 header-only implementation.
 *
 * Wire format: all values little-endian, packed structs.
 * Includes constexpr helpers and std::span-based unpack.
 */

#pragma once

#include <cstdint>
#include <cstring>
#include <array>
#include <span>
#include <stdexcept>

namespace mitch {

// ── Size constants ───────────────────────────────────────────────────────────

inline constexpr size_t SizeHeader    = 16;
inline constexpr size_t SizeTrade     = 24;
inline constexpr size_t SizeOrder     = 32;
inline constexpr size_t SizeTick      = 32;
inline constexpr size_t SizeIndex     = 40;
inline constexpr size_t SizeBar       = 128;
inline constexpr size_t SizeOrderBook = 2072;

// ── Message type ASCII codes ─────────────────────────────────────────────────

inline constexpr uint8_t MsgTrade     = 't';
inline constexpr uint8_t MsgOrder     = 'o';
inline constexpr uint8_t MsgTick      = 's';
inline constexpr uint8_t MsgIndex     = 'i';
inline constexpr uint8_t MsgOrderBook = 'b';
inline constexpr uint8_t MsgBar       = 'k';

// ── 4-bit wire codes ─────────────────────────────────────────────────────────

inline constexpr uint8_t WireTrade     = 1;
inline constexpr uint8_t WireOrder     = 2;
inline constexpr uint8_t WireTick      = 3;
inline constexpr uint8_t WireIndex     = 4;
inline constexpr uint8_t WireOrderBook = 5;
inline constexpr uint8_t WireBar       = 6;

// ── Timestamp ────────────────────────────────────────────────────────────────

inline constexpr uint64_t Epoch2010Us = 1262304000000000ULL;

constexpr uint64_t fromEpochUs(uint64_t us) { return (us - Epoch2010Us) >> 4; }
constexpr uint64_t toEpochUs(uint64_t ticks) { return (ticks << 4) + Epoch2010Us; }

// ── Wire code mapping ────────────────────────────────────────────────────────

constexpr uint8_t wireToAscii(uint8_t code) {
    constexpr uint8_t tbl[] = {0, 't', 'o', 's', 'i', 'b', 'k'};
    return code <= 6 ? tbl[code] : 0;
}

constexpr uint8_t asciiToWire(uint8_t ch) {
    switch (ch) {
        case 't': return 1; case 'o': return 2; case 's': return 3;
        case 'i': return 4; case 'b': return 5; case 'k': return 6;
        default:  return 0;
    }
}

// ── MitchHeader v2 (16 bytes) ────────────────────────────────────────────────

#pragma pack(push, 1)
struct Header {
    uint16_t type_provider;  // [3:0]=wire_code, [15:4]=provider_id
    uint8_t  timestamp[6];   // u48 LE: 16µs ticks since 2010-01-01
    uint8_t  count;          // batch entry count (1-255)
    uint8_t  flags;          // [1:0]=version(0), [7:2]=reserved
    uint16_t sequence;       // per-stream gap detection
    uint8_t  _reserved[4];

    [[nodiscard]] uint8_t  msgType()    const { return wireToAscii(type_provider & 0x0F); }
    [[nodiscard]] uint16_t providerId() const { return type_provider >> 4; }

    [[nodiscard]] uint64_t getTimestamp() const {
        uint64_t v = 0;
        std::memcpy(&v, timestamp, 6);
        return v;
    }

    void setTimestamp(uint64_t ts) { std::memcpy(timestamp, &ts, 6); }

    void init(uint8_t msg_type, uint16_t provider_id, uint64_t ts, uint8_t cnt) {
        type_provider = (asciiToWire(msg_type) & 0x0F) | (provider_id << 4);
        setTimestamp(ts);
        count = cnt;
        flags = 0;
        sequence = 0;
        std::memset(_reserved, 0, 4);
    }

    [[nodiscard]] auto pack() const -> std::array<uint8_t, SizeHeader> {
        std::array<uint8_t, SizeHeader> buf{};
        std::memcpy(buf.data(), this, SizeHeader);
        return buf;
    }

    static Header unpack(std::span<const uint8_t> data) {
        if (data.size() < SizeHeader) throw std::runtime_error("buffer too small for Header");
        Header h{};
        std::memcpy(&h, data.data(), SizeHeader);
        return h;
    }
};
#pragma pack(pop)

static_assert(sizeof(Header) == SizeHeader);

// ── Tick (32 bytes) ──────────────────────────────────────────────────────────

#pragma pack(push, 1)
struct Tick {
    uint64_t ticker;
    double   bid;
    double   ask;
    uint32_t vbid;
    uint32_t vask;

    [[nodiscard]] double mid()       const { return (bid + ask) * 0.5; }
    [[nodiscard]] double spread()    const { return ask - bid; }
    [[nodiscard]] double spreadBps() const { auto m = mid(); return m > 0 ? (ask - bid) / m * 1e4 : 0; }

    [[nodiscard]] auto pack() const -> std::array<uint8_t, SizeTick> {
        std::array<uint8_t, SizeTick> buf{};
        std::memcpy(buf.data(), this, SizeTick);
        return buf;
    }

    static Tick unpack(std::span<const uint8_t> data) {
        if (data.size() < SizeTick) throw std::runtime_error("buffer too small for Tick");
        Tick t{};
        std::memcpy(&t, data.data(), SizeTick);
        return t;
    }
};
#pragma pack(pop)

static_assert(sizeof(Tick) == SizeTick);

// ── Trade (24 bytes) ─────────────────────────────────────────────────────────

#pragma pack(push, 1)
struct Trade {
    uint64_t ticker;
    double   price;
    uint32_t qty;
    uint8_t  trade_id[3]; // u24 LE
    uint8_t  side;        // 0=Buy, 1=Sell

    [[nodiscard]] uint32_t getTradeId() const {
        return uint32_t(trade_id[0]) | (uint32_t(trade_id[1]) << 8) | (uint32_t(trade_id[2]) << 16);
    }

    void setTradeId(uint32_t id) {
        trade_id[0] = uint8_t(id);
        trade_id[1] = uint8_t(id >> 8);
        trade_id[2] = uint8_t(id >> 16);
    }

    [[nodiscard]] auto pack() const -> std::array<uint8_t, SizeTrade> {
        std::array<uint8_t, SizeTrade> buf{};
        std::memcpy(buf.data(), this, SizeTrade);
        return buf;
    }

    static Trade unpack(std::span<const uint8_t> data) {
        if (data.size() < SizeTrade) throw std::runtime_error("buffer too small for Trade");
        Trade t{};
        std::memcpy(&t, data.data(), SizeTrade);
        return t;
    }
};
#pragma pack(pop)

static_assert(sizeof(Trade) == SizeTrade);

// ── Order (32 bytes) ─────────────────────────────────────────────────────────

#pragma pack(push, 1)
struct Order {
    uint64_t ticker;
    uint32_t order_id;
    double   price;
    uint32_t qty;
    uint8_t  type_and_side; // [7:1]=order_type, [0]=side
    uint8_t  expiry[6];     // u48 LE ms since epoch
    uint8_t  _pad;

    [[nodiscard]] auto pack() const -> std::array<uint8_t, SizeOrder> {
        std::array<uint8_t, SizeOrder> buf{};
        std::memcpy(buf.data(), this, SizeOrder);
        return buf;
    }

    static Order unpack(std::span<const uint8_t> data) {
        if (data.size() < SizeOrder) throw std::runtime_error("buffer too small for Order");
        Order o{};
        std::memcpy(&o, data.data(), SizeOrder);
        return o;
    }
};
#pragma pack(pop)

static_assert(sizeof(Order) == SizeOrder);

// ── Index (40 bytes) ─────────────────────────────────────────────────────────

#pragma pack(push, 1)
struct Index {
    uint64_t ticker;
    double   bid;
    double   ask;
    uint32_t vbid;
    uint32_t vask;
    uint16_t ci;          // confidence interval, micro basis points
    uint16_t tick_count;
    uint8_t  confidence;  // active provider weight sum
    uint8_t  accepted;
    uint8_t  rejected;
    uint8_t  _pad;

    [[nodiscard]] double mid()       const { return (bid + ask) * 0.5; }
    [[nodiscard]] double spread()    const { return ask - bid; }
    [[nodiscard]] double spreadBps() const { auto m = mid(); return m > 0 ? (ask - bid) / m * 1e4 : 0; }
    [[nodiscard]] double ciPrice()   const { return mid() * double(ci) / 1e8; }

    [[nodiscard]] auto pack() const -> std::array<uint8_t, SizeIndex> {
        std::array<uint8_t, SizeIndex> buf{};
        std::memcpy(buf.data(), this, SizeIndex);
        return buf;
    }

    static Index unpack(std::span<const uint8_t> data) {
        if (data.size() < SizeIndex) throw std::runtime_error("buffer too small for Index");
        Index idx{};
        std::memcpy(&idx, data.data(), SizeIndex);
        return idx;
    }
};
#pragma pack(pop)

static_assert(sizeof(Index) == SizeIndex);

} // namespace mitch
