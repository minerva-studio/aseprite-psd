use std::collections::{HashMap, HashSet};
use std::ops::Deref;
use std::rc::Rc;

use super::matching::find_best_weighted_matching;
use super::observation::{
    FrameContainerInfo, LayerEvidence, Observation, ObservationId, ObservationStore,
};
use super::ordering::alpha_overlap;
use super::{
    AssociationDecision, AssociationDecisionStatus, AssociationExclusionKind, AssociationPhase,
    AssociationStrategy, CopySuffixMatch, GroupSegment, LayerZOrderMode, PlannedCel,
};
use crate::layer_names::{CopySuffixCatalog, ParsedLayerName};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub(super) struct TrackId(usize);

impl TrackId {
    const fn index(self) -> usize {
        self.0
    }
}
#[derive(Debug, Clone)]
pub(super) struct TrackBuilder<'doc> {
    pub(super) id: usize,
    pub(super) name: String,
    pub(super) normalized_name: String,
    pub(super) name_key: String,
    pub(super) generic_name: bool,
    pub(super) copy_suffixes: Vec<CopySuffixMatch>,
    pub(super) representative_source_layer_id: u32,
    pub(super) cels: Vec<Option<PlannedCel>>,
    pub(super) observation_ids: Vec<ObservationId>,
    pub(super) observations: Vec<ObservationSummary<'doc>>,
    pub(super) group_paths: Vec<Vec<GroupSegment>>,
    /// Whether this track is tied to one source layer for metadata preservation.
    pub(super) metadata_locked: bool,
}

#[derive(Debug, Clone)]
pub(super) struct ObservationSummary<'doc> {
    pub(super) evidence: Rc<LayerEvidence<'doc>>,
    pub(super) frame_index: usize,
    pub(super) source_order: usize,
    pub(super) x: i32,
    pub(super) y: i32,
    pub(super) width: u32,
    pub(super) height: u32,
}

impl<'doc> Deref for ObservationSummary<'doc> {
    type Target = LayerEvidence<'doc>;

    fn deref(&self) -> &Self::Target {
        &self.evidence
    }
}

#[derive(Debug)]
pub(super) struct AssociationEngine<'doc> {
    pub(super) observations: ObservationStore<'doc>,
    pub(super) selectors: HashMap<u32, FrameContainerInfo>,
    pub(super) tracks: Vec<TrackBuilder<'doc>>,
    pub(super) decisions: Vec<AssociationDecision>,
    pub(super) anchor_frame: usize,
    pub(super) frame_count: usize,
}

/// Owns the association state handed to ordering and layout after matching.
pub(super) struct AssociationOutput<'doc> {
    pub(super) observations: ObservationStore<'doc>,
    pub(super) selectors: HashMap<u32, FrameContainerInfo>,
    pub(super) tracks: Vec<TrackBuilder<'doc>>,
    pub(super) decisions: Vec<AssociationDecision>,
}

impl<'doc> AssociationEngine<'doc> {
    /// Creates the single owner of mutable association state.
    pub(super) fn new(
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
            anchor_frame,
            frame_count,
        }
    }

    /// Seeds tracks and decisions from the selected anchor frame.
    pub(super) fn seed_anchor(&mut self) {
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
    pub(super) fn associate(
        &mut self,
        strategy: AssociationStrategy,
        z_order_mode: LayerZOrderMode,
        allow_inferred_cross_source_matches: bool,
    ) {
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
            if !allow_inferred_cross_source_matches {
                associate_frame_by_source_identity(
                    &self.observations.frames[frame_index],
                    &mut self.tracks,
                    self.frame_count,
                    &mut self.decisions,
                );
                continue;
            }
            associate_frame_compact(
                &self.observations.frames[frame_index],
                &mut self.tracks,
                self.frame_count,
                &self.selectors,
                matches!(strategy, AssociationStrategy::Conservative { .. }),
                z_order_mode == LayerZOrderMode::Auto,
                &mut self.decisions,
            );
        }
    }

    /// Consumes the engine and transfers its state to the next planning stages.
    pub(super) fn into_output(self) -> AssociationOutput<'doc> {
        AssociationOutput {
            observations: self.observations,
            selectors: self.selectors,
            tracks: self.tracks,
            decisions: self.decisions,
        }
    }
}

