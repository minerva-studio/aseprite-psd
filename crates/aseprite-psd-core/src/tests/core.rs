use super::*;

use std::fs;
use std::io::Cursor;
use std::path::Path;
use std::time::{SystemTime, UNIX_EPOCH};

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

/// Builds a small PSD fixture with a bitmap user mask and known alpha values.
fn bitmap_mask_fixture(clipping: bool, parameterized: bool) -> ag_psd::psd::Psd {
    let mut layer = ag_psd::psd::Layer::default();
    layer.additional_info.name = Some("Masked layer".to_string());
    layer.additional_info.id = Some(41.0);
    layer.top = Some(0.0);
    layer.left = Some(0.0);
    layer.bottom = Some(1.0);
    layer.right = Some(2.0);
    layer.clipping = Some(clipping);
    layer.image_data = Some(ag_psd::psd::PixelData {
        width: 2,
        height: 1,
        data: vec![10, 20, 30, 200, 40, 50, 60, 100],
    });
    layer.additional_info.mask = Some(ag_psd::psd::LayerMaskData {
        top: Some(0.0),
        left: Some(0.0),
        bottom: Some(1.0),
        right: Some(2.0),
        default_color: Some(0.0),
        image_data: Some(ag_psd::psd::PixelData {
            width: 2,
            height: 1,
            data: vec![64, 64, 64, 255, 128, 128, 128, 255],
        }),
        user_mask_density: parameterized.then_some(0.5),
        ..Default::default()
    });
    ag_psd::psd::Psd {
        width: 2.0,
        height: 1.0,
        channels: Some(4.0),
        bits_per_channel: Some(8.0),
        color_mode: Some(ag_psd::psd::ColorMode::Rgb),
        children: Some(vec![layer]),
        ..Default::default()
    }
}

/// Writes a PSD fixture through the real ag-psd serializer.
fn write_psd_fixture(path: &Path, psd: &ag_psd::psd::Psd) {
    fs::write(path, ag_psd::write_psd(psd, &Default::default())).expect("write PSD fixture");
}

/// Creates a unique temporary directory for a core conversion fixture.
fn fixture_directory(prefix: &str) -> std::path::PathBuf {
    let directory = std::env::temp_dir().join(format!(
        "{prefix}-{}",
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("clock")
            .as_nanos()
    ));
    fs::create_dir_all(&directory).expect("create fixture directory");
    directory
}

#[test]
fn bitmap_user_mask_rasterizes_alpha_and_reports_editability_loss() {
    let directory = fixture_directory("aseprite-psd-bitmap-mask");
    let input = directory.join("masked.psd");
    write_psd_fixture(&input, &bitmap_mask_fixture(false, false));

    let (normalized, report) = normalize_bytes(&fs::read(&input).expect("read masked PSD"))
        .expect("normalize bitmap mask fixture");
    let layer = &normalized.root_layers[0];
    assert_eq!(layer.name, "Masked layer");
    assert_eq!(
        layer.bounds,
        NormalizedBounds {
            left: 0,
            top: 0,
            right: 2,
            bottom: 1
        }
    );
    assert_eq!(
        layer.pixels.as_ref().expect("masked layer pixels").data,
        vec![10, 20, 30, 50, 40, 50, 60, 50]
    );
    let loss = report
        .entries
        .iter()
        .find(|entry| entry.code == InformationLossCode::PixelMask)
        .expect("bitmap mask loss report");
    assert_eq!(loss.disposition, LossDisposition::Rasterized);
    assert_eq!(loss.count, 1);
    assert_eq!(loss.locations[0].path, "0");
    assert_eq!(loss.locations[0].layer_id, Some(41));
    assert!(loss.editability_impact);

    fs::remove_dir_all(directory).expect("remove bitmap mask fixture");
}

#[test]
fn bitmap_user_mask_maps_pixels_in_document_coordinates() {
    let directory = fixture_directory("aseprite-psd-bitmap-mask-offset");
    let input = directory.join("masked.psd");
    let mut psd = bitmap_mask_fixture(false, false);
    let layer = &mut psd.children.as_mut().expect("fixture layer")[0];
    let mask = layer.additional_info.mask.as_mut().expect("fixture mask");
    mask.left = Some(1.0);
    mask.right = Some(2.0);
    mask.default_color = Some(255.0);
    mask.image_data = Some(ag_psd::psd::PixelData {
        width: 1,
        height: 1,
        data: vec![64, 64, 64, 255],
    });
    write_psd_fixture(&input, &psd);

    let (normalized, _) = normalize_bytes(&fs::read(&input).expect("read offset mask PSD"))
        .expect("normalize offset bitmap mask fixture");
    assert_eq!(
        normalized.root_layers[0]
            .pixels
            .as_ref()
            .expect("offset masked pixels")
            .data,
        vec![10, 20, 30, 200, 40, 50, 60, 25]
    );

    fs::remove_dir_all(directory).expect("remove offset bitmap mask fixture");
}

