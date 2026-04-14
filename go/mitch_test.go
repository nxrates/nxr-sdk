package nxr

import (
	"math"
	"testing"
	"time"
)

func TestMitchHeaderPackUnpack(t *testing.T) {
	orig := MitchHeader{
		TypeProvider: 0,
		Timestamp:    0x0000AABBCCDDEE,
		Count:        42,
		Flags:        0,
		Sequence:     1234,
	}
	orig.SetTypeProvider(wireCodeTick, 2047)

	buf := orig.Pack()
	if len(buf) != SizeHeader {
		t.Fatalf("expected %d bytes, got %d", SizeHeader, len(buf))
	}

	var got MitchHeader
	if err := got.Unpack(buf); err != nil {
		t.Fatalf("unpack: %v", err)
	}

	if got.TypeProvider != orig.TypeProvider {
		t.Errorf("TypeProvider: got %d, want %d", got.TypeProvider, orig.TypeProvider)
	}
	if got.Timestamp != orig.Timestamp {
		t.Errorf("Timestamp: got %x, want %x", got.Timestamp, orig.Timestamp)
	}
	if got.Count != orig.Count {
		t.Errorf("Count: got %d, want %d", got.Count, orig.Count)
	}
	if got.Flags != orig.Flags {
		t.Errorf("Flags: got %d, want %d", got.Flags, orig.Flags)
	}
	if got.Sequence != orig.Sequence {
		t.Errorf("Sequence: got %d, want %d", got.Sequence, orig.Sequence)
	}
}

func TestMitchHeaderMsgTypeAndProviderID(t *testing.T) {
	var h MitchHeader
	h.SetTypeProvider(wireCodeTrade, 100)

	if h.MsgType() != MsgTrade {
		t.Errorf("MsgType: got %c, want %c", h.MsgType(), MsgTrade)
	}
	if h.ProviderID() != 100 {
		t.Errorf("ProviderID: got %d, want %d", h.ProviderID(), 100)
	}
}

func TestMitchHeaderUnpackTooSmall(t *testing.T) {
	var h MitchHeader
	if err := h.Unpack(make([]byte, 10)); err == nil {
		t.Error("expected error for undersized buffer")
	}
}

func TestTickPackUnpack(t *testing.T) {
	orig := Tick{
		Ticker: 12345,
		Bid:    1.23456,
		Ask:    1.23478,
		VBid:   5000,
		VAsk:   6000,
	}

	buf := orig.Pack()
	if len(buf) != SizeTick {
		t.Fatalf("expected %d bytes, got %d", SizeTick, len(buf))
	}

	var got Tick
	if err := got.Unpack(buf); err != nil {
		t.Fatalf("unpack: %v", err)
	}

	if got.Ticker != orig.Ticker {
		t.Errorf("Ticker: got %d, want %d", got.Ticker, orig.Ticker)
	}
	if got.Bid != orig.Bid {
		t.Errorf("Bid: got %f, want %f", got.Bid, orig.Bid)
	}
	if got.Ask != orig.Ask {
		t.Errorf("Ask: got %f, want %f", got.Ask, orig.Ask)
	}
	if got.VBid != orig.VBid {
		t.Errorf("VBid: got %d, want %d", got.VBid, orig.VBid)
	}
	if got.VAsk != orig.VAsk {
		t.Errorf("VAsk: got %d, want %d", got.VAsk, orig.VAsk)
	}
}

func TestTickUnpackTooSmall(t *testing.T) {
	var tick Tick
	if err := tick.Unpack(make([]byte, 20)); err == nil {
		t.Error("expected error for undersized buffer")
	}
}

