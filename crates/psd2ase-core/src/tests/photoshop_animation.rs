use super::*;

use ag_psd::descriptor::{Descriptor, DescriptorValue, write_version_and_descriptor};
use ag_psd::writer::{create_writer, get_writer_buffer};

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
fn group_record_without_enable_is_not_selected() {
    let layer = AnimationLayerInput {
        id: 7,
        path: "0".to_string(),
        is_group: true,
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
    assert!(states.frames[0].enabled);
    assert!(!states.frames[1].enabled);
    assert!(states.frames[1].record_present);
    assert!(!states.frames[1].explicit_enable);
}

#[test]
fn container_group_record_without_enable_inherits_state() {
    let layer = AnimationLayerInput {
        id: 7,
        path: "0".to_string(),
        is_group: true,
        is_container_group: true,
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
    let states = resolve_layer_states(&layer, &raw, &frames).expect("states should resolve");
    assert!(states.frames[0].enabled);
    assert!(states.frames[1].enabled);
    assert!(states.frames[1].record_present);
    assert!(!states.frames[1].explicit_enable);
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
