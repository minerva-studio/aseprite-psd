use std::collections::{HashMap, HashSet};
use std::ops::Deref;
use std::rc::Rc;

use super::association::{canonical_name, parse_layer_name};
use super::{GroupKey, GroupSegment};
use crate::aseprite_writer::cel_position;
use crate::layer_names::CopySuffixMatch;
use crate::photoshop_metadata::layer_has_meaningful_reference_point;
use crate::{NormalizedLayer, NormalizedLayerKind};

#[derive(Debug)]
pub(super) struct LayerEvidence<'doc> {
    pub(super) source_layer_id: u32,
    pub(super) source_path: String,
    pub(super) name: String,
    pub(super) normalized_name: String,
    pub(super) name_key: String,
    pub(super) generic_name: bool,
    pub(super) copy_suffixes: Vec<CopySuffixMatch>,
    pub(super) suffix_limit_reached: bool,
    pub(super) frame_container_ids: Vec<u32>,
    pub(super) group_path: Vec<GroupSegment>,
    pub(super) width: u32,
    pub(super) height: u32,
    pub(super) pixels: &'doc [u8],
    /// Layer-level opacity that cannot be represented independently per cel.
    pub(super) opacity: Option<f64>,
    /// Layer-level blend mode that cannot be represented independently per cel.
    pub(super) blend_mode: Option<String>,
    /// Whether this source layer must remain separate for Photoshop metadata.
    pub(super) metadata_locked: bool,
    /// Stable Feature container identity used only for Feature-mode association.
    pub(super) feature_identity: Option<FeatureIdentity>,
    /// Display name for a direct-state Feature track.
    pub(super) feature_display_name: Option<String>,
}

/// Identifies a pixel's Feature container and relative component path.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub(super) struct FeatureIdentity {
    /// Source layer ID of the outer Feature/Tag container.
    pub(super) container_id: u32,
    /// Relative component path after state-container segments are removed.
    pub(super) member_path: Vec<String>,
}

#[derive(Debug, Clone)]
pub(super) struct Observation<'doc> {
    pub(super) id: ObservationId,
    pub(super) evidence: Rc<LayerEvidence<'doc>>,
    pub(super) frame_index: usize,
    pub(super) source_order: usize,
    pub(super) x: i32,
    pub(super) y: i32,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub(super) struct ObservationId(pub(super) usize);
impl<'doc> Deref for Observation<'doc> {
    type Target = LayerEvidence<'doc>;

    fn deref(&self) -> &Self::Target {
        &self.evidence
    }
}
#[derive(Debug)]
pub(super) struct ObservationStore<'doc> {
    pub(super) evidence: Vec<Rc<LayerEvidence<'doc>>>,
    pub(super) frames: Vec<Vec<Observation<'doc>>>,
}

pub(super) struct ObservationCollectionState<'state, 'doc> {
    pub(super) selectors: &'state HashMap<u32, FrameContainerInfo>,
    pub(super) source_order: &'state mut usize,
    pub(super) next_observation_id: &'state mut usize,
    pub(super) store: &'state mut ObservationStore<'doc>,
    /// Whether meaningful Photoshop metadata is being preserved in the output.
    pub(super) preserve_photoshop_metadata: bool,
    /// Feature roots whose source state containers must not become output groups.
    pub(super) feature_containers: &'state HashMap<u32, FeatureContainerInfo>,
}

impl ObservationStore<'_> {
    /// Creates an empty observation store for the document timeline.
    pub(super) fn new(frame_count: usize) -> Self {
        Self {
            evidence: Vec::new(),
            frames: vec![Vec::new(); frame_count],
        }
    }
}
#[derive(Debug, Clone)]
pub(super) struct FrameContainerInfo {
    pub(super) parent_id: Option<u32>,
    pub(super) active_frames: HashSet<usize>,
}

/// Feature container metadata shared by observation collection and association.
#[derive(Debug, Clone)]
pub(super) struct FeatureContainerInfo {
    /// User-authored Feature/Tag name used for direct-state tracks.
    pub(super) name: String,
    /// Direct child groups that represent mutually exclusive state slots.
    pub(super) state_ids: HashSet<u32>,
}

pub(super) fn collect_pixel_layer_ids(layers: &[NormalizedLayer], output: &mut Vec<u32>) {
    for layer in layers {
        if layer.kind == NormalizedLayerKind::Pixel {
            output.push(layer.id);
        }
        collect_pixel_layer_ids(&layer.children, output);
    }
}

