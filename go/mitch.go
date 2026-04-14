package nxr

// Re-export MITCH types from the codec package for convenience.
// The canonical implementation lives at github.com/nxrates/mitch/impl/go.

import mitch "github.com/nxrates/mitch/impl/go"

// Type aliases for MITCH wire types — use these or import mitch directly.
type (
	MitchHeader = mitch.Header
	Tick        = mitch.Tick
	Trade       = mitch.Trade
	Index       = mitch.Index
	Order       = mitch.Order
	OrderBook   = mitch.OrderBook
)

// Re-export timestamp helpers.
var (
	FromEpochUs = mitch.FromEpochUs
	ToEpochUs   = mitch.ToEpochUs
)

// Re-export size constants via functions (Go doesn't allow const aliases).
const (
	SizeHeader    = mitch.SizeHeader
	SizeTick      = mitch.SizeTick
	SizeTrade     = mitch.SizeTrade
	SizeOrder     = mitch.SizeOrder
	SizeIndex     = mitch.SizeIndex
	SizeBar       = mitch.SizeBar
	SizeOrderBook = mitch.SizeOrderBook
)
