//! Experimental cross-frame layer association and Aseprite write planning.

mod matching;
mod report;

use std::collections::{BTreeMap, HashMap, HashSet};
use std::ops::Deref;
use std::rc::Rc;

use self::matching::find_best_weighted_matching;
use self::report::{
    collect_exclusion_diagnostics, collect_family_diagnostics, collect_name_diagnostics,
    format_copy_suffixes,
};

use crate::aseprite_writer::cel_position;
use crate::layer_names::{
    COPY_SUFFIX_CATALOG_VERSION, CopySuffixCatalog, CopySuffixMatch, ParsedLayerName,
};
use crate::{NormalizedDocument, NormalizedLayer, NormalizedLayerKind};

/// Selects whether the writer should preserve the PSD source tree or infer
/// long-lived logical layer tracks across animation frames.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum LayerAssociation {
    /// Preserve every normalized source layer as its own Aseprite layer.
    #[default]
    Preserve,
    /// Infer logical tracks and remove frame-container groups.
    Auto(AutoAssociationOptions),
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
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum AssociationStrategy {
    /// Reproduce the compact association behavior from the ordering baseline.
    #[default]
    Compact,
    /// Use the conservative multilingual family and candidate-folder planner.
    Conservative {
        /// Selects whether uncertain tracks are grouped for review.
        uncertain_layers: UncertainLayerMode,
    },
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

#[derive(Debug)]
struct LayerEvidence<'doc> {
    source_layer_id: u32,
    source_path: String,
    name: String,
    normalized_name: String,
    name_key: String,
    generic_name: bool,
    copy_suffixes: Vec<CopySuffixMatch>,
    suffix_limit_reached: bool,
    frame_container_ids: Vec<u32>,
    group_path: Vec<GroupSegment>,
    width: u32,
    height: u32,
    pixels: &'doc [u8],
}

#[derive(Debug, Clone)]
struct Observation<'doc> {
    id: ObservationId,
    evidence: Rc<LayerEvidence<'doc>>,
    frame_index: usize,
    source_order: usize,
    x: i32,
    y: i32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
struct ObservationId(usize);

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
struct TrackId(usize);

impl TrackId {
    const fn index(self) -> usize {
        self.0
    }
}

impl<'doc> Deref for Observation<'doc> {
    type Target = LayerEvidence<'doc>;

    fn deref(&self) -> &Self::Target {
        &self.evidence
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

#[derive(Debug, Clone)]
struct TrackBuilder<'doc> {
    id: usize,
    name: String,
    normalized_name: String,
    name_key: String,
    generic_name: bool,
    copy_suffixes: Vec<CopySuffixMatch>,
    representative_source_layer_id: u32,
    cels: Vec<Option<PlannedCel>>,
    observation_ids: Vec<ObservationId>,
    observations: Vec<ObservationSummary<'doc>>,
    group_paths: Vec<Vec<GroupSegment>>,
}

#[derive(Debug, Clone)]
struct ObservationSummary<'doc> {
    evidence: Rc<LayerEvidence<'doc>>,
    frame_index: usize,
    source_order: usize,
    x: i32,
    y: i32,
    width: u32,
    height: u32,
}

impl<'doc> Deref for ObservationSummary<'doc> {
    type Target = LayerEvidence<'doc>;

    fn deref(&self) -> &Self::Target {
        &self.evidence
    }
}

#[derive(Debug)]
struct ObservationStore<'doc> {
    evidence: Vec<Rc<LayerEvidence<'doc>>>,
    frames: Vec<Vec<Observation<'doc>>>,
}

struct ObservationCollectionState<'state, 'doc> {
    selectors: &'state HashMap<u32, FrameContainerInfo>,
    source_order: &'state mut usize,
    next_observation_id: &'state mut usize,
    store: &'state mut ObservationStore<'doc>,
}

impl ObservationStore<'_> {
    /// Creates an empty observation store for the document timeline.
    fn new(frame_count: usize) -> Self {
        Self {
            evidence: Vec::new(),
            frames: vec![Vec::new(); frame_count],
        }
    }
}

struct AssociationEngine<'doc> {
    observations: ObservationStore<'doc>,
    selectors: HashMap<u32, FrameContainerInfo>,
    tracks: Vec<TrackBuilder<'doc>>,
    decisions: Vec<AssociationDecision>,
    preassigned: HashMap<(usize, u32), usize>,
    anchor_frame: usize,
    frame_count: usize,
}

impl<'doc> AssociationEngine<'doc> {
    /// Creates the single owner of mutable association state.
    fn new(
        observations: ObservationStore<'doc>,
        selectors: HashMap<u32, FrameContainerInfo>,
        anchor_frame: usize,
        frame_count: usize,
    ) -> Self {
        Self {
            observations,
            selectors,
            tracks: Vec::new(),
            decisions: Vec::new(),
            preassigned: HashMap::new(),
            anchor_frame,
            frame_count,
        }
    }

    /// Seeds tracks and decisions from the selected anchor frame.
    fn seed_anchor(&mut self) {
        let observations = &self.observations.frames[self.anchor_frame];
        for observation in observations {
            let track_id = self.tracks.len();
            self.tracks
                .push(new_track(track_id, observation, self.frame_count));
            record_assignment(
                &mut self.tracks[track_id],
                observation,
                PlannedCel {
                    source_layer_id: observation.source_layer_id,
                    source_frame_index: observation.frame_index as u32,
                    z_index: 0,
                },
            );
            let mut anchor_decision = decision(
                observation,
                track_id,
                AssociationDecisionStatus::Strong,
                100,
                100,
                vec!["anchor frame".to_string()],
                Vec::new(),
            );
            anchor_decision.association_phase = AssociationPhase::Anchor;
            anchor_decision.same_frame_instance_count = observations
                .iter()
                .filter(|candidate| candidate.name_key == observation.name_key)
                .count();
            self.decisions.push(anchor_decision);
        }
    }

    /// Runs the configured association strategy over every non-anchor frame.
    fn associate(&mut self, strategy: AssociationStrategy) {
        if matches!(strategy, AssociationStrategy::Conservative { .. }) {
            associate_families_globally(
                &self.observations.frames,
                self.anchor_frame,
                &mut self.tracks,
                self.frame_count,
                &self.selectors,
                &mut self.decisions,
                &mut self.preassigned,
            );
        }

        let mut frame_order = (0..self.observations.frames.len()).collect::<Vec<_>>();
        frame_order.sort_by_key(|frame_index| {
            if *frame_index == self.anchor_frame {
                0
            } else {
                1 + (*frame_index + self.observations.frames.len() - self.anchor_frame)
                    % self.observations.frames.len()
            }
        });
        for frame_index in frame_order {
            if frame_index == self.anchor_frame {
                continue;
            }
            match strategy {
                AssociationStrategy::Compact => associate_frame_compact(
                    &self.observations.frames[frame_index],
                    &mut self.tracks,
                    self.frame_count,
                    &mut self.decisions,
                ),
                AssociationStrategy::Conservative { .. } => associate_frame(
                    &self.observations.frames[frame_index],
                    &mut self.tracks,
                    self.frame_count,
                    &self.selectors,
                    &mut self.decisions,
                    &self.preassigned,
                ),
            }
        }
    }
}

#[derive(Debug, Clone)]
struct FrameContainerInfo {
    parent_id: Option<u32>,
    active_frames: HashSet<usize>,
}

#[derive(Debug, Clone, Copy)]
struct AssignmentMetadata {
    score: u16,
    margin: u16,
    phase: AssociationPhase,
    matching_tie: bool,
}

struct FamilyAssignmentContext<'state, 'doc> {
    tracks: &'state mut Vec<TrackBuilder<'doc>>,
    frame_count: usize,
    selectors: &'state HashMap<u32, FrameContainerInfo>,
    decisions: &'state mut Vec<AssociationDecision>,
    preassigned: &'state mut HashMap<(usize, u32), usize>,
}

struct FamilyMatching {
    assignments: Vec<(usize, usize)>,
    tied_observations: HashSet<usize>,
}

struct GlobalFamilyMatching {
    assignments: Vec<((usize, usize), usize)>,
    tied_observations: HashSet<(usize, usize)>,
}

#[derive(Debug, Clone)]
struct CandidateGroupPath {
    name: String,
    anchor_track_id: usize,
}

#[derive(Debug, Clone)]
enum PlannedNodeBuilder {
    Group {
        key: GroupKey,
        name: String,
        source_layer_id: Option<u32>,
        children: Vec<PlannedNodeBuilder>,
    },
    Track {
        track_id: usize,
    },
}

const FAMILY_MATCH_ASSIGNMENT_BONUS: u32 = 100;
const MAX_FAMILY_MATCHING_STATES: usize = 100_000;

