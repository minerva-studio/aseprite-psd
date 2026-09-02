use super::*;

#[test]
fn missing_layer_ids_use_stable_name_independent_fallbacks() {
    let first = layer_id(None, "0/1").expect("fallback id");
    let repeated = layer_id(None, "0/1").expect("fallback id");
    let sibling = layer_id(None, "0/2").expect("fallback id");
    assert_eq!(first, repeated);
    assert_ne!(first, sibling);
}

#[test]
fn thirty_two_bit_documents_are_rejected_before_normalization() {
    let error = validate_normalization_bit_depth(Some(32.0))
        .expect_err("32-bit input must remain outside the normalized contract");
    assert!(
        error
            .to_string()
            .contains("32-bit PSD input is not supported")
    );
    assert!(validate_normalization_bit_depth(Some(8.0)).is_ok());
    assert!(validate_normalization_bit_depth(Some(16.0)).is_ok());
}

#[test]
fn non_empty_layers_without_pixels_are_rejected() {
    let layer = ag_psd::psd::Layer {
        top: Some(0.0),
        left: Some(0.0),
        bottom: Some(2.0),
        right: Some(3.0),
        ..Default::default()
    };
    let error = build_layer(&layer, &["0".to_string()])
        .expect_err("non-empty layers must not become transparent placeholders");
    assert!(
        error
            .to_string()
            .contains("non-empty pixel layer has no RGBA8 data at 0")
    );
}

#[test]
fn zero_area_layers_without_pixels_remain_empty() {
    let layer = ag_psd::psd::Layer {
        top: Some(4.0),
        left: Some(3.0),
        bottom: Some(4.0),
        right: Some(3.0),
        ..Default::default()
    };
    let normalized = build_layer(&layer, &["0".to_string()]).expect("empty layer");
    let pixels = normalized.pixels.expect("empty pixel buffer");
    assert_eq!((pixels.width, pixels.height), (0, 0));
    assert!(pixels.data.is_empty());
}

fn state(frame_index: u32, enabled: bool) -> NormalizedLayerFrameState {
    NormalizedLayerFrameState {
        frame_index,
        record_present: false,
        enabled,
        explicit_enable: false,
        offset: None,
        reference_point: None,
        opacity: None,
    }
}

fn layer(
    id: u32,
    kind: NormalizedLayerKind,
    hidden: Option<bool>,
    children: Vec<NormalizedLayer>,
    frame_states: Vec<NormalizedLayerFrameState>,
) -> NormalizedLayer {
    NormalizedLayer {
        id,
        name: String::new(),
        kind,
        bounds: NormalizedBounds {
            left: 0,
            top: 0,
            right: 1,
            bottom: 1,
        },
        opacity: None,
        blend_mode: None,
        hidden,
        pixels: None,
        children,
        frame_states,
    }
}

#[test]
fn recursive_visibility_applies_ancestor_state_without_storing_a_list() {
    let child = layer(
        2,
        NormalizedLayerKind::Pixel,
        None,
        Vec::new(),
        vec![state(0, true)],
    );
    let group = layer(
        1,
        NormalizedLayerKind::Group,
        Some(true),
        vec![child],
        vec![state(0, false)],
    );
    let mut visible = Vec::new();
    group.collect_visible_pixel_layer_ids(0, true, &mut visible);
    assert!(visible.is_empty());
    assert!(!group.is_effectively_visible(0, true));
}

#[test]
fn static_frame_has_no_serialization_duration() {
    let frame = NormalizedFrame {
        index: 0,
        source_id: None,
        duration_ms: None,
        dispose: None,
    };
    assert_eq!(frame.source_id, None);
    assert_eq!(frame.duration_ms, None);
}

#[test]
fn top_level_frame_source_keeps_background_shared() {
    let mut background = layer(
        1,
        NormalizedLayerKind::Pixel,
        Some(true),
        Vec::new(),
        vec![state(0, true)],
    );
    background.name = "Background".to_string();
    let mut first = layer(
        2,
        NormalizedLayerKind::Pixel,
        Some(true),
        Vec::new(),
        vec![state(0, true)],
    );
    first.name = "First".to_string();
    let mut hidden_child = layer(
        4,
        NormalizedLayerKind::Pixel,
        Some(true),
        Vec::new(),
        vec![state(0, true)],
    );
    hidden_child.name = "Hidden child".to_string();
    let mut second = layer(
        3,
        NormalizedLayerKind::Group,
        Some(true),
        vec![hidden_child],
        vec![state(0, true)],
    );
    second.name = "Second".to_string();
    let mut document = NormalizedDocument {
        root_layers: vec![background, first, second],
        frames: vec![NormalizedFrame {
            index: 0,
            source_id: None,
            duration_ms: None,
            dispose: None,
        }],
        ..Default::default()
    };

    let warnings = apply_frame_source(&mut document, FrameSource::TopLevel)
        .expect("top-level frames should be constructed");

    assert_eq!(document.frames.len(), 2);
    assert_eq!(
        document.root_layers[0]
            .frame_states
            .iter()
            .map(|state| state.enabled)
            .collect::<Vec<_>>(),
        vec![false, false]
    );
    assert_eq!(
        document.root_layers[1]
            .frame_states
            .iter()
            .map(|state| state.enabled)
            .collect::<Vec<_>>(),
        vec![true, false]
    );
    assert_eq!(
        document.root_layers[2]
            .frame_states
            .iter()
            .map(|state| state.enabled)
            .collect::<Vec<_>>(),
        vec![false, true]
    );
    assert_eq!(
        document.root_layers[2].children[0]
            .frame_states
            .iter()
            .map(|state| state.enabled)
            .collect::<Vec<_>>(),
        vec![false, false]
    );
    assert!(warnings[0].contains("2 top-level frames"));
    assert!(warnings[0].contains("Background"));
}

