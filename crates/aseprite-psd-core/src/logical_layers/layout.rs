use std::collections::{BTreeMap, HashMap, HashSet};

use super::association::{
    ObservationSummary, TrackBuilder, frame_containers_structurally_mutually_exclusive, ratio_score,
};
use super::observation::FrameContainerInfo;
use super::{
    AssociationDecision, AssociationDecisionStatus, CandidateGroupReport, CandidateTrackRelation,
    CandidateTrackRelationReport, GroupKey, GroupSegment, PlannedNode, UncertainLayerMode,
};
use crate::{NormalizedDocument, NormalizedLayer, NormalizedLayerKind};

pub(super) struct CandidateGroupPath {
    pub(super) name: String,
    pub(super) anchor_track_id: usize,
}

#[derive(Debug, Clone)]
pub(super) enum PlannedNodeBuilder {
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
pub(super) fn choose_group_paths(
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

pub(super) fn plan_candidate_groups(
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
pub(super) fn candidate_members_form_complete_interval(
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

/// Builds the keyed intermediate tree used by candidate-folder topology checks.
pub(super) fn build_nodes_with_keys(
    group_paths: &[Vec<GroupSegment>],
    track_order: &[usize],
    candidate_group_paths: &HashMap<usize, CandidateGroupPath>,
) -> Vec<PlannedNodeBuilder> {
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
    pub(super) fn into_planned_node(self) -> PlannedNode {
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
pub(super) fn validate_candidate_group_topology(
    root_nodes: &[PlannedNodeBuilder],
    reports: &[CandidateGroupReport],
) -> Result<(), String> {
    for report in reports.iter().filter(|report| report.emitted) {
        let mut matching_groups = Vec::new();
        collect_candidate_groups(root_nodes, report.anchor_track_id, &mut matching_groups);
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

/// Collects synthetic candidate groups with the requested planning identity.
fn collect_candidate_groups<'a>(
    nodes: &'a [PlannedNodeBuilder],
    anchor_track_id: usize,
    output: &mut Vec<&'a [PlannedNodeBuilder]>,
) {
    for node in nodes {
        if let PlannedNodeBuilder::Group {
            key: GroupKey::Candidate(anchor),
            children,
            ..
        } = node
        {
            if *anchor == anchor_track_id {
                output.push(children);
            }
            collect_candidate_groups(children, anchor_track_id, output);
        } else if let PlannedNodeBuilder::Group { children, .. } = node {
            collect_candidate_groups(children, anchor_track_id, output);
        }
    }
}

/// Collects every logical track below one planned subtree.
fn collect_planned_track_ids(nodes: &[PlannedNodeBuilder], output: &mut Vec<usize>) {
    for node in nodes {
        match node {
            PlannedNodeBuilder::Group { children, .. } => {
                collect_planned_track_ids(children, output)
            }
            PlannedNodeBuilder::Track { track_id } => output.push(*track_id),
        }
    }
}

/// Removes a semantically empty group shared by every planned track.
pub(super) fn flatten_redundant_common_root(
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
    let timeline_bound = layer_has_timeline_variation(layer);
    let all_tracks_cover_children = tracks.iter().all(|track| {
        track.group_paths.iter().any(|path| {
            path.first()
                .is_some_and(|segment| segment.key == common_key)
        })
    });
    if transparent
        && opaque
        && all_tracks_cover_children
        && (only_selectors || (only_pixels && timeline_bound))
    {
        for path in group_paths.iter_mut() {
            path.remove(0);
        }
        warnings.push(format!(
            "common wrapper group {} was flattened from the auto output",
            common_name
        ));
    }
}

/// Returns whether a group or one of its descendants changes across frames.
fn layer_has_timeline_variation(layer: &NormalizedLayer) -> bool {
    layer.frame_states.windows(2).any(|pair| {
        pair[0].enabled != pair[1].enabled
            || pair[0].offset != pair[1].offset
            || pair[0].reference_point != pair[1].reference_point
            || pair[0].opacity != pair[1].opacity
    }) || layer.children.iter().any(layer_has_timeline_variation)
}
