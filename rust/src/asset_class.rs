//! Asset-class taxonomy — runtime-classifiable from YAML config.
//!
//! Promoted from triplicated `const CRYPTO_MAJORS` / `STABLE_SYMBOLS` /
//! `FX_MAJORS` in `series-factory/src/bin/{nxr_calibrate,renko_trailing,
//! mtf_sweep}.rs` (D-audit 2026-05-29).
//!
//! Inputs are READ FROM YAML (`cexs.crypto_majors`, `cexs.stablecoins`,
//! `cexs.fx_majors`) — operator mandate: no hardcoded lists in code.
//! When the YAML field is empty, fall through to [`DEFAULT_STABLECOINS`] /
//! [`DEFAULT_CRYPTO_MAJORS`] / [`DEFAULT_FX_MAJORS`] so the binaries still
//! work on a stripped-down config (research / unit tests).

/// Audit-frozen fallback stablecoin list. The runtime list comes from
/// `cexs.stablecoins` in `config.yml`; this only kicks in when that field
/// is absent or empty (rare — e.g. unit tests).
pub const DEFAULT_STABLECOINS: &[&str] =
    &["USDT", "USDC", "FDUSD", "BUSD", "TUSD", "DAI", "USDS", "USD1", "PYUSD",
      "USD", "USDE", "RLUSD", "USDG"];

/// Audit-frozen fallback crypto-major list.
pub const DEFAULT_CRYPTO_MAJORS: &[&str] = &["BTC", "ETH", "SOL", "BNB", "XRP"];

/// Audit-frozen fallback FX-major list (G5 + JPY/CHF/AUD/NZD/CAD).
pub const DEFAULT_FX_MAJORS: &[&str] =
    &["USD", "EUR", "GBP", "JPY", "CHF", "AUD", "NZD", "CAD"];

/// Resolve effective list: prefer YAML override, fall through to default
/// when empty. `&[String]` is the YAML shape; returns `Vec<&str>` ready
/// for `.contains(&sym)`.
pub fn effective_list<'a>(yaml: &'a [String], default: &'static [&'static str])
    -> Vec<&'a str>
{
    if yaml.is_empty() {
        default.to_vec()
    } else {
        yaml.iter().map(String::as_str).collect()
    }
}

/// Asset class bucket for renko target-bpd resolution.
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

/// Classify a `base/quote` symbol pair.
///
/// `crypto_majors`, `stables`, `fx_majors` are typically obtained from
/// [`effective_list`] applied to the YAML fields.
pub fn classify_pair(
    base: &str,
    quote: &str,
    crypto_majors: &[&str],
    stables: &[&str],
    fx_majors: &[&str],
) -> AssetClassBucket {
    let base_stable = stables.contains(&base);
    let quote_stable = stables.contains(&quote);
    let base_fx = fx_majors.contains(&base);
    let quote_fx = fx_majors.contains(&quote);

    if base_stable && quote_stable {
        return AssetClassBucket::CryptoStable;
    }
    if base_fx && quote_fx {
        return AssetClassBucket::FxMajor;
    }
    if base_fx || quote_fx {
        return AssetClassBucket::FxCross;
    }
    let base_major = crypto_majors.contains(&base);
    let quote_major = crypto_majors.contains(&quote);
    if quote_stable {
        return if base_major { AssetClassBucket::CryptoMajor } else { AssetClassBucket::CryptoAlt };
    }
    if base_major || quote_major {
        AssetClassBucket::CryptoCross
    } else {
        AssetClassBucket::Default
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn defaults_classify_correctly() {
        let cm = DEFAULT_CRYPTO_MAJORS;
        let st = DEFAULT_STABLECOINS;
        let fx = DEFAULT_FX_MAJORS;
        assert_eq!(classify_pair("BTC", "USDT", cm, st, fx), AssetClassBucket::CryptoMajor);
        assert_eq!(classify_pair("PEPE", "USDT", cm, st, fx), AssetClassBucket::CryptoAlt);
        assert_eq!(classify_pair("USDC", "USDT", cm, st, fx), AssetClassBucket::CryptoStable);
        assert_eq!(classify_pair("ETH", "BTC", cm, st, fx), AssetClassBucket::CryptoCross);
        assert_eq!(classify_pair("EUR", "USD", cm, st, fx), AssetClassBucket::FxMajor);
    }
}