/// Builds an automatic logical-layer plan with an explicit, valid configuration.
pub fn build_layer_write_plan(
    document: &NormalizedDocument,
    options: AutoAssociationOptions,
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
        };
        for (root_index, layer) in document.root_layers.iter().enumerate() {
            collect_observations(
                layer,
                &[root_index.to_string()],
                &[],
                &[],
                &[],
                &mut collection,
            )?;
        }
    }
    let frames = &mut observation_store.frames;
    for frame in frames.iter_mut() {
        frame.sort_by_key(|observation| observation.source_order);
        if matches!(strategy, AssociationStrategy::Conservative { .. }) {
            for (source_order, observation) in frame.iter_mut().enumerate() {
                observation.source_order = source_order;
            }
        }
    }

    let observation_count = frames.iter().map(Vec::len).sum();
    if observation_count == 0 {
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
    engine.associate(strategy);
    let frames = &engine.observations.frames;
    let tracks = &mut engine.tracks;
    let decisions = &mut engine.decisions;
    let selectors = &engine.selectors;

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
    let (candidate_groups, candidate_group_paths) =
        if matches!(strategy, AssociationStrategy::Conservative { .. }) {
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

/// Collects every source pixel-layer ID for omission diagnostics.
fn collect_pixel_layer_ids(layers: &[NormalizedLayer], output: &mut Vec<u32>) {
    for layer in layers {
        if layer.kind == NormalizedLayerKind::Pixel {
            output.push(layer.id);
        }
        collect_pixel_layer_ids(&layer.children, output);
    }
}

fn collect_observations<'doc>(
    layer: &'doc NormalizedLayer,
    path: &[String],
    group_path: &[GroupSegment],
    ancestors: &[&NormalizedLayer],
    frame_container_ids: &[u32],
    state: &mut ObservationCollectionState<'_, 'doc>,
) -> Result<(), String> {
    let is_visible = |frame_index: usize| {
        ancestors.iter().all(|ancestor| {
            ancestor
                .frame_states
                .get(frame_index)
                .is_some_and(|state| state.enabled)
        }) && layer
            .frame_states
            .get(frame_index)
            .is_some_and(|state| state.enabled)
    };
    match layer.kind {
        NormalizedLayerKind::Group => {
            let mut next_path = group_path.to_vec();
            let mut next_frame_container_ids = frame_container_ids.to_vec();
            if state.selectors.contains_key(&layer.id) {
                next_frame_container_ids.push(layer.id);
            } else {
                next_path.push(GroupSegment {
                    source_layer_id: Some(layer.id),
                    name: layer.name.clone(),
                    key: GroupKey::Persistent(canonical_name(&layer.name)),
                });
            }
            let mut next_ancestors = ancestors.to_vec();
            next_ancestors.push(layer);
            for (index, child) in layer.children.iter().enumerate() {
                let mut child_path = path.to_vec();
                child_path.push(index.to_string());
                collect_observations(
                    child,
                    &child_path,
                    &next_path,
                    &next_ancestors,
                    &next_frame_container_ids,
                    state,
                )?;
            }
        }
        NormalizedLayerKind::Pixel => {
            let pixels = layer
                .pixels
                .as_ref()
                .ok_or_else(|| format!("pixel layer {} has no normalized pixels", layer.id))?;
            let parsed_name = parse_layer_name(&layer.name);
            let evidence = Rc::new(LayerEvidence {
                source_layer_id: layer.id,
                source_path: path.join("/"),
                name: layer.name.clone(),
                normalized_name: parsed_name.normalized_name,
                name_key: parsed_name.base_name,
                generic_name: parsed_name.generic,
                copy_suffixes: parsed_name.copy_suffixes,
                suffix_limit_reached: parsed_name.suffix_limit_reached,
                frame_container_ids: frame_container_ids.to_vec(),
                group_path: group_path.to_vec(),
                width: pixels.width,
                height: pixels.height,
                pixels: &pixels.data,
            });
            state.store.evidence.push(Rc::clone(&evidence));
            for (frame_index, frame) in state.store.frames.iter_mut().enumerate() {
                if !is_visible(frame_index) {
                    continue;
                }
                let frame_state = layer.frame_states.get(frame_index).ok_or_else(|| {
                    format!(
                        "pixel layer {} has no state for frame {frame_index}",
                        layer.id
                    )
                })?;
                let (x, y) = cel_position(pixels, frame_state)
                    .map_err(|error| format!("layer {}: {error}", layer.id))?;
                frame.push(Observation {
                    id: ObservationId(*state.next_observation_id),
                    evidence: Rc::clone(&evidence),
                    frame_index,
                    source_order: *state.source_order,
                    x: i32::from(x),
                    y: i32::from(y),
                });
                *state.next_observation_id += 1;
            }
            *state.source_order += 1;
        }
    }
    Ok(())
}

/// Associates copy-name families across all frames before residual matching.
fn associate_families_globally<'doc>(
    frames: &[Vec<Observation<'doc>>],
    anchor_frame: usize,
    tracks: &mut Vec<TrackBuilder<'doc>>,
    frame_count: usize,
    selectors: &HashMap<u32, FrameContainerInfo>,
    decisions: &mut Vec<AssociationDecision>,
    preassigned: &mut HashMap<(usize, u32), usize>,
) {
    let mut families = BTreeMap::<String, Vec<(usize, usize)>>::new();
    for (frame_index, observations) in frames.iter().enumerate() {
        if frame_index == anchor_frame {
            continue;
        }
        for (observation_index, observation) in observations.iter().enumerate() {
            if !observation.generic_name {
                families
                    .entry(observation.name_key.clone())
                    .or_default()
                    .push((frame_index, observation_index));
            }
        }
    }

    for (family_key, family_observations) in families {
        let mut all_family_observations = frames
            .iter()
            .enumerate()
            .flat_map(|(frame_index, observations)| {
                observations
                    .iter()
                    .enumerate()
                    .filter(|(_, observation)| {
                        !observation.generic_name && observation.name_key == family_key
                    })
                    .map(move |(observation_index, _)| (frame_index, observation_index))
            })
            .collect::<Vec<_>>();
        all_family_observations.sort_unstable();

        let existing_tracks = tracks
            .iter()
            .filter(|track| !track.generic_name && track.name_key == family_key)
            .map(|track| track.id)
            .collect::<Vec<_>>();
        let max_instances = frames
            .iter()
            .map(|observations| {
                observations
                    .iter()
                    .filter(|observation| {
                        !observation.generic_name && observation.name_key == family_key
                    })
                    .count()
            })
            .max()
            .unwrap_or(0);
        let required_slots = max_instances.saturating_sub(existing_tracks.len());
        let representative = all_family_observations
            .first()
            .map(|(frame_index, observation_index)| &frames[*frame_index][*observation_index]);
        let representative = all_family_observations
            .iter()
            .map(|(frame_index, observation_index)| &frames[*frame_index][*observation_index])
            .find(|observation| !observation.copy_suffixes.is_empty())
            .or(representative);
        for _ in 0..required_slots {
            if let Some(observation) = representative {
                let track_id = tracks.len();
                tracks.push(new_track(track_id, observation, frame_count));
            }
        }

        let family_tracks = tracks
            .iter()
            .filter(|track| !track.generic_name && track.name_key == family_key)
            .map(|track| track.id)
            .collect::<Vec<_>>();
        let mut candidate_map = HashMap::<(usize, usize), Vec<(usize, u16)>>::new();
        for (frame_index, observation_index) in &family_observations {
            let observation = &frames[*frame_index][*observation_index];
            let mut candidates = family_tracks
                .iter()
                .filter(|track_id| tracks[**track_id].cels[*frame_index].is_none())
                .map(|track_id| {
                    (
                        *track_id,
                        candidate_score(
                            observation,
                            &tracks[*track_id],
                            &frames[*frame_index],
                            selectors,
                        ),
                    )
                })
                .filter(|(_, score)| *score >= 40)
                .collect::<Vec<_>>();
            candidates
                .sort_by(|left, right| right.1.cmp(&left.1).then_with(|| left.0.cmp(&right.0)));
            candidate_map.insert((*frame_index, *observation_index), candidates);
        }

        let mut locked = HashMap::<(usize, usize), usize>::new();
        let mut locked_tracks = HashSet::<(usize, usize)>::new();
        for key in &family_observations {
            let observation = &frames[key.0][key.1];
            let exact_candidates = candidate_map
                .get(key)
                .into_iter()
                .flatten()
                .filter(|(track_id, _)| {
                    !tracks[*track_id].observations.is_empty()
                        && tracks[*track_id].normalized_name == observation.normalized_name
                })
                .map(|(track_id, _)| *track_id)
                .collect::<Vec<_>>();
            if exact_candidates.len() == 1 && locked_tracks.insert((key.0, exact_candidates[0])) {
                locked.insert(*key, exact_candidates[0]);
            }
        }
        for (key, candidates) in &mut candidate_map {
            candidates.retain(|(track_id, _)| !locked_tracks.contains(&(key.0, *track_id)));
        }
        let matching_observations = family_observations
            .iter()
            .filter(|key| !locked.contains_key(key))
            .copied()
            .collect::<Vec<_>>();
        let matching = find_best_global_family_matching(&matching_observations, &candidate_map);
        let selected = matching
            .assignments
            .iter()
            .copied()
            .collect::<HashMap<_, _>>();
        for (frame_index, observation_index) in family_observations {
            let observation = &frames[frame_index][observation_index];
            let instance_count = frames[frame_index]
                .iter()
                .filter(|candidate| candidate.name_key == observation.name_key)
                .count();
            let candidates = candidate_map
                .get(&(frame_index, observation_index))
                .cloned()
                .unwrap_or_default();
            let selected_track = locked
                .get(&(frame_index, observation_index))
                .copied()
                .or_else(|| selected.get(&(frame_index, observation_index)).copied());
            let Some(track_id) = selected_track else {
                create_family_new_track(
                    &mut FamilyAssignmentContext {
                        tracks,
                        frame_count,
                        selectors,
                        decisions,
                        preassigned,
                    },
                    observation,
                    &candidates,
                    false,
                    instance_count,
                    max_instances > 1,
                );
                continue;
            };
            let is_locked = locked.contains_key(&(frame_index, observation_index));
            let score = candidates
                .iter()
                .find(|(candidate, _)| *candidate == track_id)
                .map_or(if is_locked { 100 } else { 0 }, |(_, score)| *score);
            let second = candidates
                .iter()
                .filter(|(candidate, _)| *candidate != track_id)
                .map(|(_, score)| *score)
                .max()
                .unwrap_or(0);
            let exact_name = candidates
                .iter()
                .filter(|(candidate, _)| {
                    tracks[*candidate].normalized_name == observation.normalized_name
                })
                .count()
                == 1;
            let exact_pixels = tracks[track_id].observations.iter().any(|previous| {
                previous.width == observation.width
                    && previous.height == observation.height
                    && previous.pixels == observation.pixels
            });
            let new_slot = tracks[track_id].observations.is_empty();
            let tied = matching
                .tied_observations
                .contains(&(frame_index, observation_index));
            let multi_instance_family = max_instances > 1;
            if is_locked
                || (new_slot && tied)
                || exact_name
                || (multi_instance_family && exact_pixels)
                || (!multi_instance_family && score >= 75 && score.saturating_sub(second) >= 15)
            {
                let phase = if new_slot {
                    AssociationPhase::NewTrack
                } else {
                    AssociationPhase::Family
                };
                let status = if phase == AssociationPhase::NewTrack {
                    AssociationDecisionStatus::NewTrack
                } else {
                    AssociationDecisionStatus::Inferred
                };
                record_family_assignment(
                    &mut FamilyAssignmentContext {
                        tracks,
                        frame_count,
                        selectors,
                        decisions,
                        preassigned,
                    },
                    observation,
                    track_id,
                    AssignmentMetadata {
                        score,
                        margin: score.saturating_sub(second),
                        phase,
                        matching_tie: tied,
                    },
                    status,
                    &candidates,
                    instance_count,
                );
            } else {
                create_family_new_track(
                    &mut FamilyAssignmentContext {
                        tracks,
                        frame_count,
                        selectors,
                        decisions,
                        preassigned,
                    },
                    observation,
                    &candidates,
                    tied,
                    instance_count,
                    max_instances > 1,
                );
            }
        }
    }
}

fn record_family_assignment<'doc>(
    context: &mut FamilyAssignmentContext<'_, 'doc>,
    observation: &Observation<'doc>,
    track_id: usize,
    metadata: AssignmentMetadata,
    status: AssociationDecisionStatus,
    candidates: &[(usize, u16)],
    instance_count: usize,
) {
    record_assignment(
        &mut context.tracks[track_id],
        observation,
        PlannedCel {
            source_layer_id: observation.source_layer_id,
            source_frame_index: observation.frame_index as u32,
            z_index: 0,
        },
    );
    context.preassigned.insert(
        (observation.frame_index, observation.source_layer_id),
        track_id,
    );
    let mut association_decision = decision(
        observation,
        track_id,
        status,
        metadata.score,
        metadata.margin,
        evidence_for(
            observation,
            &context.tracks[track_id],
            metadata.score,
            context.selectors,
        ),
        candidate_names(candidates, context.tracks, track_id),
    );
    association_decision.association_phase = metadata.phase;
    association_decision.same_frame_instance_count = instance_count;
    association_decision.matching_tie = metadata.matching_tie;
    association_decision.exclusion_evidence =
        exclusion_evidence(observation, &context.tracks[track_id], context.selectors);
    context.decisions.push(association_decision);
}

fn create_family_new_track<'doc>(
    context: &mut FamilyAssignmentContext<'_, 'doc>,
    observation: &Observation<'doc>,
    candidates: &[(usize, u16)],
    matching_tie: bool,
    instance_count: usize,
    conservative_multi_instance: bool,
) {
    let track_id = context.tracks.len();
    context
        .tracks
        .push(new_track(track_id, observation, context.frame_count));
    let mut association_decision = decision(
        observation,
        track_id,
        if candidates.is_empty() && !matching_tie {
            AssociationDecisionStatus::NewTrack
        } else {
            AssociationDecisionStatus::Ambiguous
        },
        candidates.first().map_or(0, |(_, score)| *score),
        candidates
            .first()
            .zip(candidates.get(1))
            .map_or(0, |((_, best), (_, second))| best.saturating_sub(*second)),
        vec!["family association created a separate track".to_string()],
        candidate_names(candidates, context.tracks, track_id),
    );
    association_decision.association_phase = AssociationPhase::NewTrack;
    association_decision.same_frame_instance_count = instance_count;
    association_decision.matching_tie = matching_tie;
    association_decision.rejection_reasons = rejection_reasons(
        candidates,
        association_decision.status,
        matching_tie,
        instance_count,
    );
    if conservative_multi_instance {
        association_decision
            .rejection_reasons
            .push("multi-instance family lacked exact identity evidence".to_string());
    }
    association_decision.exclusion_evidence =
        exclusion_evidence(observation, &context.tracks[track_id], context.selectors);
    context.decisions.push(association_decision);
    record_assignment(
        &mut context.tracks[track_id],
        observation,
        PlannedCel {
            source_layer_id: observation.source_layer_id,
            source_frame_index: observation.frame_index as u32,
            z_index: 0,
        },
    );
    context.preassigned.insert(
        (observation.frame_index, observation.source_layer_id),
        track_id,
    );
}

/// Associates one frame using the compact baseline algorithm from 651eb65.
fn associate_frame_compact<'doc>(
    observations: &[Observation<'doc>],
    tracks: &mut Vec<TrackBuilder<'doc>>,
    frame_count: usize,
    decisions: &mut Vec<AssociationDecision>,
) {
    if observations.is_empty() {
        return;
    }
    let mut assigned = vec![None; observations.len()];
    let mut used_tracks = HashSet::new();
    let mut strong_assignments = HashSet::new();

    for (observation_index, observation) in observations.iter().enumerate() {
        if observation.generic_name {
            continue;
        }
        let candidates = tracks
            .iter()
            .filter(|track| {
                !used_tracks.contains(&track.id) && track.name_key == observation.name_key
            })
            .map(|track| track.id)
            .collect::<Vec<_>>();
        if candidates.len() == 1 {
            let track_id = candidates[0];
            assigned[observation_index] = Some(track_id);
            used_tracks.insert(track_id);
            strong_assignments.insert(observation_index);
        }
    }

    for (observation_index, observation) in observations.iter().enumerate() {
        if assigned[observation_index].is_some() {
            continue;
        }
        let candidates = tracks
            .iter()
            .filter(|track| {
                !used_tracks.contains(&track.id)
                    && track.observations.iter().any(|previous| {
                        previous.width == observation.width
                            && previous.height == observation.height
                            && previous.pixels == observation.pixels
                    })
            })
            .map(|track| track.id)
            .collect::<Vec<_>>();
        if candidates.len() == 1 {
            let track_id = candidates[0];
            assigned[observation_index] = Some(track_id);
            used_tracks.insert(track_id);
            strong_assignments.insert(observation_index);
        }
    }

    let residual_observations = observations
        .iter()
        .enumerate()
        .filter(|(index, _)| assigned[*index].is_none())
        .map(|(index, _)| index)
        .collect::<Vec<_>>();
    let residual_tracks = tracks
        .iter()
        .filter(|track| {
            !used_tracks.contains(&track.id) && track.cels[observations[0].frame_index].is_none()
        })
        .map(|track| track.id)
        .collect::<Vec<_>>();

    let mut candidate_map = HashMap::new();
    for observation_index in &residual_observations {
        let observation = &observations[*observation_index];
        let mut candidates = residual_tracks
            .iter()
            .map(|track_id| {
                (
                    *track_id,
                    compact_candidate_score(observation, &tracks[*track_id], observations),
                )
            })
            .filter(|(_, score)| *score >= 40)
            .collect::<Vec<_>>();
        candidates.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
        candidate_map.insert(*observation_index, candidates);
    }
    let solutions = if residual_observations.len() == 1 && residual_tracks.len() == 1 {
        vec![vec![(residual_observations[0], residual_tracks[0])]]
    } else {
        find_unique_matchings(&residual_observations, &candidate_map, 2)
    };
    if solutions.len() == 1 {
        for (observation_index, track_id) in &solutions[0] {
            let candidates = candidate_map
                .get(observation_index)
                .cloned()
                .unwrap_or_default();
            let best = candidates
                .iter()
                .find(|(candidate, _)| candidate == track_id)
                .map(|(_, score)| *score);
            let second = candidates
                .iter()
                .find(|(candidate, _)| candidate != track_id)
                .map_or(0, |(_, score)| *score);
            let is_unique_residual = residual_observations.len() == 1 && residual_tracks.len() == 1;
            if is_unique_residual
                || best.is_some_and(|score| score >= 75 && score.saturating_sub(second) >= 15)
            {
                assigned[*observation_index] = Some(*track_id);
                used_tracks.insert(*track_id);
            }
        }
    }

    for (observation_index, observation) in observations.iter().enumerate() {
        let track_id = if let Some(track_id) = assigned[observation_index] {
            let candidates = candidate_map
                .get(&observation_index)
                .cloned()
                .unwrap_or_default();
            let score = candidates
                .iter()
                .find(|(candidate, _)| *candidate == track_id)
                .map_or(100, |(_, value)| *value);
            let second = candidates
                .iter()
                .find(|(candidate, _)| *candidate != track_id)
                .map_or(0, |(_, value)| *value);
            let status = if strong_assignments.contains(&observation_index) {
                AssociationDecisionStatus::Strong
            } else {
                AssociationDecisionStatus::Inferred
            };
            let mut association_decision = decision(
                observation,
                track_id,
                status,
                score,
                score.saturating_sub(second),
                compact_evidence_for(observation, &tracks[track_id], score),
                candidate_names(&candidates, tracks, track_id),
            );
            association_decision.association_phase =
                if strong_assignments.contains(&observation_index) {
                    AssociationPhase::Family
                } else {
                    AssociationPhase::Residual
                };
            decisions.push(association_decision);
            track_id
        } else {
            let candidates = candidate_map
                .get(&observation_index)
                .cloned()
                .unwrap_or_default();
            let score = candidates.first().map_or(0, |(_, score)| *score);
            let margin = candidates
                .first()
                .zip(candidates.get(1))
                .map_or(score, |((_, best), (_, second))| {
                    best.saturating_sub(*second)
                });
            let status =
                if !candidates.is_empty() && (solutions.len() > 1 || score < 75 || margin < 15) {
                    AssociationDecisionStatus::Ambiguous
                } else {
                    AssociationDecisionStatus::NewTrack
                };
            let track_id = tracks.len();
            tracks.push(new_track(track_id, observation, frame_count));
            let mut association_decision = decision(
                observation,
                track_id,
                status,
                score,
                margin,
                if status == AssociationDecisionStatus::Ambiguous {
                    vec!["candidate margin below safe threshold".to_string()]
                } else {
                    vec!["no safe existing track".to_string()]
                },
                candidate_names(&candidates, tracks, track_id),
            );
            association_decision.association_phase = AssociationPhase::NewTrack;
            decisions.push(association_decision);
            track_id
        };
        record_assignment(
            &mut tracks[track_id],
            observation,
            PlannedCel {
                source_layer_id: observation.source_layer_id,
                source_frame_index: observation.frame_index as u32,
                z_index: 0,
            },
        );
    }
}

/// Scores a compact-mode candidate with the historical 651eb65 weights.
fn compact_candidate_score(
    observation: &Observation,
    track: &TrackBuilder,
    frame_observations: &[Observation],
) -> u16 {
    let name_available = !observation.generic_name && !track.generic_name;
    let name = if name_available && observation.name_key == track.name_key {
        30
    } else {
        0
    };
    let group_match = track
        .group_paths
        .iter()
        .map(|path| group_similarity(&observation.group_path, path))
        .max()
        .unwrap_or(0);
    let rank = frame_observations
        .iter()
        .position(|candidate| candidate.source_layer_id == observation.source_layer_id)
        .unwrap_or(0) as i32;
    let median = median_order(track).unwrap_or(rank);
    let order = 20u16.saturating_sub((rank - median).unsigned_abs().min(5) as u16 * 4);
    let geometry = track
        .observations
        .iter()
        .map(|previous| geometry_similarity(observation, previous))
        .max()
        .unwrap_or(0);
    let pixels = if track.observations.iter().any(|previous| {
        previous.width == observation.width
            && previous.height == observation.height
            && previous.pixels == observation.pixels
    }) {
        10
    } else {
        0
    };
    let score = u32::from(name + group_match + order + geometry + pixels);
    let available_weight = if name_available { 100 } else { 70 };
    ((score * 100) / available_weight).min(100) as u16
}