#[test]
fn automatic_frame_source_does_not_infer_top_level_animation() {
    let mut document = NormalizedDocument {
        root_layers: vec![
            layer(
                1,
                NormalizedLayerKind::Pixel,
                None,
                Vec::new(),
                vec![state(0, true)],
            ),
            layer(
                2,
                NormalizedLayerKind::Group,
                None,
                Vec::new(),
                vec![state(0, true)],
            ),
        ],
        frames: vec![NormalizedFrame {
            index: 0,
            source_id: None,
            duration_ms: None,
            dispose: None,
        }],
        ..Default::default()
    };

    apply_frame_source(&mut document, FrameSource::Auto)
        .expect("auto should preserve static input");

    assert_eq!(document.frames.len(), 1);
    assert!(
        document
            .root_layers
            .iter()
            .all(|layer| layer.frame_states[0].enabled)
    );
}

#[test]
fn top_level_frame_source_rejects_photoshop_timeline() {
    let mut document = NormalizedDocument {
        animation_resource_ids: vec![4000],
        ..Default::default()
    };
    let error = apply_frame_source(&mut document, FrameSource::TopLevel)
        .expect_err("timeline input must not be reinterpreted");
    assert!(error.contains("cannot replace a Photoshop timeline"));
}

#[test]
fn photoshop_frame_duration_validation_uses_ten_millisecond_quantization() {
    assert_eq!(canonical_frame_duration_ms(83), 80);
    assert_eq!(canonical_frame_duration_ms(65_535), 65_530);
    assert_eq!(canonical_frame_duration_ms(84), 80);
    assert_ne!(
        canonical_frame_duration_ms(83),
        canonical_frame_duration_ms(90)
    );
}

#[test]
fn roundtrip_preset_falls_back_to_auto_for_unmarked_documents() {
    let (exact, association) = resolve_roundtrip_association(roundtrip::RoundTripLayout {
        status: roundtrip::RoundTripStatus {
            marked: false,
            valid: true,
        },
        version: None,
        frame_count: None,
    })
    .expect("unmarked documents should use automatic association");
    assert!(!exact);
    assert!(matches!(association, LayerAssociation::Auto(_)));
}

#[test]
fn roundtrip_preset_keeps_invalid_markers_on_recovery_path() {
    let error = resolve_roundtrip_association(roundtrip::RoundTripLayout {
        status: roundtrip::RoundTripStatus {
            marked: true,
            valid: false,
        },
        version: Some(2),
        frame_count: Some(2),
    })
    .expect_err("invalid markers must require recovery");
    assert!(matches!(
        error,
        ConversionError::RoundTripRecoveryRequired(_)
    ));
}

#[test]
fn pixel_data_is_owned_and_keeps_origin() {
    let source = ag_psd::psd::PixelData {
        width: 1,
        height: 1,
        data: vec![1, 2, 3, 4],
    };
    let normalized = copy_rgba8_pixels(
        &source,
        NormalizedBounds {
            left: -4,
            top: 7,
            right: -3,
            bottom: 8,
        },
        "test",
    )
    .expect("valid RGBA8 data");
    assert_eq!(normalized.data, vec![1, 2, 3, 4]);
    assert_eq!((normalized.left, normalized.top), (-4, 7));
}

#[test]
fn malformed_pixel_length_is_rejected() {
    let source = ag_psd::psd::PixelData {
        width: 1,
        height: 1,
        data: vec![1, 2, 3],
    };
    let error = copy_rgba8_pixels(
        &source,
        NormalizedBounds {
            left: 0,
            top: 0,
            right: 1,
            bottom: 1,
        },
        "test",
    )
    .expect_err("short pixel data must fail");
    assert!(error.to_string().contains("pixel buffer length mismatch"));
}

#[test]
fn non_integral_and_out_of_range_bounds_are_rejected() {
    assert!(integral_i32(Some(1.5), "left").is_err());
    assert!(integral_i32(Some(i32::MAX as f64 + 1.0), "right").is_err());
    assert!(integral_i32(Some(i32::MIN as f64 - 1.0), "top").is_err());
}
