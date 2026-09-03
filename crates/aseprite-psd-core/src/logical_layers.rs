//! Experimental cross-frame layer association and Aseprite write planning.

mod association;
mod layout;
mod matching;
mod observation;
mod ordering;
mod report;

use std::collections::{HashMap, HashSet};

use self::association::AssociationEngine;
use self::layout::{
    build_nodes, choose_group_paths, flatten_redundant_common_root, plan_candidate_groups,
    validate_candidate_group_topology,
};
use self::observation::{
    ObservationCollectionState, ObservationStore, collect_observations, collect_pixel_layer_ids,
    find_frame_selector_groups,
};
use self::ordering::{anchor_track_order, assign_z_indices, stable_track_order};
use self::report::{
    collect_exclusion_diagnostics, collect_family_diagnostics, collect_name_diagnostics,
};

use crate::NormalizedDocument;
use crate::layer_names::{COPY_SUFFIX_CATALOG_VERSION, CopySuffixMatch};

/// Selects whether the writer should preserve the PSD source tree or infer
/// long-lived logical layer tracks across animation frames.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum LayerAssociation {
    /// Preserve every normalized source layer as its own Aseprite layer.
    #[default]
    Preserve,
    /// Infer logical tracks and remove frame-container groups.
    Auto(AutoAssociationOptions),
    /// Prefer exact converter round-trip restoration, then fall back to automatic association.
    AutoForRoundTrip,
}

/// Options used when automatic logical-layer association is enabled.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct AutoAssociationOptions {
    /// Selects the automatic identity-association strategy.
    pub strategy: AssociationStrategy,
    /// Selects stable track order or per-cel Z-Index adjustments.
    pub z_order: LayerZOrderMode,
    /// Selects the stable logical-track ordering strategy.
    pub stable_order: StableOrderMode,
}

/// Selects the automatic identity-association strategy and its valid layout policy.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AssociationStrategy {
    /// Reproduce the compact association behavior from the ordering baseline.
    Compact,
    /// Use the conservative multilingual family and candidate-folder planner.
    Conservative {
        /// Selects whether uncertain tracks are grouped for review.
        uncertain_layers: UncertainLayerMode,
    },
}

impl Default for AssociationStrategy {
    fn default() -> Self {
        Self::Conservative {
            uncertain_layers: UncertainLayerMode::Group,
        }
    }
}

impl AssociationStrategy {
    /// Returns the stable strategy name used by CLI diagnostics.
    pub const fn name(self) -> &'static str {
        match self {
            Self::Compact => "Compact",
            Self::Conservative { .. } => "Conservative",
        }
    }

    /// Returns the effective uncertain-layer presentation policy.
    pub const fn uncertain_layers(self) -> UncertainLayerMode {
        match self {
            Self::Compact => UncertainLayerMode::Group,
            Self::Conservative { uncertain_layers } => uncertain_layers,
        }
    }
}

/// Selects whether automatic association may reorder individual cels with
/// Aseprite's per-cel Z-Index field.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum LayerZOrderMode {
    /// Use the stable order established by the association anchor frame.
    #[default]
    Stable,
    /// Allow experimental per-frame Z-Index adjustments.
    Auto,
}

/// Selects how Stable mode establishes the long-lived logical-track order.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum StableOrderMode {
    /// Infer order from repeated source-order evidence on overlapping pixels.
    #[default]
    Consensus,
    /// Preserve the order from the association anchor frame.
    Anchor,
    /// Require every overlapping pair to have a reliable, acyclic consensus.
    Strict,
}

/// Selects how uncertain tracks are presented in automatic output.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum UncertainLayerMode {
    /// Group uncertain tracks with their strongest nearby anchor when safe.
    #[default]
    Group,
    /// Keep uncertain tracks flat while retaining the inferred stable order.
    Flat,
}

/// Describes the strongest exclusion relation observed for an association.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum AssociationExclusionKind {
    /// No useful exclusion relation was found.
    #[default]
    None,
    /// The observations were not simultaneously visible in the sample.
    ObservedDisjoint,
    /// The observations belong to structurally disjoint frame containers.
    StructuralMutualExclusion,
    /// The candidate track already contains an observation from this frame.
    CoVisible,
}