/// Produces the compact-mode evidence labels used by the historical planner.
fn compact_evidence_for(
    observation: &Observation,
    track: &TrackBuilder,
    score: u16,
) -> Vec<String> {
    let mut evidence = Vec::new();
    if !observation.generic_name && observation.name_key == track.name_key {
        evidence.push("normalized name".to_string());
    }
    if track
        .group_paths
        .iter()
        .any(|path| group_similarity(&observation.group_path, path) >= 20)
    {
        evidence.push("persistent group path".to_string());
    }
    if track.observations.iter().any(|previous| {
        previous.width == observation.width
            && previous.height == observation.height
            && previous.pixels == observation.pixels
    }) {
        evidence.push("exact RGBA pixels".to_string());
    }
    if score >= 40 {
        evidence.push("geometry/order context".to_string());
    }
    evidence
}

fn associate_frame<'doc>(
    observations: &[Observation<'doc>],
    tracks: &mut Vec<TrackBuilder<'doc>>,
    frame_count: usize,
    selectors: &HashMap<u32, FrameContainerInfo>,
    decisions: &mut Vec<AssociationDecision>,
    preassigned: &HashMap<(usize, u32), usize>,
) {
    if observations.is_empty() {
        return;
    }
    let mut assigned = observations
        .iter()
        .map(|observation| {
            preassigned
                .get(&(observation.frame_index, observation.source_layer_id))
                .copied()
        })
        .collect::<Vec<_>>();
    let mut assignment_metadata = vec![None; observations.len()];
    let mut used_tracks = assigned.iter().flatten().copied().collect::<HashSet<_>>();
    let mut candidate_map = HashMap::<usize, Vec<(usize, u16)>>::new();
    let mut family_handled = HashSet::new();
    let mut matching_ties = HashSet::new();

    // Lock an exact normalized name only when it identifies one currently
    // available track. This remains a strong signal without consuming one of
    // several same-frame family instances arbitrarily.
    for (observation_index, observation) in observations.iter().enumerate() {
        if observation.generic_name {
            continue;
        }
        let candidates = tracks
            .iter()
            .filter(|track| {
                !used_tracks.contains(&track.id)
                    && track.cels[observation.frame_index].is_none()
                    && !track.generic_name
                    && observation.normalized_name == track.normalized_name
            })
            .map(|track| track.id)
            .collect::<Vec<_>>();
        candidate_map.insert(
            observation_index,
            candidates.iter().map(|track_id| (*track_id, 100)).collect(),
        );
        if candidates.len() == 1 {
            let track_id = candidates[0];
            assigned[observation_index] = Some(track_id);
            used_tracks.insert(track_id);
            assignment_metadata[observation_index] = Some(AssignmentMetadata {
                score: 100,
                margin: 100,
                phase: AssociationPhase::Family,
                matching_tie: false,
            });
        }
    }

    // Exact pixels can identify a track even when a drawing tool renamed the
    // layer. It is deliberately performed before family matching.
    for (observation_index, observation) in observations.iter().enumerate() {
        if assigned[observation_index].is_some() {
            continue;
        }
        let candidates = tracks
            .iter()
            .filter(|track| {
                !used_tracks.contains(&track.id)
                    && track.observations.iter().any(|previous| {
                        previous.width == observation.width
                            && previous.height == observation.height
                            && previous.pixels == observation.pixels
                    })
            })
            .map(|track| track.id)
            .collect::<Vec<_>>();
        candidate_map.insert(
            observation_index,
            candidates.iter().map(|track_id| (*track_id, 100)).collect(),
        );
        if candidates.len() == 1 {
            let track_id = candidates[0];
            assigned[observation_index] = Some(track_id);
            used_tracks.insert(track_id);
            assignment_metadata[observation_index] = Some(AssignmentMetadata {
                score: 100,
                margin: 100,
                phase: AssociationPhase::ExactPixels,
                matching_tie: false,
            });
        }
    }

    // Match every non-generic name family as a small assignment problem. The
    // family is processed as a whole so two observations cannot greedily
    // consume the same logical slot across frames.
    let mut families = BTreeMap::<String, Vec<usize>>::new();
    for (index, observation) in observations.iter().enumerate() {
        if assigned[index].is_none() && !observation.generic_name {
            families
                .entry(observation.name_key.clone())
                .or_default()
                .push(index);
        }
    }
    for (family_key, family_observations) in families {
        let available_tracks = tracks
            .iter()
            .filter(|track| {
                !used_tracks.contains(&track.id)
                    && !track.generic_name
                    && track.name_key == family_key
                    && track.cels[observations[family_observations[0]].frame_index].is_none()
            })
            .map(|track| track.id)
            .collect::<Vec<_>>();
        for observation_index in &family_observations {
            let observation = &observations[*observation_index];
            let mut candidates = available_tracks
                .iter()
                .filter(|track_id| {
                    tracks[**track_id].cels[observation.frame_index].is_none()
                        && copy_family_candidate_allowed(observation, &tracks[**track_id])
                })
                .map(|track_id| {
                    (
                        *track_id,
                        candidate_score(observation, &tracks[*track_id], observations, selectors),
                    )
                })
                .filter(|(_, score)| *score >= 40)
                .collect::<Vec<_>>();
            candidates
                .sort_by(|left, right| right.1.cmp(&left.1).then_with(|| left.0.cmp(&right.0)));
            candidate_map.insert(*observation_index, candidates);
            if !available_tracks.is_empty() || !observation.copy_suffixes.is_empty() {
                family_handled.insert(*observation_index);
            }
        }

        let matching = find_best_family_matching(&family_observations, &candidate_map);
        matching_ties.extend(matching.tied_observations.iter().copied());
        for (observation_index, track_id) in matching.assignments {
            if matching.tied_observations.contains(&observation_index) {
                continue;
            }
            let candidates = candidate_map
                .get(&observation_index)
                .cloned()
                .unwrap_or_default();
            let score = candidates
                .iter()
                .find(|(candidate, _)| *candidate == track_id)
                .map_or(0, |(_, score)| *score);
            let second = candidates
                .iter()
                .filter(|(candidate, _)| *candidate != track_id)
                .map(|(_, score)| *score)
                .max()
                .unwrap_or(0);
            let exact_name = candidates
                .iter()
                .filter(|(candidate, _)| {
                    tracks[*candidate].normalized_name
                        == observations[observation_index].normalized_name
                })
                .count()
                == 1;
            if exact_name || (score >= 75 && score.saturating_sub(second) >= 15) {
                assigned[observation_index] = Some(track_id);
                used_tracks.insert(track_id);
                assignment_metadata[observation_index] = Some(AssignmentMetadata {
                    score,
                    margin: score.saturating_sub(second),
                    phase: AssociationPhase::Family,
                    matching_tie: false,
                });
            }
        }
    }

    let residual_observations = observations
        .iter()
        .enumerate()
        .filter(|(index, observation)| {
            assigned[*index].is_none()
                && !family_handled.contains(index)
                && (observation.generic_name || observation.copy_suffixes.is_empty())
        })
        .map(|(index, _)| index)
        .collect::<Vec<_>>();
    let residual_tracks = tracks
        .iter()
        .filter(|track| {
            !used_tracks.contains(&track.id) && track.cels[observations[0].frame_index].is_none()
        })
        .map(|track| track.id)
        .collect::<Vec<_>>();

    for observation_index in &residual_observations {
        let observation = &observations[*observation_index];
        let mut candidates = residual_tracks
            .iter()
            .filter(|track_id| copy_family_candidate_allowed(observation, &tracks[**track_id]))
            .map(|track_id| {
                let score =
                    candidate_score(observation, &tracks[*track_id], observations, selectors);
                (*track_id, score)
            })
            .filter(|(_, score)| *score >= 40)
            .collect::<Vec<_>>();
        candidates.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
        candidate_map.insert(*observation_index, candidates);
    }
    let solutions = if residual_observations.len() == 1 && residual_tracks.len() == 1 {
        if candidate_map
            .get(&residual_observations[0])
            .is_some_and(|candidates| !candidates.is_empty())
        {
            vec![vec![(residual_observations[0], residual_tracks[0])]]
        } else {
            Vec::new()
        }
    } else {
        find_unique_matchings(&residual_observations, &candidate_map, 2)
    };
    if solutions.len() == 1 {
        for (observation_index, track_id) in &solutions[0] {
            let candidates = candidate_map
                .get(observation_index)
                .cloned()
                .unwrap_or_default();
            let best = candidates
                .iter()
                .find(|(candidate, _)| candidate == track_id)
                .map(|(_, score)| *score);
            let second = candidates
                .iter()
                .find(|(candidate, _)| candidate != track_id)
                .map_or(0, |(_, score)| *score);
            let is_unique_residual = residual_observations.len() == 1 && residual_tracks.len() == 1;
            if is_unique_residual
                || best.is_some_and(|score| score >= 75 && score.saturating_sub(second) >= 15)
            {
                assigned[*observation_index] = Some(*track_id);
                used_tracks.insert(*track_id);
                let candidates = candidate_map
                    .get(observation_index)
                    .cloned()
                    .unwrap_or_default();
                let score = candidates
                    .iter()
                    .find(|(candidate, _)| candidate == track_id)
                    .map_or(100, |(_, score)| *score);
                let second = candidates
                    .iter()
                    .filter(|(candidate, _)| candidate != track_id)
                    .map(|(_, score)| *score)
                    .max()
                    .unwrap_or(0);
                assignment_metadata[*observation_index] = Some(AssignmentMetadata {
                    score,
                    margin: score.saturating_sub(second),
                    phase: AssociationPhase::Residual,
                    matching_tie: false,
                });
            }
        }
    }

    for (observation_index, observation) in observations.iter().enumerate() {
        if preassigned.contains_key(&(observation.frame_index, observation.source_layer_id)) {
            continue;
        }
        let track_id = if let Some(track_id) = assigned[observation_index] {
            let candidates = candidate_map
                .get(&observation_index)
                .cloned()
                .unwrap_or_default();
            let metadata = assignment_metadata[observation_index].unwrap_or(AssignmentMetadata {
                score: candidates
                    .iter()
                    .find(|(candidate, _)| *candidate == track_id)
                    .map_or(100, |(_, value)| *value),
                margin: 100,
                phase: AssociationPhase::Residual,
                matching_tie: false,
            });
            let status = if matches!(
                metadata.phase,
                AssociationPhase::Anchor | AssociationPhase::ExactPixels
            ) {
                AssociationDecisionStatus::Strong
            } else {
                AssociationDecisionStatus::Inferred
            };
            let evidence = evidence_for(observation, &tracks[track_id], metadata.score, selectors);
            let exclusion = exclusion_evidence(observation, &tracks[track_id], selectors);
            let mut association_decision = decision(
                observation,
                track_id,
                status,
                metadata.score,
                metadata.margin,
                evidence,
                candidate_names(&candidates, tracks, track_id),
            );
            association_decision.association_phase = metadata.phase;
            association_decision.same_frame_instance_count = observations
                .iter()
                .filter(|candidate| candidate.name_key == observation.name_key)
                .count();
            association_decision.matching_tie =
                metadata.matching_tie || matching_ties.contains(&observation_index);
            association_decision.rejection_reasons = rejection_reasons(
                &candidates,
                status,
                association_decision.matching_tie,
                association_decision.same_frame_instance_count,
            );
            association_decision.exclusion_evidence = exclusion;
            decisions.push(association_decision);
            track_id
        } else {
            let candidates = candidate_map
                .get(&observation_index)
                .cloned()
                .unwrap_or_default();
            let score = candidates.first().map_or(0, |(_, score)| *score);
            let margin = candidates
                .first()
                .zip(candidates.get(1))
                .map_or(score, |((_, best), (_, second))| {
                    best.saturating_sub(*second)
                });
            let status =
                if !candidates.is_empty() && (solutions.len() > 1 || score < 75 || margin < 15) {
                    AssociationDecisionStatus::Ambiguous
                } else {
                    AssociationDecisionStatus::NewTrack
                };
            let track_id = tracks.len();
            tracks.push(new_track(track_id, observation, frame_count));
            let mut association_decision = decision(
                observation,
                track_id,
                status,
                score,
                margin,
                if status == AssociationDecisionStatus::Ambiguous {
                    vec!["candidate margin below safe threshold".to_string()]
                } else {
                    vec!["no safe existing track".to_string()]
                },
                candidate_names(&candidates, tracks, track_id),
            );
            association_decision.association_phase = AssociationPhase::NewTrack;
            association_decision.same_frame_instance_count = observations
                .iter()
                .filter(|candidate| candidate.name_key == observation.name_key)
                .count();
            association_decision.matching_tie =
                solutions.len() > 1 || matching_ties.contains(&observation_index);
            association_decision.rejection_reasons = rejection_reasons(
                &candidates,
                status,
                association_decision.matching_tie,
                association_decision.same_frame_instance_count,
            );
            decisions.push(association_decision);
            track_id
        };
        record_assignment(
            &mut tracks[track_id],
            observation,
            PlannedCel {
                source_layer_id: observation.source_layer_id,
                source_frame_index: observation.frame_index as u32,
                z_index: 0,
            },
        );
    }
}

fn find_unique_matchings(
    observations: &[usize],
    candidates: &HashMap<usize, Vec<(usize, u16)>>,
    limit: usize,
) -> Vec<Vec<(usize, usize)>> {
    fn visit(
        position: usize,
        observations: &[usize],
        candidates: &HashMap<usize, Vec<(usize, u16)>>,
        used: &mut HashSet<usize>,
        current: &mut Vec<(usize, usize)>,
        solutions: &mut Vec<Vec<(usize, usize)>>,
        limit: usize,
    ) {
        if solutions.len() >= limit {
            return;
        }
        if position == observations.len() {
            solutions.push(current.clone());
            return;
        }
        let observation = observations[position];
        for (track_id, _) in candidates.get(&observation).into_iter().flatten() {
            if used.insert(*track_id) {
                current.push((observation, *track_id));
                visit(
                    position + 1,
                    observations,
                    candidates,
                    used,
                    current,
                    solutions,
                    limit,
                );
                current.pop();
                used.remove(track_id);
            }
        }
    }

    let mut solutions = Vec::new();
    visit(
        0,
        observations,
        candidates,
        &mut HashSet::new(),
        &mut Vec::new(),
        &mut solutions,
        limit,
    );
    solutions
}

/// Finds the highest-scoring one-to-one assignment for one name family.
fn find_best_family_matching(
    observations: &[usize],
    candidates: &HashMap<usize, Vec<(usize, u16)>>,
) -> FamilyMatching {
    let (assignments, tied_observations) =
        find_best_weighted_matching(observations, candidates, |_, track| track);
    FamilyMatching {
        assignments,
        tied_observations,
    }
}

