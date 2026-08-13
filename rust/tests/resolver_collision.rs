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

/// The BTR FX Core wrappers must resolve STRICTLY to their OWN new
/// crypto-assets.csv rows: an FNV phantom would shard them under hash-noise ids,
/// and being absorbed by the fiat row they wrap (`KRW1` → `KRW` via suffix
/// stripping is the near miss) would erase the distinct id a de-peg needs.
#[test]
fn fx_wrapper_assets_resolve_to_themselves_not_to_their_currency() {
    for (sym, base) in [
        ("EURC/USD", "euro coin"),
        ("QCAD/USD", "qcad"),
        ("AUDF/USD", "aud forte"),
        ("BRLA/USD", "brla digital"),
        ("JPYC/USD", "jpy coin"),
        ("KRW1/USD", "krw1"),
    ] {
        assert!(
            nxr_sdk::try_resolve_ticker_id(sym).is_some(),
            "{sym} must resolve strictly, not through the FNV fallback"
        );
        assert_eq!(base_name_of(sym).to_lowercase(), base, "{sym} base asset");
    }
}

/// CAT-2 custodial BTC wraps keep a DISTINCT exposed ticker_id (de-peg risk is
/// observable) but `series_canonical_ticker_id` redirects them to BTC's series.
#[test]
fn wrapped_btc_distinct_id_shares_btc_series() {
    use nxr_sdk::{resolve_ticker_id, series_canonical_ticker_id};

    let btc_usdt = id_of("BTC/USDT");
    let btc_usdc = id_of("BTC/USDC");

    // Distinct exposed ids (NOT collapsed at resolution).
    assert_ne!(resolve_ticker_id("CBBTC/USDT"), btc_usdt, "cbBTC keeps own id");
    assert_ne!(resolve_ticker_id("WBTC/USDT"), btc_usdt, "WBTC keeps own id");

    // But series-shared with BTC at the same quote.
    assert_eq!(series_canonical_ticker_id(resolve_ticker_id("CBBTC/USDT")), btc_usdt);
    assert_eq!(series_canonical_ticker_id(resolve_ticker_id("WBTC/USDT")), btc_usdt);
    assert_eq!(series_canonical_ticker_id(resolve_ticker_id("CBBTC-USDC")), btc_usdc);
    assert_eq!(series_canonical_ticker_id(resolve_ticker_id("WBTC-USDC")), btc_usdc);

    // cbBTC still resolves to its own (Coinbase BTC) asset, not Bitcoin.
    assert!(
        base_name_of("CBBTC/USDC").to_lowercase().contains("coinbase"),
        "cbBTC base must be its own asset"
    );
}

/// CAT-1 fungible aliases collapse to a SINGLE ticker_id via the CSV alias
/// column — BOTH legs (base and quote).
#[test]
fn cat1_aliases_single_id() {
    use nxr_sdk::resolve_ticker_id;

    assert_eq!(resolve_ticker_id("MATIC/USDT"), resolve_ticker_id("POL/USDT"));
    assert_eq!(resolve_ticker_id("USDT0/USDC"), resolve_ticker_id("USDT/USDC"));
    assert_eq!(resolve_ticker_id("BTC/USDT0"), resolve_ticker_id("BTC/USDT"));
    // DAI un-aliased from USDS 2026-07-08: pyth publishes DISTINCT DAI/USD
    // and USDS/USD pegs (~5 bps basis) and BTR stable pools price the two
    // tokens separately on-chain - DAI now owns crypto-assets.csv id 04801.
    assert_ne!(resolve_ticker_id("ETH/DAI"), resolve_ticker_id("ETH/USDS"));
    assert_ne!(resolve_ticker_id("DAI/USD"), resolve_ticker_id("USDS/USD"));
    assert_eq!(resolve_ticker_id("WETH/USDT"), resolve_ticker_id("ETH/USDT"));
    assert_eq!(resolve_ticker_id("WSOL/USDT"), resolve_ticker_id("SOL/USDT"));
}

/// CAT-2 wraps: distinct id, shared BTC series. cbETH stays fully distinct
/// (yield LST — neither CAT-1 nor CAT-2 series-shared).
#[test]
fn cat2_distinct_id_shared_series() {
    use nxr_sdk::{resolve_ticker_id, series_canonical_ticker_id};

    let btc_usdt = id_of("BTC/USDT");
    for wrap in ["WBTC/USDT", "CBBTC/USDT", "TBTC/USDT", "BTCB/USDT", "BBTC/USDT"] {
        let wid = resolve_ticker_id(wrap);
        assert_ne!(wid, btc_usdt, "{wrap} must keep a distinct exposed id");
        assert_eq!(
            series_canonical_ticker_id(wid),
            btc_usdt,
            "{wrap} must series-share BTC/USDT"
        );
    }

    // cbETH (yield LST) is fully distinct: not CAT-1, not series-shared.
    let cbeth = resolve_ticker_id("cbETH/USDT");
    assert_ne!(cbeth, id_of("ETH/USDT"), "cbETH must not share ETH id");
    assert_eq!(
        series_canonical_ticker_id(cbeth),
        cbeth,
        "cbETH must NOT be series-shared to BTC or ETH"
    );
}

/// Yield/LST wrappers must NOT collapse onto the canonical ETH index.
#[test]
fn cbeth_distinct_from_eth_index() {
    let eth = id_of("ETH/USDT");
    let cbeth = id_of("cbETH/USDT");
    assert_ne!(cbeth, eth, "cbETH/USDT must not share ETH/USDT id");
    assert!(
        base_name_of("cbETH/USDT").to_lowercase().contains("staked")
            || base_name_of("cbETH/USDT").to_lowercase().contains("coinbase"),
        "cbETH base must resolve to cbETH asset, not ethereum"
    );
}

#[test]
fn stablecoin_audit_2026_07_21_new_oracle_symbols_resolve_strictly() {
    // Mirrors crypto/src/bin/oracle.rs's boot-time strict resolution
    // (try_resolve_ticker_id, exit(78) on None) for every symbol newly wired
    // into oracles.providers.pyth.symbols this pass. A None here means
    // nxr-oracle would fatal at boot.
    for sym in ["U/USD", "USDG/USD", "USDF/USD", "BFUSD/USD", "USDTB/USD"] {
        assert!(
            nxr_sdk::try_resolve_ticker_id(sym).is_some(),
            "{sym} does not resolve strictly — nxr-oracle would exit(78) at boot"
        );
    }
}
