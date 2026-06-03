//! CAT-2 hidden series-share: third-party custodial BTC wraps redirect to
//! BTC's series at the SHARD chokepoint, while keeping a DISTINCT exposed
//! `ticker_id`.
//!
//! ## Two-category symbol model (operator-locked)
//!
//! - **CAT-1 — truly fungible** (MATIC→POL, USDT0→USDT, DAI→USDS, WETH→ETH,
//!   WSOL→SOL, WBNB→BNB, XBT/XXBT→BTC, XETH→ETH): folded into the canonical
//!   asset's `aliases` column in `mitch/ids/crypto-assets.csv`. The resolver
//!   collapses BOTH legs to a SINGLE MITCH `ticker_id` automatically. There is
//!   NO code path here for CAT-1 — it is pure CSV data.
//!
//! - **CAT-2 — custodial BTC wraps with de-peg risk** (WBTC, cbBTC/CBBTC,
//!   TBTC, BTCB, BBTC): KEEP their distinct crypto-assets.csv rows + distinct
//!   `ticker_id`s (exposed to clients, so a de-peg is observable). But share
//!   BTC's on-disk index/renko series via the HIDDEN map below, applied at the
//!   shard-dir chokepoint only. Clients never see these in `config.yml`.
//!
//! This is intentionally NOT a resolution-time canonicalization: a CAT-2 pair
//! resolves to its OWN id, and only the shard locator redirects it to BTC's id
//! for the SAME quote.

use mitch::common::AssetClass;

/// CAT-2 custodial-BTC-wrap crypto `class_id`s. NOTE: the MITCH `class_id` is
/// the FULL CSV id (`entry.id as u16`), e.g. `19201` for WBTC — NOT `192`.
/// From `mitch/ids/crypto-assets.csv`: WBTC=19201, cbBTC=03901, TBTC=17901,
/// BTCB(Bitcoin BEP2)=02401, BBTC(BounceBit BTC)=03201. All asset class `CR`.
/// Series-share to BTC, keeping the SAME quote leg.
const CAT2_BTC_WRAP_CLASS_IDS: &[u16] = &[19201, 3901, 17901, 2401, 3201];

/// BTC (Bitcoin, 02701) crypto `class_id` — the series-canonical base.
const BTC_CLASS_ID: u16 = 2701;

// MITCH ticker_id bit layout (see mitch::ticker::TickerId::new):
//   [60-63] instrument_type   [56-59] base_class   [40-55] base_id
//   [36-39] quote_class       [20-35] quote_id     [0-19]  sub_type
const BASE_CLASS_SHIFT: u64 = 56;
const BASE_ID_SHIFT: u64 = 40;
const BASE_CLASS_MASK: u64 = 0xF << BASE_CLASS_SHIFT;
const BASE_ID_MASK: u64 = 0xFFFF << BASE_ID_SHIFT;

/// Redirect a CAT-2 custodial-BTC-wrap `ticker_id` to BTC's canonical series id
/// for the SAME quote (the hidden CAT-2 series-share). All other ids pass
/// through unchanged.
///
/// HOT PATH: pure bit ops + a 5-element const compare, no allocation. Called
/// at the shard-dir chokepoint (`idx_dir` / `bars_dir` + the writer's per-id
/// map), so CAT-2 data lands in / reads from BTC's shards. The exposed
/// `ticker_id` (wire/API) is left DISTINCT — only the on-disk series is shared.
#[inline]
pub fn series_canonical_ticker_id(id: u64) -> u64 {
    let base_class = ((id & BASE_CLASS_MASK) >> BASE_CLASS_SHIFT) as u8;
    if base_class != AssetClass::CR as u8 {
        return id;
    }
    let base_id = ((id & BASE_ID_MASK) >> BASE_ID_SHIFT) as u16;
    if CAT2_BTC_WRAP_CLASS_IDS.contains(&base_id) {
        // Rewrite ONLY the base_id field to BTC; quote/instrument/sub_type kept.
        (id & !BASE_ID_MASK) | ((BTC_CLASS_ID as u64) << BASE_ID_SHIFT)
    } else {
        id
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::resolve::resolve_ticker;
    use mitch::common::InstrumentType;

    fn id_of(sym: &str) -> u64 {
        resolve_ticker(sym, InstrumentType::SPOT).unwrap().ticker.id
    }

    #[test]
    fn cat2_wraps_share_btc_series_but_keep_distinct_id() {
        let btc_usdt = id_of("BTC/USDT");
        for w in ["WBTC/USDT", "CBBTC/USDT", "TBTC/USDT", "BTCB/USDT", "BBTC/USDT"] {
            let wid = id_of(w);
            assert_ne!(wid, btc_usdt, "{w} must keep a DISTINCT exposed ticker_id");
            assert_eq!(
                series_canonical_ticker_id(wid),
                btc_usdt,
                "{w} must series-share BTC/USDT"
            );
        }
    }

    #[test]
    fn btc_and_non_cat2_pass_through() {
        let btc_usdt = id_of("BTC/USDT");
        assert_eq!(series_canonical_ticker_id(btc_usdt), btc_usdt);
        let eth_usdt = id_of("ETH/USDT");
        assert_eq!(series_canonical_ticker_id(eth_usdt), eth_usdt);
        let cbeth = id_of("cbETH/USDT");
        assert_eq!(series_canonical_ticker_id(cbeth), cbeth, "cbETH not series-shared");
    }
}
