//! Experimental cross-frame layer association and Aseprite write planning.

use std::collections::{HashMap, HashSet};

use crate::aseprite_writer::cel_position;
use crate::{NormalizedDocument, NormalizedLayer, NormalizedLayerKind};

/// Selects whether the writer should preserve the PSD source tree or infer
/// long-lived logical layer tracks across animation frames.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum LayerAssociationMode {
    /// Preserve every normalized source layer as its own Aseprite layer.
    #[default]
    Preserve,
    /// Infer logical tracks and remove frame-container groups.
    Auto,
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
    /// Potential per-frame order changes detected by the planner.
    pub z_order_diagnostics: Vec<String>,
    /// Evidence and fallbacks used while establishing stable track order.
    pub stable_order_diagnostics: Vec<String>,
    /// Per-observation decisions in deterministic source/frame order.
    pub decisions: Vec<AssociationDecision>,
    /// Non-fatal limitations and conservative fallbacks.
    pub warnings: Vec<String>,
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

#[derive(Debug, Clone)]
struct Observation {
    frame_index: usize,
    source_layer_id: u32,
    source_path: String,
    name: String,
    name_key: String,
    generic_name: bool,
    group_path: Vec<GroupSegment>,
    source_order: usize,
    x: i32,
    y: i32,
    width: u32,
    height: u32,
    pixels: Vec<u8>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct GroupSegment {
    id: u32,
    name: String,
    key: String,
}

#[derive(Debug, Clone)]
struct TrackBuilder {
    id: usize,
    name: String,
    name_key: String,
    generic_name: bool,
    representative_source_layer_id: u32,
    cels: Vec<Option<PlannedCel>>,
    observations: Vec<ObservationSummary>,
    group_paths: Vec<Vec<GroupSegment>>,
}

#[derive(Debug, Clone)]
struct ObservationSummary {
    source_order: usize,
    x: i32,
    y: i32,
    width: u32,
    height: u32,
    pixels: Vec<u8>,
}

/// Builds an experimental logical-layer plan using stable Z-order by default.
pub fn build_layer_write_plan(document: &NormalizedDocument) -> Result<LayerWritePlan, String> {
    build_layer_write_plan_with_order_modes(
        document,
        LayerZOrderMode::Stable,
        StableOrderMode::Consensus,
    )
}

/// Builds an experimental logical-layer plan with an explicit Z-order policy.
pub fn build_layer_write_plan_with_z_order(
    document: &NormalizedDocument,
    z_order_mode: LayerZOrderMode,
) -> Result<LayerWritePlan, String> {
    build_layer_write_plan_with_order_modes(document, z_order_mode, StableOrderMode::Consensus)
}

/// Builds an automatic logical-layer plan with explicit z-order and stable-order policies.
pub fn build_layer_write_plan_with_order_modes(
    document: &NormalizedDocument,
    z_order_mode: LayerZOrderMode,
    stable_order_mode: StableOrderMode,
) -> Result<LayerWritePlan, String> {
    let selectors = find_frame_selector_groups(&document.root_layers, document.frames.len());
    let mut frames = vec![Vec::new(); document.frames.len()];
    let mut source_order = 0;
    for (root_index, layer) in document.root_layers.iter().enumerate() {
        collect_observations(
            layer,
            &[root_index.to_string()],
            &[],
            &selectors,
            &[],
            &mut source_order,
            &mut frames,
        )?;
    }
    for frame in &mut frames {
        frame.sort_by_key(|observation| observation.source_order);
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

    let mut tracks = Vec::new();
    let mut decisions = Vec::new();
    for observation in &frames[anchor_frame] {
        let track_id = tracks.len();
        tracks.push(new_track(track_id, observation, document.frames.len()));
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
            vec!["anchor frame".to_string()],
            Vec::new(),
        ));
    }

    let mut frame_order = (0..frames.len()).collect::<Vec<_>>();
    frame_order.sort_by_key(|frame_index| {
        if *frame_index == anchor_frame {
            0
        } else {
            1 + (*frame_index + frames.len() - anchor_frame) % frames.len()
        }
    });

    for frame_index in frame_order {
        if frame_index == anchor_frame {
            continue;
        }
        associate_frame(
            &frames[frame_index],
            &mut tracks,
            document.frames.len(),
            &mut decisions,
        );
    }

