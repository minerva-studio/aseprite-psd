use std::collections::{BTreeMap, HashMap, HashSet};
use std::ops::Deref;
use std::rc::Rc;

use super::matching::find_best_weighted_matching;
use super::observation::{
    FrameContainerInfo, LayerEvidence, Observation, ObservationId, ObservationStore,
};
use super::report::format_copy_suffixes;
use super::{
    AssociationDecision, AssociationDecisionStatus, AssociationExclusionKind, AssociationPhase,
    AssociationStrategy, CopySuffixMatch, GroupSegment, PlannedCel,
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
    pub(super) preassigned: HashMap<(usize, u32), usize>,
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

#[derive(Clone, Copy)]
pub(super) struct AssignmentMetadata {
    pub(super) score: u16,
    pub(super) margin: u16,
    pub(super) phase: AssociationPhase,
    pub(super) matching_tie: bool,
}

pub(super) struct FamilyAssignmentContext<'state, 'doc> {
    pub(super) tracks: &'state mut Vec<TrackBuilder<'doc>>,
    pub(super) frame_count: usize,
    pub(super) selectors: &'state HashMap<u32, FrameContainerInfo>,
    pub(super) decisions: &'state mut Vec<AssociationDecision>,
    pub(super) preassigned: &'state mut HashMap<(usize, u32), usize>,
}

pub(super) struct FamilyMatching {
    pub(super) assignments: Vec<(usize, usize)>,
    pub(super) tied_observations: HashSet<usize>,
}

pub(super) struct GlobalFamilyMatching {
    pub(super) assignments: Vec<((usize, usize), usize)>,
    pub(super) tied_observations: HashSet<(usize, usize)>,
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
            preassigned: HashMap::new(),
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
    pub(super) fn associate(&mut self, strategy: AssociationStrategy) {
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
                .filter(|track_id| identity_allowed(observation, &tracks[**track_id]))
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
                !used_tracks.contains(&track.id)
                    && track.name_key == observation.name_key
                    && identity_allowed(observation, track)
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
                    && identity_allowed(observation, track)
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
            .filter(|track_id| identity_allowed(observation, &tracks[**track_id]))
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
    let solutions = if residual_observations.len() == 1
        && residual_tracks.len() == 1
        && candidate_map
            .get(&residual_observations[0])
            .is_some_and(|candidates| !candidates.is_empty())
    {
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
                    && identity_allowed(observation, track)
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
                    && identity_allowed(observation, track)
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
                    && identity_allowed(&observations[family_observations[0]], track)
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
    if !identity_allowed(observation, track) {
        return false;
    }
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

/// Prevents automatic association from merging unrelated Photoshop metadata sources.
fn identity_allowed(observation: &Observation, track: &TrackBuilder) -> bool {
    (!observation.metadata_locked && !track.metadata_locked)
        || (observation.metadata_locked
            && track.metadata_locked
            && observation.source_layer_id == track.representative_source_layer_id)
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

pub(super) fn rejection_reasons(
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