/// Finds a maximum-weight family assignment while enforcing one track per
/// frame. The key also keeps observations from different frames independent.
fn find_best_global_family_matching(
    observations: &[(usize, usize)],
    candidates: &HashMap<(usize, usize), Vec<(usize, u16)>>,
) -> GlobalFamilyMatching {
    let (assignments, tied_observations) =
        find_best_weighted_matching(observations, candidates, |observation, track| {
            (observation.0, track)
        });
    GlobalFamilyMatching {
        assignments,
        tied_observations,
    }
}

fn candidate_score(
    observation: &Observation,
    track: &TrackBuilder,
    frame_observations: &[Observation],
    selectors: &HashMap<u32, FrameContainerInfo>,
) -> u16 {
    let exclusion = exclusion_evidence(observation, track, selectors);
    let name_available = !observation.generic_name && !track.generic_name;
    let name = name_match_score(observation, track);
    let group_match = track
        .group_paths
        .iter()
        .map(|path| group_similarity(&observation.group_path, path))
        .max()
        .unwrap_or(0);
    let rank = frame_observations
        .iter()
        .position(|candidate| candidate.source_layer_id == observation.source_layer_id)
        .unwrap_or(0) as i32;
    let median = median_order(track).unwrap_or(rank);
    let order = if exclusion == AssociationExclusionKind::StructuralMutualExclusion {
        0
    } else {
        20u16.saturating_sub((rank - median).unsigned_abs().min(5) as u16 * 4)
    };

    let geometry = track
        .observations
        .iter()
        .map(|previous| geometry_similarity(observation, previous))
        .max()
        .unwrap_or(0);

    let pixels = if track.observations.iter().any(|previous| {
        previous.width == observation.width
            && previous.height == observation.height
            && previous.pixels == observation.pixels
    }) {
        10
    } else {
        0
    };
    let exclusion_score = match exclusion {
        AssociationExclusionKind::StructuralMutualExclusion => 30,
        AssociationExclusionKind::ObservedDisjoint => 5,
        AssociationExclusionKind::None | AssociationExclusionKind::CoVisible => 0,
    };
    let score = u32::from(name + group_match + order + geometry + pixels + exclusion_score);
    let available_weight = if !name_available {
        70
    } else if observation.copy_suffixes.is_empty() && track.copy_suffixes.is_empty() {
        100
    } else {
        85
    };
    ((score * 100) / available_weight).min(100) as u16
}

fn copy_family_candidate_allowed(observation: &Observation, track: &TrackBuilder) -> bool {
    if observation.generic_name || track.generic_name {
        // Generic names may bridge an accidental rename only when both sides
        // retain a persistent structural parent.  A bare top-level generic
        // layer remains a separate candidate instead of becoming a semantic
        // role by elimination.
        return !observation.group_path.is_empty()
            && track
                .group_paths
                .iter()
                .any(|path| path == &observation.group_path);
    }
    observation.name_key == track.name_key
}

fn name_match_score(observation: &Observation, track: &TrackBuilder) -> u16 {
    if observation.generic_name || track.generic_name || observation.name_key != track.name_key {
        return 0;
    }
    if observation.normalized_name == track.normalized_name {
        30
    } else {
        15
    }
}

fn exclusion_evidence(
    observation: &Observation,
    track: &TrackBuilder,
    selectors: &HashMap<u32, FrameContainerInfo>,
) -> AssociationExclusionKind {
    let observed_disjoint = !track.observations.is_empty();
    for previous in &track.observations {
        if previous.frame_index == observation.frame_index {
            return AssociationExclusionKind::CoVisible;
        }
        if structurally_mutually_exclusive(observation, previous, selectors) {
            return AssociationExclusionKind::StructuralMutualExclusion;
        }
    }
    if observed_disjoint {
        AssociationExclusionKind::ObservedDisjoint
    } else {
        AssociationExclusionKind::None
    }
}

fn structurally_mutually_exclusive(
    observation: &Observation,
    previous: &ObservationSummary,
    selectors: &HashMap<u32, FrameContainerInfo>,
) -> bool {
    frame_containers_structurally_mutually_exclusive(
        &observation.frame_container_ids,
        &previous.frame_container_ids,
        selectors,
    )
}

/// Returns whether two frame-container paths select disjoint sibling alternatives.
fn frame_containers_structurally_mutually_exclusive(
    left_ids: &[u32],
    right_ids: &[u32],
    selectors: &HashMap<u32, FrameContainerInfo>,
) -> bool {
    left_ids.iter().any(|left_id| {
        right_ids.iter().any(|right_id| {
            if left_id == right_id {
                return false;
            }
            let Some(left) = selectors.get(left_id) else {
                return false;
            };
            let Some(right) = selectors.get(right_id) else {
                return false;
            };
            left.parent_id == right.parent_id
                && left.active_frames.is_disjoint(&right.active_frames)
        })
    })
}

fn geometry_similarity(observation: &Observation, previous: &ObservationSummary) -> u16 {
    let width = ratio_score(observation.width, previous.width);
    let height = ratio_score(observation.height, previous.height);
    let dx = (observation.x - previous.x).unsigned_abs().min(32) as u16;
    let dy = (observation.y - previous.y).unsigned_abs().min(32) as u16;
    let position = 20u16.saturating_sub((dx + dy).min(20));
    ((width + height) / 2).saturating_add(position / 2).min(20)
}

fn ratio_score(left: u32, right: u32) -> u16 {
    if left == 0 || right == 0 {
        return 0;
    }
    ((left.min(right) as f64 / left.max(right) as f64) * 10.0).round() as u16
}

fn group_similarity(left: &[GroupSegment], right: &[GroupSegment]) -> u16 {
    let common = left
        .iter()
        .zip(right)
        .take_while(|(a, b)| a.key == b.key)
        .count();
    if common == left.len() && common == right.len() {
        20
    } else if common > 0 {
        12
    } else {
        0
    }
}

fn median_order(track: &TrackBuilder) -> Option<i32> {
    let mut values = track
        .observations
        .iter()
        .map(|observation| observation.source_order as i32)
        .collect::<Vec<_>>();
    values.sort_unstable();
    values.get(values.len() / 2).copied()
}

fn evidence_for(
    observation: &Observation,
    track: &TrackBuilder,
    score: u16,
    selectors: &HashMap<u32, FrameContainerInfo>,
) -> Vec<String> {
    let mut evidence = Vec::new();
    if !observation.generic_name
        && !track.generic_name
        && observation.normalized_name == track.normalized_name
    {
        evidence.push("normalized name".to_string());
    } else if name_match_score(observation, track) > 0 {
        evidence.push(format!(
            "copy-name family ({})",
            format_copy_suffixes(&observation.copy_suffixes)
        ));
    }
    if track
        .group_paths
        .iter()
        .any(|path| group_similarity(&observation.group_path, path) >= 20)
    {
        evidence.push("persistent group path".to_string());
    }
    if track.observations.iter().any(|previous| {
        previous.width == observation.width
            && previous.height == observation.height
            && previous.pixels == observation.pixels
    }) {
        evidence.push("exact RGBA pixels".to_string());
    }
    if score >= 40 {
        evidence.push("geometry/order context".to_string());
    }
    match exclusion_evidence(observation, track, selectors) {
        AssociationExclusionKind::ObservedDisjoint => {
            evidence.push("observed frame-disjoint hint".to_string());
        }
        AssociationExclusionKind::StructuralMutualExclusion => {
            evidence.push("structural frame-container exclusion hint".to_string());
        }
        AssociationExclusionKind::None | AssociationExclusionKind::CoVisible => {}
    }
    evidence
}

fn stable_track_order(
    tracks: &[TrackBuilder],
    frames: &[Vec<Observation>],
    decisions: &[AssociationDecision],
    anchor_order: &[usize],
    mode: StableOrderMode,
) -> Result<(Vec<usize>, Vec<String>), String> {
    if mode == StableOrderMode::Anchor {
        return Ok((anchor_order.to_vec(), Vec::new()));
    }

    let observation_map = decisions
        .iter()
        .map(|decision| {
            (
                (decision.frame_index as usize, decision.source_layer_id),
                decision.track_id,
            )
        })
        .collect::<HashMap<_, _>>();
    let mut supports = HashMap::<(usize, usize), (u32, u32)>::new();
    for observations in frames {
        for (left_index, left) in observations.iter().enumerate() {
            for right in observations.iter().skip(left_index + 1) {
                if !alpha_overlap(left, right) {
                    continue;
                }
                let Some(&left_track) =
                    observation_map.get(&(left.frame_index, left.source_layer_id))
                else {
                    continue;
                };
                let Some(&right_track) =
                    observation_map.get(&(right.frame_index, right.source_layer_id))
                else {
                    continue;
                };
                if left_track == right_track {
                    continue;
                }
                let (first, second, first_before) = if left_track < right_track {
                    (left_track, right_track, true)
                } else {
                    (right_track, left_track, false)
                };
                let support = supports.entry((first, second)).or_default();
                if first_before {
                    support.0 += 1;
                } else {
                    support.1 += 1;
                }
            }
        }
    }

    let mut base_order = HashMap::new();
    for (index, track_id) in anchor_order.iter().copied().enumerate() {
        base_order.insert(track_id, index as i32);
    }
    let anchor_positions = anchor_order
        .iter()
        .copied()
        .enumerate()
        .map(|(position, track_id)| (track_id, position))
        .collect::<HashMap<_, _>>();
    let mut relations = supports.into_iter().collect::<Vec<_>>();
    relations.sort_by_key(|((first, second), (forward, reverse))| {
        (
            std::cmp::Reverse((*forward).max(*reverse)),
            std::cmp::Reverse((*forward).abs_diff(*reverse)),
            *first,
            *second,
        )
    });

    let mut edges = Vec::new();
    let mut diagnostics = Vec::new();
    for ((first, second), (forward, reverse)) in relations {
        let winner = forward.max(reverse);
        let loser = forward.min(reverse);
        let total = winner + loser;
        let confident = winner >= 2 && winner >= loser + 2 && winner * 3 >= total * 2;
        if !confident {
            let message = format!(
                "stable order unresolved for tracks {} ({}) and {} ({}): support {}-{}; anchor order retained",
                first,
                track_name(tracks, first),
                second,
                track_name(tracks, second),
                forward,
                reverse,
            );
            if mode == StableOrderMode::Strict {
                return Err(message);
            }
            diagnostics.push(message);
            continue;
        }

        let (before, after) = if forward >= reverse {
            (first, second)
        } else {
            (second, first)
        };
        let anchor_distance = anchor_positions
            .get(&before)
            .zip(anchor_positions.get(&after))
            .map_or(usize::MAX, |(before, after)| before.abs_diff(*after));
        if anchor_distance > 1 {
            let message = format!(
                "stable order retained anchor barrier between tracks {} ({}) and {} ({}): support {}-{}",
                before,
                track_name(tracks, before),
                after,
                track_name(tracks, after),
                forward,
                reverse,
            );
            if mode == StableOrderMode::Strict {
                return Err(message);
            }
            diagnostics.push(message);
            continue;
        }
        if would_create_cycle(&edges, before, after) {
            let message = format!(
                "stable order cycle skipped for tracks {} ({}) before {} ({}): support {}-{}",
                before,
                track_name(tracks, before),
                after,
                track_name(tracks, after),
                forward,
                reverse,
            );
            if mode == StableOrderMode::Strict {
                return Err(message);
            }
            diagnostics.push(message);
            continue;
        }
        edges.push((before, after));
    }

    let track_ids = (0..tracks.len()).collect::<Vec<_>>();
    Ok((
        stable_id_order(&track_ids, &edges, &base_order),
        diagnostics,
    ))
}

fn track_name<'tracks>(tracks: &'tracks [TrackBuilder<'_>], track_id: usize) -> &'tracks str {
    tracks
        .get(track_id)
        .map(|track| track.name.as_str())
        .unwrap_or("<missing>")
}

fn would_create_cycle(edges: &[(usize, usize)], before: usize, after: usize) -> bool {
    if before == after {
        return true;
    }
    let mut pending = vec![after];
    let mut visited = HashSet::new();
    while let Some(current) = pending.pop() {
        if !visited.insert(current) {
            continue;
        }
        if current == before {
            return true;
        }
        for &(edge_before, edge_after) in edges {
            if edge_before == current {
                pending.push(edge_after);
            }
        }
    }
    false
}

fn candidate_names(
    candidates: &[(usize, u16)],
    tracks: &[TrackBuilder],
    selected: usize,
) -> Vec<String> {
    candidates
        .iter()
        .filter(|(track_id, _)| *track_id != selected)
        .filter_map(|(track_id, _)| tracks.get(*track_id).map(|track| track.name.clone()))
        .collect()
}

fn new_track<'doc>(
    track_id: usize,
    observation: &Observation<'doc>,
    frame_count: usize,
) -> TrackBuilder<'doc> {
    TrackBuilder {
        id: TrackId(track_id).index(),
        name: observation.name.clone(),
        normalized_name: observation.normalized_name.clone(),
        name_key: observation.name_key.clone(),
        generic_name: observation.generic_name,
        copy_suffixes: observation.copy_suffixes.clone(),
        representative_source_layer_id: observation.source_layer_id,
        cels: vec![None; frame_count],
        observation_ids: Vec::new(),
        observations: Vec::new(),
        group_paths: Vec::new(),
    }
}

fn record_assignment<'doc>(
    track: &mut TrackBuilder<'doc>,
    observation: &Observation<'doc>,
    cel: PlannedCel,
) {
    track.cels[observation.frame_index] = Some(cel);
    track.observation_ids.push(observation.id);
    track.group_paths.push(observation.group_path.clone());
    track.observations.push(ObservationSummary {
        evidence: Rc::clone(&observation.evidence),
        frame_index: observation.frame_index,
        source_order: observation.source_order,
        x: observation.x,
        y: observation.y,
        width: observation.width,
        height: observation.height,
    });
}

fn decision(
    observation: &Observation,
    track_id: usize,
    status: AssociationDecisionStatus,
    score: u16,
    margin: u16,
    evidence: Vec<String>,
    alternatives: Vec<String>,
) -> AssociationDecision {
    AssociationDecision {
        frame_index: observation.frame_index as u32,
        source_layer_id: observation.source_layer_id,
        source_path: observation.source_path.clone(),
        original_name: observation.name.clone(),
        normalized_name: observation.normalized_name.clone(),
        normalized_base_name: observation.name_key.clone(),
        copy_suffixes: observation.copy_suffixes.clone(),
        suffix_limit_reached: observation.suffix_limit_reached,
        name_evidence_weight: name_evidence_weight(observation),
        association_phase: AssociationPhase::Residual,
        same_frame_instance_count: 1,
        matching_tie: false,
        rejection_reasons: Vec::new(),
        exclusion_evidence: AssociationExclusionKind::None,
        track_id,
        status,
        score,
        margin,
        evidence,
        alternatives,
    }
}

