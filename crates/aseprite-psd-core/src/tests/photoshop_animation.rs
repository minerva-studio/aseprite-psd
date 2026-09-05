use super::*;

use ag_psd::descriptor::{Descriptor, DescriptorValue, write_version_and_descriptor};
use ag_psd::writer::{create_writer, get_writer_buffer};

#[test]
fn empty_reference_point_descriptor_is_treated_as_nop() {
    let mut descriptor = Descriptor::new("", "frame");
    descriptor.set(
        "FXRf",
        DescriptorValue::Descriptor(Descriptor::new("", "point")),
    );
    assert_eq!(descriptor_point(&descriptor, "FXRf"), Ok(None));
}

#[test]
fn cursor_rejects_truncated_section() {
    let mut cursor = Cursor::new(&[0, 1]);
    let error = cursor.u32("test").expect_err("truncated input must fail");
    assert!(matches!(error, AnimationParseError::Truncated { .. }));
}

#[test]
fn cursor_rejects_length_overflow() {
    let mut cursor = Cursor::new(&[0]);
    let error = cursor
        .take(usize::MAX, "test")
        .expect_err("oversized input must fail");
    assert!(matches!(error, AnimationParseError::Truncated { .. }));
}

#[test]
fn descriptor_reader_preserves_unknown_fields() {
    let mut descriptor = Descriptor::new("", "test");
    descriptor.set("Unkn", DescriptorValue::Integer(42));
    let mut writer = create_writer(128);
    write_version_and_descriptor(&mut writer, &descriptor);
    let bytes = get_writer_buffer(&writer);
    let parsed = read_descriptor(&bytes, "test").expect("descriptor should parse");
    assert!(matches!(
        parsed.get("Unkn"),
        Some(DescriptorValue::Integer(42))
    ));
}

#[test]
fn descriptor_reader_rejects_truncation() {
    let error =
        read_descriptor(&[0, 0, 0, 16], "test").expect_err("truncated descriptor must fail");
    assert!(matches!(error, AnimationParseError::InvalidData(_)));
}

#[test]
fn omitted_frame_disposal_defaults_to_auto() {
    let mut frame = Descriptor::new("", "null");
    frame.set("FrID", DescriptorValue::Integer(1));
    frame.set("FrDl", DescriptorValue::Integer(20));

    let parsed = parse_catalog_frame(&frame).expect("frame should parse");
    assert_eq!(parsed.dispose, Some("auto".to_string()));
}

#[test]
fn animation_resource_is_selected_by_signature_across_plugin_slots() {
    fn resource(id: u16, payload: &[u8]) -> Vec<u8> {
        let mut bytes = Vec::new();
        bytes.extend_from_slice(b"8BIM");
        bytes.extend_from_slice(&id.to_be_bytes());
        bytes.extend_from_slice(&[0, 0]);
        bytes.extend_from_slice(&(payload.len() as u32).to_be_bytes());
        bytes.extend_from_slice(payload);
        if !payload.len().is_multiple_of(2) {
            bytes.push(0);
        }
        bytes
    }

    let mut frame = Descriptor::new("", "null");
    frame.set("FrID", DescriptorValue::Integer(1));
    frame.set("FrDl", DescriptorValue::Integer(20));
    let mut animation_set = Descriptor::new("", "null");
    animation_set.set("FsID", DescriptorValue::Integer(2));
    animation_set.set("AFrm", DescriptorValue::Integer(0));
    animation_set.set(
        "FsFr",
        DescriptorValue::List(vec![DescriptorValue::Integer(1)]),
    );
    animation_set.set("LCnt", DescriptorValue::Integer(0));
    let mut descriptor = Descriptor::new("", "null");
    descriptor.set(
        "FrIn",
        DescriptorValue::List(vec![DescriptorValue::Descriptor(frame)]),
    );
    descriptor.set(
        "FSts",
        DescriptorValue::List(vec![DescriptorValue::Descriptor(animation_set)]),
    );
    let mut descriptor_writer = create_writer(256);
    write_version_and_descriptor(&mut descriptor_writer, &descriptor);
    let descriptor_bytes = get_writer_buffer(&descriptor_writer);

    let mut animation_payload = Vec::new();
    animation_payload.extend_from_slice(b"maniIRFR");
    let section_length = 8 + 4 + descriptor_bytes.len();
    animation_payload.extend_from_slice(&(section_length as u32).to_be_bytes());
    animation_payload.extend_from_slice(b"8BIMAnDs");
    animation_payload.extend_from_slice(&(descriptor_bytes.len() as u32).to_be_bytes());
    animation_payload.extend_from_slice(&descriptor_bytes);
    if !descriptor_bytes.len().is_multiple_of(2) {
        animation_payload.push(0);
    }

    let mut resources = resource(4000, b"mopt\0\0\0\0");
    resources.extend(resource(4999, &animation_payload));
    let mut result = ScanResult::default();
    scan_resources(&resources, &mut result).expect("plugin resources should scan");
    assert_eq!(result.resource_ids, vec![4999]);
    assert_eq!(result.animation_descriptors.len(), 1);
}

