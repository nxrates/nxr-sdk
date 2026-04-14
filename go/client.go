package nxr

import (
	"context"
	"encoding/binary"
	"encoding/json"
	"fmt"
	"math"
	"net/http"
	"net/url"
	"strings"

	"github.com/gorilla/websocket"
)

// TickerSnapshot is a snapshot returned by the /v1/tickers endpoint.
type TickerSnapshot struct {
	Symbol string  `json:"symbol"`
	Ticker uint64  `json:"ticker"`
	Bid    float64 `json:"bid"`
	Ask    float64 `json:"ask"`
}

// WsIndex represents a single index record from the WebSocket stream.
type WsIndex struct {
	TsMs       float64
	Ticker     float64
	Mid        float64
	Bid        float64
	Ask        float64
	CI         float64
	Confidence float64
	Accepted   float64
	Rejected   float64
}

// WsTick represents a single tick record from the WebSocket stream.
type WsTick struct {
	TsMs       float64
	Ticker     float64
	ProviderID float64
	Bid        float64
	Ask        float64
	Accepted   float64
}

const (
	wsTypeIndex = 1
	wsTypeTick  = 2

	wsHeaderSize   = 8
	indexStrideF64 = 9
	tickStrideF64  = 6
)

// Client is the NX Rates REST and WebSocket client.
type Client struct {
	baseURL    string
	httpClient *http.Client
}

// NewClient creates a new NXR client. baseURL is the HTTP base (e.g. "http://localhost:8080").
func NewClient(baseURL string) *Client {
	return &Client{
		baseURL:    strings.TrimRight(baseURL, "/"),
		httpClient: &http.Client{},
	}
}

// Symbols calls GET /v1/symbols and returns a map of symbol name to ticker ID.
func (c *Client) Symbols(ctx context.Context) (map[string]uint64, error) {
	var result map[string]uint64
	if err := c.getJSON(ctx, "/v1/symbols", &result); err != nil {
		return nil, fmt.Errorf("nxr: symbols: %w", err)
	}
	return result, nil
}

// Providers calls GET /v1/providers and returns a map of provider ID to name.
func (c *Client) Providers(ctx context.Context) (map[uint16]string, error) {
	// The API returns string keys; we parse them into uint16.
	var raw map[string]string
	if err := c.getJSON(ctx, "/v1/providers", &raw); err != nil {
		return nil, fmt.Errorf("nxr: providers: %w", err)
	}
	result := make(map[uint16]string, len(raw))
	for k, v := range raw {
		var id uint16
		if _, err := fmt.Sscanf(k, "%d", &id); err != nil {
			return nil, fmt.Errorf("nxr: providers: invalid key %q: %w", k, err)
		}
		result[id] = v
	}
	return result, nil
}

// Tickers calls GET /v1/tickers and returns a slice of ticker snapshots.
func (c *Client) Tickers(ctx context.Context) ([]TickerSnapshot, error) {
	var result []TickerSnapshot
	if err := c.getJSON(ctx, "/v1/tickers", &result); err != nil {
		return nil, fmt.Errorf("nxr: tickers: %w", err)
	}
	return result, nil
}

// IsHealthy calls GET /health and returns true if the server responds 200 OK.
func (c *Client) IsHealthy(ctx context.Context) bool {
	req, err := http.NewRequestWithContext(ctx, http.MethodGet, c.baseURL+"/health", nil)
	if err != nil {
		return false
	}
	resp, err := c.httpClient.Do(req)
	if err != nil {
		return false
	}
	resp.Body.Close()
	return resp.StatusCode == http.StatusOK
}

// Stream connects to the WebSocket endpoint and dispatches decoded index and tick
// records to the provided callbacks. It blocks until the context is cancelled or
// an error occurs.
func (c *Client) Stream(ctx context.Context, onIndex func([]WsIndex), onTick func([]WsTick)) error {
	wsURL, err := c.wsURL("/v1/stream")
	if err != nil {
		return fmt.Errorf("nxr: stream: %w", err)
	}

	conn, _, err := websocket.DefaultDialer.DialContext(ctx, wsURL, nil)
	if err != nil {
		return fmt.Errorf("nxr: stream dial: %w", err)
	}
	defer conn.Close()

	for {
		select {
		case <-ctx.Done():
			return ctx.Err()
		default:
		}

		_, msg, err := conn.ReadMessage()
		if err != nil {
			if ctx.Err() != nil {
				return ctx.Err()
			}
			return fmt.Errorf("nxr: stream read: %w", err)
		}

		if len(msg) < wsHeaderSize {
			continue
		}

		msgType := msg[0]
		count := binary.LittleEndian.Uint16(msg[2:4])
		payload := msg[wsHeaderSize:]

		switch msgType {
		case wsTypeIndex:
			stride := indexStrideF64 * 8
			if onIndex == nil || int(count)*stride > len(payload) {
				continue
			}
			records := make([]WsIndex, count)
			for i := range records {
				off := i * stride
				records[i] = WsIndex{
					TsMs:       readF64(payload, off+0*8),
					Ticker:     readF64(payload, off+1*8),
					Mid:        readF64(payload, off+2*8),
					Bid:        readF64(payload, off+3*8),
					Ask:        readF64(payload, off+4*8),
					CI:         readF64(payload, off+5*8),
					Confidence: readF64(payload, off+6*8),
					Accepted:   readF64(payload, off+7*8),
					Rejected:   readF64(payload, off+8*8),
				}
			}
			onIndex(records)

		case wsTypeTick:
			stride := tickStrideF64 * 8
			if onTick == nil || int(count)*stride > len(payload) {
				continue
			}
			records := make([]WsTick, count)
			for i := range records {
				off := i * stride
				records[i] = WsTick{
					TsMs:       readF64(payload, off+0*8),
					Ticker:     readF64(payload, off+1*8),
					ProviderID: readF64(payload, off+2*8),
					Bid:        readF64(payload, off+3*8),
					Ask:        readF64(payload, off+4*8),
					Accepted:   readF64(payload, off+5*8),
				}
			}
			onTick(records)
		}
	}
}

// getJSON performs a GET request and decodes the JSON response into v.
func (c *Client) getJSON(ctx context.Context, path string, v any) error {
	req, err := http.NewRequestWithContext(ctx, http.MethodGet, c.baseURL+path, nil)
	if err != nil {
		return err
	}
	req.Header.Set("Accept", "application/json")

	resp, err := c.httpClient.Do(req)
	if err != nil {
		return err
	}
	defer resp.Body.Close()

	if resp.StatusCode != http.StatusOK {
		return fmt.Errorf("unexpected status %d", resp.StatusCode)
	}

	return json.NewDecoder(resp.Body).Decode(v)
}

// wsURL converts the HTTP base URL to a WebSocket URL for the given path.
func (c *Client) wsURL(path string) (string, error) {
	u, err := url.Parse(c.baseURL)
	if err != nil {
		return "", err
	}
	switch u.Scheme {
	case "https":
		u.Scheme = "wss"
	default:
		u.Scheme = "ws"
	}
	// WebSocket endpoint uses port 40004.
	host := u.Hostname()
	u.Host = host + ":40004"
	u.Path = path
	return u.String(), nil
}

// readF64 reads a little-endian float64 from buf at the given byte offset.
func readF64(buf []byte, off int) float64 {
	return math.Float64frombits(binary.LittleEndian.Uint64(buf[off : off+8]))
}
