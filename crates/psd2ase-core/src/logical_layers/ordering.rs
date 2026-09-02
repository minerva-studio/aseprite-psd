use std::collections::{HashMap, HashSet};

use super::association::TrackBuilder;
use super::observation::Observation;
use super::{
    AssociationDecision, LayerWritePlan, LayerZOrderMode, PlannedCel, PlannedNode, StableOrderMode,
};

pub(super) fn stable_track_order(
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
        let unanimous = winner > 0 && loser == 0;
        let confident =
            unanimous || (winner >= 2 && winner >= loser + 2 && winner * 3 >= total * 2);
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
        if anchor_distance > 1 && !unanimous {
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

pub(super) fn anchor_track_order(tracks: &[TrackBuilder]) -> Vec<usize> {
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
pub(super) fn assign_z_indices(
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

pub(super) fn alpha_overlap(left: &Observation, right: &Observation) -> bool {
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
