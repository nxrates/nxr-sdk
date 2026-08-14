//! Synth-pair spec type. The SET of pairs is DERIVED (config assets crossed by
//! `all_crypto_crosses`, resolver reachability on the read path), never listed:
//! the literal fallback catalogue that used to live here advertised five
//! crosses while the system served six figures of them.

/// One synth-pair spec by SYMBOL. Resolution to MITCH ids is caller-side
/// (different crates use different policies — core warns + skips,
/// series-factory bins panic on missing ids, etc.).
#[derive(Debug, Clone, Copy)]
pub struct SynthPairSpec {
    pub synth_sym: &'static str,
    pub base_sym: &'static str,
    pub quote_sym: &'static str,
}
