//! NxrClient integration tests against a `wiremock` mock server.
//!
//! Verifies:
//! - `/v1/tickers/detail` parses into the typed `TickersDetailResponse` + caches,
//!   and its input-limited `?symbols=` form sends an explicit list.
//! - the asset-centric surface (`/v1/counts`, `/v1/assets`, `/v1/assets/{ident}`,
//!   `/v1/assets/last`) parses and keeps the class pin in the URL.
//! - `/v1/idx/{sym}` MITCH binary decode round-trips a synthetic 56B payload.
//! - URL building (dash-form symbol; range query params) matches the server spec.

use bytemuck::bytes_of;
use nxr_sdk::IndexRecord;
use nxr_sdk::client::{
    BarKindParam, DataKind, HistoryData, HistoryOpts, NxrClient, RangeOpts, ShardStatus,
};
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
                        "shards": { "first_date": "2025-01-01", "last_date": "2025-01-31", "count": 31, "status": "live" }
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
                "alias_of": "ETH/BTC",
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
    assert_eq!(d1.tickers[0].kinds["idx"].shards.status, ShardStatus::Live);
    assert_eq!(d1.tickers[1].native, false);
    assert_eq!(d1.tickers[1].alias_of.as_deref(), Some("ETH/BTC"));
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
    let bars = c
        .bars("ETH/USDC", BarKindParam::Kline, &RangeOpts::default())
        .await
        .unwrap();
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
    assert!(
        msg.contains("boom internal"),
        "expected body excerpt in {msg}"
    );
}

#[tokio::test]
async fn snapshot_carries_flags_age_and_status() {
    let server = MockServer::start().await;
    let row = json!({
        "ticker": 42u64, "mid": 10.5, "bid": 10.0, "ask": 11.0,
        "ci": 7, "confidence": 131, "flags": 4, "age_ms": 1200, "status": "fresh"
    });
    Mock::given(method("GET"))
        .and(path("/v1/price/BTC-USDT"))
        .and(query_param("max_age_ms", "5000"))
        .respond_with(ResponseTemplate::new(200).set_body_json(row.clone()))
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/v1/tickers"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!([row])))
        .mount(&server)
        .await;

    let c = NxrClient::new(server.uri());
    let p = c.price("BTC/USDT", Some(5000)).await.unwrap();
    assert_eq!(
        (p.flags, p.age_ms, p.status.as_str()),
        (4, Some(1200), "fresh")
    );
    assert_eq!(c.tickers().await.unwrap()[0].ticker, 42);
}

#[tokio::test]
async fn last_accepts_symbols_and_max_age() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/v1/last"))
        .and(query_param("symbols", "BTC-USDT,EURUSD"))
        .and(query_param("max_age_ms", "30000"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!([])))
        .expect(1)
        .mount(&server)
        .await;
    let c = NxrClient::new(server.uri());
    assert!(
        c.last(&["BTC/USDT", "EURUSD"], Some(30_000))
            .await
            .unwrap()
            .is_empty()
    );
}

#[tokio::test]
async fn freshness_splits_emit_from_provider_lag() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/v1/freshness/BTC-USDT"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "ticker": 42u64, "last_ms": 1000, "lag_ms": 900, "status": "fresh",
            "provider_last_ms": null, "provider_lag_ms": null, "provider_status": "no-data"
        })))
        .mount(&server)
        .await;
    let f = NxrClient::new(server.uri())
        .freshness("BTC/USDT")
        .await
        .unwrap();
    assert_eq!(f.lag_ms, Some(900));
    assert_eq!(f.provider_lag_ms, None);
    assert_eq!(f.provider_status, "no-data");
}

