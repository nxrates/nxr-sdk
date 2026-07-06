//! Resolve the synth-pipeline universe from operator YAML.

use crate::pipeline_config::{PipelineYml, SynthPairYml};
use crate::synth::cross_expand::expand_cross_pairs;
use crate::synth::pairs::DEFAULT_INITIAL_SYNTH_PAIRS;

/// Synth pairs for live kernel + offline backfill/calibrate.
///
/// Prefer `cexs.cross_pairs` (full universe). `synths.initial_pairs` is a
/// deprecated manual override when non-empty.
pub fn synth_pipeline_pairs(yml: &PipelineYml) -> Vec<SynthPairYml> {
    if !yml.synths.initial_pairs.is_empty() {
        return yml.synths.initial_pairs.clone();
    }
    expand_cross_pairs(
        &yml.cexs.cross_pairs,
        &yml.series.pipeline.pairs,
    )
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
