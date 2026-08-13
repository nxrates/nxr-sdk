//! Triangulation rule registry — SINGLE SOURCE OF TRUTH for `core::triangulator`.
//!
//! Two families:
//!
//! 1. **Synthesis** ([`SYNTHESIS_RULES`]): build a *new* ticker from two legs via
//!    `mid_out = (inv1 ? 1/mid1 : mid1) × (inv2 ? 1/mid2 : mid2)`. Output is
//!    appended to the snapshot map as a virtual ticker.
//!
//! 2. **Injection** ([`INJECTION_RULES`]): build a triangulated price + inject it
//!    as a synthetic provider into an existing target ticker's VWAP state.
//!    Provider id = `SYNTH_BASE + offset` (offset = registry position).
//!
//! ## Relation to [`crate::synth::cross`]
//!
//! `cross` RESOLVES any pair on demand and is the single authority on whether a
//! pair is composable. These rules only say which outputs the aggregator cycle
//! MATERIALIZES as tickers of their own, which is a disk and cardinality choice.
//! A rule must never contradict the resolver;
//! `every_synthesis_rule_is_routable_from_its_own_legs` pins that.
//!
//! ## Leg symbol convention
//!
//! Legs are resolved via `crate::resolve_ticker_id`. FX legs use the 6-char
//! upper-alpha provider form (`USDJPY`, `EURUSD`) — `resolve_ticker_id` short-circuits
//! to a direct 3+3 split (`USDJPY` → `USD/JPY`, never `JPY/USD`). Crypto legs use
//! the slash form (`BTC/USDT`).
//!
//! ## Adding a rule
//!
//! Append to the appropriate const slice. `core::triangulator::build_rules` and
//! `build_injection_rules` resolve symbols → ids at startup; no further code change
//! needed in the core crate.

/// Synthesis rule (pure data — caller resolves syms → ids).
///
/// `out = (leg1 [inverted]) × (leg2 [inverted])`.
#[derive(Debug, Clone, Copy)]
pub struct SynthesisRuleSpec {
    pub out_sym: &'static str,
    pub leg1_sym: &'static str,
    pub leg1_inv: bool,
    pub leg2_sym: &'static str,
    pub leg2_inv: bool,
}

/// Injection rule (pure data — caller resolves syms → ids + maps offset → provider_id).
#[derive(Debug, Clone, Copy)]
pub struct InjectionRuleSpec {
    pub target_sym: &'static str,
    pub leg1_sym: &'static str,
    pub leg1_inv: bool,
    pub leg2_sym: &'static str,
    pub leg2_inv: bool,
    /// Provider id offset relative to `SYNTH_BASE` (caller adds the base).
    pub provider_offset: u16,
}

// ── Synthesis rules ─────────────────────────────────────────────────────────