fn rejection_reasons(
    candidates: &[(usize, u16)],
    status: AssociationDecisionStatus,
    matching_tie: bool,
    same_frame_instance_count: usize,
) -> Vec<String> {
    let mut reasons = Vec::new();
    if same_frame_instance_count > 1 {
        reasons.push(format!(
            "same name family has {same_frame_instance_count} observations in this frame"
        ));
    }
    if matching_tie {
        reasons.push("family matching has multiple optimal assignments".to_string());
    }
    if candidates.len() > 1 {
        reasons.push("multiple existing family candidates remain".to_string());
    }
    let candidate_margin = candidates.first().zip(candidates.get(1)).map_or_else(
        || candidates.first().map_or(0, |(_, score)| *score),
        |((_, best), (_, second))| best.saturating_sub(*second),
    );
    if status == AssociationDecisionStatus::Ambiguous
        && candidates
            .first()
            .is_some_and(|(_, score)| *score < 75 || candidate_margin < 15)
    {
        reasons.push("candidate score or margin is below the safe threshold".to_string());
    }
    reasons
}

fn parse_layer_name(name: &str) -> ParsedLayerName {
    CopySuffixCatalog.parse(name)
}

fn canonical_name(name: &str) -> String {
    parse_layer_name(name).base_name
}

fn name_evidence_weight(observation: &Observation) -> u16 {
    if observation.generic_name {
        0
    } else if observation.copy_suffixes.is_empty() {
        30
    } else {
        15
    }
}

fn choose_group_paths(
    tracks: &mut [TrackBuilder],
    document: &NormalizedDocument,
    warnings: &mut Vec<String>,
) -> Vec<Vec<GroupSegment>> {
    let minimum_support = (document.frames.len() * 2).div_ceil(3);
    tracks
        .iter()
        .map(|track| {
            let mut counts = HashMap::<Vec<GroupKey>, usize>::new();
            let mut representatives = HashMap::<Vec<GroupKey>, Vec<GroupSegment>>::new();
            for path in &track.group_paths {
                let keys = path.iter().map(|segment| segment.key.clone()).collect::<Vec<_>>();
                *counts.entry(keys.clone()).or_default() += 1;
                representatives.entry(keys).or_insert_with(|| path.clone());
            }
            let mut candidates = counts.into_iter().collect::<Vec<_>>();
            candidates.sort_by(|(left_keys, left_count), (right_keys, right_count)| {
                right_count
                    .cmp(left_count)
                    .then_with(|| left_keys.cmp(right_keys))
            });
            let Some((keys, count)) = candidates.first().cloned() else {
                return Vec::new();
            };
            if count < minimum_support {
                return Vec::new();
            }
            if candidates
                .get(1)
                .is_some_and(|(_, competing_count)| *competing_count == count)
            {
                warnings.push(format!(
                    "track {} was flattened because persistent parent groups have equal support",
                    track.id
                ));
                return Vec::new();
            }
            let minimum_member_support = (track.group_paths.len() * 4).div_ceil(5);
            if count < minimum_member_support {
                warnings.push(format!(
                    "track {} was flattened because its persistent parent support is below 80%",
                    track.id
                ));
                return Vec::new();
            }
            let path = representatives[&keys]
                .iter()
                .filter(|segment| {
                    segment.source_layer_id.is_some_and(|source_layer_id| {
                        document.find_layer(source_layer_id).is_some_and(|layer| {
                        let transparent = layer
                            .blend_mode
                            .as_deref()
                            .is_none_or(|mode| mode == "pass through");
                        let opaque = layer.opacity.is_none_or(|opacity| opacity == 1.0);
                        if !transparent || !opaque {
                            warnings.push(format!(
                                "group {} was flattened because its compositing attributes are not transparent",
                                layer.id
                            ));
                        }
                            transparent && opaque
                        })
                    })
                })
                .cloned()
                .collect::<Vec<_>>();
            if path.len() > 1 {
                warnings.push(format!(
                    "nested group path for track {} was flattened to preserve cross-layer z-order",
                    track.id
                ));
            }
            path.into_iter().take(1).collect()
        })
        .collect()
}

fn anchor_track_order(tracks: &[TrackBuilder]) -> Vec<usize> {
    let mut track_ids = (0..tracks.len()).collect::<Vec<_>>();
    track_ids.sort_by_key(|track_id| {
        (
            tracks[*track_id]
                .observations
                .first()
                .map(|observation| observation.source_order as i32)
                .unwrap_or(i32::MAX),
            *track_id,
        )
    });
    track_ids
}

/// Plans presentation-only candidate folders from the conservative track order.
fn plan_candidate_groups(
    tracks: &[TrackBuilder],
    decisions: &[AssociationDecision],
    track_order: &[usize],
    selectors: &HashMap<u32, FrameContainerInfo>,
    mode: UncertainLayerMode,
    warnings: &mut Vec<String>,
) -> (
    Vec<CandidateGroupReport>,
    HashMap<usize, CandidateGroupPath>,
) {
    let positions = track_order
        .iter()
        .enumerate()
        .map(|(position, track_id)| (*track_id, position))
        .collect::<HashMap<_, _>>();
    let confirmed = tracks
        .iter()
        .filter(|track| track_is_confirmed(track.id, track, decisions))
        .map(|track| track.id)
        .collect::<Vec<_>>();
    let uncertain = tracks
        .iter()
        .filter(|track| track_is_uncertain(track.id, track, decisions))
        .map(|track| track.id)
        .collect::<Vec<_>>();
    let mut assignments = BTreeMap::<usize, Vec<usize>>::new();
    let mut assignment_evidence = BTreeMap::<usize, Vec<String>>::new();
    let mut reports = Vec::new();

    for track_id in uncertain {
        let track = &tracks[track_id];
        let family_anchors = confirmed
            .iter()
            .copied()
            .filter(|anchor_id| {
                let anchor = &tracks[*anchor_id];
                !track.generic_name && !anchor.generic_name && track.name_key == anchor.name_key
            })
            .collect::<Vec<_>>();
        let (anchor, evidence) = if family_anchors.len() == 1 {
            (
                Some(family_anchors[0]),
                vec!["same normalized name family".to_string()],
            )
        } else {
            if let Some((anchor_id, reason)) =
                boundary_anchor_preference(track, tracks, &confirmed, &positions)
            {
                (Some(anchor_id), vec![reason])
            } else {
                let mut nearby = confirmed
                    .iter()
                    .copied()
                    .map(|anchor_id| {
                        (
                            anchor_id,
                            candidate_anchor_score(track, &tracks[anchor_id], &positions),
                        )
                    })
                    .collect::<Vec<_>>();
                nearby
                    .sort_by(|left, right| right.1.cmp(&left.1).then_with(|| left.0.cmp(&right.0)));
                match nearby.as_slice() {
                    [(anchor_id, score), second, ..]
                        if *score >= 50
                            && score.saturating_sub(second.1) >= 10
                            && !tracks[*anchor_id].generic_name =>
                    {
                        (
                            Some(*anchor_id),
                            vec![format!("stable-order candidate affinity score {score}")],
                        )
                    }
                    [(anchor_id, score), ..]
                        if *score >= 50 && !tracks[*anchor_id].generic_name =>
                    {
                        (
                            Some(*anchor_id),
                            vec![format!("stable-order candidate affinity score {score}")],
                        )
                    }
                    _ => (None, Vec::new()),
                }
            }
        };

        let Some(anchor_id) = anchor else {
            reports.push(CandidateGroupReport {
                name: format!("候选 - {}", track.name),
                anchor_track_id: track_id,
                member_track_ids: vec![track_id],
                evidence: Vec::new(),
                relations: Vec::new(),
                complete_interval: false,
                emitted: false,
                rejection_reason: Some(
                    "no unique confirmed anchor within the stable-order neighborhood".to_string(),
                ),
            });
            continue;
        };

        let relation = candidate_track_relation_report(anchor_id, track_id, tracks, selectors);
        if relation.relation != CandidateTrackRelation::StructuralMutualExclusion {
            let reason = match relation.relation {
                CandidateTrackRelation::CoVisible => {
                    "candidate is effectively co-visible with the proposed anchor"
                }
                CandidateTrackRelation::ObservedDisjoint => {
                    "candidate is only observed-disjoint without structural exclusion evidence"
                }
                CandidateTrackRelation::Unrelated => {
                    "candidate has no structural exclusion relationship with the proposed anchor"
                }
                CandidateTrackRelation::StructuralMutualExclusion => unreachable!(),
            };
            let mut members = vec![anchor_id, track_id];
            members.sort_by_key(|member| positions.get(member).copied().unwrap_or(usize::MAX));
            let complete_interval =
                candidate_members_form_complete_interval(&members, track_order, &positions);
            reports.push(CandidateGroupReport {
                name: format!("候选 - {}", tracks[anchor_id].name),
                anchor_track_id: anchor_id,
                member_track_ids: members,
                evidence,
                relations: vec![relation],
                complete_interval,
                emitted: false,
                rejection_reason: Some(reason.to_string()),
            });
            continue;
        }
        assignments.entry(anchor_id).or_default().push(track_id);
        assignment_evidence.entry(anchor_id).or_default().extend(
            evidence
                .into_iter()
                .chain(["structural mutual exclusion".to_string()]),
        );
    }

    let mut paths = HashMap::new();
    for (anchor_id, mut candidates) in assignments {
        candidates.sort_by_key(|track_id| positions.get(track_id).copied().unwrap_or(usize::MAX));
        candidates.dedup();
        let mut members = candidates;
        members.push(anchor_id);
        members.sort_by_key(|track_id| positions.get(track_id).copied().unwrap_or(usize::MAX));
        members.dedup();
        let complete_interval =
            candidate_members_form_complete_interval(&members, track_order, &positions);
        let relations = candidate_group_relations(&members, tracks, selectors);
        let has_co_visible_pair = relations
            .iter()
            .any(|relation| relation.relation == CandidateTrackRelation::CoVisible);
        let name = format!("候选 - {}", tracks[anchor_id].name);
        let mut evidence = assignment_evidence.remove(&anchor_id).unwrap_or_default();
        evidence.push("stable-order neighborhood".to_string());
        evidence.sort();
        evidence.dedup();
        let rejection_reason = if has_co_visible_pair {
            Some("candidate folder contains tracks that are effectively co-visible".to_string())
        } else if !complete_interval {
            Some("candidate members do not occupy one complete stable-order interval".to_string())
        } else if mode == UncertainLayerMode::Flat {
            Some("uncertain layer mode is flat".to_string())
        } else {
            None
        };
        let emitted = rejection_reason.is_none() && mode == UncertainLayerMode::Group;
        if emitted {
            for member in &members {
                paths.insert(
                    *member,
                    CandidateGroupPath {
                        name: name.clone(),
                        anchor_track_id: anchor_id,
                    },
                );
            }
        } else {
            warnings.push(format!(
                "candidate group {name} was not emitted: {}",
                rejection_reason.as_deref().unwrap_or("unknown reason")
            ));
        }
        reports.push(CandidateGroupReport {
            name,
            anchor_track_id: anchor_id,
            member_track_ids: members,
            evidence,
            relations,
            complete_interval,
            emitted,
            rejection_reason,
        });
    }
    reports.sort_by(|left, right| {
        left.anchor_track_id
            .cmp(&right.anchor_track_id)
            .then_with(|| left.name.cmp(&right.name))
    });
    (reports, paths)
}

/// Builds the complete pairwise relation matrix for one proposed candidate folder.
fn candidate_group_relations(
    members: &[usize],
    tracks: &[TrackBuilder],
    selectors: &HashMap<u32, FrameContainerInfo>,
) -> Vec<CandidateTrackRelationReport> {
    let mut relations = Vec::new();
    for (left_index, left_track_id) in members.iter().enumerate() {
        for right_track_id in members.iter().skip(left_index + 1) {
            relations.push(candidate_track_relation_report(
                *left_track_id,
                *right_track_id,
                tracks,
                selectors,
            ));
        }
    }
    relations
}

/// Classifies effective co-visibility before considering structural exclusion evidence.
fn candidate_track_relation_report(
    left_track_id: usize,
    right_track_id: usize,
    tracks: &[TrackBuilder],
    selectors: &HashMap<u32, FrameContainerInfo>,
) -> CandidateTrackRelationReport {
    let left = &tracks[left_track_id];
    let right = &tracks[right_track_id];
    let left_frames = left
        .observations
        .iter()
        .map(|observation| observation.frame_index)
        .collect::<HashSet<_>>();
    let mut co_visible_frames = right
        .observations
        .iter()
        .filter_map(|observation| {
            left_frames
                .contains(&observation.frame_index)
                .then_some(observation.frame_index as u32)
        })
        .collect::<Vec<_>>();
    co_visible_frames.sort_unstable();
    co_visible_frames.dedup();

    let relation = if !co_visible_frames.is_empty() {
        CandidateTrackRelation::CoVisible
    } else if left.observations.iter().any(|left_observation| {
        right.observations.iter().any(|right_observation| {
            frame_containers_structurally_mutually_exclusive(
                &left_observation.frame_container_ids,
                &right_observation.frame_container_ids,
                selectors,
            )
        })
    }) {
        CandidateTrackRelation::StructuralMutualExclusion
    } else if !left.observations.is_empty() && !right.observations.is_empty() {
        CandidateTrackRelation::ObservedDisjoint
    } else {
        CandidateTrackRelation::Unrelated
    };

    CandidateTrackRelationReport {
        left_track_id,
        right_track_id,
        relation,
        co_visible_frames,
    }
}

/// Returns whether proposed members fill their entire stable-order interval.
fn candidate_members_form_complete_interval(
    members: &[usize],
    track_order: &[usize],
    positions: &HashMap<usize, usize>,
) -> bool {
    let member_set = members.iter().copied().collect::<HashSet<_>>();
    let Some(left) = members
        .iter()
        .filter_map(|track_id| positions.get(track_id).copied())
        .min()
    else {
        return false;
    };
    let Some(right) = members
        .iter()
        .filter_map(|track_id| positions.get(track_id).copied())
        .max()
    else {
        return false;
    };
    track_order[left..=right]
        .iter()
        .all(|track_id| member_set.contains(track_id))
        && right - left + 1 == member_set.len()
}

fn boundary_anchor_preference(
    candidate: &TrackBuilder,
    tracks: &[TrackBuilder],
    confirmed: &[usize],
    positions: &HashMap<usize, usize>,
) -> Option<(usize, String)> {
    let candidate_position = positions.get(&candidate.id).copied()?;
    let previous = confirmed
        .iter()
        .copied()
        .filter(|track_id| positions[track_id] < candidate_position)
        .max_by_key(|track_id| positions[track_id]);
    let next = confirmed
        .iter()
        .copied()
        .filter(|track_id| positions[track_id] > candidate_position)
        .min_by_key(|track_id| positions[track_id]);
    let (Some(previous), Some(next)) = (previous, next) else {
        return None;
    };

    let mut candidate_is_after_previous = false;
    let mut candidate_is_before_next = false;
    for candidate_observation in &candidate.observations {
        candidate_is_after_previous |= tracks[previous].observations.iter().any(|anchor| {
            anchor.frame_index == candidate_observation.frame_index
                && candidate_observation.source_order > anchor.source_order
        });
        candidate_is_before_next |= tracks[next].observations.iter().any(|anchor| {
            anchor.frame_index == candidate_observation.frame_index
                && candidate_observation.source_order < anchor.source_order
        });
    }
    match (candidate_is_after_previous, candidate_is_before_next) {
        (true, false) => Some((
            next,
            "source-order boundary after previous anchor".to_string(),
        )),
        (false, true) => Some((
            previous,
            "source-order boundary before next anchor".to_string(),
        )),
        _ => None,
    }
}