    let mut warnings = Vec::new();
    let anchor_order = anchor_track_order(&tracks);
    let (track_order, stable_order_diagnostics) = if z_order_mode == LayerZOrderMode::Stable {
        stable_track_order(
            &tracks,
            &frames,
            &decisions,
            &anchor_order,
            stable_order_mode,
        )?
    } else {
        (anchor_order, Vec::new())
    };
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
    let mut group_paths = choose_group_paths(&mut tracks, document, &mut warnings);
    flatten_redundant_common_root(
        &mut group_paths,
        &tracks,
        document,
        &selectors,
        &mut warnings,
    );
    let mut plan = LayerWritePlan {
        root_nodes: build_nodes(&group_paths, &track_order),
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
            z_order_diagnostics: Vec::new(),
            stable_order_diagnostics,
            decisions,
            warnings,
        },
    };
    plan.report.z_order_diagnostics = assign_z_indices(&mut plan, &frames, z_order_mode)?;
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

fn collect_observations(
    layer: &NormalizedLayer,
    path: &[String],
    group_path: &[GroupSegment],
    selectors: &HashSet<u32>,
    ancestors: &[&NormalizedLayer],
    source_order: &mut usize,
    frames: &mut [Vec<Observation>],
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
            if !selectors.contains(&layer.id) {
                next_path.push(GroupSegment {
                    id: layer.id,
                    name: layer.name.clone(),
                    key: canonical_name(&layer.name).0,
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
                    selectors,
                    &next_ancestors,
                    source_order,
                    frames,
                )?;
            }
        }
        NormalizedLayerKind::Pixel => {
            let pixels = layer
                .pixels
                .as_ref()
                .ok_or_else(|| format!("pixel layer {} has no normalized pixels", layer.id))?;
            for (frame_index, frame) in frames.iter_mut().enumerate() {
                if !is_visible(frame_index) {
                    continue;
                }
                let state = layer.frame_states.get(frame_index).ok_or_else(|| {
                    format!(
                        "pixel layer {} has no state for frame {frame_index}",
                        layer.id
                    )
                })?;
                let (x, y) = cel_position(pixels, state)
                    .map_err(|error| format!("layer {}: {error}", layer.id))?;
                let (name_key, generic_name) = canonical_name(&layer.name);
                frame.push(Observation {
                    frame_index,
                    source_layer_id: layer.id,
                    source_path: path.join("/"),
                    name: layer.name.clone(),
                    name_key,
                    generic_name,
                    group_path: group_path.to_vec(),
                    source_order: *source_order,
                    x: i32::from(x),
                    y: i32::from(y),
                    width: pixels.width,
                    height: pixels.height,
                    pixels: pixels.data.clone(),
                });
            }
            *source_order += 1;
        }
    }
    Ok(())
}

