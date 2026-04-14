package nxr

import (
	"encoding/binary"
	"errors"
	"math"
	"time"
)

// Size constants for MITCH message types.
const (
	SizeHeader    = 16
	SizeTrade     = 24
	SizeOrder     = 32
	SizeTick      = 32
	SizeIndex     = 40
	SizeBar       = 128
	SizeOrderBook = 2072
)

// Message type ASCII codes.
const (
	MsgTrade     = 't'
	MsgOrder     = 'o'
	MsgTick      = 's'
	MsgIndex     = 'i'
	MsgBar       = 'k'
	MsgOrderBook = 'b'
)

// Wire codes for the type_provider field.
const (
	wireCodeTrade     = 1
	wireCodeOrder     = 2
	wireCodeTick      = 3
	wireCodeIndex     = 4
	wireCodeOrderBook = 5
	wireCodeBar       = 6
)

// mitchEpochUs is microseconds from Unix epoch to 2010-01-01T00:00:00Z.
const mitchEpochUs = 1262304000000000

// wireToASCII maps wire codes to ASCII message type bytes.
var wireToASCII = map[uint8]byte{
	wireCodeTrade:     MsgTrade,
	wireCodeOrder:     MsgOrder,
	wireCodeTick:      MsgTick,
	wireCodeIndex:     MsgIndex,
	wireCodeOrderBook: MsgOrderBook,
	wireCodeBar:       MsgBar,
}

// asciiToWire maps ASCII message type bytes to wire codes.
var asciiToWire = map[byte]uint8{
	MsgTrade:     wireCodeTrade,
	MsgOrder:     wireCodeOrder,
	MsgTick:      wireCodeTick,
	MsgIndex:     wireCodeIndex,
	MsgOrderBook: wireCodeOrderBook,
	MsgBar:       wireCodeBar,
}

// FromEpochUs converts a MITCH u48 timestamp (16us ticks since 2010-01-01) to time.Time.
func FromEpochUs(ticks uint64) time.Time {
	us := int64(ticks)*16 + mitchEpochUs
	return time.UnixMicro(us).UTC()
}

// ToEpochUs converts a time.Time to a MITCH u48 timestamp (16us ticks since 2010-01-01).
func ToEpochUs(t time.Time) uint64 {
	us := t.UnixMicro() - mitchEpochUs
	return uint64(us) / 16
}

// MitchHeader is the 16-byte MITCH v2 header.
type MitchHeader struct {
	TypeProvider uint16   // [3:0]=msg_type wire code, [15:4]=provider_id
	Timestamp    uint64   // u48: 16us ticks since 2010-01-01
	Count        uint8    // batch entry count
	Flags        uint8    // [1:0]=version, [7:2]=reserved
	Sequence     uint16   // per-stream gap detection
	Reserved     [4]byte  // reserved
}

// MsgType returns the ASCII message type code (e.g. 't', 's', 'i').
func (h *MitchHeader) MsgType() byte {
	wire := uint8(h.TypeProvider & 0x0F)
	if c, ok := wireToASCII[wire]; ok {
		return c
	}
	return 0
}

// ProviderID returns the provider ID from the type_provider field (bits 4-15).
func (h *MitchHeader) ProviderID() uint16 {
	return h.TypeProvider >> 4
}

// SetTypeProvider sets the type_provider field from a wire code and provider ID.
func (h *MitchHeader) SetTypeProvider(wireCode uint8, providerID uint16) {
	h.TypeProvider = uint16(wireCode&0x0F) | (providerID << 4)
}

// Pack serializes the header into a 16-byte little-endian slice.
func (h *MitchHeader) Pack() []byte {
	buf := make([]byte, SizeHeader)
	binary.LittleEndian.PutUint16(buf[0:2], h.TypeProvider)
	// timestamp is u48: write 6 bytes LE
	for i := 0; i < 6; i++ {
		buf[2+i] = byte(h.Timestamp >> (8 * i))
	}
	buf[8] = h.Count
	buf[9] = h.Flags
	binary.LittleEndian.PutUint16(buf[10:12], h.Sequence)
	copy(buf[12:16], h.Reserved[:])
	return buf
}

// Unpack deserializes a 16-byte little-endian slice into the header.
func (h *MitchHeader) Unpack(buf []byte) error {
	if len(buf) < SizeHeader {
		return errors.New("nxr: buffer too small for MitchHeader")
	}
	h.TypeProvider = binary.LittleEndian.Uint16(buf[0:2])
	// read u48 LE
	var ts uint64
	for i := 0; i < 6; i++ {
		ts |= uint64(buf[2+i]) << (8 * i)
	}
	h.Timestamp = ts
	h.Count = buf[8]
	h.Flags = buf[9]
	h.Sequence = binary.LittleEndian.Uint16(buf[10:12])
	copy(h.Reserved[:], buf[12:16])
	return nil
}

// Tick is a 32-byte MITCH tick message body.
type Tick struct {
	Ticker uint64
	Bid    float64
	Ask    float64
	VBid   uint32
	VAsk   uint32
}

