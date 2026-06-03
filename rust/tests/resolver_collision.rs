//! Resolver collision CI gate (RCA 2026-06-01, ROOT1).
//!
//! The MITCH ticker_id is a deterministic bit-pack of
//! (base class_id, quote class_id, instrument) — NOT a hash. Two distinct
//! symbols may only share an id if they genuinely denote the same pair.
//!
//! These tests enumerate the launch priority set plus the five pairs that
//! previously collided (SOLV/HYPER/ALGO/H base absorption + fiat-quote
//! whole-pair fuzz) and assert every id is distinct, and that the normal
//! majors still resolve to the expected base asset.

use std::collections::HashMap;

use mitch::common::InstrumentType;
use nxr_sdk::resolve::resolve_ticker;

/// Resolve a symbol to its MITCH ticker id (SPOT), failing the test loudly
/// when the resolver returns an error for a symbol that must resolve.
fn id_of(sym: &str) -> u64 {
    resolve_ticker(sym, InstrumentType::SPOT)
        .unwrap_or_else(|e| panic!("resolve_ticker({sym}) must succeed, got: {e:?}"))
        .ticker
        .id
}

fn base_name_of(sym: &str) -> String {
    resolve_ticker(sym, InstrumentType::SPOT)
        .unwrap_or_else(|e| panic!("resolve_ticker({sym}) must succeed, got: {e:?}"))
        .ticker
        .base
        .name
}

/// The previously-colliding pairs + the priority majors must all map to
/// DISTINCT ids. This is the boot-time SLA gate, exercised in CI.
#[test]
fn priority_and_collision_set_ids_distinct() {
    let symbols = [
        // Priority majors (must keep working).
        "BTC/USDT",
        "ETH/USDT",
        "SOL/USDT",
        // ROOT1a — short-ticker base absorption (SOL←SOLV, HYPE←HYPER).
        "SOLV/USDT",
        "HYPER/USDT",
        // ROOT1b — whole-pair fuzz forced quote=USD and collapsed these.
        "ALGO/USDT",
        "ALGO/USD",
        "H/USDT",
        "USDT/THB",
        "USDT/BRL",
    ];

    let mut seen: HashMap<u64, &str> = HashMap::new();
    for sym in symbols {
        let id = id_of(sym);
        if let Some(prev) = seen.insert(id, sym) {
            panic!("ticker_id collision: {prev} and {sym} both resolved to id {id:#x}");
        }
    }
    assert_eq!(seen.len(), symbols.len(), "all priority+collision ids distinct");
}

/// The majors must resolve to the CORRECT base asset, not a fuzzy neighbour.
#[test]
fn majors_resolve_to_correct_base() {
    assert_eq!(base_name_of("BTC/USDT").to_lowercase(), "bitcoin");
    assert_eq!(base_name_of("ETH/USDT").to_lowercase(), "ethereum");
    assert_eq!(base_name_of("SOL/USDT").to_lowercase(), "solana");
}

/// Each new asset resolves to ITSELF (exact-alias, conf 1.0), not a neighbour.
#[test]
fn new_assets_resolve_to_themselves() {
    assert_eq!(base_name_of("SOLV/USDT").to_lowercase(), "solv protocol");
    assert_eq!(base_name_of("HYPER/USDT").to_lowercase(), "hyperlane");
    assert_eq!(base_name_of("ALGO/USDT").to_lowercase(), "algorand");
    assert_eq!(base_name_of("H/USDT").to_lowercase(), "humanity");
}

/// ETH+ (Ethereum Plus, 5701) must stay distinct from ETH (Ethereum, 5801);
/// ETH/USDT must NOT resolve to "Ethereum Plus" (ROOT1c last-write-wins).
#[test]
fn eth_not_absorbed_by_ethereum_plus() {
    assert_eq!(base_name_of("ETH/USDT").to_lowercase(), "ethereum");
}

/// Fiat-quote pairs must stay distinct from USD-forced fuzz (ROOT1b).
#[test]
fn fiat_quote_resolves_in_fx() {
    let m = resolve_ticker("USDT/THB", InstrumentType::SPOT)
        .expect("USDT/THB must resolve");
    assert_eq!(m.ticker.base.name.to_lowercase(), "tether");
    assert_eq!(m.ticker.quote.name.to_lowercase(), "thai baht");
}

/// 1:1 BTC wrappers share the canonical BTC/* ticker id (price_canonical).
#[test]
fn wrapped_btc_shares_btc_index_id() {
    use nxr_sdk::{canonical_price_pair, resolve_ticker_id};

    let btc_usdt = id_of("BTC/USDT");
    let btc_usdc = id_of("BTC/USDC");
    assert_eq!(resolve_ticker_id("CBBTC/USDT"), btc_usdt);
    assert_eq!(resolve_ticker_id("WBTC/USDT"), btc_usdt);
    assert_eq!(resolve_ticker_id("CBBTC-USDC"), btc_usdc);
    assert_eq!(resolve_ticker_id("WBTC-USDC"), btc_usdc);
    assert_eq!(canonical_price_pair("CBBTC/USDC"), "BTC/USDC");
    assert_eq!(base_name_of("CBBTC/USDC").to_lowercase(), "bitcoin");
}

/// Yield/LST wrappers must NOT collapse onto the canonical ETH index.
#[test]
fn cbeth_distinct_from_eth_index() {
    use nxr_sdk::resolve_ticker_id;

    let eth = id_of("ETH/USDT");
    let cbeth = id_of("cbETH/USDT");
    assert_ne!(cbeth, eth, "cbETH/USDT must not share ETH/USDT id");
    assert!(
        base_name_of("cbETH/USDT").to_lowercase().contains("staked")
            || base_name_of("cbETH/USDT").to_lowercase().contains("coinbase"),
        "cbETH base must resolve to cbETH asset, not ethereum"
    );
    assert_eq!(resolve_ticker_id("CBBTC/USDT"), id_of("BTC/USDT"));
}