fn associate_frame(
    observations: &[Observation],
    tracks: &mut Vec<TrackBuilder>,
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
                let score = candidate_score(observation, &tracks[*track_id], observations);
                (*track_id, score)
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
            let evidence = evidence_for(observation, &tracks[track_id], score);
            decisions.push(decision(
                observation,
                track_id,
                status,
                score,
                score.saturating_sub(second),
                evidence,
                candidate_names(&candidates, tracks, track_id),
            ));
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
            decisions.push(decision(
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
            ));
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

fn candidate_score(
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
    if left.is_empty() && right.is_empty() {
        20
    } else if common == left.len() && common == right.len() {
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

fn evidence_for(observation: &Observation, track: &TrackBuilder, score: u16) -> Vec<String> {
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

fn track_name(tracks: &[TrackBuilder], track_id: usize) -> &str {
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

fn new_track(track_id: usize, observation: &Observation, frame_count: usize) -> TrackBuilder {
    TrackBuilder {
        id: track_id,
        name: observation.name.clone(),
        name_key: observation.name_key.clone(),
        generic_name: observation.generic_name,
        representative_source_layer_id: observation.source_layer_id,
        cels: vec![None; frame_count],
        observations: Vec::new(),
        group_paths: Vec::new(),
    }
}

fn record_assignment(track: &mut TrackBuilder, observation: &Observation, cel: PlannedCel) {
    track.cels[observation.frame_index] = Some(cel);
    track.group_paths.push(observation.group_path.clone());
    track.observations.push(ObservationSummary {
        source_order: observation.source_order,
        x: observation.x,
        y: observation.y,
        width: observation.width,
        height: observation.height,
        pixels: observation.pixels.clone(),
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
        track_id,
        status,
        score,
        margin,
        evidence,
        alternatives,
    }
}

fn canonical_name(name: &str) -> (String, bool) {
    let normalized = name
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .to_lowercase();
    let mut key = normalized.clone();
    for marker in [" copy", " 拷贝", " 副本"] {
        if let Some(index) = key.rfind(marker) {
            let suffix = &key[index + marker.len()..];
            if suffix.trim().is_empty()
                || suffix
                    .trim()
                    .chars()
                    .all(|character| character.is_ascii_digit())
            {
                key.truncate(index);
                break;
            }
        }
    }
    let generic = key
        .strip_prefix("图层 ")
        .or_else(|| key.strip_prefix("layer "))
        .is_some_and(|value| {
            !value.is_empty() && value.chars().all(|character| character.is_ascii_digit())
        });
    (key, generic)
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
            let mut counts = HashMap::<Vec<String>, usize>::new();
            let mut representatives = HashMap::<Vec<String>, Vec<GroupSegment>>::new();
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
                    find_layer(document, segment.id).is_some_and(|layer| {
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

fn build_nodes(group_paths: &[Vec<GroupSegment>], track_order: &[usize]) -> Vec<PlannedNode> {
    let mut roots = Vec::new();
    for &track_id in track_order {
        insert_track(&mut roots, &group_paths[track_id], track_id);
    }
    roots
}

fn insert_track(nodes: &mut Vec<PlannedNode>, path: &[GroupSegment], track_id: usize) {
    if let Some(segment) = path.first() {
        let index = nodes.iter().position(|node| {
            matches!(node, PlannedNode::Group { name, .. } if canonical_name(name).0 == segment.key)
        });
        let index = if let Some(index) = index {
            index
        } else {
            nodes.push(PlannedNode::Group {
                name: segment.name.clone(),
                source_layer_id: Some(segment.id),
                children: Vec::new(),
            });
            nodes.len() - 1
        };
        if let PlannedNode::Group { children, .. } = &mut nodes[index] {
            insert_track(children, &path[1..], track_id);
        }
    } else {
        nodes.push(PlannedNode::Track { track_id });
    }
}

/// Removes a semantically empty group shared by every planned track.
fn flatten_redundant_common_root(
    group_paths: &mut [Vec<GroupSegment>],
    tracks: &[TrackBuilder],
    document: &NormalizedDocument,
    selectors: &HashSet<u32>,
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
    let Some(layer) = find_layer(document, common.id) else {
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
            .all(|child| selectors.contains(&child.id));
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
                if mode == LayerZOrderMode::Auto {
                    if let Some(cel_slot) = plan.tracks[*track_id].cels[frame_index].as_mut() {
                        cel_slot.z_index = z;
                    }
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
    let left_pixels = left.pixels.chunks_exact(4);
    for (index, pixel) in left_pixels.enumerate() {
        if pixel[3] == 0 {
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

fn find_frame_selector_groups(layers: &[NormalizedLayer], frame_count: usize) -> HashSet<u32> {
    let mut selectors = HashSet::new();
    for layer in layers {
        identify_selectors(layer, frame_count, &mut selectors);
    }
    selectors
}

fn identify_selectors(layer: &NormalizedLayer, frame_count: usize, selectors: &mut HashSet<u32>) {
    if layer.kind == NormalizedLayerKind::Group {
        let group_children = layer
            .children
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
                for child in group_children {
                    selectors.insert(child.id);
                }
            }
        }
        for child in &layer.children {
            identify_selectors(child, frame_count, selectors);
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

fn find_layer(document: &NormalizedDocument, id: u32) -> Option<&NormalizedLayer> {
    fn find(layers: &[NormalizedLayer], id: u32) -> Option<&NormalizedLayer> {
        for layer in layers {
            if layer.id == id {
                return Some(layer);
            }
            if let Some(found) = find(&layer.children, id) {
                return Some(found);
            }
        }
        None
    }
    find(&document.root_layers, id)
}

#[cfg(test)]
mod tests {
    use super::*;
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
    }

    #[test]
    fn unique_remaining_track_absorbs_a_generic_renamed_observation() {
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
        assert_eq!(plan.tracks.len(), 2);
        assert!(plan.tracks.iter().any(|track| {
            track.name == "rear foot" && track.cels[1].is_some_and(|cel| cel.source_layer_id == 3)
        }));
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
    ) -> Observation {
        Observation {
            frame_index,
            source_layer_id,
            source_path: source_layer_id.to_string(),
            name: name.to_string(),
            name_key: name.to_ascii_lowercase(),
            generic_name: false,
            group_path: Vec::new(),
            source_order,
            x: 0,
            y: 0,
            width: 1,
            height: 1,
            pixels: vec![1, 2, 3, 255],
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
        let error = build_layer_write_plan_with_order_modes(
            &two_frame_order_document(false),
            LayerZOrderMode::Stable,
            StableOrderMode::Strict,
        )
        .expect_err("strict mode must reject a one-to-one order tie");
        assert!(error.contains("stable order unresolved"));
    }

    #[test]
    fn overlapping_order_changes_use_per_frame_z_indices() {
        let plan = build_layer_write_plan_with_z_order(
            &two_frame_order_document(false),
            LayerZOrderMode::Auto,
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
        let plan = build_layer_write_plan_with_z_order(
            &two_frame_order_document(true),
            LayerZOrderMode::Auto,
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
