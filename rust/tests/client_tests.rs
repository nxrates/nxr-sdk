//! NxrClient integration tests against a `wiremock` mock server.
//!
//! Verifies:
//! - `/v1/tickers/detail` parses into the typed `TickersDetailResponse` + caches.
//! - `/v1/idx/{sym}` MITCH binary decode round-trips a synthetic 56B payload.
//! - URL building (dash-form symbol; range query params) matches the server spec.

use bytemuck::bytes_of;
use nxr_sdk::client::{BarKindParam, DataKind, HistoryData, HistoryOpts, NxrClient, RangeOpts};
use nxr_sdk::IndexRecord;
use serde_json::json;
use wiremock::matchers::{header, method, path, query_param};
use wiremock::{Mock, MockServer, ResponseTemplate};

fn build_index_record(ticker: u64, bid: f64, ask: f64) -> Vec<u8> {
    // 56B: 16B header (zeroed except msg_type/wire_code at [0]) + 40B Index body.
    // We only need a few fields the decoder reads back. Zero-init via bytemuck.
    let mut rec: IndexRecord = bytemuck::Zeroable::zeroed();
    rec.index.ticker = ticker;
    rec.index.bid = bid;
    rec.index.ask = ask;
    bytes_of(&rec).to_vec()
}

#[tokio::test]
async fn tickers_detail_typed_parse_and_cache() {
    let server = MockServer::start().await;
    let sample = json!({
        "idx_aggregation_ms": 100,
        "count": 2,
        "tickers": [
            {
                "ticker_id": 435315551398526976u64,
                "ticker": "BTC/USDT",
                "base": "BTC",
                "quote": "USDT",
                "base_class": "CR",
                "quote_class": "CR",
                "instrument_type": "SPOT",
                "native": true,
                "kinds": {
                    "idx": {
                        "fields": ["ts", "ticker"],
                        "stride_bytes": 56,
                        "shards": { "first_date": "2025-01-01", "last_date": "2025-01-31", "count": 31 }
                    }
                }
            },
            {
                "ticker_id": 0,
                "ticker": "ETH-BTC",
                "base": "ETH",
                "quote": "BTC",
                "base_class": "",
                "quote_class": "",
                "instrument_type": "SPOT",
                "native": false,
                "synth_legs": [
                    { "sym": "ETH/USDT", "exp": 1 },
                    { "sym": "BTC/USDT", "exp": -1 }
                ],
                "kinds": {}
            }
        ]
    });
    Mock::given(method("GET"))
        .and(path("/v1/tickers/detail"))
        .and(header("accept", "application/json"))
        .respond_with(ResponseTemplate::new(200).set_body_json(sample))
        // First-call hits the server, second-call uses cache.
        .expect(1)
        .mount(&server)
        .await;

    let c = NxrClient::new(server.uri());
    let d1 = c.tickers_detail().await.unwrap();
    let d2 = c.tickers_detail().await.unwrap();
    assert_eq!(d1.count, 2);
    assert_eq!(d1.tickers.len(), 2);
    assert_eq!(d1.tickers[0].ticker, "BTC/USDT");
    assert_eq!(d1.tickers[0].ticker_id, 435315551398526976);
    assert_eq!(d1.tickers[0].kinds["idx"].stride_bytes, 56);
    assert_eq!(d1.tickers[1].native, false);
    let legs = d1.tickers[1].synth_legs.as_ref().unwrap();
    assert_eq!(legs.len(), 2);
    assert_eq!(legs[1].exp, -1);
    // Cache hit.
    assert_eq!(d2.count, 2);
}

#[tokio::test]
async fn idx_binary_decode_roundtrip() {
    let server = MockServer::start().await;
    let payload = build_index_record(99, 10.0, 11.0);
    Mock::given(method("GET"))
        .and(path("/v1/idx/BTC-USDT"))
        .and(query_param("limit", "1"))
        .and(header("accept", "application/octet-stream"))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("content-type", "application/octet-stream")
                .set_body_bytes(payload),
        )
        .mount(&server)
        .await;

    let c = NxrClient::new(server.uri());
    let recs = c
        .idx(
            "BTC/USDT",
            &RangeOpts {
                limit: Some(1),
                ..Default::default()
            },
        )
        .await
        .unwrap();
    assert_eq!(recs.len(), 1);
    let rec = &recs[0];
    // Copy out of packed struct to avoid unaligned refs.
    let ticker = rec.index.ticker;
    let bid = rec.index.bid;
    let ask = rec.index.ask;
    assert_eq!(ticker, 99);
    assert_eq!(bid, 10.0);
    assert_eq!(ask, 11.0);
}

#[tokio::test]
async fn history_chainable_matches_object_form() {
    let server = MockServer::start().await;
    // Two requests, identical empty body. The mock counts hits.
    Mock::given(method("GET"))
        .and(path("/v1/bars/BTC-USDT/renko"))
        .and(query_param("limit", "5"))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("content-type", "application/octet-stream")
                .set_body_bytes(Vec::<u8>::new()),
        )
        .expect(2)
        .mount(&server)
        .await;

    let c = NxrClient::new(server.uri());
    let a = c
        .history(HistoryOpts {
            ticker: Some("BTC/USDT".into()),
            kind: Some(DataKind::Renko),
            limit: Some(5),
            ..Default::default()
        })
        .await
        .unwrap();
    let b = c
        .get()
        .history()
        .pair("BTC/USDT")
        .renko()
        .limit(5)
        .fetch()
        .await
        .unwrap();
    let (a_kind, b_kind) = match (&a, &b) {
        (HistoryData::Bars { kind: k1, .. }, HistoryData::Bars { kind: k2, .. }) => (*k1, *k2),
        _ => panic!("expected Bars envelopes"),
    };
    assert_eq!(a_kind, DataKind::Renko);
    assert_eq!(b_kind, DataKind::Renko);
}

#[tokio::test]
async fn bars_kline_endpoint_format() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/v1/bars/ETH-USDC/kline"))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("content-type", "application/octet-stream")
                .set_body_bytes(Vec::<u8>::new()),
        )
        .expect(1)
        .mount(&server)
        .await;
    let c = NxrClient::new(server.uri());
    let bars = c.bars("ETH/USDC", BarKindParam::Kline, &RangeOpts::default()).await.unwrap();
    assert!(bars.is_empty());
}

#[tokio::test]
async fn http_error_surfaces_body_excerpt() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/v1/tickers"))
        .respond_with(ResponseTemplate::new(500).set_body_string("boom internal"))
        .mount(&server)
        .await;
    let c = NxrClient::new(server.uri());
    let err = c.tickers().await.unwrap_err();
    let msg = err.to_string();
    assert!(msg.contains("500"), "expected 500 in {msg}");
    assert!(msg.contains("boom internal"), "expected body excerpt in {msg}");
}