/// The asset surface: one small row per asset, and the endpoints that back a
/// dashboard's counts. Each mock asserts the exact path, so a client that
/// silently fell back to a ticker-list endpoint would fail here.
#[tokio::test]
async fn asset_surface_parses_and_uses_the_asset_routes() {
    let server = MockServer::start().await;
    let row = json!({
        "asset": "BTC", "class": "CR", "class_id": 2001, "asset_id": 133969,
        "storage_quote": "USD", "market_count": 3, "venue_count": 2,
        "native_ticker": "BTC/USDT"
    });

    Mock::given(method("GET"))
        .and(path("/v1/counts"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "assets": 409, "tickers": 156656, "registered_tickers": 3445,
            "venues": 12, "markets": 83, "aggregation_interval_ms": 50
        })))
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/v1/assets"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!([row])))
        .expect(1)
        .mount(&server)
        .await;
    // The class pin travels percent-encoded and is NOT split into path segments.
    Mock::given(method("GET"))
        .and(path("/v1/assets/CR:BTC"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "asset": "BTC", "class": "CR", "class_id": 2001, "asset_id": 133969,
            "storage_quote": "USD", "market_count": 1, "venue_count": 1,
            "native_ticker": "BTC/USDT",
            "markets": [{"venue": "Binance", "pair": "BTC/USDT", "volume_usd": 1.5e9, "inverted": false}],
            "tickers": ["BTC/USDT"], "ticker_count": 412
        })))
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/v1/assets/last"))
        .and(query_param("quote", "USDC"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!([{
            "asset": "BTC", "quote": "USDC", "ticker": 435315551398526976u64,
            "mid": 60006.0, "bid": 60000.0, "ask": 60012.0, "ci": 42,
            "confidence": 4, "flags": 129, "age_ms": 25, "status": "fresh"
        }])))
        .expect(1)
        .mount(&server)
        .await;

    let c = NxrClient::new(server.uri());
    let counts = c.counts().await.unwrap();
    assert_eq!((counts.assets, counts.tickers), (409, 156_656));
    assert_eq!(counts.registered_tickers, 3445);

    let assets = c.assets().await.unwrap();
    assert_eq!(assets.len(), 1);
    assert_eq!(assets[0].asset, "BTC");
    assert_eq!(assets[0].storage_quote, "USD");
    assert_eq!(assets[0].native_ticker.as_deref(), Some("BTC/USDT"));

    let d = c.asset("CR:BTC").await.unwrap();
    // The capped sample never masquerades as the total.
    assert_eq!(d.ticker_count, 412);
    assert_eq!(d.tickers.len(), 1);
    assert_eq!(d.markets[0].venue, "Binance");
    assert_eq!(d.row.asset_id, 133_969);

    let last = c.assets_last(Some("USDC")).await.unwrap();
    assert_eq!(last[0].quote, "USDC");
    assert_eq!(last[0].px.ticker, 435_315_551_398_526_976);
    assert_eq!(last[0].px.status, "fresh");
}

/// `/v1/tickers/ids` is the bulk shape, and `?symbols=` is how rich rows are
/// asked for. An empty list is answered locally: no request, no 400.
#[tokio::test]
async fn ids_and_explicit_detail_list() {
    let server = MockServer::start().await;
    let ids: Vec<u64> = vec![435_315_551_398_526_976, 1, u64::MAX];
    let packed: Vec<u8> = ids.iter().flat_map(|i| i.to_le_bytes()).collect();

    Mock::given(method("GET"))
        .and(path("/v1/tickers/ids"))
        .and(header("accept", "application/octet-stream"))
        .respond_with(ResponseTemplate::new(200).set_body_bytes(packed))
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/v1/tickers/detail"))
        .and(query_param("symbols", "BTC-USDT,ETH-USDT"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "idx_aggregation_ms": 50, "count": 0, "tickers": []
        })))
        .expect(1)
        .mount(&server)
        .await;

    let c = NxrClient::new(server.uri());
    assert_eq!(c.tickers_ids().await.unwrap(), ids);
    assert_eq!(
        c.tickers_detail_for(&["BTC/USDT", "ETH/USDT"])
            .await
            .unwrap()
            .count,
        0
    );
    assert_eq!(c.tickers_detail_for(&[]).await.unwrap().count, 0);
}