// Pack serializes the Tick into a 32-byte little-endian slice.
func (t *Tick) Pack() []byte {
	buf := make([]byte, SizeTick)
	binary.LittleEndian.PutUint64(buf[0:8], t.Ticker)
	binary.LittleEndian.PutUint64(buf[8:16], math.Float64bits(t.Bid))
	binary.LittleEndian.PutUint64(buf[16:24], math.Float64bits(t.Ask))
	binary.LittleEndian.PutUint32(buf[24:28], t.VBid)
	binary.LittleEndian.PutUint32(buf[28:32], t.VAsk)
	return buf
}

// Unpack deserializes a 32-byte little-endian slice into the Tick.
func (t *Tick) Unpack(buf []byte) error {
	if len(buf) < SizeTick {
		return errors.New("nxr: buffer too small for Tick")
	}
	t.Ticker = binary.LittleEndian.Uint64(buf[0:8])
	t.Bid = math.Float64frombits(binary.LittleEndian.Uint64(buf[8:16]))
	t.Ask = math.Float64frombits(binary.LittleEndian.Uint64(buf[16:24]))
	t.VBid = binary.LittleEndian.Uint32(buf[24:28])
	t.VAsk = binary.LittleEndian.Uint32(buf[28:32])
	return nil
}

// Trade is a 24-byte MITCH trade message body.
type Trade struct {
	Ticker  uint64
	Price   float64
	Qty     uint32
	TradeID uint32 // only lower 24 bits used (3 LE bytes on wire)
	Side    uint8
}

// Pack serializes the Trade into a 24-byte little-endian slice.
func (t *Trade) Pack() []byte {
	buf := make([]byte, SizeTrade)
	binary.LittleEndian.PutUint64(buf[0:8], t.Ticker)
	binary.LittleEndian.PutUint64(buf[8:16], math.Float64bits(t.Price))
	binary.LittleEndian.PutUint32(buf[16:20], t.Qty)
	// TradeID as u24 LE (3 bytes)
	buf[20] = byte(t.TradeID)
	buf[21] = byte(t.TradeID >> 8)
	buf[22] = byte(t.TradeID >> 16)
	buf[23] = t.Side
	return buf
}

// Unpack deserializes a 24-byte little-endian slice into the Trade.
func (t *Trade) Unpack(buf []byte) error {
	if len(buf) < SizeTrade {
		return errors.New("nxr: buffer too small for Trade")
	}
	t.Ticker = binary.LittleEndian.Uint64(buf[0:8])
	t.Price = math.Float64frombits(binary.LittleEndian.Uint64(buf[8:16]))
	t.Qty = binary.LittleEndian.Uint32(buf[16:20])
	t.TradeID = uint32(buf[20]) | uint32(buf[21])<<8 | uint32(buf[22])<<16
	t.Side = buf[23]
	return nil
}

// Index is a 40-byte MITCH index message body.
type Index struct {
	Ticker     uint64
	Bid        float64
	Ask        float64
	VBid       uint32
	VAsk       uint32
	CI         uint16
	TickCount  uint16
	Confidence uint8
	Accepted   uint8
	Rejected   uint8
	// pad byte at offset 39
}

// Pack serializes the Index into a 40-byte little-endian slice.
func (idx *Index) Pack() []byte {
	buf := make([]byte, SizeIndex)
	binary.LittleEndian.PutUint64(buf[0:8], idx.Ticker)
	binary.LittleEndian.PutUint64(buf[8:16], math.Float64bits(idx.Bid))
	binary.LittleEndian.PutUint64(buf[16:24], math.Float64bits(idx.Ask))
	binary.LittleEndian.PutUint32(buf[24:28], idx.VBid)
	binary.LittleEndian.PutUint32(buf[28:32], idx.VAsk)
	binary.LittleEndian.PutUint16(buf[32:34], idx.CI)
	binary.LittleEndian.PutUint16(buf[34:36], idx.TickCount)
	buf[36] = idx.Confidence
	buf[37] = idx.Accepted
	buf[38] = idx.Rejected
	buf[39] = 0 // pad
	return buf
}

// Unpack deserializes a 40-byte little-endian slice into the Index.
func (idx *Index) Unpack(buf []byte) error {
	if len(buf) < SizeIndex {
		return errors.New("nxr: buffer too small for Index")
	}
	idx.Ticker = binary.LittleEndian.Uint64(buf[0:8])
	idx.Bid = math.Float64frombits(binary.LittleEndian.Uint64(buf[8:16]))
	idx.Ask = math.Float64frombits(binary.LittleEndian.Uint64(buf[16:24]))
	idx.VBid = binary.LittleEndian.Uint32(buf[24:28])
	idx.VAsk = binary.LittleEndian.Uint32(buf[28:32])
	idx.CI = binary.LittleEndian.Uint16(buf[32:34])
	idx.TickCount = binary.LittleEndian.Uint16(buf[34:36])
	idx.Confidence = buf[36]
	idx.Accepted = buf[37]
	idx.Rejected = buf[38]
	return nil
}
