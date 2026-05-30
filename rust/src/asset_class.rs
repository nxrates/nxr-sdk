//! Asset-class taxonomy — derived from MITCH wire bits + operator lists.
//!
//! MITCH ticker_id encodes `base_asset_class` + `quote_asset_class` via
//! 4-bit `AssetClass` enums (CR / SD / FX / PM / CM / EQ / …). The wire is
//! the canonical source for the COARSE classification.
//!
//! What the wire does NOT (and should not) encode:
//!   1. "major vs alt" inside `AssetClass::CR` (Binance-volume-cutoff
//!      judgment) — operator policy.
//!   2. "stablecoin" inside `AssetClass::CR` — USDT/USDC/DAI/USDS/… all
//!      live in `crypto-assets.csv` (correctly: they ARE crypto assets
//!      on-chain). MITCH does not have a "Stablecoin" class and would
//!      not be the right layer to add one (the same ERC-20 token both
//!      trades against USD and pegs to it; class is the issuance
//!      taxonomy, not the trading-microstructure taxonomy).
//!   3. "FX major" inside `AssetClass::FX` — same shape as crypto-major.
//!
//! All three judgment lists live in YAML `cexs.{crypto_majors,
//! stablecoins, fx_majors}`. This module exposes audit-frozen fallbacks
//! that callsites use when the YAML field is empty (warn-logged).

use mitch::common::AssetClass;
use mitch::ticker::TickerId;

/// Asset class bucket for renko target_bpd resolution.
/// Maps MITCH wire `(base_class, quote_class)` + operator lists → a
/// stable string key consumed by `CalibrationYml::target_for_class`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum AssetClassBucket {
    CryptoMajor,
    CryptoAlt,
    CryptoStable,
    CryptoCross,
    FxMajor,
    FxCross,
    Default,
}

impl AssetClassBucket {
    pub fn as_key(&self) -> &'static str {
        match self {
            Self::CryptoMajor => "crypto_major",
            Self::CryptoAlt => "crypto_alt",
            Self::CryptoStable => "crypto_stable",
            Self::CryptoCross => "crypto_cross",
            Self::FxMajor => "fx_major",
            Self::FxCross => "fx_cross",
            Self::Default => "default",
        }
    }
}

/// Case-insensitive membership check against a `&[&str]` operator list.
#[inline]
fn contains_ci(list: &[&str], sym: &str) -> bool {
    let up = sym.to_uppercase();
    list.iter().any(|s| s.eq_ignore_ascii_case(&up))
}

/// Classify by MITCH wire bits + operator lists.
///
/// - `base_sym` / `quote_sym` are the human symbols (e.g. "BTC", "USDT").
///   Needed for the within-class judgments (major-vs-alt for CR,
///   major-vs-cross for FX, stablecoin detection within CR).
/// - `crypto_majors` — top-cap CR symbols (BTC/ETH/SOL/...). Cross with
///   a stablecoin quote ⇒ `CryptoMajor`, else `CryptoAlt`.
/// - `stablecoins` — pegged CR symbols (USDT/USDC/DAI/USDS/...).
///   Pair where BOTH legs are stable ⇒ `CryptoStable` (renko target ~50).
/// - `fx_majors` — top-tier FX symbols (USD/EUR/JPY/GBP/CHF/...). Both
///   legs major ⇒ `FxMajor`, else any FX-touching pair ⇒ `FxCross`.
///
/// Rules (precedence top-to-bottom):
///   1. FX/FX → fx_major if both ∈ fx_majors, else fx_cross.
///   2. Any FX touch (FX,*) | (*,FX) → fx_cross.
///   3. CR base + stable quote (BOTH ∈ stablecoins) AND base also stable
///      → crypto_stable.
///   4. CR base + CR quote where base is NOT stable but quote IS stable
///      → crypto_major (if base ∈ majors) else crypto_alt.
///      (covers BTC/USDT, ETH/USDC, PEPE/USDT, …)
///   5. CR/CR otherwise → crypto_cross. (covers ETH/BTC, SOL/ETH, …)
///   6. Anything else → default.
pub fn classify_ticker(
    ticker: &TickerId,
    base_sym: &str,
    quote_sym: &str,
    crypto_majors: &[&str],
    stablecoins: &[&str],
    fx_majors: &[&str],
) -> AssetClassBucket {
    let b = ticker.base_asset_class();
    let q = ticker.quote_asset_class();
    use AssetClass::*;
    match (b, q) {
        (FX, FX) => {
            if contains_ci(fx_majors, base_sym) && contains_ci(fx_majors, quote_sym) {
                AssetClassBucket::FxMajor
            } else {
                AssetClassBucket::FxCross
            }
        }
        (FX, _) | (_, FX) => AssetClassBucket::FxCross,
        (CR, CR) => {
            let base_stable = contains_ci(stablecoins, base_sym);
            let quote_stable = contains_ci(stablecoins, quote_sym);
            if base_stable && quote_stable {
                AssetClassBucket::CryptoStable
            } else if quote_stable {
                if contains_ci(crypto_majors, base_sym) {
                    AssetClassBucket::CryptoMajor
                } else {
                    AssetClassBucket::CryptoAlt
                }
            } else {
                // Crypto/crypto cross (e.g. ETH/BTC, SOL/ETH).
                AssetClassBucket::CryptoCross
            }
        }
        _ => AssetClassBucket::Default,
    }
}

