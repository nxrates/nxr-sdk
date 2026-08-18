//! CAT-2 hidden series-share: a wrap redirects to its underlying asset's series
//! at the SHARD chokepoint, while keeping a DISTINCT exposed `ticker_id`.
//!
//! ## Two-category symbol model (operator-locked)
//!
//! - **CAT-1 — truly fungible** (MATIC→POL, USDT0→USDT, WETH→ETH,
//!   WSOL→SOL, WBNB→BNB, XBT/XXBT→BTC, XETH→ETH): folded into the canonical
//!   asset's `aliases` column in `mitch/ids/crypto-assets.csv`. The resolver
//!   collapses BOTH legs to a SINGLE MITCH `ticker_id` automatically. There is
//!   NO code path here for CAT-1 — it is pure CSV data.
//!   (DAI un-aliased from USDS 2026-07-08: pyth publishes distinct DAI/USD
//!   vs USDS/USD pegs (~5 bps basis) and BTR pools price the tokens
//!   separately — DAI owns crypto-assets.csv id 04801. Do NOT re-fold.)
//!
//! - **CAT-2 — wraps with de-peg risk** (custodial BTC wraps WBTC, cbBTC/CBBTC,
//!   TBTC, BTCB, BBTC; FX wrappers EURC, QCAD, AUDF, BRLA, JPYC, KRW1): KEEP
//!   their distinct crypto-assets.csv rows + distinct `ticker_id`s (exposed to
//!   clients, so a de-peg is observable). But share the underlying's on-disk
//!   index/renko series via the HIDDEN map below, applied at the shard-dir
//!   chokepoint only. Clients never see these in `config.yml`.
//!
//! This is intentionally NOT a resolution-time canonicalization: a CAT-2 pair
//! resolves to its OWN id, and only the shard locator redirects it to the
//! underlying's id for the SAME quote.
//!
//! ## Parity rows
//!
//! A CAT-2 row is additionally `parity` when NOTHING quotes the token anywhere,
//! so the underlying's mark IS the wrap's and the crossing resolver treats the
//! two as ONE graph node. See `docs/architecture.md` for the model and the
//! de-peg-visibility tradeoff it buys.

use mitch::common::AssetClass::{CR, FX};
use mitch::ticker::{TickerId, pack_asset as asset};

/// Wrap → underlying, as packed MITCH asset ids, plus whether the wrap prices AT
/// PARITY off the underlying. THE single declaration of both the series-share and
/// the 1:1 peg: add a wrap here and nowhere else.
///
/// The `class_id` is the FULL CSV id (`entry.id as u16`), e.g. `19201` for WBTC,
/// NOT `192`. A parity row claims that no market prices the wrap independently;
/// if one ever does, delete the row rather than keep proxying a stale parity
/// (the DAI/USDS precedent).
/// ponytail: parity assumes no feed. Ingesting a wrap's own tape while the row
/// stands would interleave it into the underlying's shards, so `registry_gate`
/// refuses to boot on that config instead of trusting the reader to notice.
const WRAPS: &[(u32, u32, bool)] = &[
    // WBTC carries real CEX depth, so it is in `cexs.assets` and the weights
    // routine marks it from its own books: series-share only, never parity.
    (asset(CR, 19201), BTC, false), // WBTC
    // The remaining custodial wraps have no off-chain book to mark from, so the
    // underlying's mark IS theirs. Listing one in `cexs.assets` while the row
    // stands is refused at boot rather than silently interleaving two tapes.
    (asset(CR, 3901), BTC, true),  // cbBTC
    (asset(CR, 17901), BTC, true), // TBTC
    (asset(CR, 2401), BTC, true),  // BTCB (Bitcoin BEP2)
    (asset(CR, 3201), BTC, true),  // BBTC (BounceBit BTC)
    // FX wrappers (BTR FX Core pool): no feed of their own, priced at parity.
    (asset(CR, 6251), asset(FX, 1301), true),  // EURC → EUR
    (asset(CR, 21601), asset(FX, 601), true),  // QCAD → CAD
    (asset(CR, 21701), asset(FX, 101), true),  // AUDF → AUD
    (asset(CR, 21801), asset(FX, 301), true),  // BRLA → BRL
    (asset(CR, 21901), asset(FX, 2001), true), // JPYC → JPY
    (asset(CR, 22001), asset(FX, 4201), true), // KRW1 → KRW
];

/// BTC (Bitcoin, 02701) — the series-canonical base of the custodial wraps.
const BTC: u32 = asset(CR, 2701);

/// The packed base asset of a `ticker_id`.
#[inline]
fn base_asset(id: u64) -> u32 {
    let t = TickerId::from_raw(id);
    asset(t.base_asset_class(), t.base_asset_id())
}