#[test]
fn duplicate_input_layer_ids_are_rejected() {
    let layers = vec![
        AnimationLayerInput {
            id: 7,
            path: "0".to_string(),
            is_group: false,
            is_container_group: false,
            hidden: false,
            ancestor_ids: Vec::new(),
        },
        AnimationLayerInput {
            id: 7,
            path: "1".to_string(),
            is_group: false,
            is_container_group: false,
            hidden: false,
            ancestor_ids: Vec::new(),
        },
    ];
    assert!(matches!(
        validate_input_layers(&layers),
        Err(AnimationParseError::DuplicateId { .. })
    ));
}

#[test]
fn missing_enable_inherits_previous_state() {
    let layer = AnimationLayerInput {
        id: 7,
        path: "0".to_string(),
        is_group: false,
        is_container_group: false,
        hidden: false,
        ancestor_ids: Vec::new(),
    };
    let raw = RawLayer {
        id: Some(7),
        shmd: Some(LayerMetadata {
            frames: vec![
                RawFrameState {
                    frame_id: 1,
                    enable: Some(false),
                    offset: None,
                    reference_point: None,
                    opacity: None,
                },
                RawFrameState {
                    frame_id: 2,
                    enable: None,
                    offset: None,
                    reference_point: None,
                    opacity: None,
                },
            ],
            flags: None,
        }),
        flags: None,
        is_bounding_divider: false,
    };
    let frames = vec![
        PhotoshopFrame {
            id: 1,
            duration_ms: 100,
            dispose: None,
        },
        PhotoshopFrame {
            id: 2,
            duration_ms: 100,
            dispose: None,
        },
    ];
    let states = resolve_layer_states(&layer, &raw, &frames).expect("states should resolve");
    assert!(!states.frames[0].enabled);
    assert!(!states.frames[1].enabled);
    assert!(states.frames[0].record_present);
    assert!(states.frames[1].record_present);
    assert!(!states.frames[1].explicit_enable);
}

#[test]
fn missing_animation_record_does_not_inherit_visibility() {
    let layer = AnimationLayerInput {
        id: 7,
        path: "0".to_string(),
        is_group: false,
        is_container_group: false,
        hidden: false,
        ancestor_ids: Vec::new(),
    };
    let raw = RawLayer {
        id: Some(7),
        shmd: Some(LayerMetadata {
            frames: vec![
                RawFrameState {
                    frame_id: 1,
                    enable: Some(true),
                    offset: None,
                    reference_point: None,
                    opacity: None,
                },
                RawFrameState {
                    frame_id: 3,
                    enable: Some(true),
                    offset: None,
                    reference_point: None,
                    opacity: None,
                },
            ],
            flags: None,
        }),
        flags: None,
        is_bounding_divider: false,
    };
    let frames = vec![
        PhotoshopFrame {
            id: 1,
            duration_ms: 100,
            dispose: None,
        },
        PhotoshopFrame {
            id: 2,
            duration_ms: 100,
            dispose: None,
        },
        PhotoshopFrame {
            id: 3,
            duration_ms: 100,
            dispose: None,
        },
    ];
    let states = resolve_layer_states(&layer, &raw, &frames).expect("states should resolve");
    assert!(states.frames[0].enabled);
    assert!(states.frames[0].record_present);
    assert!(!states.frames[1].enabled);
    assert!(!states.frames[1].record_present);
    assert!(states.frames[2].enabled);
    assert!(states.frames[2].record_present);
}

