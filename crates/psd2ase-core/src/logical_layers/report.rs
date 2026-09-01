use std::collections::BTreeMap;

use super::{
    AssociationDecision, AssociationDecisionStatus, AssociationExclusionKind, CopySuffixMatch,
};

pub(super) fn format_copy_suffixes(suffixes: &[CopySuffixMatch]) -> String {
    suffixes
        .iter()
        .map(|suffix| {
            suffix.ordinal.map_or_else(
                || suffix.token.clone(),
                |ordinal| format!("{} {ordinal}", suffix.token),
            )
        })
        .collect::<Vec<_>>()
        .join(" + ")
}

/// Formats name parsing and unresolved-name diagnostics.
pub(super) fn collect_name_diagnostics(decisions: &[AssociationDecision]) -> Vec<String> {
    decisions
        .iter()
        .filter(|decision| {
            decision.suffix_limit_reached
                || matches!(
                    decision.status,
                    AssociationDecisionStatus::Ambiguous | AssociationDecisionStatus::NewTrack
                )
        })
        .map(|decision| {
            let suffix = if decision.copy_suffixes.is_empty() {
                "no recognized copy suffix".to_string()
            } else {
                format!(
                    "recognized copy suffixes: {}",
                    format_copy_suffixes(&decision.copy_suffixes)
                )
            };
            format!(
                "frame {} source {} name {:?} family {:?} -> track {} ({suffix})",
                decision.frame_index,
                decision.source_layer_id,
                decision.original_name,
                decision.normalized_base_name,
                decision.track_id
            )
        })
        .collect()
}

/// Formats multi-observation name-family diagnostics.
pub(super) fn collect_family_diagnostics(decisions: &[AssociationDecision]) -> Vec<String> {
    let mut families = BTreeMap::<String, Vec<&AssociationDecision>>::new();
    for decision in decisions {
        if !decision.normalized_base_name.is_empty() {
            families
                .entry(decision.normalized_base_name.clone())
                .or_default()
                .push(decision);
        }
    }
    families
        .into_iter()
        .filter(|(_, decisions)| {
            decisions
                .iter()
                .any(|decision| !decision.copy_suffixes.is_empty())
        })
        .map(|(family_key, decisions)| {
            let mut variants = decisions
                .iter()
                .map(|decision| decision.original_name.clone())
                .collect::<Vec<_>>();
            variants.sort();
            variants.dedup();
            let mut tracks = decisions
                .iter()
                .map(|decision| decision.track_id)
                .collect::<Vec<_>>();
            tracks.sort_unstable();
            tracks.dedup();
            let mut mappings = decisions
                .iter()
                .map(|decision| format!("{}->{}", decision.original_name, decision.track_id))
                .collect::<Vec<_>>();
            mappings.sort();
            mappings.dedup();
            let ambiguous = decisions
                .iter()
                .filter(|decision| {
                    decision.status == AssociationDecisionStatus::Ambiguous
                        || decision.matching_tie
                })
                .count();
            format!(
                "family {:?}: {} observations -> {} tracks; variants={:?}; mappings={:?}; ambiguous={ambiguous}",
                family_key,
                decisions.len(),
                tracks.len(),
                variants,
                mappings,
            )
        })
        .collect()
}

/// Formats mutual-exclusion and ignored-order diagnostics.
pub(super) fn collect_exclusion_diagnostics(decisions: &[AssociationDecision]) -> Vec<String> {
    decisions
        .iter()
        .filter(|decision| {
            decision.exclusion_evidence != AssociationExclusionKind::None
                || decision.same_frame_conflict()
                || decision.order_evidence_ignored()
        })
        .map(|decision| {
            format!(
                "frame {} source {} name {:?} -> track {} exclusion={:?}, same_frame_conflict={}, order_ignored={}",
                decision.frame_index,
                decision.source_layer_id,
                decision.original_name,
                decision.track_id,
                decision.exclusion_evidence,
                decision.same_frame_conflict(),
                decision.order_evidence_ignored(),
            )
        })
        .collect()
}