/// Classify a `<BASE>/<QUOTE>` pair string + numeric `ticker_id` to a
/// bucket. Single SDK home for the pattern previously inlined in
/// `series-factory::bin::nxr_calibrate::classify_pair` (and elsewhere).
///
/// Uppercase-folds base/quote before delegating to [`classify_ticker`]
/// (operator lists are case-insensitive but the wire-class check is
/// driven purely by `TickerId` bits — so this only matters for the
/// `contains_ci` lookup, which is already case-insensitive). Returns
/// [`AssetClassBucket::Default`] when the pair is malformed.
pub fn bucket_for_pair(
    pair: &str,
    ticker_id: u64,
    crypto_majors: &[&str],
    stablecoins: &[&str],
    fx_majors: &[&str],
) -> AssetClassBucket {
    let Some((base, quote)) = crate::ticker::split_pair(pair) else {
        return AssetClassBucket::Default;
    };
    let base_uc = base.to_uppercase();
    let quote_uc = quote.to_uppercase();
    let ticker = TickerId::from_raw(ticker_id);
    classify_ticker(&ticker, &base_uc, &quote_uc, crypto_majors, stablecoins, fx_majors)
}

/// Resolve a YAML `Vec<String>` list to `Vec<&str>`, falling back to a
/// compile-time list only when the YAML field is empty. Callers should
/// `warn!` on empty YAML so operators notice cfg drift (see
/// `nxr_calibrate.rs`).
pub fn effective_list<'a>(yaml: &'a [String], default: &'static [&'static str])
    -> Vec<&'a str>
{
    if yaml.is_empty() {
        default.to_vec()
    } else {
        yaml.iter().map(String::as_str).collect()
    }
}

/// Audit-frozen fallback crypto-major list. Only used when
/// `cexs.crypto_majors` in YAML is empty (warn-logged).
pub const DEFAULT_CRYPTO_MAJORS: &[&str] = &["BTC", "ETH", "SOL", "BNB", "XRP"];

/// Audit-frozen fallback stablecoin list. Mirrors the Tier-1 set in
/// `config.yml::cexs.stablecoins`. Used only when YAML empty (warn).
/// `EURC` intentionally omitted from the fallback — Tier-1 is USD-pegged.
pub const DEFAULT_STABLECOINS: &[&str] = &[
    "USDT", "USDC", "FDUSD", "BUSD", "TUSD", "DAI",
    "PYUSD", "USDD", "USDS", "USD1", "USDe",
];

/// Audit-frozen fallback FX-major list. Used when `cexs.fx_majors`
/// empty (warn). DXY basket + CHF (peer of EUR for SNB-correlated flow).
pub const DEFAULT_FX_MAJORS: &[&str] = &["USD", "EUR", "JPY", "GBP", "CAD", "AUD", "CHF"];

