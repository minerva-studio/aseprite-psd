use std::collections::{BTreeMap, BTreeSet, HashMap};

use super::PlannedNode;
use super::association::TrackBuilder;

#[derive(Debug, Clone, Default)]
struct NodeFeatureSummary {
    feature_ids: BTreeSet<u32>,
    has_unassigned_source: bool,
}

impl NodeFeatureSummary {
    /// Returns the sole Feature identity when the node has an unambiguous source.
    fn pure_feature(&self) -> Option<u32> {
        (!self.has_unassigned_source && self.feature_ids.len() == 1)
            .then(|| *self.feature_ids.first().expect("one feature was checked"))
    }
}

/// Reorders and groups Feature nodes while preserving association and cel ownership.
pub(super) fn organize_feature_nodes(
    nodes: &mut Vec<PlannedNode>,
    tracks: &mut [TrackBuilder<'_>],
    feature_meta: &HashMap<u32, (String, usize)>,
) -> Vec<String> {
    for track in tracks.iter_mut() {
        track.name = source_display_name(track);
    }

    let mut diagnostics = tracks.iter().map(track_source_mapping).collect::<Vec<_>>();
    let feature_ids = collect_feature_ids(nodes, tracks);
    let group_names = feature_group_names_for_ids(&feature_ids, feature_meta);
    layout_feature_children(nodes, tracks, feature_meta, &group_names, &mut diagnostics);

    for track in tracks.iter() {
        let summary = track_feature_summary(track);
        if summary.feature_ids.is_empty() && summary.has_unassigned_source {
            diagnostics.push(format!(
                "track {} not grouped: no Feature source identity; kept at its current location",
                track.id
            ));
        }
    }
    rename_duplicate_track_names(nodes, tracks);
    diagnostics.sort();
    diagnostics
}

#[derive(Debug, Clone)]
struct LayoutUnit {
    node: PlannedNode,
    track_ids: Vec<usize>,
    feature_id: Option<u32>,
    original_index: usize,
}

/// Collects every Feature identity represented by the final output tree.
fn collect_feature_ids(nodes: &[PlannedNode], tracks: &[TrackBuilder<'_>]) -> BTreeSet<u32> {
    nodes
        .iter()
        .flat_map(|node| summarize_node(node, tracks).feature_ids)
        .collect()
}

/// Applies Feature-aware constrained layout recursively at one output parent.
fn layout_feature_children(
    nodes: &mut Vec<PlannedNode>,
    tracks: &[TrackBuilder<'_>],
    feature_meta: &HashMap<u32, (String, usize)>,
    group_names: &BTreeMap<u32, String>,
    diagnostics: &mut Vec<String>,
) {
    let mut units = nodes
        .iter()
        .enumerate()
        .map(|(index, node)| LayoutUnit {
            node: node.clone(),
            track_ids: node_track_ids(node),
            feature_id: summarize_node(node, tracks).pure_feature(),
            original_index: index,
        })
        .collect::<Vec<_>>();
    let feature_ids = units
        .iter()
        .filter_map(|unit| unit.feature_id)
        .collect::<BTreeSet<_>>();
    for unit in &units {
        let summary = summarize_node(&unit.node, tracks);
        if summary.feature_ids.len() > 1 || summary.has_unassigned_source {
            let reason = if summary.feature_ids.len() > 1 {
                "shared track has multiple Feature identities"
            } else {
                "track has an unassigned source"
            };
            diagnostics.push(format!(
                "feature-group not created: reason={reason}; kept root node={:?} feature_ids={:?}",
                node_label(&unit.node, tracks),
                summary.feature_ids
            ));
        }
    }
    let mut ordered_feature_ids = feature_ids.into_iter().collect::<Vec<_>>();
    ordered_feature_ids.sort_by_key(|feature_id| {
        (
            feature_meta
                .get(feature_id)
                .map(|(_, order)| *order)
                .unwrap_or(usize::MAX),
            *feature_id,
        )
    });

    for feature_id in ordered_feature_ids {
        let candidate_indices = units
            .iter()
            .enumerate()
            .filter_map(|(index, unit)| (unit.feature_id == Some(feature_id)).then_some(index))
            .collect::<Vec<_>>();
        if candidate_indices.is_empty() {
            continue;
        }
        let candidate_labels = candidate_indices
            .iter()
            .map(|index| node_label(&units[*index].node, tracks))
            .collect::<Vec<_>>();
        if !contract_feature_units(&units, &candidate_indices, tracks) {
            let conflict_chain =
                contracted_constraint_diagnostics(&units, &candidate_indices, tracks);
            diagnostics.push(format!(
                "feature-group not created: feature_id={feature_id} reason=layout-constraint-cycle candidate_nodes={candidate_labels:?} conflict_chain={conflict_chain:?}"
            ));
            continue;
        }
        let name = group_names
            .get(&feature_id)
            .cloned()
            .unwrap_or_else(|| format!("Feature {feature_id}"));
        let candidate_nodes = candidate_indices
            .iter()
            .map(|index| units[*index].node.clone())
            .collect::<Vec<_>>();
        let group = LayoutUnit {
            node: PlannedNode::Group {
                name: name.clone(),
                source_layer_id: None,
                children: candidate_nodes,
            },
            track_ids: candidate_indices
                .iter()
                .flat_map(|index| units[*index].track_ids.iter().copied())
                .collect(),
            feature_id: None,
            original_index: candidate_indices
                .iter()
                .map(|index| units[*index].original_index)
                .min()
                .unwrap_or_default(),
        };
        let first = candidate_indices[0];
        let mut replacement = Vec::with_capacity(units.len() - candidate_indices.len() + 1);
        for (index, unit) in units.drain(..).enumerate() {
            if index == first {
                replacement.push(group.clone());
            }
            if !candidate_indices.contains(&index) {
                replacement.push(unit);
            }
        }
        units = replacement;
        diagnostics.push(format!(
            "feature-group created: feature_id={feature_id} name={name:?} child_nodes={candidate_labels:?} layout=constrained-topology moved_from_positions={:?}",
            candidate_indices
        ));
    }

    let reordered = stable_layout_units(&units, tracks);
    *nodes = reordered.into_iter().map(|unit| unit.node).collect();
}

/// Contracts one Feature candidate set and checks whether the resulting layout is acyclic.
fn contract_feature_units(
    units: &[LayoutUnit],
    candidate_indices: &[usize],
    tracks: &[TrackBuilder<'_>],
) -> bool {
    let candidate_set = candidate_indices.iter().copied().collect::<BTreeSet<_>>();
    let graph = build_unit_constraints(units, tracks);
    let block = units.len();
    let mut contracted = BTreeMap::<usize, BTreeSet<usize>>::new();
    for unit in 0..units.len() {
        contracted
            .entry(if candidate_set.contains(&unit) {
                block
            } else {
                unit
            })
            .or_default();
    }
    for (before, successors) in graph {
        for after in successors {
            let before = if candidate_set.contains(&before) {
                block
            } else {
                before
            };
            let after = if candidate_set.contains(&after) {
                block
            } else {
                after
            };
            if before == after {
                continue;
            }
            contracted.entry(before).or_default().insert(after);
        }
    }
    let block_original_index = candidate_indices
        .iter()
        .map(|index| units[*index].original_index)
        .min()
        .unwrap_or(usize::MAX);
    stable_topological_order(&contracted, units, Some((block, block_original_index))).is_some()
}

/// Explains the cross-boundary ordering edges that make a Feature contraction cyclic.
fn contracted_constraint_diagnostics(
    units: &[LayoutUnit],
    candidate_indices: &[usize],
    tracks: &[TrackBuilder<'_>],
) -> Vec<String> {
    let candidate_set = candidate_indices.iter().copied().collect::<BTreeSet<_>>();
    let graph = build_unit_constraints(units, tracks);
    let mut details = Vec::new();
    for (before, successors) in graph {
        for after in successors {
            let crosses_boundary =
                candidate_set.contains(&before) != candidate_set.contains(&after);
            if !crosses_boundary {
                continue;
            }
            let evidence = constraint_evidence(units, before, after, tracks);
            details.push(format!(
                "{} -> {} evidence={evidence:?}",
                unit_label(&units[before], tracks),
                unit_label(&units[after], tracks),
            ));
        }
    }
    details
}

/// Returns frame and property evidence for one directed layout edge.
fn constraint_evidence(
    units: &[LayoutUnit],
    before: usize,
    after: usize,
    tracks: &[TrackBuilder<'_>],
) -> Vec<String> {
    let mut evidence = Vec::new();
    if unit_has_compositing_effect(&units[before], tracks)
        || unit_has_compositing_effect(&units[after], tracks)
    {
        evidence.push("conservative compositing/order constraint".to_string());
    }
    for &before_id in &units[before].track_ids {
        for &after_id in &units[after].track_ids {
            let Some(before_track) = tracks.get(before_id) else {
                continue;
            };
            let Some(after_track) = tracks.get(after_id) else {
                continue;
            };
            for before_observation in &before_track.observations {
                for after_observation in &after_track.observations {
                    if before_observation.frame_index == after_observation.frame_index
                        && summary_alpha_overlap(before_observation, after_observation)
                        && before_observation.source_order <= after_observation.source_order
                    {
                        evidence.push(format!(
                            "frame={} source_order={}->{}",
                            before_observation.frame_index,
                            before_observation.source_order,
                            after_observation.source_order
                        ));
                    }
                }
            }
        }
    }
    evidence.sort();
    evidence.dedup();
    evidence
}

/// Produces a stable diagnostic label for one layout unit.
fn unit_label(unit: &LayoutUnit, tracks: &[TrackBuilder<'_>]) -> String {
    let mut labels = unit
        .track_ids
        .iter()
        .filter_map(|track_id| tracks.get(*track_id))
        .map(|track| format!("track {} {:?}", track.id, track.name))
        .collect::<Vec<_>>();
    labels.sort();
    format!("unit({})", labels.join(", "))
}

/// Builds per-parent ordering constraints from effective source-pixel overlap.
fn build_unit_constraints(
    units: &[LayoutUnit],
    tracks: &[TrackBuilder<'_>],
) -> BTreeMap<usize, BTreeSet<usize>> {
    let mut graph = BTreeMap::<usize, BTreeSet<usize>>::new();
    for unit in 0..units.len() {
        graph.entry(unit).or_default();
    }
    for left in 0..units.len() {
        for right in left + 1..units.len() {
            if unit_has_compositing_effect(&units[left], tracks)
                || unit_has_compositing_effect(&units[right], tracks)
            {
                graph.entry(left).or_default().insert(right);
            }
            for &left_id in &units[left].track_ids {
                for &right_id in &units[right].track_ids {
                    let Some(left_track) = tracks.get(left_id) else {
                        continue;
                    };
                    let Some(right_track) = tracks.get(right_id) else {
                        continue;
                    };
                    for left_observation in &left_track.observations {
                        for right_observation in &right_track.observations {
                            if left_observation.frame_index != right_observation.frame_index
                                || !summary_alpha_overlap(left_observation, right_observation)
                            {
                                continue;
                            }
                            let (before, after) = if left_observation.source_order
                                <= right_observation.source_order
                            {
                                (left, right)
                            } else {
                                (right, left)
                            };
                            graph.entry(before).or_default().insert(after);
                        }
                    }
                }
            }
        }
    }
    graph
}

/// Returns whether a unit carries a non-neutral opacity or blend mode.
fn unit_has_compositing_effect(unit: &LayoutUnit, tracks: &[TrackBuilder<'_>]) -> bool {
    unit.track_ids.iter().any(|track_id| {
        tracks.get(*track_id).is_some_and(|track| {
            track.observations.iter().any(|observation| {
                observation
                    .opacity
                    .is_some_and(|opacity| (opacity - 1.0).abs() > f64::EPSILON)
                    || observation
                        .blend_mode
                        .as_deref()
                        .is_some_and(|mode| !mode.eq_ignore_ascii_case("normal"))
            })
        })
    })
}

/// Performs a deterministic topological sort using the pre-layout order as a tie-breaker.
fn stable_topological_order(
    graph: &BTreeMap<usize, BTreeSet<usize>>,
    units: &[LayoutUnit],
    block: Option<(usize, usize)>,
) -> Option<Vec<usize>> {
    let mut indegree = graph
        .keys()
        .map(|id| (*id, 0usize))
        .collect::<BTreeMap<_, _>>();
    for successors in graph.values() {
        for successor in successors {
            *indegree.entry(*successor).or_default() += 1;
        }
    }
    let mut ready = indegree
        .iter()
        .filter_map(|(id, degree)| (*degree == 0).then_some(*id))
        .collect::<Vec<_>>();
    let sort_key = |id: &usize| {
        if let Some((block_id, block_index)) = block
            && *id == block_id
        {
            block_index
        } else {
            units
                .get(*id)
                .map(|unit| unit.original_index)
                .unwrap_or(usize::MAX)
        }
    };
    let mut output = Vec::new();
    while !ready.is_empty() {
        ready.sort_by_key(sort_key);
        let id = ready.remove(0);
        output.push(id);
        for successor in graph.get(&id).into_iter().flatten() {
            let degree = indegree.get_mut(successor).expect("graph node exists");
            *degree -= 1;
            if *degree == 0 {
                ready.push(*successor);
            }
        }
    }
    (output.len() == indegree.len()).then_some(output)
}

/// Sorts existing sibling nodes without changing their grouping structure.
fn stable_layout_units(units: &[LayoutUnit], tracks: &[TrackBuilder<'_>]) -> Vec<LayoutUnit> {
    let graph = build_unit_constraints(units, tracks);
    let order = stable_topological_order(&graph, units, None)
        .unwrap_or_else(|| (0..units.len()).collect::<Vec<_>>());
    order
        .into_iter()
        .filter_map(|index| units.get(index).cloned())
        .collect()
}

/// Returns all logical track IDs represented by one output node.
fn node_track_ids(node: &PlannedNode) -> Vec<usize> {
    match node {
        PlannedNode::Track { track_id } => vec![*track_id],
        PlannedNode::Group { children, .. } => children.iter().flat_map(node_track_ids).collect(),
    }
}

/// Tests alpha overlap between two source observations in one frame.
fn summary_alpha_overlap(
    left: &super::association::ObservationSummary<'_>,
    right: &super::association::ObservationSummary<'_>,
) -> bool {
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

/// Summarizes every Feature identity represented by one output node.
fn summarize_node(node: &PlannedNode, tracks: &[TrackBuilder<'_>]) -> NodeFeatureSummary {
    match node {
        PlannedNode::Track { track_id } => tracks
            .get(*track_id)
            .map(track_feature_summary)
            .unwrap_or_else(|| NodeFeatureSummary {
                has_unassigned_source: true,
                ..NodeFeatureSummary::default()
            }),
        PlannedNode::Group { children, .. } => {
            children
                .iter()
                .fold(NodeFeatureSummary::default(), |mut summary, child| {
                    let child_summary = summarize_node(child, tracks);
                    summary.feature_ids.extend(child_summary.feature_ids);
                    summary.has_unassigned_source |= child_summary.has_unassigned_source;
                    summary
                })
        }
    }
}

/// Returns source Feature identities for one logical track.
fn track_feature_summary(track: &TrackBuilder<'_>) -> NodeFeatureSummary {
    let mut summary = NodeFeatureSummary::default();
    for observation in &track.observations {
        if let Some(identity) = &observation.feature_identity {
            summary.feature_ids.insert(identity.container_id);
        } else {
            summary.has_unassigned_source = true;
        }
    }
    summary
}

/// Returns a display name from the first two distinct source component names.
fn source_display_name(track: &TrackBuilder<'_>) -> String {
    let mut observations = track.observations.iter().collect::<Vec<_>>();
    observations.sort_by(|left, right| {
        (
            left.frame_index,
            left.source_order,
            left.source_path.clone(),
        )
            .cmp(&(
                right.frame_index,
                right.source_order,
                right.source_path.clone(),
            ))
    });
    let mut names = Vec::new();
    for observation in observations {
        if !names.iter().any(|name| name == &observation.name) {
            names.push(observation.name.clone());
        }
    }
    match names.as_slice() {
        [] => track.name.clone(),
        [name] => name.clone(),
        [first, second] => format!("{first} / {second}"),
        [first, second, ..] => format!("{first} / {second} …"),
    }
}

/// Assigns stable suffixes to equal track names under one parent node.
fn rename_duplicate_track_names(nodes: &[PlannedNode], tracks: &mut [TrackBuilder<'_>]) {
    let mut counts = BTreeMap::<String, usize>::new();
    for node in nodes {
        if let PlannedNode::Track { track_id } = node {
            if let Some(track) = tracks.get(*track_id) {
                *counts.entry(track.name.clone()).or_default() += 1;
            }
        }
    }
    let mut seen = BTreeMap::<String, usize>::new();
    for node in nodes {
        match node {
            PlannedNode::Track { track_id } => {
                if let Some(track) = tracks.get_mut(*track_id) {
                    if counts.get(&track.name).copied().unwrap_or_default() > 1 {
                        let base = track.name.clone();
                        let ordinal = seen.entry(base.clone()).or_default();
                        *ordinal += 1;
                        track.name = format!("{base} {ordinal}");
                    }
                }
            }
            PlannedNode::Group { children, .. } => rename_duplicate_track_names(children, tracks),
        }
    }
}

/// Creates stable display names for all Feature identities seen in the output.
fn feature_group_names_for_ids(
    feature_ids: &BTreeSet<u32>,
    feature_meta: &HashMap<u32, (String, usize)>,
) -> BTreeMap<u32, String> {
    let mut by_name = BTreeMap::<String, Vec<(usize, u32)>>::new();
    for feature_id in feature_ids {
        let (name, order) = feature_meta
            .get(feature_id)
            .cloned()
            .unwrap_or_else(|| (format!("Feature {feature_id}"), usize::MAX));
        by_name.entry(name).or_default().push((order, *feature_id));
    }
    let mut output = BTreeMap::new();
    for (name, mut ids) in by_name {
        ids.sort_unstable();
        for (index, (_, feature_id)) in ids.into_iter().enumerate() {
            let display = if index == 0 {
                name.clone()
            } else {
                format!("{name} ({})", index + 1)
            };
            output.insert(feature_id, display);
        }
    }
    output
}

/// Records every final track's source paths and Feature identities.
fn track_source_mapping(track: &TrackBuilder<'_>) -> String {
    let mut sources = track
        .observations
        .iter()
        .map(|observation| {
            let feature = observation
                .feature_identity
                .as_ref()
                .map(|identity| identity.container_id.to_string())
                .unwrap_or_else(|| "unassigned".to_string());
            format!(
                "source_id={} path={:?} feature={feature}",
                observation.source_layer_id, observation.source_path
            )
        })
        .collect::<Vec<_>>();
    sources.sort();
    sources.dedup();
    format!("track {} {:?} sources={sources:?}", track.id, track.name)
}

/// Produces a stable label for a node used in grouping diagnostics.
fn node_label(node: &PlannedNode, tracks: &[TrackBuilder<'_>]) -> String {
    match node {
        PlannedNode::Track { track_id } => tracks
            .get(*track_id)
            .map(|track| format!("track {} {:?}", track.id, track.name))
            .unwrap_or_else(|| format!("track {track_id}")),
        PlannedNode::Group { name, .. } => format!("group {name:?}"),
    }
}
