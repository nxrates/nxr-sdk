//! Resolution invariants for the pyth oracle feed universe (nxr-oracle).
//!
//! Locks: (a) every configured oracle symbol resolves STRICTLY (no FNV
//! fallback - a miss means phantom shard ids on disk), (b) slash form ≡
//! MT4 6-char form for FX/metals so the pyth provider MERGES into the
//! MT4-fed tickers instead of forking them, (c) base assets land in the
//! intended class/id (cross-class shadowing guard: the resolver's
//! normalized-key map is last-write-wins across classes).

use nxr_sdk::try_resolve_ticker_id;

fn base_class(id: u64) -> u64 {
    (id >> 56) & 0xF
}
fn base_id(id: u64) -> u64 {
    (id >> 40) & 0xFFFF
}

const EQ: u64 = 0x0;
const FX: u64 = 0x3;
const CM: u64 = 0x4;
const CR: u64 = 0x6;

#[test]
fn oracle_fx_slash_equals_mt4_form() {
    for (slash, mt4) in [
        ("EUR/USD", "EURUSD"),
        ("GBP/USD", "GBPUSD"),
        ("AUD/USD", "AUDUSD"),
        ("NZD/USD", "NZDUSD"),
        ("USD/JPY", "USDJPY"),
        ("USD/HKD", "USDHKD"),
        ("USD/CNH", "USDCNH"),
        ("USD/CAD", "USDCAD"),
        ("USD/CHF", "USDCHF"),
        ("USD/SEK", "USDSEK"),
        ("USD/SGD", "USDSGD"),
        ("USD/NOK", "USDNOK"),
        ("USD/KRW", "USDKRW"),
        ("USD/ZAR", "USDZAR"),
        ("USD/MXN", "USDMXN"),
        ("USD/IDR", "USDIDR"),
        ("USD/BRL", "USDBRL"),
        ("USD/TRY", "USDTRY"),
        ("USD/TWD", "USDTWD"),
        ("USD/INR", "USDINR"),
        ("USD/PLN", "USDPLN"),
        ("USD/CZK", "USDCZK"),
        ("USD/HUF", "USDHUF"),
        ("USD/DKK", "USDDKK"),
        ("USD/PHP", "USDPHP"),
        ("USD/MYR", "USDMYR"),
        ("USD/RON", "USDRON"),
        ("USD/THB", "USDTHB"),
    ] {
        let a = try_resolve_ticker_id(slash).unwrap_or_else(|| panic!("{slash} unresolvable"));
        let b = try_resolve_ticker_id(mt4).unwrap_or_else(|| panic!("{mt4} unresolvable"));
        assert_eq!(a, b, "{slash} must merge with MT4 {mt4} ticker");
        assert_eq!(base_class(a), FX, "{slash} base class");
    }
}

#[test]
fn oracle_metals_slash_equals_mt4_form() {
    for (slash, mt4, cm_id) in [
        ("XAU/USD", "XAUUSD", 161),
        ("XAG/USD", "XAGUSD", 411),
        ("XPT/USD", "XPTUSD", 321),
        ("XPD/USD", "XPDUSD", 301),
    ] {
        let a = try_resolve_ticker_id(slash).unwrap_or_else(|| panic!("{slash} unresolvable"));
        let b = try_resolve_ticker_id(mt4).unwrap_or_else(|| panic!("{mt4} unresolvable"));
        assert_eq!(a, b, "{slash} must merge with MT4 {mt4} ticker");
        assert_eq!((base_class(a), base_id(a)), (CM, cm_id), "{slash} base");
    }
    // Tokenized golds stay DISTINCT from spot gold.
    let xau = try_resolve_ticker_id("XAU/USD").unwrap();
    let xaut = try_resolve_ticker_id("XAUT/USD").unwrap();
    let paxg = try_resolve_ticker_id("PAXG/USD").unwrap();
    assert_ne!(xau, xaut);
    assert_ne!(xau, paxg);
    assert_ne!(xaut, paxg);
    assert_eq!(base_class(xaut), CR);
    assert_eq!(base_class(paxg), CR);
}

