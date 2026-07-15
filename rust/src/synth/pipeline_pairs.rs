//! Resolve the synth-pipeline universe from operator YAML.

use crate::pipeline_config::{PipelineYml, SynthPairYml};
use crate::synth::cross_expand::{all_crypto_crosses, expand_cross_pairs};
use crate::synth::pairs::DEFAULT_INITIAL_SYNTH_PAIRS;

/// Synth cross catalog for offline backfill/calibrate (and cross enumeration).
///
/// ALL crosses by default: the full directed N² over the canonical crypto
/// universe `cexs.assets`, USDT-pivoted. `expand_cross_pairs` drops any pair
/// whose `A/USDT`/`B/USDT` legs don't resolve. No per-cross declaration —
/// `cexs.cross_pairs` is retired. `synths.initial_pairs` stays only as a
/// deprecated manual override when non-empty (empty in prod).
pub fn synth_pipeline_pairs(yml: &PipelineYml) -> Vec<SynthPairYml> {
    if !yml.synths.initial_pairs.is_empty() {
        return yml.synths.initial_pairs.clone();
    }
    let crosses = all_crypto_crosses(&yml.cexs.assets);
    expand_cross_pairs(&crosses, &yml.series.pipeline.pairs)
}

/// Fallback when YAML is unavailable (tests / minimal boot).
pub fn default_synth_pipeline_pairs() -> Vec<SynthPairYml> {
    DEFAULT_INITIAL_SYNTH_PAIRS
        .iter()
        .map(|p| SynthPairYml {
            synth_sym: p.synth_sym.to_string(),
            base_sym: p.base_sym.to_string(),
            quote_sym: p.quote_sym.to_string(),
        })
        .collect()
}