/// All currently-active synthesis rules. Order does NOT affect correctness:
/// aggregator emit-paths key by `ticker_id` so per-rule output is
/// set-equivalent.
///
/// Sections:
/// 1. USDT/<fiat> = USDT/USD × USD<FIAT>          (26 entries)
/// 2. <crypto>/USD = <crypto>/USDT × USDT/USD     (11 majors)
/// 3. USDT/USDC = USDT/USD × (USDC/USD)⁻¹         (1 entry)
/// 4. <crypto>/EUR, <crypto>/GBP via inverse FX   (14 entries)
/// 5. BTC + ETH /AUD /CHF crosses                 (4 entries)
pub const SYNTHESIS_RULES: &[SynthesisRuleSpec] = &[
    // ── USDT/<EM fiat> = USDT/USD × USD/<CCY> ──
    SynthesisRuleSpec { out_sym: "USDT/JPY", leg1_sym: "USDT/USD", leg1_inv: false, leg2_sym: "USDJPY", leg2_inv: false },
    SynthesisRuleSpec { out_sym: "USDT/MXN", leg1_sym: "USDT/USD", leg1_inv: false, leg2_sym: "USDMXN", leg2_inv: false },
    SynthesisRuleSpec { out_sym: "USDT/SGD", leg1_sym: "USDT/USD", leg1_inv: false, leg2_sym: "USDSGD", leg2_inv: false },
    SynthesisRuleSpec { out_sym: "USDT/TRY", leg1_sym: "USDT/USD", leg1_inv: false, leg2_sym: "USDTRY", leg2_inv: false },
    SynthesisRuleSpec { out_sym: "USDT/HKD", leg1_sym: "USDT/USD", leg1_inv: false, leg2_sym: "USDHKD", leg2_inv: false },
    SynthesisRuleSpec { out_sym: "USDT/ZAR", leg1_sym: "USDT/USD", leg1_inv: false, leg2_sym: "USDZAR", leg2_inv: false },
    SynthesisRuleSpec { out_sym: "USDT/CNH", leg1_sym: "USDT/USD", leg1_inv: false, leg2_sym: "USDCNH", leg2_inv: false },
    SynthesisRuleSpec { out_sym: "USDT/INR", leg1_sym: "USDT/USD", leg1_inv: false, leg2_sym: "USDINR", leg2_inv: false },
    SynthesisRuleSpec { out_sym: "USDT/NOK", leg1_sym: "USDT/USD", leg1_inv: false, leg2_sym: "USDNOK", leg2_inv: false },
    SynthesisRuleSpec { out_sym: "USDT/SEK", leg1_sym: "USDT/USD", leg1_inv: false, leg2_sym: "USDSEK", leg2_inv: false },
    SynthesisRuleSpec { out_sym: "USDT/DKK", leg1_sym: "USDT/USD", leg1_inv: false, leg2_sym: "USDDKK", leg2_inv: false },
    SynthesisRuleSpec { out_sym: "USDT/PLN", leg1_sym: "USDT/USD", leg1_inv: false, leg2_sym: "USDPLN", leg2_inv: false },
    SynthesisRuleSpec { out_sym: "USDT/HUF", leg1_sym: "USDT/USD", leg1_inv: false, leg2_sym: "USDHUF", leg2_inv: false },
    SynthesisRuleSpec { out_sym: "USDT/CZK", leg1_sym: "USDT/USD", leg1_inv: false, leg2_sym: "USDCZK", leg2_inv: false },
    SynthesisRuleSpec { out_sym: "USDT/BRL", leg1_sym: "USDT/USD", leg1_inv: false, leg2_sym: "USDBRL", leg2_inv: false },
    SynthesisRuleSpec { out_sym: "USDT/KRW", leg1_sym: "USDT/USD", leg1_inv: false, leg2_sym: "USDKRW", leg2_inv: false },
    SynthesisRuleSpec { out_sym: "USDT/AED", leg1_sym: "USDT/USD", leg1_inv: false, leg2_sym: "USDAED", leg2_inv: false },
    SynthesisRuleSpec { out_sym: "USDT/PHP", leg1_sym: "USDT/USD", leg1_inv: false, leg2_sym: "USDPHP", leg2_inv: false },
    SynthesisRuleSpec { out_sym: "USDT/THB", leg1_sym: "USDT/USD", leg1_inv: false, leg2_sym: "USDTHB", leg2_inv: false },
    SynthesisRuleSpec { out_sym: "USDT/IDR", leg1_sym: "USDT/USD", leg1_inv: false, leg2_sym: "USDIDR", leg2_inv: false },
    SynthesisRuleSpec { out_sym: "USDT/MYR", leg1_sym: "USDT/USD", leg1_inv: false, leg2_sym: "USDMYR", leg2_inv: false },
    SynthesisRuleSpec { out_sym: "USDT/NGN", leg1_sym: "USDT/USD", leg1_inv: false, leg2_sym: "USDNGN", leg2_inv: false },
    SynthesisRuleSpec { out_sym: "USDT/VND", leg1_sym: "USDT/USD", leg1_inv: false, leg2_sym: "USDVND", leg2_inv: false },
    SynthesisRuleSpec { out_sym: "USDT/SAR", leg1_sym: "USDT/USD", leg1_inv: false, leg2_sym: "USDSAR", leg2_inv: false },
    SynthesisRuleSpec { out_sym: "USDT/QAR", leg1_sym: "USDT/USD", leg1_inv: false, leg2_sym: "USDQAR", leg2_inv: false },
    SynthesisRuleSpec { out_sym: "USDT/EGP", leg1_sym: "USDT/USD", leg1_inv: false, leg2_sym: "USDEGP", leg2_inv: false },

    // ── <crypto>/USD = <crypto>/USDT × USDT/USD ──
    // Synthesises a USD-quoted reference from CEX USDT liquidity. SOLE source:
    // no crypto CFD feed exists (the MT4 forwarder was removed 2026-08-10).
    SynthesisRuleSpec { out_sym: "BTC/USD",  leg1_sym: "BTC/USDT",  leg1_inv: false, leg2_sym: "USDT/USD", leg2_inv: false },
    SynthesisRuleSpec { out_sym: "ETH/USD",  leg1_sym: "ETH/USDT",  leg1_inv: false, leg2_sym: "USDT/USD", leg2_inv: false },
    SynthesisRuleSpec { out_sym: "SOL/USD",  leg1_sym: "SOL/USDT",  leg1_inv: false, leg2_sym: "USDT/USD", leg2_inv: false },
    SynthesisRuleSpec { out_sym: "BNB/USD",  leg1_sym: "BNB/USDT",  leg1_inv: false, leg2_sym: "USDT/USD", leg2_inv: false },
    SynthesisRuleSpec { out_sym: "XRP/USD",  leg1_sym: "XRP/USDT",  leg1_inv: false, leg2_sym: "USDT/USD", leg2_inv: false },
    SynthesisRuleSpec { out_sym: "ADA/USD",  leg1_sym: "ADA/USDT",  leg1_inv: false, leg2_sym: "USDT/USD", leg2_inv: false },
    SynthesisRuleSpec { out_sym: "DOGE/USD", leg1_sym: "DOGE/USDT", leg1_inv: false, leg2_sym: "USDT/USD", leg2_inv: false },
    SynthesisRuleSpec { out_sym: "AVAX/USD", leg1_sym: "AVAX/USDT", leg1_inv: false, leg2_sym: "USDT/USD", leg2_inv: false },
    SynthesisRuleSpec { out_sym: "LINK/USD", leg1_sym: "LINK/USDT", leg1_inv: false, leg2_sym: "USDT/USD", leg2_inv: false },
    SynthesisRuleSpec { out_sym: "DOT/USD",  leg1_sym: "DOT/USDT",  leg1_inv: false, leg2_sym: "USDT/USD", leg2_inv: false },
    SynthesisRuleSpec { out_sym: "LTC/USD",  leg1_sym: "LTC/USDT",  leg1_inv: false, leg2_sym: "USDT/USD", leg2_inv: false },
    // No XAUT/USD, USDG/USD or STABLE/USD rule here: the Pyth grant covers 7
    // feeds (`oracles.providers.pyth.symbols`), everything else is parked since
    // 2026-08-10 and composes on read off the CEX legs.

    // ── USDT/USDC = USDT/USD × (USDC/USD)⁻¹ ──
    // Only STABLE/USDC cross with both legs live. The NATIVE deep-book ticker
    // is USDC/USDT (kept untouched); this inverse form gives the BTR Stable
    // Core keeper a uniform X/USDC universe. Distinct ticker id, no overwrite.
    SynthesisRuleSpec { out_sym: "USDT/USDC",  leg1_sym: "USDT/USD",  leg1_inv: false, leg2_sym: "USDC/USD", leg2_inv: true },

    // ── <crypto>/EUR + <crypto>/GBP = <crypto>/USDT × (1 / <FX>USD) ──
    // FX provider symbol (EURUSD, GBPUSD) is inverted: USD/<fiat> = 1 / EURUSD.
    SynthesisRuleSpec { out_sym: "BTC/EUR",  leg1_sym: "BTC/USDT",  leg1_inv: false, leg2_sym: "EURUSD", leg2_inv: true },
    SynthesisRuleSpec { out_sym: "BTC/GBP",  leg1_sym: "BTC/USDT",  leg1_inv: false, leg2_sym: "GBPUSD", leg2_inv: true },
    SynthesisRuleSpec { out_sym: "ETH/EUR",  leg1_sym: "ETH/USDT",  leg1_inv: false, leg2_sym: "EURUSD", leg2_inv: true },
    SynthesisRuleSpec { out_sym: "ETH/GBP",  leg1_sym: "ETH/USDT",  leg1_inv: false, leg2_sym: "GBPUSD", leg2_inv: true },
    SynthesisRuleSpec { out_sym: "SOL/EUR",  leg1_sym: "SOL/USDT",  leg1_inv: false, leg2_sym: "EURUSD", leg2_inv: true },
    SynthesisRuleSpec { out_sym: "SOL/GBP",  leg1_sym: "SOL/USDT",  leg1_inv: false, leg2_sym: "GBPUSD", leg2_inv: true },
    SynthesisRuleSpec { out_sym: "BNB/EUR",  leg1_sym: "BNB/USDT",  leg1_inv: false, leg2_sym: "EURUSD", leg2_inv: true },
    SynthesisRuleSpec { out_sym: "BNB/GBP",  leg1_sym: "BNB/USDT",  leg1_inv: false, leg2_sym: "GBPUSD", leg2_inv: true },
    SynthesisRuleSpec { out_sym: "XRP/EUR",  leg1_sym: "XRP/USDT",  leg1_inv: false, leg2_sym: "EURUSD", leg2_inv: true },
    SynthesisRuleSpec { out_sym: "XRP/GBP",  leg1_sym: "XRP/USDT",  leg1_inv: false, leg2_sym: "GBPUSD", leg2_inv: true },
    SynthesisRuleSpec { out_sym: "ADA/EUR",  leg1_sym: "ADA/USDT",  leg1_inv: false, leg2_sym: "EURUSD", leg2_inv: true },
    SynthesisRuleSpec { out_sym: "ADA/GBP",  leg1_sym: "ADA/USDT",  leg1_inv: false, leg2_sym: "GBPUSD", leg2_inv: true },
    SynthesisRuleSpec { out_sym: "DOGE/EUR", leg1_sym: "DOGE/USDT", leg1_inv: false, leg2_sym: "EURUSD", leg2_inv: true },
    SynthesisRuleSpec { out_sym: "DOGE/GBP", leg1_sym: "DOGE/USDT", leg1_inv: false, leg2_sym: "GBPUSD", leg2_inv: true },

    // ── BTC + ETH AUD/CHF crosses (sufficient FX provider depth). ──
    // AUDUSD inverted → USD/AUD. USDCHF NOT inverted (already USD/CHF).
    SynthesisRuleSpec { out_sym: "BTC/AUD", leg1_sym: "BTC/USDT", leg1_inv: false, leg2_sym: "AUDUSD", leg2_inv: true },
    SynthesisRuleSpec { out_sym: "BTC/CHF", leg1_sym: "BTC/USDT", leg1_inv: false, leg2_sym: "USDCHF", leg2_inv: false },
    SynthesisRuleSpec { out_sym: "ETH/AUD", leg1_sym: "ETH/USDT", leg1_inv: false, leg2_sym: "AUDUSD", leg2_inv: true },
    SynthesisRuleSpec { out_sym: "ETH/CHF", leg1_sym: "ETH/USDT", leg1_inv: false, leg2_sym: "USDCHF", leg2_inv: false },
];