#[test]
fn oracle_crypto_bases() {
    for (sym, cr_id) in [
        ("GHO/USD", 201),
        ("SUSDE/USD", 16301),
        ("WSTETH/USD", 8901),
        ("XAUT/USD", 20201),
        ("PAXG/USD", 20101),
    ] {
        let id = try_resolve_ticker_id(sym).unwrap_or_else(|| panic!("{sym} unresolvable"));
        assert_eq!((base_class(id), base_id(id)), (CR, cr_id), "{sym} base");
    }
}

#[test]
fn oracle_equity_bases() {
    // Cross-class shadowing guard: base lookup runs with class_filter=None
    // for USD-quoted pairs; these must land on the EQ rows.
    for (sym, eq_id) in [
        ("TSLA/USD", 13711),
        ("CRCL/USD", 2746),
        ("MSTR/USD", 9111),
        ("NVDA/USD", 10191),
        ("HOOD/USD", 11461),
        ("AAPL/USD", 831),
        ("GOOGL/USD", 531),
        ("MSFT/USD", 9101),
        ("INTC/USD", 6991),
        ("MU/USD", 9091),
        ("COIN/USD", 2971),
        ("AMZN/USD", 591),
        ("PLTR/USD", 10541),
        ("ORCL/USD", 10391),
        ("META/USD", 9031),
        ("SNDK/USD", 11876),
        ("EWY/USD", 7156),
        ("C3M/EUR", 716),
    ] {
        let id = try_resolve_ticker_id(sym).unwrap_or_else(|| panic!("{sym} unresolvable"));
        assert_eq!((base_class(id), base_id(id)), (EQ, eq_id), "{sym} base");
    }
}

#[test]
fn oracle_watch_commodities() {
    // XTI moved from WTI Crude's alias column to the new Titanium row
    // (2026-07-08, pyth Metal.XTI = spot titanium); WTI keeps CL/WTI/USOIL.
    for (sym, cm_id) in [
        ("XCU/USD", 101),  // Copper
        ("XTI/USD", 486),  // Titanium (NOT WTI crude)
        ("XAL/USD", 1),    // Aluminum
        ("XGR/USD", 169),  // Graphite
        ("XCO/USD", 76),   // Cobalt
        ("XLI/USD", 216),  // Lithium
        ("XNI/USD", 261),  // Nickel
        ("OGV6/USD", 168), // Gold Futures Sep 2026
        ("OGZ6/USD", 166), // Gold Futures Nov 2026
    ] {
        let id = try_resolve_ticker_id(sym).unwrap_or_else(|| panic!("{sym} unresolvable"));
        assert_eq!((base_class(id), base_id(id)), (CM, cm_id), "{sym} base");
    }
    assert_eq!(
        base_id(try_resolve_ticker_id("USOIL").unwrap()),
        521,
        "WTI crude still resolves via USOIL"
    );
}

#[test]
fn oracle_watch_indices_merge_mt4() {
    // Pyth Index.US500/US30/US100 map onto the SAME tickers the MT4 CFDs
    // already feed (aliases on the indices.csv rows).
    for (a, b) in [("US500", "SPX"), ("US30", "DJI"), ("NAS100", "US100")] {
        assert_eq!(
            try_resolve_ticker_id(a).unwrap_or_else(|| panic!("{a} unresolvable")),
            try_resolve_ticker_id(b).unwrap_or_else(|| panic!("{b} unresolvable")),
            "{a} vs {b}"
        );
    }
    assert!(try_resolve_ticker_id("DRAM/USD").is_some(), "DRAM index");
    assert!(try_resolve_ticker_id("NATGAS").is_some(), "NATGAS");
}
