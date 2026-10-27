//! Config class pins (`set_class_pins`): a symbol NXR collects as crypto names
//! its CR row on an unpinned lookup, and NOTHING else changes. Own test binary:
//! pins are process-global and additive.

use std::collections::BTreeMap;

use mitch::common::AssetClass::{self, CM, CR, EQ};
use mitch::constants::{
    COMMODITIES_DATA, CRYPTO_ASSETS_DATA, EQUITIES_DATA, FOREX_DATA, INDICES_DATA,
};
use mitch::ticker::TickerId;
use nxr_sdk::resolve::{resolve_asset_in_class, set_class_pins};
use nxr_sdk::try_resolve_ticker_id;

/// The ten `cexs.assets` tokens whose ticker an equity/commodity row also holds.
const CLASHES: [&str; 10] = ["CFG", "CVX", "DASH", "FF", "LDO", "MET", "SKY", "SOLV", "SUI", "W"];

fn base(sym: &str) -> Option<(AssetClass, u16)> {
    let t = TickerId::from_raw(try_resolve_ticker_id(sym)?);
    Some((t.base_asset_class(), t.base_asset_id()))
}

/// Every alias of every row, bare and `/USD`, the whole universe a client can
/// spell: the resolution of each must be unchanged unless it is a pinned key.
fn universe() -> BTreeMap<String, Option<u64>> {
    let mut out = BTreeMap::new();
    for data in [COMMODITIES_DATA, CRYPTO_ASSETS_DATA, EQUITIES_DATA, FOREX_DATA, INDICES_DATA] {
        for e in data {
            for a in e.aliases.split('|').filter(|a| !a.is_empty()) {
                for sym in [a.to_string(), format!("{a}/USD"), format!("{a}-USD")] {
                    let id = try_resolve_ticker_id(&sym);
                    out.insert(sym, id);
                }
            }
        }
    }
    out
}

#[test]
fn pins_move_only_the_clashing_symbols_to_cr() {
    for s in CLASHES {
        assert_ne!(base(&format!("{s}/USD")).unwrap().0, CR, "{s}/USD is a clash before pinning");
    }
    let before = universe();

    let unmapped = set_class_pins(CR, CLASHES.iter().copied().chain(["BTC", "QQQ"]));
    assert_eq!(unmapped, ["QQQ"], "QQQ has no CR row: unmapped, never pinned");

    let after = universe();
    let mut moved: Vec<String> = before
        .iter()
        .filter(|(k, v)| after[*k] != **v)
        .map(|(k, _)| k.to_ascii_uppercase())
        .collect();
    moved.sort();
    moved.dedup();
    let mut want: Vec<String> = CLASHES
        .iter()
        .flat_map(|s| [s.to_string(), format!("{s}/USD"), format!("{s}-USD")])
        .collect();
    want.sort();
    assert_eq!(moved, want, "only the pinned symbols may change id");

    for s in CLASHES {
        for sym in [s.to_string(), format!("{s}/USD"), format!("{s}-USD"), format!("{s}/USDT")] {
            assert_eq!(base(&sym).unwrap().0, CR, "{sym} names the crypto row");
        }
    }
    assert_eq!(base("SUI/USD").unwrap(), (CR, 20301));
    assert_eq!(base("W/USD").unwrap(), (CR, 19101));
    // The homonyms stay reachable by class and by their other aliases.
    assert_eq!(resolve_asset_in_class("SUI", 0.95, EQ).unwrap().asset.class_id, 13131);
    assert_eq!(base("WHEAT/USD").unwrap(), (CM, 491));
    assert_eq!(base("ZW").unwrap(), (CM, 491));
    // Not pinned: unchanged (Soybeans keeps `S`, the equity keeps `QQQ`).
    assert_eq!(base("S/USD").unwrap().0, CM);
    assert_eq!(base("QQQ/USD").unwrap().0, EQ);
}