#[test]
fn bitmap_user_mask_survives_convert_as_masked_aseprite_pixels() {
    let directory = fixture_directory("aseprite-psd-bitmap-mask-convert");
    let input = directory.join("masked.psd");
    let output = directory.join("masked.aseprite");
    write_psd_fixture(&input, &bitmap_mask_fixture(false, false));

    let report = convert(
        &input,
        &output,
        &ConvertOptions {
            layer_association: LayerAssociation::Preserve,
            ..Default::default()
        },
    )
    .expect("convert bitmap mask fixture");
    let loss = report
        .information_loss
        .entries
        .iter()
        .find(|entry| entry.code == InformationLossCode::PixelMask)
        .expect("bitmap mask conversion report");
    assert_eq!(loss.disposition, LossDisposition::Rasterized);

    let bytes = fs::read(&output).expect("read converted Aseprite");
    let file =
        aseprite::AsepriteFile::from_reader(Cursor::new(bytes)).expect("parse converted Aseprite");
    assert_eq!(file.frames().len(), 1);
    assert_eq!(file.layers().len(), 1);
    assert_eq!(file.layers()[0].name, "Masked layer");
    let cel = file
        .cel(file.layer_ref(0).expect("converted pixel layer"), 0)
        .expect("converted masked cel");
    match &cel.kind {
        aseprite::CelKind::Raw { pixels, x, y }
        | aseprite::CelKind::Compressed { pixels, x, y, .. } => {
            assert_eq!((*x, *y), (0, 0));
            assert_eq!(pixels.data, vec![10, 20, 30, 50, 40, 50, 60, 50]);
        }
        kind => panic!("unexpected converted cel kind: {kind:?}"),
    }

    fs::remove_dir_all(directory).expect("remove bitmap mask conversion fixture");
}

#[test]
fn clipping_is_rejected_before_output_commit() {
    let directory = fixture_directory("aseprite-psd-clipping");
    let input = directory.join("clipping.psd");
    let output = directory.join("clipping.aseprite");
    write_psd_fixture(&input, &bitmap_mask_fixture(true, false));

    let error = convert(
        &input,
        &output,
        &ConvertOptions {
            overwrite: true,
            layer_association: LayerAssociation::Preserve,
            ..Default::default()
        },
    )
    .expect_err("clipping must be rejected");
    let message = error.to_string();
    assert!(message.contains("clipping is unsupported at layer path 0"));
    assert!(message.contains("layer id Some(41)"));
    assert!(!output.exists(), "rejected clipping must not commit output");

    fs::remove_dir_all(directory).expect("remove clipping fixture");
}

#[test]
fn parameterized_bitmap_mask_remains_explicitly_unsupported() {
    let directory = fixture_directory("aseprite-psd-parameterized-mask");
    let input = directory.join("parameterized.psd");
    write_psd_fixture(&input, &bitmap_mask_fixture(false, true));

    let (normalized, report) = normalize_bytes(&fs::read(&input).expect("read parameterized PSD"))
        .expect("normalize parameterized mask fixture");
    assert_eq!(
        normalized.root_layers[0]
            .pixels
            .as_ref()
            .expect("parameterized mask pixels")
            .data,
        vec![10, 20, 30, 200, 40, 50, 60, 100]
    );
    let loss = report
        .entries
        .iter()
        .find(|entry| entry.code == InformationLossCode::PixelMask)
        .expect("parameterized mask loss report");
    assert_eq!(loss.disposition, LossDisposition::Dropped);
    assert!(loss.detail.contains("not represented"));

    fs::remove_dir_all(directory).expect("remove parameterized mask fixture");
}

#[test]
fn vector_derived_mask_remains_explicitly_unsupported() {
    let directory = fixture_directory("aseprite-psd-vector-mask");
    let input = directory.join("vector-mask.psd");
    let mut psd = bitmap_mask_fixture(false, false);
    psd.children.as_mut().expect("fixture layer")[0]
        .additional_info
        .mask
        .as_mut()
        .expect("fixture mask")
        .from_vector_data = Some(true);
    write_psd_fixture(&input, &psd);

    let (normalized, report) = normalize_bytes(&fs::read(&input).expect("read vector mask PSD"))
        .expect("normalize vector mask fixture");
    let loss = report
        .entries
        .iter()
        .find(|entry| entry.code == InformationLossCode::PixelMask)
        .expect("vector mask loss report");
    assert_eq!(loss.disposition, LossDisposition::Dropped);
    assert_eq!(
        normalized.root_layers[0]
            .pixels
            .as_ref()
            .expect("vector mask pixels")
            .data,
        vec![10, 20, 30, 200, 40, 50, 60, 100]
    );

    fs::remove_dir_all(directory).expect("remove vector mask fixture");
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
