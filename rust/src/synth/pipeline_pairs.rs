//! Resolve the synth-pipeline universe from operator YAML.

use crate::pipeline_config::{PipelineYml, SynthPairYml};
use crate::synth::cross_expand::{all_crypto_crosses, expand_cross_pairs};

/// Synth cross catalog for offline backfill/calibrate (and cross enumeration).
///
/// ALL crosses by default: the full directed N² over the canonical crypto
/// universe `cexs.assets`, composed off the `<asset>/<storage_quote>` primaries.
/// `expand_cross_pairs` drops any pair whose legs don't resolve. No per-cross
/// declaration. `synths.initial_pairs` stays only as a deprecated manual
/// override when non-empty (empty in prod).
pub fn synth_pipeline_pairs(yml: &PipelineYml) -> Vec<SynthPairYml> {
    if !yml.synths.initial_pairs.is_empty() {
        return yml.synths.initial_pairs.clone();
    }
    let crosses = all_crypto_crosses(&yml.cexs.assets);
    expand_cross_pairs(&crosses, &yml.series.pipeline.pairs, &yml.cexs.pivot.storage_quote_for(""))
}