#[test]
fn first_missing_animation_record_starts_hidden() {
    let layer = AnimationLayerInput {
        id: 7,
        path: "0".to_string(),
        is_group: false,
        is_container_group: false,
        hidden: false,
        ancestor_ids: Vec::new(),
    };
    let raw = RawLayer {
        id: Some(7),
        shmd: Some(LayerMetadata {
            frames: vec![RawFrameState {
                frame_id: 2,
                enable: Some(true),
                offset: None,
                reference_point: None,
                opacity: None,
            }],
            flags: None,
        }),
        flags: None,
        is_bounding_divider: false,
    };
    let frames = vec![
        PhotoshopFrame {
            id: 1,
            duration_ms: 100,
            dispose: None,
        },
        PhotoshopFrame {
            id: 2,
            duration_ms: 100,
            dispose: None,
        },
    ];
    let states = resolve_layer_states(&layer, &raw, &frames).expect("states should resolve");
    assert!(!states.frames[0].enabled);
    assert!(!states.frames[0].record_present);
    assert!(states.frames[1].enabled);
    assert!(states.frames[1].record_present);
}

#[test]
fn present_record_without_enable_inherits_for_groups_and_pixels() {
    let resolve = |is_group, is_container_group| {
        let layer = AnimationLayerInput {
            id: 7,
            path: "0".to_string(),
            is_group,
            is_container_group,
            hidden: false,
            ancestor_ids: Vec::new(),
        };
        let raw = RawLayer {
            id: Some(7),
            shmd: Some(LayerMetadata {
                frames: vec![
                    RawFrameState {
                        frame_id: 1,
                        enable: Some(true),
                        offset: None,
                        reference_point: None,
                        opacity: None,
                    },
                    RawFrameState {
                        frame_id: 2,
                        enable: None,
                        offset: None,
                        reference_point: None,
                        opacity: None,
                    },
                ],
                flags: None,
            }),
            flags: None,
            is_bounding_divider: false,
        };
        let frames = vec![
            PhotoshopFrame {
                id: 1,
                duration_ms: 100,
                dispose: None,
            },
            PhotoshopFrame {
                id: 2,
                duration_ms: 100,
                dispose: None,
            },
        ];
        resolve_layer_states(&layer, &raw, &frames).expect("states should resolve")
    };

    for (is_group, is_container_group) in [(false, false), (true, false), (true, true)] {
        let states = resolve(is_group, is_container_group);
        assert!(states.frames[0].enabled);
        assert!(states.frames[1].enabled);
        assert!(states.frames[1].record_present);
        assert!(!states.frames[1].explicit_enable);
    }
}

#[test]
fn enable_inheritance_uses_catalog_order_not_last_record_storage_order() {
    let layer = AnimationLayerInput {
        id: 7,
        path: "0".to_string(),
        is_group: true,
        is_container_group: false,
        hidden: true,
        ancestor_ids: Vec::new(),
    };
    let raw = RawLayer {
        id: Some(7),
        shmd: Some(LayerMetadata {
            // Deliberately reverse physical `LaSt` order; the catalog order is 10, 20, 30.
            frames: vec![
                RawFrameState {
                    frame_id: 30,
                    enable: None,
                    offset: None,
                    reference_point: None,
                    opacity: None,
                },
                RawFrameState {
                    frame_id: 10,
                    enable: Some(true),
                    offset: None,
                    reference_point: None,
                    opacity: None,
                },
                RawFrameState {
                    frame_id: 20,
                    enable: None,
                    offset: None,
                    reference_point: None,
                    opacity: None,
                },
            ],
            flags: None,
        }),
        flags: None,
        is_bounding_divider: false,
    };
    let frames = [10, 20, 30]
        .into_iter()
        .map(|id| PhotoshopFrame {
            id,
            duration_ms: 100,
            dispose: None,
        })
        .collect::<Vec<_>>();

    let states = resolve_layer_states(&layer, &raw, &frames).expect("states should resolve");
    assert_eq!(
        states
            .frames
            .iter()
            .map(|state| state.enabled)
            .collect::<Vec<_>>(),
        vec![true, true, true]
    );
    assert!(states.frames.iter().all(|state| state.record_present));
    assert_eq!(
        states
            .frames
            .iter()
            .map(|state| state.explicit_enable)
            .collect::<Vec<_>>(),
        vec![true, false, false]
    );
}

