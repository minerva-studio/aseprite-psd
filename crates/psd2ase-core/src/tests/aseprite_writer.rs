use super::*;
use crate::{AnimationPoint, NormalizedBounds, NormalizedFrame, NormalizedLayerFrameState};

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
