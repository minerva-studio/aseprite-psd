use std::rc::Rc;

use super::association::{decision, new_track, parse_layer_name, record_assignment};
use super::layout::{CandidateGroupPath, candidate_members_form_complete_interval};
use super::observation::{FrameContainerInfo, LayerEvidence, Observation, ObservationId};
use super::*;
use crate::{NormalizedLayer, NormalizedLayerKind};

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
fn default_plan_uses_conservative_strategy() {
    let plan = super::build_layer_write_plan(
        &document(vec![pixel(1, "rear foot", 0, [1, 2, 3, 255])]),
        AutoAssociationOptions::default(),
    )
    .expect("conservative association should succeed");
    assert_eq!(
        plan.report.strategy,
        AssociationStrategy::Conservative {
            uncertain_layers: UncertainLayerMode::Group,
        }
    );
    assert!(plan.report.candidate_groups.is_empty());
}

#[test]
fn automatic_association_allows_documents_without_source_layers() {
    let plan = build_layer_write_plan(&document(Vec::new()))
        .expect("automatic association should allow slice-only documents");
    assert!(plan.root_nodes.is_empty());
    assert!(plan.tracks.is_empty());
    assert_eq!(plan.report.observation_count, 0);
    assert_eq!(plan.report.track_count, 0);
    assert!(plan.report.warnings.iter().any(|warning| {
        warning.contains("no source layers") && warning.contains("empty layer plan")
    }));
}