#[test]
fn six_frame_group_enable_sequences_preserve_omitted_values() {
    let frames = (1..=6)
        .map(|id| PhotoshopFrame {
            id,
            duration_ms: 100,
            dispose: None,
        })
        .collect::<Vec<_>>();
    let state = |frame_id, enable| RawFrameState {
        frame_id,
        enable,
        offset: None,
        reference_point: None,
        opacity: None,
    };
    let resolve = |id, hidden, sequence: [Option<bool>; 6]| {
        let layer = AnimationLayerInput {
            id,
            path: id.to_string(),
            is_group: true,
            is_container_group: false,
            hidden,
            ancestor_ids: Vec::new(),
        };
        let raw = RawLayer {
            id: Some(id),
            shmd: Some(LayerMetadata {
                frames: sequence
                    .into_iter()
                    .enumerate()
                    .map(|(index, enable)| state(index as u32 + 1, enable))
                    .collect(),
                flags: None,
            }),
            flags: None,
            is_bounding_divider: false,
        };
        resolve_layer_states(&layer, &raw, &frames).expect("states should resolve")
    };

    let first = resolve(
        1,
        false,
        [Some(true), None, Some(true), Some(false), None, Some(false)],
    );
    let second = resolve(
        2,
        true,
        [Some(false), None, Some(false), Some(true), None, Some(true)],
    );
    assert_eq!(
        first
            .frames
            .iter()
            .map(|state| state.enabled)
            .collect::<Vec<_>>(),
        vec![true, true, true, false, false, false]
    );
    assert_eq!(
        second
            .frames
            .iter()
            .map(|state| state.enabled)
            .collect::<Vec<_>>(),
        vec![false, false, false, true, true, true]
    );
    for states in [&first, &second] {
        assert!(states.frames[1].record_present && !states.frames[1].explicit_enable);
        assert!(states.frames[4].record_present && !states.frames[4].explicit_enable);
        assert!(states.frames[3].explicit_enable);
    }
}

#[test]
fn first_present_record_without_enable_uses_static_visibility() {
    let frames = vec![PhotoshopFrame {
        id: 1,
        duration_ms: 100,
        dispose: None,
    }];
    for (id, hidden, expected) in [(1, false, true), (2, true, false)] {
        let layer = AnimationLayerInput {
            id,
            path: id.to_string(),
            is_group: true,
            is_container_group: false,
            hidden,
            ancestor_ids: Vec::new(),
        };
        let raw = RawLayer {
            id: Some(id),
            shmd: Some(LayerMetadata {
                frames: vec![RawFrameState {
                    frame_id: 1,
                    enable: None,
                    offset: None,
                    reference_point: None,
                    opacity: None,
                }],
                flags: None,
            }),
            flags: None,
            is_bounding_divider: false,
        };
        let states = resolve_layer_states(&layer, &raw, &frames).expect("states should resolve");
        assert_eq!(states.frames[0].enabled, expected);
        assert!(states.frames[0].record_present);
        assert!(!states.frames[0].explicit_enable);
    }
}

#[test]
fn inherited_parent_group_keeps_animated_child_visible() {
    let frames = [1, 2]
        .into_iter()
        .map(|id| PhotoshopFrame {
            id,
            duration_ms: 100,
            dispose: None,
        })
        .collect::<Vec<_>>();
    let group = AnimationLayerInput {
        id: 1,
        path: "parent".to_string(),
        is_group: true,
        is_container_group: false,
        hidden: false,
        ancestor_ids: Vec::new(),
    };
    let child = AnimationLayerInput {
        id: 2,
        path: "parent/child".to_string(),
        is_group: false,
        is_container_group: false,
        hidden: false,
        ancestor_ids: vec![1],
    };
    let raw = |id: u32, enables: [Option<bool>; 2]| RawLayer {
        id: Some(id),
        shmd: Some(LayerMetadata {
            frames: enables
                .into_iter()
                .enumerate()
                .map(|(index, enable)| RawFrameState {
                    frame_id: index as u32 + 1,
                    enable,
                    offset: None,
                    reference_point: None,
                    opacity: None,
                })
                .collect(),
            flags: None,
        }),
        flags: None,
        is_bounding_divider: false,
    };
    let group_states = resolve_layer_states(&group, &raw(1, [Some(true), None]), &frames)
        .expect("group states should resolve");
    let child_states = resolve_layer_states(&child, &raw(2, [Some(true), Some(true)]), &frames)
        .expect("child states should resolve");

    let visible = resolve_visible_layers(&[group, child], &[group_states, child_states], &frames);
    assert_eq!(visible[0].layer_ids, vec![2]);
    assert_eq!(visible[1].layer_ids, vec![2]);
}

