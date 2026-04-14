/**
 * MITCH Protocol v2 — C99 header-only implementation.
 *
 * Wire format: all values little-endian.
 * Link against libmitch.so/dll for FFI functions, or use these structs directly.
 *
 * Build: include this header. No .c file needed.
 */

#ifndef MITCH_H
#define MITCH_H

#include <stdint.h>
#include <string.h>

#ifdef __cplusplus
extern "C" {
#endif

/* ── Size constants ───────────────────────────────────────────────────────── */

#define MITCH_SIZE_HEADER      16
#define MITCH_SIZE_TRADE       24
#define MITCH_SIZE_ORDER       32
#define MITCH_SIZE_TICK        32
#define MITCH_SIZE_INDEX       40
#define MITCH_SIZE_BAR        128
#define MITCH_SIZE_ORDER_BOOK 2072

/* ── Message type ASCII codes ─────────────────────────────────────────────── */

#define MITCH_MSG_TRADE      't'  /* 0x74 */
#define MITCH_MSG_ORDER      'o'  /* 0x6F */
#define MITCH_MSG_TICK       's'  /* 0x73 */
#define MITCH_MSG_INDEX      'i'  /* 0x69 */
#define MITCH_MSG_ORDER_BOOK 'b'  /* 0x62 */
#define MITCH_MSG_BAR        'k'  /* 0x6B */

/* ── 4-bit wire codes (low nibble of type_provider) ───────────────────────── */

#define MITCH_WIRE_TRADE      1
#define MITCH_WIRE_ORDER      2
#define MITCH_WIRE_TICK       3
#define MITCH_WIRE_INDEX      4
#define MITCH_WIRE_ORDER_BOOK 5
#define MITCH_WIRE_BAR        6

/* ── Timestamp ────────────────────────────────────────────────────────────── */

/** 2010-01-01T00:00:00Z in microseconds since Unix epoch. */
#define MITCH_EPOCH_2010_US 1262304000000000ULL

/** Encode Unix-epoch microseconds to u48 mts ticks. */
static inline uint64_t mitch_from_epoch_us(uint64_t us) {
    return (us - MITCH_EPOCH_2010_US) >> 4;
}

/** Decode u48 mts ticks to Unix-epoch microseconds. */
static inline uint64_t mitch_to_epoch_us(uint64_t ticks) {
    return (ticks << 4) + MITCH_EPOCH_2010_US;
}

/* ── Wire code ↔ ASCII mapping ────────────────────────────────────────────── */

static inline uint8_t mitch_wire_to_ascii(uint8_t code) {
    static const uint8_t tbl[] = {0, 't', 'o', 's', 'i', 'b', 'k'};
    return code <= 6 ? tbl[code] : 0;
}

static inline uint8_t mitch_ascii_to_wire(uint8_t ch) {
    switch (ch) {
        case 't': return 1; case 'o': return 2; case 's': return 3;
        case 'i': return 4; case 'b': return 5; case 'k': return 6;
        default:  return 0;
    }
}

/* ── MitchHeader v2 (16 bytes) ────────────────────────────────────────────── */

/**
 * Wire layout:
 *   [0..1]  type_provider : u16 LE — [3:0]=wire_code, [15:4]=provider_id
 *   [2..7]  timestamp     : u48 LE — 16µs ticks since 2010-01-01
 *   [8]     count         : u8     — batch entry count (1-255)
 *   [9]     flags         : u8     — [1:0]=version(0), [7:2]=reserved
 *   [10..11] sequence     : u16 LE — per-stream gap detection
 *   [12..15] _reserved    : 4 bytes
 */
#pragma pack(push, 1)
typedef struct {
    uint16_t type_provider;
    uint8_t  timestamp[6];
    uint8_t  count;
    uint8_t  flags;
    uint16_t sequence;
    uint8_t  _reserved[4];
} MitchHeader;
#pragma pack(pop)

_Static_assert(sizeof(MitchHeader) == 16, "MitchHeader must be 16 bytes");

static inline uint8_t mitch_header_msg_type(const MitchHeader *h) {
    return mitch_wire_to_ascii(h->type_provider & 0x0F);
}

static inline uint16_t mitch_header_provider_id(const MitchHeader *h) {
    return h->type_provider >> 4;
}

static inline uint64_t mitch_header_get_timestamp(const MitchHeader *h) {
    uint64_t v = 0;
    memcpy(&v, h->timestamp, 6);
    return v; /* LE on LE platforms; for BE, byte-swap */
}

static inline void mitch_header_set_timestamp(MitchHeader *h, uint64_t ts) {
    memcpy(h->timestamp, &ts, 6);
}

static inline void mitch_header_init(MitchHeader *h, uint8_t msg_type,
                                      uint16_t provider_id, uint64_t ts,
                                      uint8_t count) {
    uint8_t code = mitch_ascii_to_wire(msg_type);
    h->type_provider = (code & 0x0F) | (provider_id << 4);
    mitch_header_set_timestamp(h, ts);
    h->count = count;
    h->flags = 0;
    h->sequence = 0;
    memset(h->_reserved, 0, 4);
}

/* ── Tick (32 bytes) ──────────────────────────────────────────────────────── */

#pragma pack(push, 1)
typedef struct {
    uint64_t ticker;
    double   bid;
    double   ask;
    uint32_t vbid;
    uint32_t vask;
} MitchTick;
#pragma pack(pop)

_Static_assert(sizeof(MitchTick) == 32, "MitchTick must be 32 bytes");

/* ── Trade (24 bytes) ─────────────────────────────────────────────────────── */

#pragma pack(push, 1)
typedef struct {
    uint64_t ticker;
    double   price;
    uint32_t qty;
    uint8_t  trade_id[3]; /* u24 LE */
    uint8_t  side;        /* 0=Buy, 1=Sell */
} MitchTrade;
#pragma pack(pop)

_Static_assert(sizeof(MitchTrade) == 24, "MitchTrade must be 24 bytes");

static inline uint32_t mitch_trade_get_id(const MitchTrade *t) {
    return (uint32_t)t->trade_id[0]
         | ((uint32_t)t->trade_id[1] << 8)
         | ((uint32_t)t->trade_id[2] << 16);
}

static inline void mitch_trade_set_id(MitchTrade *t, uint32_t id) {
    t->trade_id[0] = (uint8_t)(id);
    t->trade_id[1] = (uint8_t)(id >> 8);
    t->trade_id[2] = (uint8_t)(id >> 16);
}

/* ── Order (32 bytes) ─────────────────────────────────────────────────────── */

#pragma pack(push, 1)
typedef struct {
    uint64_t ticker;
    uint32_t order_id;
    double   price;
    uint32_t qty;
    uint8_t  type_and_side; /* [7:1]=order_type, [0]=side */
    uint8_t  expiry[6];     /* u48 LE — ms since epoch */
    uint8_t  _pad;
} MitchOrder;
#pragma pack(pop)

_Static_assert(sizeof(MitchOrder) == 32, "MitchOrder must be 32 bytes");

/* ── Index (40 bytes) ─────────────────────────────────────────────────────── */

#pragma pack(push, 1)
typedef struct {
    uint64_t ticker;
    double   bid;
    double   ask;
    uint32_t vbid;
    uint32_t vask;
    uint16_t ci;         /* confidence interval, micro basis points */
    uint16_t tick_count;
    uint8_t  confidence; /* active provider weight sum */
    uint8_t  accepted;
    uint8_t  rejected;
    uint8_t  _pad;
} MitchIndex;
#pragma pack(pop)

_Static_assert(sizeof(MitchIndex) == 40, "MitchIndex must be 40 bytes");

/** Mid price: (bid + ask) / 2. */
static inline double mitch_index_mid(const MitchIndex *idx) {
    return (idx->bid + idx->ask) * 0.5;
}

/** Spread in basis points: (ask - bid) / mid * 10000. */
static inline double mitch_index_spread_bps(const MitchIndex *idx) {
    double m = mitch_index_mid(idx);
    return m > 0.0 ? (idx->ask - idx->bid) / m * 10000.0 : 0.0;
}

/** CI in price units: mid * ci / 1e8. */
static inline double mitch_index_ci_price(const MitchIndex *idx) {
    return mitch_index_mid(idx) * (double)idx->ci / 1e8;
}

#ifdef __cplusplus
}
#endif

#endif /* MITCH_H */