func TestTradePackUnpack(t *testing.T) {
	orig := Trade{
		Ticker:  99,
		Price:   42.5,
		Qty:     100,
		TradeID: 0xABCDEF, // 24-bit max
		Side:    1,
	}

	buf := orig.Pack()
	if len(buf) != SizeTrade {
		t.Fatalf("expected %d bytes, got %d", SizeTrade, len(buf))
	}

	var got Trade
	if err := got.Unpack(buf); err != nil {
		t.Fatalf("unpack: %v", err)
	}

	if got.Ticker != orig.Ticker {
		t.Errorf("Ticker: got %d, want %d", got.Ticker, orig.Ticker)
	}
	if got.Price != orig.Price {
		t.Errorf("Price: got %f, want %f", got.Price, orig.Price)
	}
	if got.Qty != orig.Qty {
		t.Errorf("Qty: got %d, want %d", got.Qty, orig.Qty)
	}
	if got.TradeID != orig.TradeID {
		t.Errorf("TradeID: got 0x%X, want 0x%X", got.TradeID, orig.TradeID)
	}
	if got.Side != orig.Side {
		t.Errorf("Side: got %d, want %d", got.Side, orig.Side)
	}
}

func TestTradeID24Bit(t *testing.T) {
	// Ensure only lower 24 bits survive the roundtrip.
	orig := Trade{
		Ticker:  1,
		Price:   1.0,
		Qty:     1,
		TradeID: 0xFFFFFF, // max u24
		Side:    0,
	}
	buf := orig.Pack()
	var got Trade
	if err := got.Unpack(buf); err != nil {
		t.Fatalf("unpack: %v", err)
	}
	if got.TradeID != 0xFFFFFF {
		t.Errorf("TradeID max u24: got 0x%X, want 0xFFFFFF", got.TradeID)
	}
}

func TestTradeUnpackTooSmall(t *testing.T) {
	var trade Trade
	if err := trade.Unpack(make([]byte, 16)); err == nil {
		t.Error("expected error for undersized buffer")
	}
}

func TestIndexPackUnpack(t *testing.T) {
	orig := Index{
		Ticker:     55,
		Bid:        100.25,
		Ask:        100.50,
		VBid:       3000,
		VAsk:       4000,
		CI:         500,
		TickCount:  12,
		Confidence: 95,
		Accepted:   10,
		Rejected:   2,
	}

	buf := orig.Pack()
	if len(buf) != SizeIndex {
		t.Fatalf("expected %d bytes, got %d", SizeIndex, len(buf))
	}

	var got Index
	if err := got.Unpack(buf); err != nil {
		t.Fatalf("unpack: %v", err)
	}

	if got.Ticker != orig.Ticker {
		t.Errorf("Ticker: got %d, want %d", got.Ticker, orig.Ticker)
	}
	if got.Bid != orig.Bid {
		t.Errorf("Bid: got %f, want %f", got.Bid, orig.Bid)
	}
	if got.Ask != orig.Ask {
		t.Errorf("Ask: got %f, want %f", got.Ask, orig.Ask)
	}
	if got.VBid != orig.VBid {
		t.Errorf("VBid: got %d, want %d", got.VBid, orig.VBid)
	}
	if got.VAsk != orig.VAsk {
		t.Errorf("VAsk: got %d, want %d", got.VAsk, orig.VAsk)
	}
	if got.CI != orig.CI {
		t.Errorf("CI: got %d, want %d", got.CI, orig.CI)
	}
	if got.TickCount != orig.TickCount {
		t.Errorf("TickCount: got %d, want %d", got.TickCount, orig.TickCount)
	}
	if got.Confidence != orig.Confidence {
		t.Errorf("Confidence: got %d, want %d", got.Confidence, orig.Confidence)
	}
	if got.Accepted != orig.Accepted {
		t.Errorf("Accepted: got %d, want %d", got.Accepted, orig.Accepted)
	}
	if got.Rejected != orig.Rejected {
		t.Errorf("Rejected: got %d, want %d", got.Rejected, orig.Rejected)
	}
}

func TestIndexUnpackTooSmall(t *testing.T) {
	var idx Index
	if err := idx.Unpack(make([]byte, 30)); err == nil {
		t.Error("expected error for undersized buffer")
	}
}

func TestTimestampRoundtrip(t *testing.T) {
	// Use a known time and verify roundtrip.
	ref := time.Date(2024, 6, 15, 12, 30, 0, 0, time.UTC)
	ticks := ToEpochUs(ref)
	recovered := FromEpochUs(ticks)

	// Resolution is 16us, so we accept up to 16us drift.
	diff := ref.Sub(recovered).Abs()
	if diff > 16*time.Microsecond {
		t.Errorf("timestamp roundtrip drift: %v (ref=%v, recovered=%v)", diff, ref, recovered)
	}
}