fn candidate_anchor_score(
    candidate: &TrackBuilder,
    anchor: &TrackBuilder,
    positions: &HashMap<usize, usize>,
) -> u16 {
    let rank = positions
        .get(&candidate.id)
        .zip(positions.get(&anchor.id))
        .map_or(0, |(candidate, anchor)| candidate.abs_diff(*anchor));
    let adjacency = match rank {
        0 => 50,
        1 => 40,
        2 => 30,
        3 => 20,
        _ => 0,
    };
    let geometry = candidate
        .observations
        .iter()
        .flat_map(|left| {
            anchor
                .observations
                .iter()
                .map(move |right| geometry_similarity_summary(left, right))
        })
        .max()
        .unwrap_or(0);
    let same_frame_order = candidate
        .observations
        .iter()
        .flat_map(|left| {
            anchor
                .observations
                .iter()
                .filter(move |right| right.frame_index == left.frame_index)
                .map(move |right| {
                    15u16.saturating_sub(
                        left.source_order.abs_diff(right.source_order).min(7) as u16 * 2,
                    )
                })
        })
        .max()
        .unwrap_or(0);
    (adjacency + geometry + same_frame_order).min(100)
}

fn geometry_similarity_summary(
    observation: &ObservationSummary,
    previous: &ObservationSummary,
) -> u16 {
    let width = ratio_score(observation.width, previous.width);
    let height = ratio_score(observation.height, previous.height);
    let dx = (observation.x - previous.x).unsigned_abs().min(32) as u16;
    let dy = (observation.y - previous.y).unsigned_abs().min(32) as u16;
    let position = 20u16.saturating_sub((dx + dy).min(20));
    ((width + height) / 2).saturating_add(position / 2).min(20)
}

fn track_is_confirmed(
    track_id: usize,
    track: &TrackBuilder,
    decisions: &[AssociationDecision],
) -> bool {
    !track.generic_name
        && track.copy_suffixes.is_empty()
        && decisions.iter().any(|decision| {
            decision.track_id == track_id && decision.status == AssociationDecisionStatus::Strong
        })
}

fn track_is_uncertain(
    track_id: usize,
    track: &TrackBuilder,
    decisions: &[AssociationDecision],
) -> bool {
    track.generic_name
        || !track.copy_suffixes.is_empty()
        || decisions.iter().any(|decision| {
            decision.track_id == track_id
                && matches!(
                    decision.status,
                    AssociationDecisionStatus::Ambiguous | AssociationDecisionStatus::NewTrack
                )
        })
}

fn build_nodes(
    group_paths: &[Vec<GroupSegment>],
    track_order: &[usize],
    candidate_group_paths: &HashMap<usize, CandidateGroupPath>,
) -> Vec<PlannedNode> {
    let mut roots = Vec::new();
    for &track_id in track_order {
        let mut path = Vec::new();
        if let Some(candidate_path) = candidate_group_paths.get(&track_id) {
            path.push(GroupSegment {
                source_layer_id: None,
                name: candidate_path.name.clone(),
                key: GroupKey::Candidate(candidate_path.anchor_track_id),
            });
        }
        path.extend(group_paths[track_id].iter().cloned());
        insert_track(&mut roots, &path, track_id);
    }
    roots
        .into_iter()
        .map(PlannedNodeBuilder::into_planned_node)
        .collect()
}

/// Inserts one track into the keyed intermediate tree.
fn insert_track(nodes: &mut Vec<PlannedNodeBuilder>, path: &[GroupSegment], track_id: usize) {
    if let Some(segment) = path.first() {
        let index = nodes.iter().position(
            |node| matches!(node, PlannedNodeBuilder::Group { key, .. } if key == &segment.key),
        );
        let index = if let Some(index) = index {
            index
        } else {
            nodes.push(PlannedNodeBuilder::Group {
                key: segment.key.clone(),
                name: segment.name.clone(),
                source_layer_id: segment.source_layer_id,
                children: Vec::new(),
            });
            nodes.len() - 1
        };
        if let PlannedNodeBuilder::Group { children, .. } = &mut nodes[index] {
            insert_track(children, &path[1..], track_id);
        }
    } else {
        nodes.push(PlannedNodeBuilder::Track { track_id });
    }
}

impl PlannedNodeBuilder {
    /// Removes planning-only identities and produces the public writer tree.
    fn into_planned_node(self) -> PlannedNode {
        match self {
            Self::Group {
                name,
                source_layer_id,
                children,
                ..
            } => PlannedNode::Group {
                name,
                source_layer_id,
                children: children.into_iter().map(Self::into_planned_node).collect(),
            },
            Self::Track { track_id } => PlannedNode::Track { track_id },
        }
    }
}

/// Verifies that every emitted candidate folder exists once with exactly its reported tracks.
fn validate_candidate_group_topology(
    root_nodes: &[PlannedNode],
    reports: &[CandidateGroupReport],
) -> Result<(), String> {
    for report in reports.iter().filter(|report| report.emitted) {
        let mut matching_groups = Vec::new();
        collect_candidate_groups(root_nodes, &report.name, &mut matching_groups);
        if matching_groups.len() != 1 {
            return Err(format!(
                "candidate folder {} was emitted {} times instead of once",
                report.name,
                matching_groups.len()
            ));
        }
        let mut actual_track_ids = Vec::new();
        collect_planned_track_ids(matching_groups[0], &mut actual_track_ids);
        actual_track_ids.sort_unstable();
        let mut expected_track_ids = report.member_track_ids.clone();
        expected_track_ids.sort_unstable();
        if actual_track_ids != expected_track_ids {
            return Err(format!(
                "candidate folder {} contains tracks {:?}, expected {:?}",
                report.name, actual_track_ids, expected_track_ids
            ));
        }
    }
    Ok(())
}

/// Collects synthetic candidate groups with the requested display name.
fn collect_candidate_groups<'a>(
    nodes: &'a [PlannedNode],
    name: &str,
    output: &mut Vec<&'a [PlannedNode]>,
) {
    for node in nodes {
        if let PlannedNode::Group {
            name: group_name,
            source_layer_id,
            children,
        } = node
        {
            if source_layer_id.is_none() && group_name == name {
                output.push(children);
            }
            collect_candidate_groups(children, name, output);
        }
    }
}

/// Collects every logical track below one planned subtree.
fn collect_planned_track_ids(nodes: &[PlannedNode], output: &mut Vec<usize>) {
    for node in nodes {
        match node {
            PlannedNode::Group { children, .. } => collect_planned_track_ids(children, output),
            PlannedNode::Track { track_id } => output.push(*track_id),
        }
    }
}

/// Removes a semantically empty group shared by every planned track.
fn flatten_redundant_common_root(
    group_paths: &mut [Vec<GroupSegment>],
    tracks: &[TrackBuilder],
    document: &NormalizedDocument,
    selectors: &HashMap<u32, FrameContainerInfo>,
    warnings: &mut Vec<String>,
) {
    if group_paths.is_empty() || group_paths.iter().any(Vec::is_empty) {
        return;
    }
    let Some(common) = group_paths[0].first() else {
        return;
    };
    let common_key = common.key.clone();
    let common_name = common.name.clone();
    if group_paths
        .iter()
        .any(|path| path.first().is_none_or(|segment| segment.key != common_key))
    {
        return;
    }
    let Some(layer) = common
        .source_layer_id
        .and_then(|source_layer_id| document.find_layer(source_layer_id))
    else {
        return;
    };
    let transparent = layer
        .blend_mode
        .as_deref()
        .is_none_or(|mode| mode == "pass through");
    let opaque = layer.opacity.is_none_or(|opacity| opacity == 1.0);
    let only_selectors = !layer.children.is_empty()
        && layer
            .children
            .iter()
            .all(|child| selectors.contains_key(&child.id));
    let only_pixels = !layer.children.is_empty()
        && layer
            .children
            .iter()
            .all(|child| child.kind == NormalizedLayerKind::Pixel);
    let all_tracks_cover_children = tracks.iter().all(|track| {
        track.group_paths.iter().any(|path| {
            path.first()
                .is_some_and(|segment| segment.key == common_key)
        })
    });
    if transparent && opaque && all_tracks_cover_children && (only_selectors || only_pixels) {
        for path in group_paths.iter_mut() {
            path.remove(0);
        }
        warnings.push(format!(
            "common wrapper group {} was flattened from the auto output",
            common_name
        ));
    }
}

fn assign_z_indices(
    plan: &mut LayerWritePlan,
    frames: &[Vec<Observation>],
    mode: LayerZOrderMode,
) -> Result<Vec<String>, String> {
    let mut diagnostics = Vec::new();
    let mut observation_map = HashMap::new();
    for decision in &plan.report.decisions {
        observation_map.insert(
            (decision.frame_index as usize, decision.source_layer_id),
            decision.track_id,
        );
    }
    let mut base_order = HashMap::new();
    let mut flat_index = 0i32;
    collect_track_indices(&plan.root_nodes, &mut flat_index, &mut base_order);
    for (frame_index, observations) in frames.iter().enumerate() {
        let active = plan
            .tracks
            .iter()
            .filter_map(|track| track.cels[frame_index].map(|cel| (track.id, cel)))
            .collect::<Vec<_>>();
        let mut edges = Vec::new();
        for (left_index, left) in observations.iter().enumerate() {
            for right in observations.iter().skip(left_index + 1) {
                let Some(&left_track) = observation_map.get(&(frame_index, left.source_layer_id))
                else {
                    continue;
                };
                let Some(&right_track) = observation_map.get(&(frame_index, right.source_layer_id))
                else {
                    continue;
                };
                if alpha_overlap(left, right) {
                    edges.push((left_track, right_track));
                }
            }
        }
        let ordered = stable_topological_order(&active, &edges, &base_order);
        let mut slots = active
            .iter()
            .filter_map(|(track_id, _)| base_order.get(track_id).copied())
            .collect::<Vec<_>>();
        slots.sort_unstable();
        for (rank, track_id) in ordered.iter().enumerate() {
            let target = slots.get(rank).copied().unwrap_or(0);
            let current = base_order.get(track_id).copied().unwrap_or(0);
            let z = target - current;
            let z = i16::try_from(z).map_err(|_| {
                format!(
                    "logical track {} frame {} requires z-index {} outside i16 range",
                    track_id, frame_index, z
                )
            })?;
            if z != 0 {
                diagnostics.push(format!(
                    "frame {} track {} has a potential source-order change requiring z-index {}",
                    frame_index, track_id, z
                ));
                if mode == LayerZOrderMode::Auto
                    && let Some(cel_slot) = plan.tracks[*track_id].cels[frame_index].as_mut()
                {
                    cel_slot.z_index = z;
                }
            }
        }
    }
    Ok(diagnostics)
}

fn collect_track_indices(nodes: &[PlannedNode], next: &mut i32, output: &mut HashMap<usize, i32>) {
    for node in nodes {
        match node {
            PlannedNode::Group { children, .. } => {
                *next += 1;
                collect_track_indices(children, next, output);
            }
            PlannedNode::Track { track_id } => {
                output.insert(*track_id, *next);
                *next += 1;
            }
        }
    }
}

fn stable_topological_order(
    active: &[(usize, PlannedCel)],
    edges: &[(usize, usize)],
    base_order: &HashMap<usize, i32>,
) -> Vec<usize> {
    let ids = active.iter().map(|(id, _)| *id).collect::<Vec<_>>();
    stable_id_order(&ids, edges, base_order)
}

fn stable_id_order(
    ids: &[usize],
    edges: &[(usize, usize)],
    base_order: &HashMap<usize, i32>,
) -> Vec<usize> {
    let mut result = Vec::new();
    let mut remaining = ids.iter().copied().collect::<HashSet<_>>();
    while !remaining.is_empty() {
        let mut candidates = remaining
            .iter()
            .filter(|id| {
                !edges
                    .iter()
                    .any(|(before, after)| *after == **id && remaining.contains(before))
            })
            .copied()
            .collect::<Vec<_>>();
        if candidates.is_empty() {
            candidates = remaining.iter().copied().collect();
        }
        candidates.sort_by_key(|id| base_order.get(id).copied().unwrap_or(i32::MAX));
        let selected = candidates[0];
        remaining.remove(&selected);
        result.push(selected);
    }
    result
}

fn alpha_overlap(left: &Observation, right: &Observation) -> bool {
    for index in 0..left.pixels.len() / 4 {
        if left.pixels[index * 4 + 3] == 0 {
            continue;
        }
        let x = left.x + (index as i32 % left.width as i32);
        let y = left.y + (index as i32 / left.width as i32);
        let right_x = x - right.x;
        let right_y = y - right.y;
        if right_x < 0
            || right_y < 0
            || right_x >= right.width as i32
            || right_y >= right.height as i32
        {
            continue;
        }
        let right_index = (right_y as usize * right.width as usize + right_x as usize) * 4 + 3;
        if right.pixels[right_index] != 0 {
            return true;
        }
    }
    false
}

fn find_frame_selector_groups(
    layers: &[NormalizedLayer],
    frame_count: usize,
) -> HashMap<u32, FrameContainerInfo> {
    let mut selectors = HashMap::new();
    identify_selector_siblings(layers, frame_count, None, &mut selectors);
    selectors
}

fn identify_selector_siblings(
    layers: &[NormalizedLayer],
    frame_count: usize,
    parent_id: Option<u32>,
    selectors: &mut HashMap<u32, FrameContainerInfo>,
) {
    let group_children = layers
        .iter()
        .filter(|child| child.kind == NormalizedLayerKind::Group)
        .collect::<Vec<_>>();
    if group_children.len() >= 2 {
        let sets = group_children
            .iter()
            .map(|child| active_frames(child, frame_count))
            .collect::<Vec<_>>();
        let disjoint = sets.iter().enumerate().all(|(index, left)| {
            sets.iter()
                .skip(index + 1)
                .all(|right| left.is_disjoint(right))
        });
        let union = sets
            .iter()
            .flat_map(|set| set.iter().copied())
            .collect::<HashSet<_>>();
        if disjoint && union.len() >= 2 {
            for (child, active_frames) in group_children.iter().zip(sets) {
                selectors.insert(
                    child.id,
                    FrameContainerInfo {
                        parent_id,
                        active_frames,
                    },
                );
            }
        }
    }
    for layer in layers {
        if layer.kind == NormalizedLayerKind::Group {
            identify_selector_siblings(&layer.children, frame_count, Some(layer.id), selectors);
        }
    }
}

fn active_frames(layer: &NormalizedLayer, frame_count: usize) -> HashSet<usize> {
    (0..frame_count)
        .filter(|frame_index| has_visible_pixel(layer, *frame_index, true))
        .collect()
}