#[cfg(test)]
mod tests {
    use super::*;
    use mitch::common::{AssetClass, InstrumentType};
    use mitch::ticker::TickerId;

    fn t(base: AssetClass, quote: AssetClass) -> TickerId {
        TickerId::new(InstrumentType::SPOT, base, 0, quote, 0, 0).unwrap()
    }

    fn classify(ticker: &TickerId, base: &str, quote: &str) -> AssetClassBucket {
        classify_ticker(
            ticker, base, quote,
            DEFAULT_CRYPTO_MAJORS, DEFAULT_STABLECOINS, DEFAULT_FX_MAJORS,
        )
    }

    #[test]
    fn crypto_major_vs_stable_quote() {
        // BTC/USDT — major base, stable quote → crypto_major.
        assert_eq!(classify(&t(AssetClass::CR, AssetClass::CR), "BTC", "USDT"), AssetClassBucket::CryptoMajor);
        // ETH/USDC — major base, stable quote.
        assert_eq!(classify(&t(AssetClass::CR, AssetClass::CR), "ETH", "USDC"), AssetClassBucket::CryptoMajor);
        // PEPE/USDT — alt base, stable quote.
        assert_eq!(classify(&t(AssetClass::CR, AssetClass::CR), "PEPE", "USDT"), AssetClassBucket::CryptoAlt);
    }

    #[test]
    fn stable_stable_pair() {
        // USDC/USDT, USDe/USDT, USDS/USDT, USD1/USDT — both stable.
        assert_eq!(classify(&t(AssetClass::CR, AssetClass::CR), "USDC", "USDT"), AssetClassBucket::CryptoStable);
        assert_eq!(classify(&t(AssetClass::CR, AssetClass::CR), "USDe", "USDT"), AssetClassBucket::CryptoStable);
        assert_eq!(classify(&t(AssetClass::CR, AssetClass::CR), "USDS", "USDT"), AssetClassBucket::CryptoStable);
        assert_eq!(classify(&t(AssetClass::CR, AssetClass::CR), "USD1", "USDT"), AssetClassBucket::CryptoStable);
    }

    #[test]
    fn crypto_cross() {
        // ETH/BTC, SOL/ETH, BNB/BTC — crypto/crypto neither side stable.
        assert_eq!(classify(&t(AssetClass::CR, AssetClass::CR), "ETH", "BTC"), AssetClassBucket::CryptoCross);
        assert_eq!(classify(&t(AssetClass::CR, AssetClass::CR), "SOL", "ETH"), AssetClassBucket::CryptoCross);
    }

    #[test]
    fn fx_major_vs_cross() {
        // EUR/USD, GBP/USD, USD/JPY — both ∈ fx_majors → fx_major.
        assert_eq!(classify(&t(AssetClass::FX, AssetClass::FX), "EUR", "USD"), AssetClassBucket::FxMajor);
        assert_eq!(classify(&t(AssetClass::FX, AssetClass::FX), "USD", "JPY"), AssetClassBucket::FxMajor);
        // USD/TRY, USD/MXN — major + EM → fx_cross.
        assert_eq!(classify(&t(AssetClass::FX, AssetClass::FX), "USD", "TRY"), AssetClassBucket::FxCross);
        assert_eq!(classify(&t(AssetClass::FX, AssetClass::FX), "USD", "MXN"), AssetClassBucket::FxCross);
    }

    #[test]
    fn fx_touching_other_class() {
        // EQ/FX e.g. SPY/USD → fx_cross.
        assert_eq!(classify(&t(AssetClass::EQ, AssetClass::FX), "SPY", "USD"), AssetClassBucket::FxCross);
        // CM/FX e.g. XAU/USD → fx_cross.
        assert_eq!(classify(&t(AssetClass::CM, AssetClass::FX), "XAU", "USD"), AssetClassBucket::FxCross);
    }

    #[test]
    fn case_insensitive_stable_detection() {
        // `USDe` (mixed case in mitch CSV) — detection must be CI.
        assert_eq!(classify(&t(AssetClass::CR, AssetClass::CR), "usde", "usdt"), AssetClassBucket::CryptoStable);
    }
}