/// Associates only observations carrying the exact same PSD source-layer identity.
fn associate_frame_by_source_identity<'doc>(
    observations: &[Observation<'doc>],
    tracks: &mut Vec<TrackBuilder<'doc>>,
    frame_count: usize,
    decisions: &mut Vec<AssociationDecision>,
) {
    for observation in observations {
        let matching_tracks = tracks
            .iter()
            .filter(|track| {
                track.cels[observation.frame_index].is_none()
                    && track
                        .observations
                        .iter()
                        .all(|previous| previous.source_layer_id == observation.source_layer_id)
            })
            .map(|track| track.id)
            .collect::<Vec<_>>();
        let (track_id, status, evidence) = if matching_tracks.len() == 1 {
            (
                matching_tracks[0],
                AssociationDecisionStatus::Strong,
                vec!["exact source-layer identity".to_string()],
            )
        } else {
            let track_id = tracks.len();
            tracks.push(new_track(track_id, observation, frame_count));
            (
                track_id,
                AssociationDecisionStatus::NewTrack,
                vec!["synthetic-frame identity unproven".to_string()],
            )
        };
        let mut association_decision = decision(
            observation,
            track_id,
            status,
            if status == AssociationDecisionStatus::Strong {
                100
            } else {
                0
            },
            100,
            evidence,
            Vec::new(),
        );
        association_decision.association_phase = if status == AssociationDecisionStatus::Strong {
            AssociationPhase::Family
        } else {
            AssociationPhase::NewTrack
        };
        decisions.push(association_decision);
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

fn associate_frame_compact<'doc>(
    observations: &[Observation<'doc>],
    tracks: &mut Vec<TrackBuilder<'doc>>,
    frame_count: usize,
    selectors: &HashMap<u32, FrameContainerInfo>,
    protect_new_generic_batches: bool,
    allow_order_crossings: bool,
    decisions: &mut Vec<AssociationDecision>,
) {
    if observations.is_empty() {
        return;
    }
    let mut assigned = vec![None; observations.len()];
    let mut used_tracks = HashSet::new();
    let mut strong_assignments = HashSet::new();
    // Conservative mode treats several previously unseen generic names that
    // appear together as a new effect batch, rather than letting empty slots
    // in unrelated named tracks define their identities.
    let new_generic_observations = observations
        .iter()
        .enumerate()
        .filter(|(_, observation)| {
            observation.generic_name
                && !decisions
                    .iter()
                    .any(|decision| decision.normalized_name == observation.normalized_name)
        })
        .map(|(index, _)| index)
        .collect::<Vec<_>>();
    let protected_generic_observations =
        if protect_new_generic_batches && new_generic_observations.len() >= 2 {
            new_generic_observations.into_iter().collect::<HashSet<_>>()
        } else {
            HashSet::new()
        };

    for (observation_index, observation) in observations.iter().enumerate() {
        if observation.generic_name {
            continue;
        }
        let candidates = tracks
            .iter()
            .filter(|track| {
                !used_tracks.contains(&track.id)
                    && track.normalized_name == observation.normalized_name
                    && identity_allowed(observation, track)
                    && candidate_order_safe(
                        observation_index,
                        track.id,
                        observations,
                        tracks,
                        &assigned,
                        allow_order_crossings,
                    )
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
        if assigned[observation_index].is_some()
            || protected_generic_observations.contains(&observation_index)
        {
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
                    && identity_allowed(observation, track)
                    && candidate_order_safe(
                        observation_index,
                        track.id,
                        observations,
                        tracks,
                        &assigned,
                        allow_order_crossings,
                    )
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
        if protected_generic_observations.contains(observation_index) {
            candidate_map.insert(*observation_index, Vec::new());
            continue;
        }
        let mut candidates = residual_tracks
            .iter()
            .filter(|track_id| {
                compact_candidate_allowed(observation, &tracks[**track_id], selectors)
                    && candidate_order_safe(
                        *observation_index,
                        **track_id,
                        observations,
                        tracks,
                        &assigned,
                        allow_order_crossings,
                    )
            })
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
    let (residual_assignments, matching_ties) =
        find_best_weighted_matching(&residual_observations, &candidate_map, |_, track| track);
    let residual_assignments = if allow_order_crossings {
        residual_assignments
    } else {
        resolve_order_crossings(residual_assignments, observations, tracks, &candidate_map)
    };
    for (observation_index, track_id) in residual_assignments {
        if !matching_ties.contains(&observation_index) {
            assigned[observation_index] = Some(track_id);
            used_tracks.insert(track_id);
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
            if !strong_assignments.contains(&observation_index) {
                association_decision
                    .evidence
                    .push("order-safe global assignment".to_string());
            }
            association_decision.exclusion_evidence =
                exclusion_evidence(observation, &tracks[track_id], selectors);
            if observation.generic_name
                && association_decision.exclusion_evidence
                    == AssociationExclusionKind::StructuralMutualExclusion
            {
                association_decision
                    .evidence
                    .push("structural-empty-slot".to_string());
            }
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
            let status = if !candidates.is_empty() && matching_ties.contains(&observation_index) {
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
                if protected_generic_observations.contains(&observation_index) {
                    vec!["new-generic-batch-protected".to_string()]
                } else if status == AssociationDecisionStatus::Ambiguous {
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

/// Allows global named candidates while keeping generic and metadata identities conservative.
fn compact_candidate_allowed(
    observation: &Observation,
    track: &TrackBuilder,
    selectors: &HashMap<u32, FrameContainerInfo>,
) -> bool {
    if !identity_allowed(observation, track) {
        return false;
    }
    if observation.generic_name || track.generic_name {
        return exclusion_evidence(observation, track, selectors)
            == AssociationExclusionKind::StructuralMutualExclusion;
    }
    true
}

/// Rejects order crossings that can change pixels in the current frame.
fn candidate_order_safe(
    observation_index: usize,
    track_id: usize,
    observations: &[Observation],
    tracks: &[TrackBuilder],
    assigned: &[Option<usize>],
    allow_order_crossings: bool,
) -> bool {
    if allow_order_crossings {
        return true;
    }
    let candidate_order = median_order(&tracks[track_id]).unwrap_or(observation_index as i32);
    assigned
        .iter()
        .enumerate()
        .filter_map(|(other_index, other_track)| other_track.map(|track| (other_index, track)))
        .all(|(other_index, other_track)| {
            let other_order = median_order(&tracks[other_track]).unwrap_or(other_index as i32);
            let source_relation = observation_index.cmp(&other_index);
            let track_relation = candidate_order.cmp(&other_order);
            source_relation == track_relation
                || source_relation == std::cmp::Ordering::Equal
                || track_relation == std::cmp::Ordering::Equal
                || !alpha_overlap(&observations[observation_index], &observations[other_index])
        })
}

/// Removes or swaps residual assignments whose effective layer order would change pixels.
fn resolve_order_crossings(
    mut assignments: Vec<(usize, usize)>,
    observations: &[Observation],
    tracks: &[TrackBuilder],
    candidates: &HashMap<usize, Vec<(usize, u16)>>,
) -> Vec<(usize, usize)> {
    loop {
        let mut changed = false;
        'pairs: for left in 0..assignments.len() {
            for right in left + 1..assignments.len() {
                let (left_observation, left_track) = assignments[left];
                let (right_observation, right_track) = assignments[right];
                let source_crosses = left_observation.cmp(&right_observation)
                    != median_order(&tracks[left_track]).cmp(&median_order(&tracks[right_track]));
                if !source_crosses
                    || !alpha_overlap(
                        &observations[left_observation],
                        &observations[right_observation],
                    )
                {
                    continue;
                }
                let can_swap = candidates
                    .get(&left_observation)
                    .is_some_and(|values| values.iter().any(|(track, _)| *track == right_track))
                    && candidates
                        .get(&right_observation)
                        .is_some_and(|values| values.iter().any(|(track, _)| *track == left_track));
                if can_swap {
                    assignments[left].1 = right_track;
                    assignments[right].1 = left_track;
                } else {
                    let left_score = candidate_edge_score(candidates, left_observation, left_track);
                    let right_score =
                        candidate_edge_score(candidates, right_observation, right_track);
                    assignments.remove(if left_score <= right_score {
                        left
                    } else {
                        right
                    });
                }
                changed = true;
                break 'pairs;
            }
        }
        if !changed {
            break;
        }
    }
    assignments
}

fn candidate_edge_score(
    candidates: &HashMap<usize, Vec<(usize, u16)>>,
    observation: usize,
    track: usize,
) -> u16 {
    candidates
        .get(&observation)
        .and_then(|values| values.iter().find(|(candidate, _)| *candidate == track))
        .map_or(0, |(_, score)| *score)
}

/// Scores a compact-mode candidate with the historical 651eb65 weights.
fn compact_candidate_score(
    observation: &Observation,
    track: &TrackBuilder,
    frame_observations: &[Observation],
) -> u16 {
    let name_available = !observation.generic_name && !track.generic_name;
    let name = if name_available && observation.normalized_name == track.normalized_name {
        20
    } else if name_available && observation.name_key == track.name_key {
        5
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
    let available_weight = if name_available { 90 } else { 70 };
    ((score * 100) / available_weight).min(100) as u16
}

/// Produces the compact-mode evidence labels used by the historical planner.
fn compact_evidence_for(
    observation: &Observation,
    track: &TrackBuilder,
    score: u16,
) -> Vec<String> {
    let mut evidence = Vec::new();
    if !observation.generic_name && observation.normalized_name == track.normalized_name {
        evidence.push("normalized name".to_string());
    } else if !observation.generic_name && observation.name_key == track.name_key {
        evidence.push("copy-name family".to_string());
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

fn identity_allowed(observation: &Observation, track: &TrackBuilder) -> bool {
    (!observation.metadata_locked && !track.metadata_locked)
        || (observation.metadata_locked
            && track.metadata_locked
            && observation.source_layer_id == track.representative_source_layer_id)
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
pub(super) fn frame_containers_structurally_mutually_exclusive(
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

pub(super) fn ratio_score(left: u32, right: u32) -> u16 {
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

pub(super) fn candidate_names(
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

pub(super) fn new_track<'doc>(
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
        metadata_locked: observation.metadata_locked,
    }
}

pub(super) fn record_assignment<'doc>(
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

pub(super) fn decision(
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

pub(super) fn parse_layer_name(name: &str) -> ParsedLayerName {
    CopySuffixCatalog.parse(name)
}

pub(super) fn canonical_name(name: &str) -> String {
    parse_layer_name(name).base_name
}

pub(super) fn name_evidence_weight(observation: &Observation) -> u16 {
    if observation.generic_name {
        0
    } else if observation.copy_suffixes.is_empty() {
        30
    } else {
        15
    }
}