pub(super) fn collect_observations<'doc>(
    layer: &'doc NormalizedLayer,
    path: &[String],
    group_path: &[GroupSegment],
    ancestors: &[&NormalizedLayer],
    frame_container_ids: &[u32],
    metadata_locked: bool,
    feature_root_id: Option<u32>,
    feature_member_path: &[String],
    feature_state_slot: bool,
    state: &mut ObservationCollectionState<'_, 'doc>,
) -> Result<(), String> {
    let is_visible = |frame_index: usize| {
        let ancestors_visible = ancestors.iter().fold(true, |visible, ancestor| {
            ancestor.is_effectively_visible(frame_index, visible)
        });
        layer.is_effectively_visible(frame_index, ancestors_visible)
    };
    match layer.kind {
        NormalizedLayerKind::Group => {
            let next_metadata_locked = metadata_locked
                || (state.preserve_photoshop_metadata
                    && layer_has_meaningful_reference_point(layer));
            let mut next_path = group_path.to_vec();
            let mut next_frame_container_ids = frame_container_ids.to_vec();
            let (next_feature_root_id, is_feature_root, is_state_container) =
                if let Some(root_id) = feature_root_id {
                    let is_state = state
                        .feature_containers
                        .get(&root_id)
                        .is_some_and(|container| container.state_ids.contains(&layer.id));
                    (Some(root_id), false, is_state)
                } else if state.feature_containers.contains_key(&layer.id) {
                    (Some(layer.id), true, false)
                } else {
                    (None, false, false)
                };
            let mut next_feature_member_path = feature_member_path.to_vec();
            let next_feature_state_slot = feature_state_slot || is_state_container;
            if state.selectors.contains_key(&layer.id) {
                next_frame_container_ids.push(layer.id);
            } else if !is_feature_root && !is_state_container {
                next_path.push(GroupSegment {
                    source_layer_id: Some(layer.id),
                    name: layer.name.clone(),
                    key: GroupKey::Persistent(canonical_name(&layer.name)),
                });
            }
            if !is_feature_root && !is_state_container && feature_root_id.is_some() {
                next_feature_member_path.push(layer.name.clone());
            } else if is_state_container {
                next_feature_member_path.clear();
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
                    next_metadata_locked,
                    next_feature_root_id,
                    &next_feature_member_path,
                    next_feature_state_slot,
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
                opacity: layer.opacity,
                blend_mode: layer.blend_mode.clone(),
                metadata_locked: metadata_locked
                    || (state.preserve_photoshop_metadata
                        && layer_has_meaningful_reference_point(layer)),
                feature_identity: feature_root_id.map(|container_id| {
                    let member_path = if feature_member_path.is_empty() && !feature_state_slot {
                        Vec::new()
                    } else {
                        let mut path = feature_member_path.to_vec();
                        path.push(layer.name.clone());
                        path
                    };
                    FeatureIdentity {
                        container_id,
                        member_path,
                    }
                }),
                feature_display_name: feature_root_id
                    .filter(|_| feature_member_path.is_empty() && !feature_state_slot)
                    .and_then(|container_id| {
                        state
                            .feature_containers
                            .get(&container_id)
                            .map(|container| container.name.clone())
                    }),
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

/// Finds top-level Feature/Tag containers and their mutually exclusive state slots.
pub(super) fn find_feature_containers(
    layers: &[NormalizedLayer],
    selectors: &HashMap<u32, FrameContainerInfo>,
) -> HashMap<u32, FeatureContainerInfo> {
    layers
        .iter()
        .filter_map(|layer| {
            let NormalizedLayerKind::Group = layer.kind else {
                return None;
            };
            if selectors.contains_key(&layer.id) {
                return None;
            }
            let mut state_ids = layer
                .children
                .iter()
                .filter(|child| {
                    child.kind == NormalizedLayerKind::Group
                        && selectors
                            .get(&child.id)
                            .is_some_and(|info| info.parent_id == Some(layer.id))
                })
                .map(|child| child.id)
                .collect::<HashSet<_>>();
            let direct_pixel_feature = layer
                .children
                .iter()
                .all(|child| child.kind == NormalizedLayerKind::Pixel)
                && !layer.children.is_empty()
                && layer_has_timeline_variation(layer);
            let timeline_bound_container = layer_has_timeline_variation(layer);
            // A timeline-bound Feature may wrap one state group whose own
            // selector is static because every state is enabled in the source
            // record. Treat that sole child as the state container so its
            // path does not prevent cross-Tag Feature association.
            if state_ids.is_empty()
                && timeline_bound_container
                && layer.children.len() == 1
                && layer.children[0].kind == NormalizedLayerKind::Group
            {
                state_ids.insert(layer.children[0].id);
            }
            if direct_pixel_feature || timeline_bound_container || !state_ids.is_empty() {
                Some((
                    layer.id,
                    FeatureContainerInfo {
                        name: layer.name.clone(),
                        state_ids,
                    },
                ))
            } else {
                None
            }
        })
        .collect()
}

fn layer_has_timeline_variation(layer: &NormalizedLayer) -> bool {
    layer.frame_states.windows(2).any(|pair| {
        pair[0].enabled != pair[1].enabled
            || pair[0].offset != pair[1].offset
            || pair[0].reference_point != pair[1].reference_point
            || pair[0].opacity != pair[1].opacity
    }) || layer.children.iter().any(layer_has_timeline_variation)
}

/// Associates copy-name families across all frames before residual matching.
pub(super) fn find_frame_selector_groups(
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
    let visible = layer.is_effectively_visible(frame_index, ancestors_visible);
    match layer.kind {
        NormalizedLayerKind::Pixel => visible,
        NormalizedLayerKind::Group => layer
            .children
            .iter()
            .any(|child| has_visible_pixel(child, frame_index, visible)),
    }
}