func TestTimestampEpoch(t *testing.T) {
	// 2010-01-01T00:00:00Z should encode to tick 0.
	epoch := time.Date(2010, 1, 1, 0, 0, 0, 0, time.UTC)
	ticks := ToEpochUs(epoch)
	if ticks != 0 {
		t.Errorf("epoch ticks: got %d, want 0", ticks)
	}

	recovered := FromEpochUs(0)
	if !recovered.Equal(epoch) {
		t.Errorf("epoch from ticks=0: got %v, want %v", recovered, epoch)
	}
}

func TestTimestampU48Max(t *testing.T) {
	// Verify we can handle large tick values without overflow.
	maxU48 := uint64(1<<48 - 1)
	result := FromEpochUs(maxU48)
	if result.IsZero() {
		t.Error("FromEpochUs with max u48 returned zero time")
	}
	back := ToEpochUs(result)
	if back != maxU48 {
		t.Errorf("u48 max roundtrip: got %d, want %d", back, maxU48)
	}
}

func TestWireCodeASCIIMapping(t *testing.T) {
	tests := []struct {
		wire uint8
		want byte
	}{
		{wireCodeTrade, 't'},
		{wireCodeOrder, 'o'},
		{wireCodeTick, 's'},
		{wireCodeIndex, 'i'},
		{wireCodeOrderBook, 'b'},
		{wireCodeBar, 'k'},
	}

	for _, tt := range tests {
		var h MitchHeader
		h.SetTypeProvider(tt.wire, 0)
		got := h.MsgType()
		if got != tt.want {
			t.Errorf("wire %d: MsgType() = %c, want %c", tt.wire, got, tt.want)
		}
	}
}

func TestWireCodeASCIIReverseMapping(t *testing.T) {
	// Verify asciiToWire map is consistent with wireToASCII.
	for wire, ascii := range wireToASCII {
		gotWire, ok := asciiToWire[ascii]
		if !ok {
			t.Errorf("ASCII %c not in asciiToWire", ascii)
			continue
		}
		if gotWire != wire {
			t.Errorf("ASCII %c: asciiToWire=%d, want %d", ascii, gotWire, wire)
		}
	}
}

func TestWireCodeUnknown(t *testing.T) {
	var h MitchHeader
	h.TypeProvider = 0x000F // wire code 15, not defined
	if h.MsgType() != 0 {
		t.Errorf("unknown wire code: MsgType() = %c, want 0", h.MsgType())
	}
}

func TestHeaderProviderIDRange(t *testing.T) {
	// Max provider ID is 4095 (12 bits).
	var h MitchHeader
	h.SetTypeProvider(wireCodeTick, 4095)

	if h.ProviderID() != 4095 {
		t.Errorf("ProviderID: got %d, want 4095", h.ProviderID())
	}
	if h.MsgType() != MsgTick {
		t.Errorf("MsgType: got %c, want %c", h.MsgType(), MsgTick)
	}

	// Verify the encoded value.
	expected := uint16(wireCodeTick) | (4095 << 4)
	if h.TypeProvider != expected {
		t.Errorf("TypeProvider: got 0x%04X, want 0x%04X", h.TypeProvider, expected)
	}
}

func TestTickSpecialFloats(t *testing.T) {
	orig := Tick{
		Ticker: 1,
		Bid:    math.Inf(1),
		Ask:    math.NaN(),
		VBid:   0,
		VAsk:   0,
	}
	buf := orig.Pack()
	var got Tick
	if err := got.Unpack(buf); err != nil {
		t.Fatalf("unpack: %v", err)
	}
	if !math.IsInf(got.Bid, 1) {
		t.Errorf("Bid: expected +Inf, got %f", got.Bid)
	}
	if !math.IsNaN(got.Ask) {
		t.Errorf("Ask: expected NaN, got %f", got.Ask)
	}
}