#[test]
fn no_animation_psd_has_explicit_empty_result() {
    let mut bytes = vec![0; 38];
    bytes[0..4].copy_from_slice(b"8BPS");
    bytes[4..6].copy_from_slice(&1_u16.to_be_bytes());
    assert_eq!(
        parse_photoshop_animation(&bytes, &[]).expect("valid empty PSD"),
        None
    );
}

#[test]
fn layer_visibility_includes_ancestors() {
    let layers = vec![
        AnimationLayerInput {
            id: 1,
            path: "0".to_string(),
            is_group: true,
            is_container_group: false,
            hidden: true,
            ancestor_ids: Vec::new(),
        },
        AnimationLayerInput {
            id: 2,
            path: "0/0".to_string(),
            is_group: false,
            is_container_group: false,
            hidden: false,
            ancestor_ids: vec![1],
        },
    ];
    let frame = PhotoshopFrame {
        id: 7,
        duration_ms: 100,
        dispose: None,
    };
    let states = vec![
        LayerAnimationState {
            layer_id: 1,
            path: "0".to_string(),
            frames: vec![LayerFrameState {
                frame_id: 7,
                record_present: false,
                enabled: false,
                explicit_enable: false,
                offset: None,
                reference_point: None,
                opacity: None,
            }],
        },
        LayerAnimationState {
            layer_id: 2,
            path: "0/0".to_string(),
            frames: vec![LayerFrameState {
                frame_id: 7,
                record_present: false,
                enabled: true,
                explicit_enable: false,
                offset: None,
                reference_point: None,
                opacity: None,
            }],
        },
    ];
    assert!(
        resolve_visible_layers(&layers, &states, &[frame])[0]
            .layer_ids
            .is_empty()
    );
}

#[test]
fn hidden_animation_container_does_not_suppress_animated_descendant() {
    let layers = vec![
        AnimationLayerInput {
            id: 1,
            path: "0".to_string(),
            is_group: true,
            is_container_group: false,
            hidden: true,
            ancestor_ids: Vec::new(),
        },
        AnimationLayerInput {
            id: 2,
            path: "0/0".to_string(),
            is_group: false,
            is_container_group: false,
            hidden: true,
            ancestor_ids: vec![1],
        },
    ];
    let states = vec![
        LayerAnimationState {
            layer_id: 1,
            path: "0".to_string(),
            frames: vec![
                LayerFrameState {
                    frame_id: 7,
                    record_present: true,
                    enabled: false,
                    explicit_enable: true,
                    offset: None,
                    reference_point: None,
                    opacity: None,
                },
                LayerFrameState {
                    frame_id: 8,
                    record_present: true,
                    enabled: false,
                    explicit_enable: false,
                    offset: None,
                    reference_point: None,
                    opacity: None,
                },
            ],
        },
        LayerAnimationState {
            layer_id: 2,
            path: "0/0".to_string(),
            frames: vec![
                LayerFrameState {
                    frame_id: 7,
                    record_present: true,
                    enabled: true,
                    explicit_enable: true,
                    offset: None,
                    reference_point: None,
                    opacity: None,
                },
                LayerFrameState {
                    frame_id: 8,
                    record_present: true,
                    enabled: true,
                    explicit_enable: true,
                    offset: None,
                    reference_point: None,
                    opacity: None,
                },
            ],
        },
    ];
    let frames = vec![
        PhotoshopFrame {
            id: 7,
            duration_ms: 100,
            dispose: None,
        },
        PhotoshopFrame {
            id: 8,
            duration_ms: 100,
            dispose: None,
        },
    ];
    let visible = resolve_visible_layers(&layers, &states, &frames);
    assert_eq!(visible[0].layer_ids, vec![2]);
    assert_eq!(visible[1].layer_ids, vec![2]);
}