/// Identifies the association pass that produced one decision.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum AssociationPhase {
    /// The observation initialized a track from the anchor frame.
    Anchor,
    /// A family-level multi-track matcher associated the observation.
    Family,
    /// An exact RGBA match associated the observation.
    ExactPixels,
    /// The general residual matcher associated the observation.
    #[default]
    Residual,
    /// No existing track was safe, so a new track was created.
    NewTrack,
}

/// A derived write plan that references source layers by ID without copying pixels.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LayerWritePlan {
    /// Top-level logical groups and tracks in output order.
    pub root_nodes: Vec<PlannedNode>,
    /// Logical tracks addressed by `PlannedNode::Track`.
    pub tracks: Vec<LogicalLayerTrack>,
    /// Explainable decisions made while constructing the plan.
    pub report: AssociationReport,
}

/// A logical group or track in a derived write plan.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PlannedNode {
    /// A group reconstructed from a persistent source group.
    Group {
        /// Output group name.
        name: String,
        /// Representative source group ID, when available.
        source_layer_id: Option<u32>,
        /// Child nodes in output order.
        children: Vec<PlannedNode>,
    },
    /// A reference to one logical track.
    Track {
        /// Index into [`LayerWritePlan::tracks`].
        track_id: usize,
    },
}

/// One logical pixel layer spanning zero or more animation frames.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LogicalLayerTrack {
    /// Stable index within the derived plan.
    pub id: usize,
    /// Representative output name.
    pub name: String,
    /// Source layer used for static attributes such as opacity and blend mode.
    pub representative_source_layer_id: u32,
    /// Persistent group path after frame-container groups are removed.
    pub group_path: Vec<String>,
    /// Source cel references, indexed by normalized frame index.
    pub cels: Vec<Option<PlannedCel>>,
}

/// A cel reference in a logical track.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PlannedCel {
    /// Source normalized pixel-layer ID.
    pub source_layer_id: u32,
    /// Source normalized frame index.
    pub source_frame_index: u32,
    /// Per-cel Aseprite Z-Index adjustment.
    pub z_index: i16,
}

/// Structured diagnostics for automatic association.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AssociationReport {
    /// Number of effective visible source observations considered.
    pub observation_count: usize,
    /// Number of logical tracks produced.
    pub track_count: usize,
    /// Source pixel layers that were never effectively visible in any frame.
    pub omitted_source_layer_ids: Vec<u32>,
    /// Z-order policy used to construct the plan.
    pub z_order_mode: LayerZOrderMode,
    /// Stable-order policy requested for the automatic plan.
    pub stable_order_mode: StableOrderMode,
    /// Presentation policy used for uncertain automatic tracks.
    pub uncertain_layer_mode: UncertainLayerMode,
    /// Automatic identity-association strategy used for this plan.
    pub strategy: AssociationStrategy,
    /// Version of the copy-suffix catalog used for name analysis.
    pub name_catalog_version: u16,
    /// Potential per-frame order changes detected by the planner.
    pub z_order_diagnostics: Vec<String>,
    /// Evidence and fallbacks used while establishing stable track order.
    pub stable_order_diagnostics: Vec<String>,
    /// Candidate-folder decisions derived from ordering evidence.
    pub candidate_groups: Vec<CandidateGroupReport>,
    /// Per-observation decisions in deterministic source/frame order.
    pub decisions: Vec<AssociationDecision>,
    /// Non-fatal limitations and conservative fallbacks.
    pub warnings: Vec<String>,
}

