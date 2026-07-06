//! Canonical synth-pair registry — SINGLE SOURCE OF TRUTH.
//!
//! Promoted from 3-way duplication (D-audit 2026-05-29):
//! - was: `core/src/synth_registry.rs::INITIAL_PAIRS`
//! - was: `series-factory/src/bin/nxr_calibrate.rs::SYNTH_PAIRS`
//! - was: `series-factory/src/bin/synth_backfill_from_idx.rs` inline tuples
//!
//! Order matters: synth_kernel returns per-pair receivers in this order,
//! and main.rs zips with downstream consumer spawn loop. New pairs append.

/// One synth-pair spec by SYMBOL. Resolution to MITCH ids is caller-side
/// (different crates use different fallback policies — core warns + skips,
/// series-factory bins panic on missing ids, etc.).
#[derive(Debug, Clone, Copy)]
pub struct SynthPairSpec {
    pub synth_sym: &'static str,
    pub base_sym: &'static str,
    pub quote_sym: &'static str,
}

/// Audit-frozen fallback synth-pair list. Only used when
/// `synths.initial_pairs` in YAML is empty (warn-logged).
/// Promoted to sdk to eliminate drift between core kernel +
/// series-factory calibrator + synth-backfill bin.
pub const DEFAULT_INITIAL_SYNTH_PAIRS: &[SynthPairSpec] = &[
    SynthPairSpec { synth_sym: "ETH/BTC", base_sym: "ETH/USDT", quote_sym: "BTC/USDT" },
    SynthPairSpec { synth_sym: "SOL/BTC", base_sym: "SOL/USDT", quote_sym: "BTC/USDT" },
    SynthPairSpec { synth_sym: "BNB/BTC", base_sym: "BNB/USDT", quote_sym: "BTC/USDT" },
    SynthPairSpec { synth_sym: "BNB/ETH", base_sym: "BNB/USDT", quote_sym: "ETH/USDT" },
    SynthPairSpec { synth_sym: "SOL/ETH", base_sym: "SOL/USDT", quote_sym: "ETH/USDT" },
];

/// Audit-frozen fallback `mtf_sweep` calibration sweep universe. Only used
/// when `pipeline.sweep.pairs` in YAML is empty (warn-logged). Promoted to
/// sdk from `series-factory/src/bin/mtf_sweep.rs::SWEEP_PAIRS` (phase 59.R3.M2).
pub const DEFAULT_SWEEP_PAIRS: &[(&str, &str)] = &[
    // Volatile USDT-quoted
    ("BTC", "USDT"),
    ("ETH", "USDT"),
    ("BNB", "USDT"),
    ("SOL", "USDT"),
    ("XAUT", "USDT"),
    // Crypto crosses (operator priority)
    ("ETH", "BTC"),
    ("BNB", "ETH"),
    ("BNB", "BTC"),
    ("SOL", "BTC"),
    ("SOL", "ETH"),
    // Stable/USDT (target = 50)
    ("USDC", "USDT"),
    ("USDE", "USDT"),
    ("USD1", "USDT"),
];

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn nonempty_and_well_formed() {
        assert!(!DEFAULT_INITIAL_SYNTH_PAIRS.is_empty());
        for p in DEFAULT_INITIAL_SYNTH_PAIRS {
            assert!(p.synth_sym.contains('/'));
            assert!(p.base_sym.contains('/'));
            assert!(p.quote_sym.contains('/'));
        }
    }
}