/// The underlying a parity wrap is pegged 1:1 to; any other asset unchanged.
///
/// Read by the crossing resolver so a parity wrap and its underlying are ONE
/// node: `EURC/USD` then routes over the EUR legs, carrying the EUR leg's own
/// age and confidence, instead of needing a feed nothing publishes.
#[inline]
pub fn peg_asset(a: u32) -> u32 {
    WRAPS
        .iter()
        .find(|w| w.0 == a && w.2)
        .map_or(a, |&(_, under, _)| under)
}

/// True when the pair's BASE is a parity wrap, i.e. the pair is derived from the
/// underlying's legs and observed by nobody.
///
/// Materialization gates need this: such an id's series-canonical form names the
/// UNDERLYING's series, and a gate that reads that as "this pair has a book"
/// would persist a derived tape into a native one.
#[inline]
pub fn is_parity_wrap(ticker_id: u64) -> bool {
    let base = base_asset(ticker_id);
    peg_asset(base) != base
}

/// Redirect a CAT-2 wrap `ticker_id` to its underlying's canonical series id for
/// the SAME quote (the hidden CAT-2 series-share). All other ids pass through
/// unchanged.
///
/// HOT PATH: a scan of the const table + one repack, no allocation. Called at the
/// shard-dir chokepoint (`idx_dir` / `bars_dir` + the writer's per-id map), so
/// CAT-2 data lands in / reads from the underlying's shards and a wrap can never
/// open a second on-disk series. The exposed `ticker_id` (wire/API) is left
/// DISTINCT — only the on-disk series is shared. The BASE CLASS moves too: an FX
/// wrapper crosses asset classes (CR → FX), so a base_id-only rewrite would file
/// it under a nonexistent CR id.
#[inline]
pub fn series_canonical_ticker_id(id: u64) -> u64 {
    let Some(&(_, under, _)) = WRAPS.iter().find(|w| w.0 == base_asset(id)) else {
        return id;
    };
    // Only the base asset changes; quote, instrument type and sub_type are kept.
    let t = TickerId::from_raw(id);
    let (class, class_id) = mitch::ticker::unpack_asset(under);
    TickerId::new(
        t.instrument_type(),
        class,
        class_id,
        t.quote_asset_class(),
        t.quote_asset_id(),
        t.sub_type(),
    )
    .map_or(id, |t| t.raw)
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

    /// WBTC marks from its OWN books, the other custodial wraps mark at parity
    /// off BTC. The split is what decides whether a wrap needs its own tape: a
    /// parity wrap is routed over the underlying's legs, while WBTC is expected
    /// in `cexs.assets` so the weights routine gives it an independent mark.
    #[test]
    fn only_wbtc_marks_from_its_own_book() {
        assert!(
            !is_parity_wrap(id_of("WBTC/USDT")),
            "WBTC has CEX depth: parity would proxy BTC's mark and hide a de-peg"
        );
        for w in ["CBBTC/USDT", "TBTC/USDT", "BTCB/USDT", "BBTC/USDT"] {
            assert!(is_parity_wrap(id_of(w)), "{w} has no book of its own");
            assert_eq!(
                peg_asset(base_asset(id_of(w))),
                base_asset(id_of("BTC/USDT")),
                "{w} marks at parity off BTC"
            );
        }
    }

    /// The FX wrappers: distinct exposed id (a de-peg stays visible on the wire),
    /// the UNDERLYING currency's series on disk, across asset classes.
    #[test]
    fn fx_wrappers_share_their_currency_series_but_keep_distinct_ids() {
        for (wrap, under) in [
            ("EURC/USD", "EUR/USD"),
            ("QCAD/USD", "CAD/USD"),
            ("AUDF/USD", "AUD/USD"),
            ("BRLA/USD", "BRL/USD"),
            ("JPYC/USD", "JPY/USD"),
            ("KRW1/USD", "KRW/USD"),
        ] {
            let (w, u) = (id_of(wrap), id_of(under));
            assert_ne!(w, u, "{wrap} must keep a DISTINCT exposed ticker_id");
            assert_eq!(series_canonical_ticker_id(w), u, "{wrap} series-shares {under}");
            assert_eq!(peg_asset(base_asset(w)), base_asset(u), "{wrap} pegs 1:1");
        }
    }

    /// A wrap must never open a series of its own, and the shard chokepoint is
    /// what enforces it — so pin the DIRECTORY, not just the id.
    #[test]
    fn a_wrap_never_gets_its_own_idx_or_bars_dir() {
        let root = std::path::Path::new("/data");
        for (wrap, under) in [("EURC/USD", "EUR/USD"), ("WBTC/USDT", "BTC/USDT")] {
            let (w, u) = (id_of(wrap), id_of(under));
            assert_eq!(crate::shard::idx_dir(root, w), crate::shard::idx_dir(root, u));
            assert_eq!(crate::shard::bars_dir(root, w), crate::shard::bars_dir(root, u));
            assert!(
                !crate::shard::idx_dir(root, w).ends_with(w.to_string()),
                "{wrap} must not name its own idx dir"
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