#[test]
fn metadata_preservation_isolates_only_layers_with_reference_points() {
    let mut referenced = pixel(1, "role", 0, [1, 2, 3, 255]);
    referenced.frame_states[0].reference_point = Some(crate::AnimationPoint { x: 8.0, y: 8.0 });
    referenced.frame_states.push(second_frame_state(false));

    let mut ordinary = pixel(2, "role", 1, [1, 2, 3, 255]);
    ordinary.frame_states[0].enabled = false;
    ordinary.frame_states.push(second_frame_state(true));

    let mut source = two_frame_document(vec![referenced, ordinary]);
    source.root_layers[0].frame_states[1].enabled = false;
    source.root_layers[1].frame_states[1].enabled = true;

    let isolated = super::build_layer_write_plan_with_metadata(
        &source,
        AutoAssociationOptions::default(),
        true,
    )
    .expect("metadata-aware association should succeed");
    assert_eq!(isolated.tracks.len(), 2);

    let merged = super::build_layer_write_plan_with_metadata(
        &source,
        AutoAssociationOptions::default(),
        false,
    )
    .expect("ordinary association should succeed");
    assert_eq!(merged.tracks.len(), 1);
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

/// Builds a deterministic frame-container document for solver invariant checks.
fn generated_solver_document(seed: u32) -> (NormalizedDocument, Vec<u32>, Vec<u32>) {
    let frame_count = 2 + seed as usize % 2;
    let mut source_ids = Vec::new();
    let mut metadata_locked_ids = Vec::new();
    let mut groups = Vec::new();
    let mut next_id = 1u32;
    for frame_index in 0..frame_count {
        let layer_count = 1 + ((seed.rotate_left(frame_index as u32) >> 3) as usize % 2);
        let mut children = Vec::new();
        for layer_index in 0..layer_count {
            let name = match (seed as usize + frame_index * 3 + layer_index) % 4 {
                0 => format!("role {}", layer_index % 2),
                1 => format!("role {} copy", layer_index % 2),
                2 => format!("Layer {}", layer_index + 1),
                _ => format!("图层 {}", layer_index + 1),
            };
            let x = if (seed >> (layer_index % 16)) & 1 == 0 {
                layer_index as i32
            } else {
                0
            };
            let mut child = pixel(
                next_id,
                &name,
                x,
                [next_id as u8, frame_index as u8, layer_index as u8, 255],
            );
            child.frame_states = (0..frame_count)
                .map(|index| NormalizedLayerFrameState {
                    frame_index: index as u32,
                    record_present: true,
                    enabled: true,
                    explicit_enable: true,
                    offset: None,
                    reference_point: None,
                    opacity: None,
                })
                .collect();
            if seed.wrapping_add(next_id).is_multiple_of(17) {
                child.frame_states[frame_index].reference_point =
                    Some(crate::AnimationPoint { x: 3.0, y: 5.0 });
                metadata_locked_ids.push(next_id);
            }
            source_ids.push(next_id);
            children.push(child);
            next_id += 1;
        }
        let bounds = children[0].bounds;
        groups.push(NormalizedLayer {
            id: 10_000 + frame_index as u32,
            name: format!("frame {}", frame_index + 1),
            kind: NormalizedLayerKind::Group,
            bounds,
            opacity: None,
            blend_mode: Some("pass through".to_string()),
            hidden: Some(false),
            pixels: None,
            children,
            frame_states: (0..frame_count)
                .map(|index| NormalizedLayerFrameState {
                    frame_index: index as u32,
                    record_present: true,
                    enabled: index == frame_index,
                    explicit_enable: true,
                    offset: None,
                    reference_point: None,
                    opacity: None,
                })
                .collect(),
        });
    }
    let mut generated = document(groups);
    generated.frames = (0..frame_count)
        .map(|index| NormalizedFrame {
            index: index as u32,
            source_id: Some(index as u32),
            duration_ms: Some(100),
            dispose: None,
        })
        .collect();
    (generated, source_ids, metadata_locked_ids)
}

/// Returns the stable source-to-track mapping used for determinism assertions.
fn source_track_mapping(plan: &LayerWritePlan) -> Vec<(u32, usize)> {
    let mut mapping = plan
        .tracks
        .iter()
        .flat_map(|track| {
            track
                .cels
                .iter()
                .flatten()
                .map(move |cel| (cel.source_layer_id, track.id))
        })
        .collect::<Vec<_>>();
    mapping.sort_unstable();
    mapping
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

    let plan = build_layer_write_plan(&two_frame_document(vec![foot, body, foot_copy, body_copy]))
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
fn auto_association_writes_one_layer_with_linked_frame_cels() {
    let document = three_frame_document(vec![
        three_frame_family_layer(1, "前翅膀", 0, [1, 2, 3, 255]),
        three_frame_family_layer(2, "前翅膀 拷贝 2", 1, [1, 2, 3, 255]),
        three_frame_family_layer(3, "前翅膀 拷贝 5", 2, [1, 2, 3, 255]),
    ]);
    let plan = build_layer_write_plan(&document).expect("association should succeed");
    assert_eq!(plan.tracks.len(), 1);

    let encoded = crate::aseprite_writer::encode_with_plan_and_linked_cels(
        &document,
        &plan,
        crate::LinkedCelMode::Identical,
    )
    .expect("planned output should encode");
    assert_eq!(encoded.cel_reuse.pixel_cel_count, 1);
    assert_eq!(encoded.cel_reuse.linked_cel_count, 2);
    let file = aseprite::AsepriteFile::from_reader(&encoded.bytes[..]).expect("valid Aseprite");
    assert_eq!(file.layers().len(), 1);
    let layer = file.layer_ref(0).expect("logical layer");
    assert!(matches!(
        &file.cel(layer, 1).unwrap().kind,
        aseprite::CelKind::Linked {
            source_frame: 0,
            ..
        }
    ));
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
fn single_new_generic_can_fill_a_structurally_exclusive_slot() {
    let first =
        top_level_frame_container(10, "frame 1", 0, pixel(1, "rear foot", 0, [1, 2, 3, 255]));
    let second =
        top_level_frame_container(11, "frame 2", 1, pixel(2, "Layer 5", 0, [4, 5, 6, 255]));
    let plan = build_layer_write_plan(&two_frame_document(vec![first, second]))
        .expect("single generic replacement should associate");
    assert_eq!(plan.tracks.len(), 1);
    assert!(plan.report.decisions.iter().any(|decision| {
        decision.original_name == "Layer 5"
            && decision
                .evidence
                .iter()
                .any(|value| value == "structural-empty-slot")
    }));
}

#[test]
fn conservative_protects_a_batch_of_new_generic_effects() {
    let first =
        top_level_frame_container(10, "frame 1", 0, pixel(1, "backpack", 0, [1, 2, 3, 255]));
    let effects = top_level_frame_container_with_children(
        11,
        "frame 2",
        1,
        vec![
            pixel(2, "Layer 7", 0, [4, 5, 6, 255]),
            pixel(3, "Layer 8", 1, [7, 8, 9, 255]),
        ],
    );
    let document = two_frame_document(vec![first, effects]);

    let compact = super::build_layer_write_plan(
        &document,
        AutoAssociationOptions {
            strategy: AssociationStrategy::Compact,
            ..AutoAssociationOptions::default()
        },
    )
    .expect("compact association should use the safe empty slot");
    assert_eq!(compact.tracks.len(), 2);

    let conservative = build_layer_write_plan(&document)
        .expect("conservative association should protect the new effect batch");
    assert_eq!(conservative.tracks.len(), 3);
    assert_eq!(
        conservative
            .report
            .decisions
            .iter()
            .filter(|decision| {
                decision
                    .evidence
                    .iter()
                    .any(|value| value == "new-generic-batch-protected")
            })
            .count(),
        2
    );
}

#[test]
fn synthetic_top_level_frames_require_exact_source_identity() {
    let first = top_level_frame_container(10, "frame 1", 0, pixel(1, "Layer 1", 0, [1, 2, 3, 255]));
    let second =
        top_level_frame_container(11, "frame 2", 1, pixel(2, "Layer 1", 0, [1, 2, 3, 255]));
    let document = two_frame_document(vec![first, second]);

    let plan = super::build_layer_write_plan_with_context(
        &document,
        AutoAssociationOptions::default(),
        false,
        false,
    )
    .expect("synthetic frame association should preserve distinct sources");

    assert_eq!(plan.tracks.len(), 2);
    assert!(plan.report.candidate_groups.is_empty());
    assert!(plan.report.decisions.iter().any(|decision| {
        decision.frame_index == 1
            && decision
                .evidence
                .iter()
                .any(|value| value == "synthetic-frame identity unproven")
    }));
}

#[test]
fn generated_solver_cases_preserve_core_invariants() {
    for seed in 0..256u32 {
        let (document, mut expected_sources, metadata_locked_ids) = generated_solver_document(seed);
        let compact_options = AutoAssociationOptions {
            strategy: AssociationStrategy::Compact,
            ..AutoAssociationOptions::default()
        };
        let conservative_options = AutoAssociationOptions::default();
        let compact = super::build_layer_write_plan_with_metadata(&document, compact_options, true)
            .unwrap_or_else(|error| panic!("seed {seed} compact failed: {error}"));
        let conservative =
            super::build_layer_write_plan_with_metadata(&document, conservative_options, true)
                .unwrap_or_else(|error| panic!("seed {seed} conservative failed: {error}"));
        expected_sources.sort_unstable();
        for (strategy, plan) in [("compact", &compact), ("conservative", &conservative)] {
            let mut actual_sources = source_track_mapping(plan)
                .into_iter()
                .map(|(source, _)| source)
                .collect::<Vec<_>>();
            actual_sources.sort_unstable();
            assert_eq!(
                actual_sources, expected_sources,
                "seed {seed} {strategy} must cover every observation exactly once"
            );
            assert!(
                plan.tracks
                    .iter()
                    .all(|track| track.cels.len() == document.frames.len()),
                "seed {seed} {strategy} produced an invalid frame span"
            );
            for locked_id in &metadata_locked_ids {
                let containing = plan
                    .tracks
                    .iter()
                    .find(|track| {
                        track
                            .cels
                            .iter()
                            .flatten()
                            .any(|cel| cel.source_layer_id == *locked_id)
                    })
                    .unwrap_or_else(|| panic!("seed {seed} lost metadata source {locked_id}"));
                assert!(
                    containing
                        .cels
                        .iter()
                        .flatten()
                        .all(|cel| cel.source_layer_id == *locked_id),
                    "seed {seed} {strategy} merged metadata source {locked_id}"
                );
            }
        }
        assert!(
            conservative.tracks.len() >= compact.tracks.len(),
            "seed {seed} conservative became more aggressive: {} < {}",
            conservative.tracks.len(),
            compact.tracks.len()
        );
        if seed % 64 == 0 {
            let compact_repeat =
                super::build_layer_write_plan_with_metadata(&document, compact_options, true)
                    .unwrap_or_else(|error| panic!("seed {seed} compact repeat failed: {error}"));
            assert_eq!(
                source_track_mapping(&compact),
                source_track_mapping(&compact_repeat),
                "seed {seed} compact mapping was not deterministic"
            );
        }
    }
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
fn flat_layout_keeps_a_merged_single_generic_at_root() {
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
    assert_eq!(plan.tracks.len(), 1);
    assert!(plan.report.candidate_groups.is_empty());
}

#[test]
fn exact_unique_name_is_used_as_a_strong_anchor() {
    let plan = build_layer_write_plan(&document(vec![pixel(1, "Rear Foot", 0, [1, 2, 3, 255])]))
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
            metadata_locked: false,
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
fn strict_order_splits_a_track_instead_of_changing_effective_order() {
    let plan = super::build_layer_write_plan(
        &two_frame_order_document(false),
        AutoAssociationOptions {
            strategy: AssociationStrategy::Conservative {
                uncertain_layers: UncertainLayerMode::Group,
            },
            stable_order: StableOrderMode::Strict,
            ..AutoAssociationOptions::default()
        },
    )
    .expect("strict mode should preserve order by splitting the unsafe association");
    assert_eq!(plan.tracks.len(), 3);
    assert!(
        plan.tracks
            .iter()
            .all(|track| { track.cels.iter().flatten().all(|cel| cel.z_index == 0) })
    );
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
fn stable_order_splits_effective_order_changes() {
    let plan = build_layer_write_plan(&two_frame_order_document(false))
        .expect("association should succeed");
    assert_eq!(plan.report.z_order_mode, LayerZOrderMode::Stable);
    assert_eq!(plan.report.stable_order_mode, StableOrderMode::Consensus);
    assert!(
        plan.tracks
            .iter()
            .all(|track| track.cels.iter().flatten().all(|cel| cel.z_index == 0))
    );
    assert!(plan.report.z_order_diagnostics.is_empty());
    assert_eq!(plan.tracks.len(), 3);
    assert!(!plan.report.stable_order_diagnostics.is_empty());
}
