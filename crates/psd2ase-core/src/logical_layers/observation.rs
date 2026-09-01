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
    /// Whether this source layer must remain separate for Photoshop metadata.
    pub(super) metadata_locked: bool,
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
            let next_metadata_locked = metadata_locked
                || (state.preserve_photoshop_metadata
                    && layer_has_meaningful_reference_point(layer));
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
                    next_metadata_locked,
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
                metadata_locked: metadata_locked
                    || (state.preserve_photoshop_metadata
                        && layer_has_meaningful_reference_point(layer)),
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