/// Describes one presentation-only candidate folder.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CandidateGroupReport {
    /// Folder name shown in the output document.
    pub name: String,
    /// Confirmed or strongest nearby track used as the candidate anchor.
    pub anchor_track_id: usize,
    /// Independent tracks placed in the folder, including the anchor.
    pub member_track_ids: Vec<usize>,
    /// Human-readable ordering and structure evidence.
    pub evidence: Vec<String>,
    /// Pairwise visibility and frame-container relations considered for the folder.
    pub relations: Vec<CandidateTrackRelationReport>,
    /// Whether every proposed member occupies one complete stable-order interval.
    pub complete_interval: bool,
    /// Whether the folder was emitted or only diagnosed.
    pub emitted: bool,
    /// Why a candidate group was not emitted, when applicable.
    pub rejection_reason: Option<String>,
}

/// Visibility relation between two candidate-folder tracks.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CandidateTrackRelation {
    /// Both tracks have effective pixels in at least one common frame.
    CoVisible,
    /// The tracks belong to sibling frame containers with disjoint active frames.
    StructuralMutualExclusion,
    /// The tracks never co-occur in this document without structural exclusion evidence.
    ObservedDisjoint,
    /// No useful relationship could be established.
    Unrelated,
}

/// Explainable pairwise evidence used by candidate-folder planning.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CandidateTrackRelationReport {
    /// First logical track in the relation.
    pub left_track_id: usize,
    /// Second logical track in the relation.
    pub right_track_id: usize,
    /// Strongest relation observed between the tracks.
    pub relation: CandidateTrackRelation,
    /// Effective normalized frames in which both tracks are visible.
    pub co_visible_frames: Vec<u32>,
}

/// One explainable observation-to-track decision.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AssociationDecision {
    /// Normalized frame index.
    pub frame_index: u32,
    /// Source layer ID.
    pub source_layer_id: u32,
    /// Source tree path for diagnostics.
    pub source_path: String,
    /// Original source layer name.
    pub original_name: String,
    /// Whitespace- and case-normalized source name.
    pub normalized_name: String,
    /// Copy-stripped name family used for matching.
    pub normalized_base_name: String,
    /// Copy suffixes recognized at the end of the source name.
    pub copy_suffixes: Vec<CopySuffixMatch>,
    /// Whether copy suffix parsing stopped at the safety limit.
    pub suffix_limit_reached: bool,
    /// Name evidence capacity used by the candidate scorer.
    pub name_evidence_weight: u16,
    /// Association pass that produced this decision.
    pub association_phase: AssociationPhase,
    /// Number of observations from the same name family in this frame.
    pub same_frame_instance_count: usize,
    /// Whether the family matcher found tied optimal assignments.
    pub matching_tie: bool,
    /// Reasons candidate tracks were rejected for this observation.
    pub rejection_reasons: Vec<String>,
    /// Exclusion relation between the selected observation and prior track data.
    pub exclusion_evidence: AssociationExclusionKind,
    /// Output logical track ID.
    pub track_id: usize,
    /// Decision category.
    pub status: AssociationDecisionStatus,
    /// Score in hundredths of the normalized 0..=1 range.
    pub score: u16,
    /// Difference from the second-best candidate, in the same scale.
    pub margin: u16,
    /// Human-readable evidence labels.
    pub evidence: Vec<String>,
    /// Names of close alternatives that were not selected.
    pub alternatives: Vec<String>,
}

/// Classification of an automatic association decision.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AssociationDecisionStatus {
    /// A unique strong name or exact-pixel anchor matched.
    Strong,
    /// A unique residual assignment or high-confidence score matched.
    Inferred,
    /// A new track was created because no safe match existed.
    NewTrack,
    /// The observation was intentionally kept separate because candidates tied.
    Ambiguous,
}

impl AssociationReport {
    /// Formats name diagnostics from the authoritative decisions.
    pub fn name_diagnostics(&self) -> Vec<String> {
        collect_name_diagnostics(&self.decisions)
    }

    /// Formats family diagnostics from the authoritative decisions.
    pub fn family_diagnostics(&self) -> Vec<String> {
        collect_family_diagnostics(&self.decisions)
    }

    /// Formats exclusion diagnostics from the authoritative decisions.
    pub fn exclusion_diagnostics(&self) -> Vec<String> {
        collect_exclusion_diagnostics(&self.decisions)
    }
}

