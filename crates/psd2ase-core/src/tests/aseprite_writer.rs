use super::*;
use crate::{
    AnimationPoint, JitterPlan, NormalizedBounds, NormalizedFrame, NormalizedLayerFrameState,
};

fn pixel_document(width: u32, height: u32, left: i32, top: i32) -> NormalizedDocument {
    NormalizedDocument {
        canvas: (width, height),
        frames: vec![NormalizedFrame {
            index: 0,
            source_id: None,
            duration_ms: None,
            dispose: None,
        }],
        root_layers: vec![NormalizedLayer {
            id: 1,
            name: "pixel".to_string(),
            kind: NormalizedLayerKind::Pixel,
            bounds: NormalizedBounds {
                left,
                top,
                right: left + 1,
                bottom: top + 1,
            },
            opacity: None,
            blend_mode: Some("normal".to_string()),
            hidden: Some(false),
            pixels: Some(NormalizedPixels {
                width: 1,
                height: 1,
                left,
                top,
                data: vec![1, 2, 3, 4],
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
        }],
        ..NormalizedDocument::default()
    }
}

#[test]
fn encodes_static_frame_with_serialization_default() {
    let encoded = encode(&pixel_document(8, 8, -2, 3)).expect("valid normalized document");
    let file = AsepriteFile::from_reader(&encoded.bytes[..]).expect("valid Aseprite bytes");
    assert_eq!(file.frames()[0].duration_ms, DEFAULT_FRAME_DURATION_MS);
    let layer = file.layer_ref(0).expect("pixel layer");
    let cel = file.cel(layer, 0).expect("visible pixel cel");
    match &cel.kind {
        aseprite::CelKind::Raw { x, y, pixels } => {
            assert_eq!((*x, *y), (-2, 3));
            assert_eq!(pixels.data, vec![1, 2, 3, 4]);
        }
        _ => panic!("expected compressed pixel cel"),
    }
}

#[test]
fn resolved_jitter_pixels_are_written_and_valid_for_linking() {
    let document = pixel_document(8, 8, 0, 0);
    let mut jitter = JitterPlan::default();
    jitter.repaired_pixels.insert(1, vec![9, 8, 7, 6]);
    let encoded = encode_with_linked_cels_and_jitter(&document, crate::LinkedCelMode::Off, &jitter)
        .expect("resolved jitter pixels should encode");
    let file = AsepriteFile::from_reader(&encoded.bytes[..]).expect("valid Aseprite bytes");
    let layer = file.layer_ref(0).expect("pixel layer");
    let cel = file.cel(layer, 0).expect("visible pixel cel");
    let pixels = match &cel.kind {
        aseprite::CelKind::Raw { pixels, .. } | aseprite::CelKind::Compressed { pixels, .. } => {
            pixels
        }
        _ => panic!("expected a pixel cel"),
    };
    assert_eq!(pixels.data, vec![9, 8, 7, 6]);
}

#[test]
fn rejects_coordinates_outside_aseprite_cel_range() {
    let error = encode(&pixel_document(8, 8, i32::from(i16::MAX) + 1, 0))
        .expect_err("out-of-range cel coordinate must fail");
    assert!(matches!(error, WriterError::FormatLimit { .. }));
}

#[test]
fn rejects_non_contiguous_normalized_frame_indices() {
    let mut document = pixel_document(8, 8, 0, 0);
    document.frames[0].index = 1;
    let error = encode(&document).expect_err("frame indices must be contiguous");
    assert!(matches!(error, WriterError::InvalidFrameIndex { .. }));
}

#[test]
fn does_not_reuse_cels_between_frames() {
    let mut document = pixel_document(8, 8, 0, 0);
    document.frames.push(NormalizedFrame {
        index: 1,
        source_id: Some(2),
        duration_ms: Some(100),
        dispose: None,
    });
    document.frames[0].source_id = Some(1);
    document.root_layers[0]
        .frame_states
        .push(NormalizedLayerFrameState {
            frame_index: 1,
            record_present: true,
            enabled: false,
            explicit_enable: true,
            offset: None,
            reference_point: None,
            opacity: None,
        });

    let mut second = document.root_layers[0].clone();
    second.id = 2;
    second.name = "second".to_string();
    second.frame_states[0].enabled = false;
    second.frame_states[1].enabled = true;
    document.root_layers.push(second);

    let encoded = encode(&document).expect("valid normalized document");
    let file = AsepriteFile::from_reader(&encoded.bytes[..]).expect("valid Aseprite bytes");
    let first = file.layer_ref(0).expect("first pixel layer");
    let second = file.layer_ref(1).expect("second pixel layer");
    assert!(file.cel(first, 0).is_some());
    assert!(file.cel(first, 1).is_none());
    assert!(file.cel(second, 0).is_none());
    assert!(file.cel(second, 1).is_some());
}

#[test]
fn links_identical_pixels_on_one_layer_and_reports_reuse() {
    let mut document = pixel_document(8, 8, 0, 0);
    document.frames.push(NormalizedFrame {
        index: 1,
        source_id: Some(2),
        duration_ms: Some(120),
        dispose: None,
    });
    document.frames.push(NormalizedFrame {
        index: 2,
        source_id: Some(3),
        duration_ms: Some(120),
        dispose: None,
    });
    document.root_layers[0]
        .frame_states
        .push(NormalizedLayerFrameState {
            frame_index: 1,
            record_present: true,
            enabled: true,
            explicit_enable: true,
            offset: None,
            reference_point: None,
            opacity: None,
        });
    document.root_layers[0]
        .frame_states
        .push(NormalizedLayerFrameState {
            frame_index: 2,
            record_present: true,
            enabled: true,
            explicit_enable: true,
            offset: None,
            reference_point: None,
            opacity: None,
        });

    let encoded = encode_with_linked_cels(&document, crate::LinkedCelMode::Identical)
        .expect("identical pixels should encode");
    assert_eq!(encoded.cel_reuse.pixel_cel_count, 1);
    assert_eq!(encoded.cel_reuse.linked_cel_count, 2);
    let file = AsepriteFile::from_reader(&encoded.bytes[..]).expect("valid Aseprite bytes");
    let layer = file.layer_ref(0).expect("pixel layer");
    assert!(matches!(
        &file.cel(layer, 0).unwrap().kind,
        aseprite::CelKind::Raw { .. }
    ));
    assert!(matches!(
        &file.cel(layer, 1).unwrap().kind,
        aseprite::CelKind::Linked {
            source_frame: 0,
            ..
        }
    ));
    assert!(matches!(
        &file.cel(layer, 2).unwrap().kind,
        aseprite::CelKind::Linked {
            source_frame: 0,
            ..
        }
    ));
    assert_eq!(
        &file.resolve_cel(layer, 1).unwrap().kind,
        &file.cel(layer, 0).unwrap().kind
    );
}

#[test]
fn linked_cel_keeps_frame_specific_attributes() {
    let mut document = pixel_document(8, 8, 0, 0);
    document.frames.push(NormalizedFrame {
        index: 1,
        source_id: Some(2),
        duration_ms: Some(120),
        dispose: None,
    });
    document.root_layers[0]
        .frame_states
        .push(NormalizedLayerFrameState {
            frame_index: 1,
            record_present: true,
            enabled: true,
            explicit_enable: true,
            offset: Some(AnimationPoint { x: 4.0, y: 3.0 }),
            reference_point: None,
            opacity: Some(0.5),
        });

    let encoded = encode_with_linked_cels(&document, crate::LinkedCelMode::Identical)
        .expect("identical pixels should encode");
    let file = AsepriteFile::from_reader(&encoded.bytes[..]).expect("valid Aseprite bytes");
    let layer = file.layer_ref(0).expect("pixel layer");
    let cel = file.cel(layer, 1).expect("linked cel");
    assert_eq!(cel.opacity, 128);
    assert!(matches!(
        &cel.kind,
        aseprite::CelKind::Linked { x: 4, y: 3, .. }
    ));
}

#[test]
fn off_mode_keeps_identical_pixels_independent() {
    let mut document = pixel_document(8, 8, 0, 0);
    document.frames.push(NormalizedFrame {
        index: 1,
        source_id: Some(2),
        duration_ms: Some(120),
        dispose: None,
    });
    document.root_layers[0]
        .frame_states
        .push(NormalizedLayerFrameState {
            frame_index: 1,
            record_present: true,
            enabled: true,
            explicit_enable: true,
            offset: None,
            reference_point: None,
            opacity: None,
        });

    let encoded = encode(&document).expect("default mode should encode");
    assert_eq!(encoded.cel_reuse.pixel_cel_count, 2);
    assert_eq!(encoded.cel_reuse.linked_cel_count, 0);
}

#[test]
fn different_pixels_are_not_linked() {
    let mut file = AsepriteFile::new(8, 8, ColorMode::Rgba);
    let layer = file.add_layer("pixel");
    file.add_frame(100);
    file.add_frame(100);
    let mut reuse = CelReuseTracker::new(crate::LinkedCelMode::Identical);
    let first = Pixels::new(vec![1, 2, 3, 4], 1, 1, ColorMode::Rgba).unwrap();
    let second = Pixels::new(vec![9, 2, 3, 4], 1, 1, ColorMode::Rgba).unwrap();
    emit_cel(
        &mut file,
        layer,
        0,
        PreparedCel {
            pixels: first,
            x: 0,
            y: 0,
            opacity: 255,
            z_index: 0,
        },
        &mut reuse,
    )
    .unwrap();
    emit_cel(
        &mut file,
        layer,
        1,
        PreparedCel {
            pixels: second,
            x: 0,
            y: 0,
            opacity: 255,
            z_index: 0,
        },
        &mut reuse,
    )
    .unwrap();
    assert_eq!(reuse.report.pixel_cel_count, 2);
    assert_eq!(reuse.report.linked_cel_count, 0);
}

#[test]
fn applies_frame_offset_to_cel_origin() {
    let mut document = pixel_document(8, 8, 14, 51);
    document.root_layers[0].frame_states[0].offset = Some(AnimationPoint { x: 6.0, y: 2.0 });
    let encoded = encode(&document).expect("valid normalized document");
    let file = AsepriteFile::from_reader(&encoded.bytes[..]).expect("valid Aseprite bytes");
    let layer = file.layer_ref(0).expect("pixel layer");
    let cel = file.cel(layer, 0).expect("visible pixel cel");
    match &cel.kind {
        aseprite::CelKind::Raw { x, y, .. } => assert_eq!((*x, *y), (20, 53)),
        _ => panic!("expected raw pixel cel"),
    }
}

#[test]
fn rejects_non_integral_frame_offset() {
    let mut document = pixel_document(8, 8, 0, 0);
    document.root_layers[0].frame_states[0].offset = Some(AnimationPoint { x: 0.5, y: 0.0 });
    let error = encode(&document).expect_err("non-integral frame offset must fail");
    assert!(matches!(error, WriterError::InvalidCoordinate { .. }));
}

#[test]
fn reports_unknown_blend_mode_instead_of_silently_accepting_it() {
    let mut document = pixel_document(8, 8, 0, 0);
    document.root_layers[0].blend_mode = Some("pass through".to_string());
    let encoded = encode(&document).expect("unknown blend mode has a safe fallback");
    assert!(
        encoded
            .warnings
            .iter()
            .any(|warning| warning.contains("mapped to normal"))
    );
}

#[test]
fn converts_normalized_opacity_to_aseprite_scale() {
    assert_eq!(opacity_to_u8(None, "layer"), Ok(255));
    assert_eq!(opacity_to_u8(Some(0.0), "layer"), Ok(0));
    assert_eq!(opacity_to_u8(Some(1.0), "layer"), Ok(255));
    assert_eq!(opacity_to_u8(Some(0.5), "layer"), Ok(128));
    assert!(opacity_to_u8(Some(255.0), "layer").is_err());
}