// ── Injection rules ─────────────────────────────────────────────────────────

/// All currently-active injection rules. Inject USDC-quoted pair × USDC/USDT into
/// the corresponding USDT-quoted target VWAP, adding alt-liquidity-pool depth.
///
/// `provider_offset` is **registry position** = (provider_id - SYNTH_BASE). Keep
/// the order stable; reshuffling silently breaks provider_id continuity.
pub const INJECTION_RULES: &[InjectionRuleSpec] = &[
    InjectionRuleSpec { target_sym: "BTC/USDT",  leg1_sym: "BTC/USDC",  leg1_inv: false, leg2_sym: "USDC/USDT", leg2_inv: false, provider_offset: 0 },
    InjectionRuleSpec { target_sym: "ETH/USDT",  leg1_sym: "ETH/USDC",  leg1_inv: false, leg2_sym: "USDC/USDT", leg2_inv: false, provider_offset: 1 },
    InjectionRuleSpec { target_sym: "SOL/USDT",  leg1_sym: "SOL/USDC",  leg1_inv: false, leg2_sym: "USDC/USDT", leg2_inv: false, provider_offset: 2 },
    InjectionRuleSpec { target_sym: "BNB/USDT",  leg1_sym: "BNB/USDC",  leg1_inv: false, leg2_sym: "USDC/USDT", leg2_inv: false, provider_offset: 3 },
    InjectionRuleSpec { target_sym: "XRP/USDT",  leg1_sym: "XRP/USDC",  leg1_inv: false, leg2_sym: "USDC/USDT", leg2_inv: false, provider_offset: 4 },
    InjectionRuleSpec { target_sym: "ADA/USDT",  leg1_sym: "ADA/USDC",  leg1_inv: false, leg2_sym: "USDC/USDT", leg2_inv: false, provider_offset: 5 },
    InjectionRuleSpec { target_sym: "DOGE/USDT", leg1_sym: "DOGE/USDC", leg1_inv: false, leg2_sym: "USDC/USDT", leg2_inv: false, provider_offset: 6 },
    InjectionRuleSpec { target_sym: "AVAX/USDT", leg1_sym: "AVAX/USDC", leg1_inv: false, leg2_sym: "USDC/USDT", leg2_inv: false, provider_offset: 7 },
    InjectionRuleSpec { target_sym: "LINK/USDT", leg1_sym: "LINK/USDC", leg1_inv: false, leg2_sym: "USDC/USDT", leg2_inv: false, provider_offset: 8 },
    InjectionRuleSpec { target_sym: "DOT/USDT",  leg1_sym: "DOT/USDC",  leg1_inv: false, leg2_sym: "USDC/USDT", leg2_inv: false, provider_offset: 9 },
    InjectionRuleSpec { target_sym: "LTC/USDT",  leg1_sym: "LTC/USDC",  leg1_inv: false, leg2_sym: "USDC/USDT", leg2_inv: false, provider_offset: 10 },
];

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn synthesis_rules_unique_outputs() {
        let mut seen = std::collections::HashSet::new();
        for r in SYNTHESIS_RULES {
            assert!(seen.insert(r.out_sym), "duplicate synthesis out_sym: {}", r.out_sym);
        }
    }

    #[test]
    fn injection_rules_unique_offsets_and_targets() {
        let mut seen_off = std::collections::HashSet::new();
        let mut seen_tgt = std::collections::HashSet::new();
        for r in INJECTION_RULES {
            assert!(seen_off.insert(r.provider_offset), "duplicate offset {}", r.provider_offset);
            assert!(seen_tgt.insert(r.target_sym), "duplicate target {}", r.target_sym);
        }
    }

    #[test]
    fn injection_offsets_dense_from_zero() {
        // Provider id continuity: offsets must be 0..n contiguous (or core's
        // SYNTH_BASE arithmetic + downstream provider-id reservations break).
        for (i, r) in INJECTION_RULES.iter().enumerate() {
            assert_eq!(r.provider_offset as usize, i,
                "INJECTION_RULES[{}].provider_offset = {} (expected {})",
                i, r.provider_offset, i);
        }
    }

    #[test]
    fn expected_rule_counts() {
        // Locks the universe size — bump explicitly when rules are added or
        // removed so reviewers notice the registry change.
        assert_eq!(SYNTHESIS_RULES.len(), 56);
        assert_eq!(INJECTION_RULES.len(), 11);
    }

    #[test]
    fn synthesis_rules_never_chain_off_another_synthesis_output() {
        // 2026-07-22: compose-on-read (core/src/server/rest.rs `compose_cross_ohlc`)
        // does not recurse through synthetic legs: it only reads each leg's own
        // persisted `.idx`/`.s10` shard. If a rule's leg were itself another
        // synthesis output, and disk persistence for synth/auto-cross tickers is
        // ever turned off, that leg would resolve to nothing and compose would
        // silently go dark. Today's registry has zero such chains by
        // construction; this test locks that invariant so a future rule
        // addition that violates it fails CI instead of prod.
        let outs: std::collections::HashSet<&str> =
            SYNTHESIS_RULES.iter().map(|r| r.out_sym).collect();
        for r in SYNTHESIS_RULES {
            assert!(
                !outs.contains(r.leg1_sym),
                "{} leg1 {} chains off another synthesis output (compose-on-read can't recurse)",
                r.out_sym, r.leg1_sym
            );
            assert!(
                !outs.contains(r.leg2_sym),
                "{} leg2 {} chains off another synthesis output (compose-on-read can't recurse)",
                r.out_sym, r.leg2_sym
            );
        }
    }

    /// Every synthesis rule's output must be routable by the ONE resolver from
    /// the rule's own legs, and never through the output itself.
    ///
    /// The graph is built from the rule LEGS only, which is the resolver's
    /// contract (edges are primaries). That contract is what retired the
    /// `derive_legs` self-reference guard: the USDT/JPY bug was a registered
    /// synthesis OUTPUT winning a pivot leg, and an output is not a primary, so
    /// it is not in the graph to win.
    #[test]
    fn every_synthesis_rule_is_routable_from_its_own_legs() {
        use crate::synth::cross::CrossGraph;

        let primaries: Vec<(String, u64)> = SYNTHESIS_RULES
            .iter()
            .flat_map(|r| [r.leg1_sym, r.leg2_sym])
            .map(|s| (s.to_string(), crate::resolve_ticker_id(s)))
            .collect();
        let g = CrossGraph::from_primaries(primaries);

        for r in SYNTHESIS_RULES {
            let self_id = crate::resolve_ticker_id(r.out_sym);
            let route = g
                .route_sym(r.out_sym, &|_| 0.0)
                .unwrap_or_else(|| panic!("{} must route from its own legs", r.out_sym));
            assert!(
                route.legs.iter().all(|l| l.ticker_id != self_id),
                "{} routed through itself: {:?}",
                r.out_sym,
                route.legs
            );
            // Route length may EXCEED the rule's 2 legs: several rules treat
            // USDT as if it were USD (`BTC/EUR = BTC/USDT × (EUR/USD)⁻¹`), which
            // silently prices the peg at 1. The graph is dimensionally honest
            // and inserts the USDT/USD hop, so it can only be longer, never
            // shorter, than a rule that skips a conversion.
            assert!(!route.legs.is_empty());
        }
    }
}