fn has_visible_pixel(layer: &NormalizedLayer, frame_index: usize, ancestors_visible: bool) -> bool {
    let visible = ancestors_visible
        && layer
            .frame_states
            .get(frame_index)
            .is_some_and(|state| state.enabled);
    match layer.kind {
        NormalizedLayerKind::Pixel => visible,
        NormalizedLayerKind::Group => layer
            .children
            .iter()
            .any(|child| has_visible_pixel(child, frame_index, visible)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn build_layer_write_plan(document: &NormalizedDocument) -> Result<LayerWritePlan, String> {
        super::build_layer_write_plan(
            document,
            AutoAssociationOptions {
                strategy: AssociationStrategy::Conservative {
                    uncertain_layers: UncertainLayerMode::Group,
                },
                ..AutoAssociationOptions::default()
            },
        )
    }

    #[test]
    fn default_plan_uses_compact_strategy_without_candidate_folders() {
        let plan = super::build_layer_write_plan(
            &document(vec![pixel(1, "rear foot", 0, [1, 2, 3, 255])]),
            AutoAssociationOptions::default(),
        )
        .expect("compact association should succeed");
        assert_eq!(plan.report.strategy, AssociationStrategy::Compact);
        assert!(plan.report.candidate_groups.is_empty());
    }
    use crate::{NormalizedBounds, NormalizedFrame, NormalizedLayerFrameState, NormalizedPixels};

    fn pixel(id: u32, name: &str, x: i32, color: [u8; 4]) -> NormalizedLayer {
        NormalizedLayer {
            id,
            name: name.to_string(),
            kind: NormalizedLayerKind::Pixel,
            bounds: NormalizedBounds {
                left: x,
                top: 0,
                right: x + 1,
                bottom: 1,
            },
            opacity: None,
            blend_mode: Some("normal".to_string()),
            hidden: Some(false),
            pixels: Some(NormalizedPixels {
                width: 1,
                height: 1,
                left: x,
                top: 0,
                data: color.to_vec(),
            }),
            children: Vec::new(),
            frame_states: vec![NormalizedLayerFrameState {
                frame_index: 0,
                record_present: false,
                enabled: true,
                explicit_enable: false,
                offset: None,
                reference_point: None,
                opacity: None,
            }],
        }
    }

    fn document(layers: Vec<NormalizedLayer>) -> NormalizedDocument {
        NormalizedDocument {
            canvas: (8, 8),
            frames: vec![NormalizedFrame {
                index: 0,
                source_id: None,
                duration_ms: None,
                dispose: None,
            }],
            root_layers: layers,
            ..NormalizedDocument::default()
        }
    }

    fn second_frame_state(enabled: bool) -> NormalizedLayerFrameState {
        NormalizedLayerFrameState {
            frame_index: 1,
            record_present: true,
            enabled,
            explicit_enable: true,
            offset: None,
            reference_point: None,
            opacity: None,
        }
    }

    fn two_frame_document(layers: Vec<NormalizedLayer>) -> NormalizedDocument {
        let mut document = document(layers);
        document.frames.push(NormalizedFrame {
            index: 1,
            source_id: Some(1),
            duration_ms: Some(100),
            dispose: None,
        });
        document
    }

    fn three_frame_family_layer(
        id: u32,
        name: &str,
        active_frame: usize,
        color: [u8; 4],
    ) -> NormalizedLayer {
        three_frame_family_layer_at(id, name, active_frame, 0, color)
    }

    fn three_frame_family_layer_at(
        id: u32,
        name: &str,
        active_frame: usize,
        x: i32,
        color: [u8; 4],
    ) -> NormalizedLayer {
        let mut layer = pixel(id, name, x, color);
        layer.frame_states[0].enabled = active_frame == 0;
        for frame_index in 1..3 {
            layer.frame_states.push(NormalizedLayerFrameState {
                frame_index,
                record_present: true,
                enabled: active_frame == frame_index as usize,
                explicit_enable: true,
                offset: None,
                reference_point: None,
                opacity: None,
            });
        }
        layer
    }

    fn three_frame_document(layers: Vec<NormalizedLayer>) -> NormalizedDocument {
        let mut document = document(layers);
        document.frames = (0..3)
            .map(|index| NormalizedFrame {
                index,
                source_id: Some(index),
                duration_ms: Some(100),
                dispose: None,
            })
            .collect();
        document
    }

    fn top_level_frame_container(
        id: u32,
        name: &str,
        active_frame: usize,
        child: NormalizedLayer,
    ) -> NormalizedLayer {
        top_level_frame_container_with_children(id, name, active_frame, vec![child])
    }

    fn top_level_frame_container_with_children(
        id: u32,
        name: &str,
        active_frame: usize,
        mut children: Vec<NormalizedLayer>,
    ) -> NormalizedLayer {
        for child in &mut children {
            child.frame_states[0].enabled = true;
            child.frame_states.push(second_frame_state(true));
        }
        let bounds = children
            .first()
            .expect("test frame container requires a child")
            .bounds;
        NormalizedLayer {
            id,
            name: name.to_string(),
            kind: NormalizedLayerKind::Group,
            bounds,
            opacity: None,
            blend_mode: Some("pass through".to_string()),
            hidden: Some(false),
            pixels: None,
            children,
            frame_states: vec![
                NormalizedLayerFrameState {
                    frame_index: 0,
                    record_present: true,
                    enabled: active_frame == 0,
                    explicit_enable: true,
                    offset: None,
                    reference_point: None,
                    opacity: None,
                },
                NormalizedLayerFrameState {
                    frame_index: 1,
                    record_present: true,
                    enabled: active_frame == 1,
                    explicit_enable: true,
                    offset: None,
                    reference_point: None,
                    opacity: None,
                },
            ],
        }
    }

    /// Creates a two-frame group whose child order can differ by frame.
    fn two_frame_order_document(non_overlapping: bool) -> NormalizedDocument {
        let mut a0 = pixel(1, "A", 0, [1, 2, 3, 255]);
        let mut b0 = pixel(2, "B", if non_overlapping { 1 } else { 0 }, [4, 5, 6, 255]);
        let mut a1 = pixel(3, "A", 0, [7, 8, 9, 255]);
        let mut b1 = pixel(
            4,
            "B",
            if non_overlapping { 1 } else { 0 },
            [10, 11, 12, 255],
        );
        for layer in [&mut a0, &mut b0, &mut a1, &mut b1] {
            layer.frame_states.push(NormalizedLayerFrameState {
                frame_index: 1,
                record_present: true,
                enabled: false,
                explicit_enable: true,
                offset: None,
                reference_point: None,
                opacity: None,
            });
        }
        a1.frame_states[0].enabled = false;
        a1.frame_states[1].enabled = true;
        b1.frame_states[0].enabled = false;
        b1.frame_states[1].enabled = true;
        let selector_one = NormalizedLayer {
            id: 10,
            name: "frame one".to_string(),
            kind: NormalizedLayerKind::Group,
            bounds: NormalizedBounds {
                left: 0,
                top: 0,
                right: 2,
                bottom: 1,
            },
            opacity: None,
            blend_mode: Some("pass through".to_string()),
            hidden: Some(false),
            pixels: None,
            children: vec![a0, b0],
            frame_states: vec![
                NormalizedLayerFrameState {
                    frame_index: 0,
                    record_present: true,
                    enabled: true,
                    explicit_enable: true,
                    offset: None,
                    reference_point: None,
                    opacity: None,
                },
                NormalizedLayerFrameState {
                    frame_index: 1,
                    record_present: true,
                    enabled: true,
                    explicit_enable: true,
                    offset: None,
                    reference_point: None,
                    opacity: None,
                },
            ],
        };
        let selector_two = NormalizedLayer {
            id: 11,
            name: "frame two".to_string(),
            kind: NormalizedLayerKind::Group,
            bounds: selector_one.bounds,
            opacity: None,
            blend_mode: Some("pass through".to_string()),
            hidden: Some(false),
            pixels: None,
            children: vec![b1, a1],
            frame_states: selector_one.frame_states.clone(),
        };
        let root = NormalizedLayer {
            id: 12,
            name: "root".to_string(),
            kind: NormalizedLayerKind::Group,
            bounds: selector_one.bounds,
            opacity: None,
            blend_mode: Some("pass through".to_string()),
            hidden: Some(false),
            pixels: None,
            children: vec![selector_one, selector_two],
            frame_states: vec![
                NormalizedLayerFrameState {
                    frame_index: 0,
                    record_present: false,
                    enabled: true,
                    explicit_enable: false,
                    offset: None,
                    reference_point: None,
                    opacity: None,
                },
                NormalizedLayerFrameState {
                    frame_index: 1,
                    record_present: false,
                    enabled: true,
                    explicit_enable: false,
                    offset: None,
                    reference_point: None,
                    opacity: None,
                },
            ],
        };
        let mut document = document(vec![root]);
        document.frames.push(NormalizedFrame {
            index: 1,
            source_id: Some(1),
            duration_ms: Some(100),
            dispose: None,
        });
        document
    }

    #[test]
    fn normalizes_copy_suffix_without_merging_same_frame_duplicates() {
        let plan = build_layer_write_plan(&document(vec![
            pixel(1, "foot", 0, [1, 2, 3, 255]),
            pixel(2, "foot Copy", 1, [4, 5, 6, 255]),
        ]))
        .expect("association should succeed");
        assert_eq!(plan.tracks.len(), 2);
        assert_eq!(
            plan.report.name_catalog_version,
            COPY_SUFFIX_CATALOG_VERSION
        );
        assert_eq!(plan.report.decisions[1].normalized_base_name, "foot");
        assert_eq!(plan.report.decisions[1].copy_suffixes[0].language, "en");
        assert_eq!(plan.report.decisions[1].name_evidence_weight, 15);
    }

    #[test]
    fn copy_name_family_preserves_weak_name_evidence_across_frames() {
        let mut foot = pixel(1, "foot", 0, [1, 2, 3, 255]);
        let mut body = pixel(2, "body", 1, [4, 5, 6, 255]);
        let mut foot_copy = pixel(3, "foot Copy #2", 0, [1, 2, 3, 255]);
        let mut body_copy = pixel(4, "body 拷贝", 1, [4, 5, 6, 255]);
        foot.frame_states.push(second_frame_state(false));
        body.frame_states.push(second_frame_state(false));
        foot_copy.frame_states[0].enabled = false;
        foot_copy.frame_states.push(second_frame_state(true));
        body_copy.frame_states[0].enabled = false;
        body_copy.frame_states.push(second_frame_state(true));

        let plan =
            build_layer_write_plan(&two_frame_document(vec![foot, body, foot_copy, body_copy]))
                .expect("association should succeed");
        let foot_track = plan
            .tracks
            .iter()
            .find(|track| track.name == "foot")
            .expect("foot track should exist");
        assert_eq!(
            foot_track.cels[1].expect("foot copy cel").source_layer_id,
            3
        );
        assert!(plan.report.decisions.iter().any(|decision| {
            decision.source_layer_id == 3
                && decision.name_evidence_weight == 15
                && decision
                    .evidence
                    .iter()
                    .any(|evidence| evidence.contains("copy-name family"))
        }));
    }

    #[test]
    fn family_association_matches_copy_variants_across_multiple_frames() {
        let plan = build_layer_write_plan(&three_frame_document(vec![
            three_frame_family_layer(1, "前翅膀", 0, [1, 2, 3, 255]),
            three_frame_family_layer(2, "前翅膀 拷贝 2", 1, [1, 2, 3, 255]),
            three_frame_family_layer(3, "前翅膀 拷贝 5", 2, [1, 2, 3, 255]),
        ]))
        .expect("family association should succeed");
        assert_eq!(plan.tracks.len(), 1);
        assert_eq!(
            plan.tracks[0]
                .cels
                .iter()
                .map(|cel| cel.expect("family cel").source_layer_id)
                .collect::<Vec<_>>(),
            vec![1, 2, 3]
        );
        assert!(plan.report.family_diagnostics().iter().any(|diagnostic| {
            diagnostic.contains("前翅膀") && diagnostic.contains("3 observations")
        }));
    }

    #[test]
    fn family_association_keeps_same_frame_instances_on_separate_tracks() {
        let plan = build_layer_write_plan(&document(vec![
            pixel(1, "wing", 0, [1, 2, 3, 255]),
            pixel(2, "wing Copy 2", 1, [4, 5, 6, 255]),
        ]))
        .expect("same-frame family should remain representable");
        assert_eq!(plan.tracks.len(), 2);
        assert!(plan.report.decisions.iter().all(|decision| {
            decision.same_frame_instance_count == 2 && decision.track_id < plan.tracks.len()
        }));
    }

    #[test]
    fn family_association_preserves_multiple_slots_when_instance_count_changes() {
        let plan = build_layer_write_plan(&three_frame_document(vec![
            three_frame_family_layer(1, "前翅膀", 0, [1, 2, 3, 255]),
            three_frame_family_layer_at(5, "前翅膀 拷贝", 0, 20, [4, 5, 6, 255]),
            three_frame_family_layer_at(2, "前翅膀 拷贝 2", 1, 0, [1, 2, 3, 255]),
            three_frame_family_layer_at(3, "前翅膀 拷贝 5", 1, 20, [4, 5, 6, 255]),
            three_frame_family_layer(4, "前翅膀 拷贝 7", 2, [1, 2, 3, 255]),
        ]))
        .expect("multi-track family should succeed");
        assert_eq!(plan.tracks.len(), 2);
        assert_eq!(
            plan.tracks
                .iter()
                .filter(|track| track.cels[1].is_some())
                .count(),
            2
        );
        let assigned_source_ids = plan
            .tracks
            .iter()
            .flat_map(|track| track.cels.iter().flatten().map(|cel| cel.source_layer_id))
            .collect::<HashSet<_>>();
        assert_eq!(assigned_source_ids, HashSet::from([1, 2, 3, 4, 5]));
    }

    #[test]
    fn structurally_mutually_exclusive_copy_names_associate_despite_position_change() {
        let plan = build_layer_write_plan(&two_frame_document(vec![
            top_level_frame_container(10, "frame 1", 0, pixel(1, "后衣摆", 0, [1, 2, 3, 255])),
            top_level_frame_container(
                11,
                "frame 2",
                1,
                pixel(2, "后衣摆 拷贝", 100, [4, 5, 6, 255]),
            ),
        ]))
        .expect("association should succeed");
        assert_eq!(plan.tracks.len(), 1);
        assert_eq!(
            plan.tracks[0].cels[0].expect("frame 1 cel").source_layer_id,
            1
        );
        assert_eq!(
            plan.tracks[0].cels[1].expect("frame 2 cel").source_layer_id,
            2
        );
        let decision = plan
            .report
            .decisions
            .iter()
            .find(|decision| decision.source_layer_id == 2)
            .expect("copy decision should exist");
        assert_eq!(
            decision.exclusion_evidence,
            AssociationExclusionKind::StructuralMutualExclusion
        );
        assert!(decision.order_evidence_ignored());
    }

    #[test]
    fn generic_renamed_observation_stays_separate_from_named_track() {
        let mut first = pixel(1, "rear foot", 0, [1, 2, 3, 255]);
        let mut second = pixel(2, "body", 1, [4, 5, 6, 255]);
        first.frame_states.push(NormalizedLayerFrameState {
            frame_index: 1,
            record_present: true,
            enabled: false,
            explicit_enable: true,
            offset: None,
            reference_point: None,
            opacity: None,
        });
        second.frame_states.push(NormalizedLayerFrameState {
            frame_index: 1,
            record_present: true,
            enabled: true,
            explicit_enable: true,
            offset: None,
            reference_point: None,
            opacity: None,
        });
        let mut renamed = pixel(3, "Layer 16", 0, [7, 8, 9, 255]);
        renamed.frame_states = vec![
            NormalizedLayerFrameState {
                frame_index: 0,
                record_present: true,
                enabled: false,
                explicit_enable: true,
                offset: None,
                reference_point: None,
                opacity: None,
            },
            NormalizedLayerFrameState {
                frame_index: 1,
                record_present: true,
                enabled: true,
                explicit_enable: true,
                offset: None,
                reference_point: None,
                opacity: None,
            },
        ];
        let mut document = document(vec![first, second, renamed]);
        document.frames.push(NormalizedFrame {
            index: 1,
            source_id: Some(2),
            duration_ms: Some(100),
            dispose: None,
        });
        let plan = build_layer_write_plan(&document).expect("association should succeed");
        assert_eq!(plan.tracks.len(), 3);
        assert!(plan.tracks.iter().any(|track| {
            track.name == "Layer 16" && track.cels[1].is_some_and(|cel| cel.source_layer_id == 3)
        }));
        assert!(!plan.report.decisions.iter().any(|decision| {
            decision.source_layer_id == 3
                && plan
                    .tracks
                    .get(decision.track_id)
                    .is_some_and(|track| track.name == "rear foot")
        }));
    }

    #[test]
    fn uncertain_candidate_is_grouped_with_its_anchor_without_merging_tracks() {
        let first =
            top_level_frame_container(10, "frame 1", 0, pixel(1, "rear foot", 0, [1, 2, 3, 255]));
        let second =
            top_level_frame_container(11, "frame 2", 1, pixel(2, "Layer 5", 0, [4, 5, 6, 255]));
        let plan = build_layer_write_plan(&two_frame_document(vec![first, second]))
            .expect("candidate grouping should succeed");
        assert_eq!(plan.tracks.len(), 2);
        let group = plan
            .report
            .candidate_groups
            .iter()
            .find(|group| group.name == "候选 - rear foot")
            .expect("rear-foot candidate group should be reported");
        assert!(group.emitted);
        assert_eq!(group.member_track_ids.len(), 2);
        assert!(group.complete_interval);
        assert!(group.relations.iter().any(|relation| {
            relation.relation == CandidateTrackRelation::StructuralMutualExclusion
                && relation.co_visible_frames.is_empty()
        }));
        assert!(matches!(
            plan.root_nodes.first(),
            Some(PlannedNode::Group { name, .. }) if name == "候选 - rear foot"
        ));
        let candidate_folders = plan
            .root_nodes
            .iter()
            .filter_map(|node| match node {
                PlannedNode::Group {
                    name,
                    source_layer_id: None,
                    children,
                } if name == "候选 - rear foot" => Some(children),
                _ => None,
            })
            .collect::<Vec<_>>();
        assert_eq!(candidate_folders.len(), 1);
        assert_eq!(
            candidate_folders[0],
            &vec![
                PlannedNode::Track { track_id: 0 },
                PlannedNode::Track { track_id: 1 },
            ]
        );
        assert!(plan.report.decisions.iter().any(|decision| {
            decision.original_name == "Layer 5"
                && plan
                    .tracks
                    .get(decision.track_id)
                    .is_some_and(|track| track.name == "Layer 5")
        }));
    }

    #[test]
    fn same_candidate_display_name_with_distinct_keys_stays_separate() {
        let group_paths = vec![Vec::new(), Vec::new()];
        let candidate_paths = HashMap::from([
            (
                0,
                CandidateGroupPath {
                    name: "候选 - wing".to_string(),
                    anchor_track_id: 0,
                },
            ),
            (
                1,
                CandidateGroupPath {
                    name: "候选 - wing".to_string(),
                    anchor_track_id: 1,
                },
            ),
        ]);
        let nodes = build_nodes(&group_paths, &[0, 1], &candidate_paths);
        assert_eq!(nodes.len(), 2);
        assert!(matches!(
            &nodes[0],
            PlannedNode::Group { children, .. }
                if children == &vec![PlannedNode::Track { track_id: 0 }]
        ));
        assert!(matches!(
            &nodes[1],
            PlannedNode::Group { children, .. }
                if children == &vec![PlannedNode::Track { track_id: 1 }]
        ));
    }

    #[test]
    fn co_visible_neighbor_is_rejected_from_candidate_folder() {
        let plan = build_layer_write_plan(&document(vec![
            pixel(1, "rear foot", 0, [1, 2, 3, 255]),
            pixel(2, "Layer 5", 1, [4, 5, 6, 255]),
        ]))
        .expect("co-visible candidate planning should succeed");
        assert_eq!(plan.tracks.len(), 2);
        assert!(plan.root_nodes.iter().all(|node| {
            !matches!(node, PlannedNode::Group { name, .. } if name.starts_with("候选 - "))
        }));
        let group = plan
            .report
            .candidate_groups
            .iter()
            .find(|group| group.name == "候选 - rear foot")
            .expect("co-visible candidate should be diagnosed");
        assert!(!group.emitted);
        assert!(group.relations.iter().any(|relation| {
            relation.relation == CandidateTrackRelation::CoVisible
                && relation.co_visible_frames == vec![0]
        }));
    }

    #[test]
    fn observed_disjoint_candidate_is_reported_without_folder() {
        let mut anchor = pixel(1, "rear foot", 0, [1, 2, 3, 255]);
        anchor.frame_states[0].enabled = true;
        anchor.frame_states.push(second_frame_state(false));
        let mut candidate = pixel(2, "Layer 5", 1, [4, 5, 6, 255]);
        candidate.frame_states[0].enabled = false;
        candidate.frame_states.push(second_frame_state(true));
        let plan = build_layer_write_plan(&two_frame_document(vec![anchor, candidate]))
            .expect("observed-disjoint candidate planning should succeed");
        assert_eq!(plan.tracks.len(), 2);
        let group = plan
            .report
            .candidate_groups
            .iter()
            .find(|group| group.name == "候选 - rear foot")
            .expect("observed-disjoint candidate should be diagnosed");
        assert!(!group.emitted);
        assert!(group.relations.iter().any(|relation| {
            relation.relation == CandidateTrackRelation::ObservedDisjoint
                && relation.co_visible_frames.is_empty()
        }));
    }

    #[test]
    fn any_co_visible_member_rejects_the_whole_candidate_folder() {
        let mut anchor = overlapping_observation(0, 1, "rear foot", 0);
        Rc::get_mut(&mut anchor.evidence)
            .expect("test observation evidence should be unique")
            .frame_container_ids = vec![10];
        let mut first_candidate = overlapping_observation(1, 2, "Layer 5", 1);
        Rc::get_mut(&mut first_candidate.evidence)
            .expect("test observation evidence should be unique")
            .frame_container_ids = vec![11];
        let mut second_candidate = overlapping_observation(1, 3, "Layer 6", 2);
        Rc::get_mut(&mut second_candidate.evidence)
            .expect("test observation evidence should be unique")
            .frame_container_ids = vec![11];
        let observations = [anchor, first_candidate, second_candidate];
        let mut tracks = observations
            .iter()
            .enumerate()
            .map(|(track_id, observation)| new_track(track_id, observation, 2))
            .collect::<Vec<_>>();
        for (track, observation) in tracks.iter_mut().zip(&observations) {
            record_assignment(
                track,
                observation,
                PlannedCel {
                    source_layer_id: observation.source_layer_id,
                    source_frame_index: observation.frame_index as u32,
                    z_index: 0,
                },
            );
        }
        let decisions = observations
            .iter()
            .enumerate()
            .map(|(track_id, observation)| {
                decision(
                    observation,
                    track_id,
                    if track_id == 0 {
                        AssociationDecisionStatus::Strong
                    } else {
                        AssociationDecisionStatus::NewTrack
                    },
                    100,
                    100,
                    Vec::new(),
                    Vec::new(),
                )
            })
            .collect::<Vec<_>>();
        let selectors = HashMap::from([
            (
                10,
                FrameContainerInfo {
                    parent_id: None,
                    active_frames: HashSet::from([0]),
                },
            ),
            (
                11,
                FrameContainerInfo {
                    parent_id: None,
                    active_frames: HashSet::from([1]),
                },
            ),
        ]);
        let mut warnings = Vec::new();
        let (groups, paths) = plan_candidate_groups(
            &tracks,
            &decisions,
            &[0, 1, 2],
            &selectors,
            UncertainLayerMode::Group,
            &mut warnings,
        );
        let group = groups
            .iter()
            .find(|group| group.anchor_track_id == 0 && group.member_track_ids.len() == 3)
            .expect("whole candidate group should be diagnosed");
        assert!(!group.emitted);
        assert!(group.relations.iter().any(|relation| {
            relation.relation == CandidateTrackRelation::CoVisible
                && relation.co_visible_frames == vec![1]
        }));
        assert!(paths.is_empty());
    }

    #[test]
    fn non_contiguous_candidate_interval_is_rejected_as_a_whole() {
        let positions = HashMap::from([(0, 0), (1, 1), (2, 2)]);
        assert!(!candidate_members_form_complete_interval(
            &[0, 2],
            &[0, 1, 2],
            &positions,
        ));
        assert!(candidate_members_form_complete_interval(
            &[0, 1, 2],
            &[0, 1, 2],
            &positions,
        ));
    }

    #[test]
    fn flat_uncertain_layout_keeps_candidate_tracks_at_root() {
        let first =
            top_level_frame_container(10, "frame 1", 0, pixel(1, "rear foot", 0, [1, 2, 3, 255]));
        let second =
            top_level_frame_container(11, "frame 2", 1, pixel(2, "Layer 5", 0, [4, 5, 6, 255]));
        let document = two_frame_document(vec![first, second]);
        let plan = super::build_layer_write_plan(
            &document,
            AutoAssociationOptions {
                strategy: AssociationStrategy::Conservative {
                    uncertain_layers: UncertainLayerMode::Flat,
                },
                ..AutoAssociationOptions::default()
            },
        )
        .expect("flat candidate layout should succeed");
        assert!(plan.root_nodes.iter().all(|node| {
            !matches!(node, PlannedNode::Group { name, .. } if name.starts_with("候选 - "))
        }));
        assert!(
            plan.report
                .candidate_groups
                .iter()
                .any(|group| group.name == "候选 - rear foot" && !group.emitted)
        );
    }

    #[test]
    fn exact_unique_name_is_used_as_a_strong_anchor() {
        let plan =
            build_layer_write_plan(&document(vec![pixel(1, "Rear Foot", 0, [1, 2, 3, 255])]))
                .expect("association should succeed");
        assert_eq!(
            plan.report.decisions[0].status,
            AssociationDecisionStatus::Strong
        );
    }

    fn overlapping_observation(
        frame_index: usize,
        source_layer_id: u32,
        name: &str,
        source_order: usize,
    ) -> Observation<'_> {
        let parsed_name = parse_layer_name(name);
        Observation {
            id: ObservationId(source_layer_id as usize),
            evidence: Rc::new(LayerEvidence {
                source_layer_id,
                source_path: source_layer_id.to_string(),
                name: name.to_string(),
                normalized_name: parsed_name.normalized_name,
                name_key: parsed_name.base_name,
                generic_name: parsed_name.generic,
                copy_suffixes: parsed_name.copy_suffixes,
                suffix_limit_reached: parsed_name.suffix_limit_reached,
                frame_container_ids: Vec::new(),
                group_path: Vec::new(),
                width: 1,
                height: 1,
                pixels: &[1, 2, 3, 255],
            }),
            frame_index,
            source_order,
            x: 0,
            y: 0,
        }
    }

    #[test]
    fn consensus_prefers_repeated_overlapping_order() {
        let frames = vec![
            vec![
                overlapping_observation(0, 10, "A", 0),
                overlapping_observation(0, 11, "B", 1),
            ],
            vec![
                overlapping_observation(1, 20, "B", 0),
                overlapping_observation(1, 21, "A", 1),
            ],
            vec![
                overlapping_observation(2, 30, "B", 0),
                overlapping_observation(2, 31, "A", 1),
            ],
            vec![
                overlapping_observation(3, 40, "B", 0),
                overlapping_observation(3, 41, "A", 1),
            ],
        ];
        let mut tracks = vec![
            new_track(0, &frames[0][0], frames.len()),
            new_track(1, &frames[0][1], frames.len()),
        ];
        let mut decisions = Vec::new();
        for observations in &frames {
            for observation in observations {
                let track_id = if observation.name == "A" { 0 } else { 1 };
                record_assignment(
                    &mut tracks[track_id],
                    observation,
                    PlannedCel {
                        source_layer_id: observation.source_layer_id,
                        source_frame_index: observation.frame_index as u32,
                        z_index: 0,
                    },
                );
                decisions.push(decision(
                    observation,
                    track_id,
                    AssociationDecisionStatus::Strong,
                    100,
                    100,
                    Vec::new(),
                    Vec::new(),
                ));
            }
        }

        let (order, diagnostics) = stable_track_order(
            &tracks,
            &frames,
            &decisions,
            &[0, 1],
            StableOrderMode::Consensus,
        )
        .expect("repeated order should be accepted");
        assert_eq!(order, vec![1, 0]);
        assert!(diagnostics.is_empty());
    }

    #[test]
    fn strict_order_rejects_an_unresolved_tie() {
        let error = super::build_layer_write_plan(
            &two_frame_order_document(false),
            AutoAssociationOptions {
                strategy: AssociationStrategy::Conservative {
                    uncertain_layers: UncertainLayerMode::Group,
                },
                stable_order: StableOrderMode::Strict,
                ..AutoAssociationOptions::default()
            },
        )
        .expect_err("strict mode must reject a one-to-one order tie");
        assert!(error.contains("stable order unresolved"));
    }

    #[test]
    fn overlapping_order_changes_use_per_frame_z_indices() {
        let plan = super::build_layer_write_plan(
            &two_frame_order_document(false),
            AutoAssociationOptions {
                strategy: AssociationStrategy::Conservative {
                    uncertain_layers: UncertainLayerMode::Group,
                },
                z_order: LayerZOrderMode::Auto,
                ..AutoAssociationOptions::default()
            },
        )
        .expect("association should succeed");
        let track_a = plan
            .tracks
            .iter()
            .find(|track| track.name == "A")
            .expect("track A should exist");
        let track_b = plan
            .tracks
            .iter()
            .find(|track| track.name == "B")
            .expect("track B should exist");
        assert_ne!(track_a.cels[1].expect("A frame 2").z_index, 0);
        assert_ne!(track_b.cels[1].expect("B frame 2").z_index, 0);
    }

    #[test]
    fn non_overlapping_order_changes_do_not_add_z_indices() {
        let plan = super::build_layer_write_plan(
            &two_frame_order_document(true),
            AutoAssociationOptions {
                strategy: AssociationStrategy::Conservative {
                    uncertain_layers: UncertainLayerMode::Group,
                },
                z_order: LayerZOrderMode::Auto,
                ..AutoAssociationOptions::default()
            },
        )
        .expect("association should succeed");
        assert!(
            plan.tracks
                .iter()
                .all(|track| track.cels[1].is_none_or(|cel| cel.z_index == 0))
        );
    }

    #[test]
    fn stable_order_ignores_frame_order_changes() {
        let plan = build_layer_write_plan(&two_frame_order_document(false))
            .expect("association should succeed");
        assert_eq!(plan.report.z_order_mode, LayerZOrderMode::Stable);
        assert_eq!(plan.report.stable_order_mode, StableOrderMode::Consensus);
        assert!(
            plan.tracks
                .iter()
                .all(|track| track.cels.iter().flatten().all(|cel| cel.z_index == 0))
        );
        assert!(!plan.report.z_order_diagnostics.is_empty());
        assert!(!plan.report.stable_order_diagnostics.is_empty());
        assert!(
            plan.root_nodes
                .iter()
                .all(|node| matches!(node, PlannedNode::Track { .. }))
        );
        assert!(
            plan.report
                .warnings
                .iter()
                .any(|warning| warning.contains("common wrapper group root"))
        );
    }
}
