//! `data/class-pins.csv`: a token NXR collects as crypto names its CR row on a
//! class-blind lookup, and its homonyms stay reachable. That NOTHING else moves
//! is proven by diffing every alias spelling against the previous build.

use mitch::common::AssetClass::{self, CM, CR, EQ};
use mitch::ticker::TickerId;
use nxr_sdk::resolve::{class_pins, resolve_asset_in_class, unpinned_clashes};
use nxr_sdk::try_resolve_ticker_id;

const CLASHES: [&str; 10] = ["CFG", "CVX", "DASH", "FF", "LDO", "MET", "SKY", "SOLV", "SUI", "W"];

fn base(sym: &str) -> (AssetClass, u16) {
    let t = TickerId::from_raw(try_resolve_ticker_id(sym).unwrap_or_else(|| panic!("{sym} resolves")));
    (t.base_asset_class(), t.base_asset_id())
}

#[test]
fn pinned_tokens_name_their_crypto_row() {
    let pins: Vec<String> = class_pins().into_iter().map(|(k, c)| format!("{k}:{c:?}")).collect();
    assert_eq!(pins.len(), CLASHES.len());
    for s in CLASHES {
        assert!(pins.contains(&format!("{}:CR", s.to_lowercase())), "{s} pinned CR");
        for sym in [s.to_string(), format!("{s}/USD"), format!("{s}-USD"), format!("{s}/EUR"), format!("{s}/USDT")] {
            assert_eq!(base(&sym).0, CR, "{sym} names the crypto row");
        }
    }
    assert_eq!(base("SUI/USD"), (CR, 20301));
    assert_eq!(base("W/USD"), (CR, 19101));
}

#[test]
fn homonyms_stay_reachable() {
    assert_eq!(resolve_asset_in_class("SUI", 0.95, EQ).unwrap().asset.class_id, 13131);
    assert_eq!(base("WHEAT/USD"), (CM, 491));
    assert_eq!(base("ZW"), (CM, 491));
    // A stripped broker suffix is an equity spelling: no pin.
    assert_eq!(base("CVX.US").0, EQ);
    // Not pinned: unchanged.
    assert_eq!(base("S/USD").0, CM);
    assert_eq!(base("QQQ/USD").0, EQ);
}

#[test]
fn an_unpinned_clash_is_reported() {
    // Sonic (`S`) loses to Soybeans: a config listing it as crypto is refused.
    assert_eq!(unpinned_clashes(CR, ["S", "SUI", "BTC", "QQQ"]), ["S"]);
}
