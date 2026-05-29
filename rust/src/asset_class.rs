//! Asset-class taxonomy — derived from MITCH wire bits.
//!
//! MITCH ticker_id already encodes `base_asset_class` + `quote_asset_class`
//! via 4-bit `AssetClass` enums (CR / SD / FX / PM / CM / EQ / …). There is
//! NO reason to maintain string-list duplicates of "what's a stablecoin"
//! or "what's an FX symbol" in Rust — the wire protocol is the canonical
//! source.
//!
//! The ONE thing the wire does NOT encode is "major vs alt" inside
//! `AssetClass::CR` (Binance-volume-cutoff judgment). That single list
//! lives in YAML `cexs.crypto_majors` and is the only configurable list
//! this module exposes.

use mitch::common::AssetClass;
use mitch::ticker::TickerId;

/// Asset class bucket for renko target_bpd resolution.
/// Maps MITCH wire `(base_class, quote_class)` + operator-defined
/// `crypto_majors` list → a stable string key used by
/// `CalibrationYml::target_for_class`.
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

/// Classify by MITCH wire bits + crypto-major list.
///
/// `base_sym` is needed ONLY to look up the major-vs-alt judgment within
/// `AssetClass::CR`. Stable / FX / metal / etc. classification comes from
/// the ticker's encoded `base_asset_class()` / `quote_asset_class()`.
pub fn classify_ticker(ticker: &TickerId, base_sym: &str, crypto_majors: &[&str]) -> AssetClassBucket {
    let b = ticker.base_asset_class();
    let q = ticker.quote_asset_class();
    use AssetClass::*;
    match (b, q) {
        (SD, SD) => AssetClassBucket::CryptoStable,
        (FX, FX) => AssetClassBucket::FxMajor,
        (_, FX) | (FX, _) => AssetClassBucket::FxCross,
        (CR, CR) => AssetClassBucket::CryptoCross,
        (CR, SD) | (CR, _) => {
            if crypto_majors.contains(&base_sym.to_uppercase().as_str()) {
                AssetClassBucket::CryptoMajor
            } else {
                AssetClassBucket::CryptoAlt
            }
        }
        _ => AssetClassBucket::Default,
    }
}

/// Resolve a YAML `Vec<String>` list to `Vec<&str>`, falling through to a
/// compile-time fallback only when the YAML field is empty. Used for the
/// single remaining configurable list (`cexs.crypto_majors`).
pub fn effective_list<'a>(yaml: &'a [String], default: &'static [&'static str])
    -> Vec<&'a str>
{
    if yaml.is_empty() {
        default.to_vec()
    } else {
        yaml.iter().map(String::as_str).collect()
    }
}

/// Audit-frozen fallback crypto-major list. ONLY used when
/// `cexs.crypto_majors` in YAML is empty. Operator may override per env
/// (e.g. add SUI/TON/HYPE) without rebuilding.
pub const DEFAULT_CRYPTO_MAJORS: &[&str] = &["BTC", "ETH", "SOL", "BNB", "XRP"];

#[cfg(test)]
mod tests {
    use super::*;
    use mitch::common::{AssetClass, InstrumentType};
    use mitch::ticker::TickerId;

    fn t(base: AssetClass, quote: AssetClass) -> TickerId {
        TickerId::new(InstrumentType::SPOT, base, 0, quote, 0, 0).unwrap()
    }

    #[test]
    fn wire_classification_no_string_lists() {
        let majors = DEFAULT_CRYPTO_MAJORS;
        // CR base + SD quote → major if BTC, alt otherwise
        assert_eq!(classify_ticker(&t(AssetClass::CR, AssetClass::SD), "BTC", majors), AssetClassBucket::CryptoMajor);
        assert_eq!(classify_ticker(&t(AssetClass::CR, AssetClass::SD), "PEPE", majors), AssetClassBucket::CryptoAlt);
        // SD/SD → stable
        assert_eq!(classify_ticker(&t(AssetClass::SD, AssetClass::SD), "USDC", majors), AssetClassBucket::CryptoStable);
        // CR/CR → cross
        assert_eq!(classify_ticker(&t(AssetClass::CR, AssetClass::CR), "ETH", majors), AssetClassBucket::CryptoCross);
        // FX/FX → major
        assert_eq!(classify_ticker(&t(AssetClass::FX, AssetClass::FX), "EUR", majors), AssetClassBucket::FxMajor);
    }
}
