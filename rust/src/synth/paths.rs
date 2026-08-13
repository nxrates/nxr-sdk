//! Signed-leg path types.
//!
//! A path is `synth = Π leg_i^{exp_i}` with `exp_i ∈ {+1, -1}`:
//! - `exp = +1` → multiply by the leg's price
//! - `exp = -1` → divide by it (also swaps bid↔ask on tick composition)
//!
//! Which legs a pair needs is NOT declared here: it is derived from the live
//! primaries by [`super::cross::CrossGraph`], the single crossing resolver.
//! These types are what a resolved route is handed to the composition maths as
//! (`tick`, `ohlc`, `bar`, `compose`).

/// Signed leg: `(symbol, exponent)`. Exponent must be `+1` or `-1`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Leg {
    /// Leg symbol (slash format, e.g. `BTC/USDT`).
    pub sym: String,
    /// Signed exponent in the multiplicative composition (`+1` or `-1`).
    pub exp: i8,
}

impl Leg {
    /// Construct a leg from `(sym, exp)`.
    #[inline]
    pub fn new(sym: impl Into<String>, exp: i8) -> Self {
        debug_assert!(
            exp == 1 || exp == -1,
            "Leg exponent must be \u{00b1}1, got {exp}"
        );
        Self {
            sym: sym.into(),
            exp,
        }
    }
}

/// A synthetic symbol = ordered product of signed legs.
#[derive(Debug, Clone)]
pub struct SynthPath {
    /// Synth symbol name (e.g. `ETH/BTC`).
    pub sym: String,
    /// Ordered legs. Empty legs vector = trivial identity (1, 1, 1).
    pub legs: Vec<Leg>,
}

/// Normalize any text symbol form to slash. Dash \u{2192} slash. No-op on slash.
/// Numeric / hex prefixed forms are returned as-is (caller resolves separately).
#[inline]
pub fn normalize_to_slash(s: &str) -> String {
    if s.contains('-') {
        s.replace('-', "/")
    } else {
        s.to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dash_normalizes_to_slash() {
        assert_eq!(normalize_to_slash("BTC-USDT"), "BTC/USDT");
        assert_eq!(normalize_to_slash("BTC/USDT"), "BTC/USDT");
        assert_eq!(
            normalize_to_slash("0x060A8D644C100000"),
            "0x060A8D644C100000"
        );
    }
}