impl AssociationDecision {
    /// Returns whether the selected track had a same-frame conflict.
    pub const fn same_frame_conflict(&self) -> bool {
        matches!(self.exclusion_evidence, AssociationExclusionKind::CoVisible)
    }

    /// Returns whether source order was ignored for incomparable containers.
    pub const fn order_evidence_ignored(&self) -> bool {
        matches!(
            self.exclusion_evidence,
            AssociationExclusionKind::StructuralMutualExclusion
        )
    }
}
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
enum GroupKey {
    Persistent(String),
    Candidate(usize),
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct GroupSegment {
    source_layer_id: Option<u32>,
    name: String,
    key: GroupKey,
}
pub fn build_layer_write_plan(
    document: &NormalizedDocument,
    options: AutoAssociationOptions,
) -> Result<LayerWritePlan, String> {
    build_layer_write_plan_with_metadata(document, options, false)
}

/// Builds an automatic plan while isolating layers carrying Photoshop metadata.
pub(crate) fn build_layer_write_plan_with_metadata(
    document: &NormalizedDocument,
    options: AutoAssociationOptions,
    preserve_photoshop_metadata: bool,
) -> Result<LayerWritePlan, String> {
    build_layer_write_plan_with_context(document, options, preserve_photoshop_metadata, true)
}

/// Builds an automatic plan with source-specific generic identity evidence policy.
pub(crate) fn build_layer_write_plan_with_context(
    document: &NormalizedDocument,
    options: AutoAssociationOptions,
    preserve_photoshop_metadata: bool,
    allow_inferred_cross_source_matches: bool,
) -> Result<LayerWritePlan, String> {
    let strategy = options.strategy;
    let z_order_mode = options.z_order;
    let stable_order_mode = options.stable_order;
    let uncertain_layer_mode = strategy.uncertain_layers();
    let selectors = find_frame_selector_groups(&document.root_layers, document.frames.len());
    let mut observation_store = ObservationStore::new(document.frames.len());
    let mut source_order = 0;
    let mut next_observation_id = 0;
    {
        let mut collection = ObservationCollectionState {
            selectors: &selectors,
            source_order: &mut source_order,
            next_observation_id: &mut next_observation_id,
            store: &mut observation_store,
            preserve_photoshop_metadata,
        };
        for (root_index, layer) in document.root_layers.iter().enumerate() {
            collect_observations(
                layer,
                &[root_index.to_string()],
                &[],
                &[],
                &[],
                false,
                &mut collection,
            )?;
        }
    }
    let frames = &mut observation_store.frames;
    for frame in frames.iter_mut() {
        frame.sort_by_key(|observation| observation.source_order);
        for (source_order, observation) in frame.iter_mut().enumerate() {
            observation.source_order = source_order;
        }
    }

    let observation_count = frames.iter().map(Vec::len).sum();
    if observation_count == 0 {
        if document.root_layers.is_empty() {
            return Ok(LayerWritePlan {
                root_nodes: Vec::new(),
                tracks: Vec::new(),
                report: AssociationReport {
                    observation_count: 0,
                    track_count: 0,
                    omitted_source_layer_ids: Vec::new(),
                    z_order_mode,
                    stable_order_mode,
                    uncertain_layer_mode,
                    strategy,
                    name_catalog_version: COPY_SUFFIX_CATALOG_VERSION,
                    z_order_diagnostics: Vec::new(),
                    stable_order_diagnostics: Vec::new(),
                    candidate_groups: Vec::new(),
                    decisions: Vec::new(),
                    warnings: vec![
                        "automatic layer association found no source layers; emitted an empty layer plan"
                            .to_string(),
                    ],
                },
            });
        }
        return Err(
            "automatic layer association found no effective visible pixel layers".to_string(),
        );
    }

    let anchor_frame = frames
        .iter()
        .enumerate()
        .max_by_key(|(_, frame)| {
            (
                frame
                    .iter()
                    .filter(|observation| !observation.generic_name)
                    .count(),
                frame.len(),
                usize::MAX
                    - frame
                        .first()
                        .map_or(0, |observation| observation.frame_index),
            )
        })
        .map(|(index, _)| index)
        .unwrap_or(0);

    let mut engine = AssociationEngine::new(
        observation_store,
        selectors,
        anchor_frame,
        document.frames.len(),
    );
    engine.seed_anchor();
    engine.associate(strategy, z_order_mode, allow_inferred_cross_source_matches);
    let mut association = engine.into_output();
    let frames = &association.observations.frames;
    let tracks = &mut association.tracks;
    let decisions = &mut association.decisions;
    let selectors = &association.selectors;

    let mut warnings = Vec::new();
    let anchor_order = anchor_track_order(tracks);
    let (track_order, mut stable_order_diagnostics) = if z_order_mode == LayerZOrderMode::Stable {
        stable_track_order(tracks, frames, decisions, &anchor_order, stable_order_mode)?
    } else {
        (anchor_order, Vec::new())
    };
    stable_order_diagnostics.push(format!(
        "stable track order: {:?}",
        track_order
            .iter()
            .map(|track_id| format!("{}:{}", track_id, tracks[*track_id].name))
            .collect::<Vec<_>>()
    ));
    let observed_source_layer_ids = frames
        .iter()
        .flat_map(|frame| frame.iter().map(|observation| observation.source_layer_id))
        .collect::<HashSet<_>>();
    let mut pixel_layer_ids = Vec::new();
    collect_pixel_layer_ids(&document.root_layers, &mut pixel_layer_ids);
    pixel_layer_ids.retain(|id| !observed_source_layer_ids.contains(id));
    pixel_layer_ids.sort_unstable();
    if !pixel_layer_ids.is_empty() {
        warnings.push(format!(
            "{} source pixel layers were never effectively visible and were omitted from auto association",
            pixel_layer_ids.len()
        ));
    }
    decisions.sort_by(|left, right| {
        left.frame_index
            .cmp(&right.frame_index)
            .then_with(|| left.source_path.cmp(&right.source_path))
            .then_with(|| left.source_layer_id.cmp(&right.source_layer_id))
    });
    let mut group_paths = choose_group_paths(tracks, document, &mut warnings);
    flatten_redundant_common_root(&mut group_paths, tracks, document, selectors, &mut warnings);
    let (candidate_groups, candidate_group_paths) = if allow_inferred_cross_source_matches
        && matches!(strategy, AssociationStrategy::Conservative { .. })
    {
        plan_candidate_groups(
            tracks,
            decisions,
            &track_order,
            selectors,
            uncertain_layer_mode,
            &mut warnings,
        )
    } else {
        (Vec::new(), HashMap::new())
    };
    let root_nodes = build_nodes(&group_paths, &track_order, &candidate_group_paths);
    validate_candidate_group_topology(&root_nodes, &candidate_groups)?;
    let mut plan = LayerWritePlan {
        root_nodes,
        tracks: tracks
            .iter()
            .map(|track| LogicalLayerTrack {
                id: track.id,
                name: track.name.clone(),
                representative_source_layer_id: track.representative_source_layer_id,
                group_path: group_paths[track.id]
                    .iter()
                    .map(|segment| segment.name.clone())
                    .collect(),
                cels: track.cels.clone(),
            })
            .collect(),
        report: AssociationReport {
            observation_count,
            track_count: tracks.len(),
            omitted_source_layer_ids: pixel_layer_ids,
            z_order_mode,
            stable_order_mode,
            uncertain_layer_mode,
            strategy,
            name_catalog_version: COPY_SUFFIX_CATALOG_VERSION,
            z_order_diagnostics: Vec::new(),
            stable_order_diagnostics,
            candidate_groups,
            decisions: std::mem::take(decisions),
            warnings,
        },
    };
    plan.report.z_order_diagnostics = assign_z_indices(&mut plan, frames, z_order_mode)?;
    Ok(plan)
}
#[cfg(test)]
#[path = "tests/logical_layers.rs"]
mod tests;
