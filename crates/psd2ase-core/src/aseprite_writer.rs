use std::collections::HashSet;

use aseprite::{
    AsepriteFile, BlendMode, CelOptions, ColorMode, GroupRef, LayerOptions, LayerRef,
    LoopDirection, Pixels,
};

use crate::{
    LayerWritePlan, LogicalLayerTrack, NormalizedDocument, NormalizedLayer,
    NormalizedLayerFrameState, NormalizedLayerKind, NormalizedLoopMode, NormalizedPixels,
    PlannedNode,
};

/// Serialization default used only when a static normalized frame has no source duration.
pub const DEFAULT_FRAME_DURATION_MS: u16 = 100;

/// Bytes and warnings produced by the Aseprite serializer.
#[derive(Debug, PartialEq, Eq)]
pub struct EncodedAseprite {
    /// Complete Aseprite file bytes.
    pub bytes: Vec<u8>,
    /// Non-fatal compatibility warnings collected during mapping.
    pub warnings: Vec<String>,
}

/// Errors raised while mapping the normalized model to Aseprite.
#[derive(Debug, PartialEq, Eq)]
pub enum WriterError {
    /// A normalized frame index is not the expected contiguous playback index.
    InvalidFrameIndex { expected: usize, actual: u32 },
    /// A normalized integer cannot be represented by the Aseprite format.
    FormatLimit { field: String, value: i64, max: i64 },
    /// A normalized opacity is outside the supported 0.0..=1.0 range.
    InvalidOpacity { field: String, value: String },
    /// A normalized pixel buffer is inconsistent with its dimensions.
    InvalidPixels { layer_id: u32, message: String },
    /// A frame-local coordinate is not an integral value representable by the model.
    InvalidCoordinate { field: String, value: String },
    /// The underlying Aseprite library rejected a write operation.
    Aseprite(String),
}

impl std::fmt::Display for WriterError {
    /// Formats a writer error without exposing the third-party error type.
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::InvalidFrameIndex { expected, actual } => {
                write!(
                    formatter,
                    "invalid normalized frame index: expected {expected}, got {actual}"
                )
            }
            Self::FormatLimit { field, value, max } => {
                write!(formatter, "{field} is out of range: {value} > {max}")
            }
            Self::InvalidOpacity { field, value } => {
                write!(formatter, "invalid opacity for {field}: {value}")
            }
            Self::InvalidPixels { layer_id, message } => {
                write!(formatter, "invalid pixels for layer {layer_id}: {message}")
            }
            Self::InvalidCoordinate { field, value } => {
                write!(formatter, "invalid coordinate for {field}: {value}")
            }
            Self::Aseprite(error) => write!(formatter, "Aseprite writer failed: {error}"),
        }
    }
}

impl std::error::Error for WriterError {}

/// Encodes one normalized document as an RGBA Aseprite file.
pub fn encode(document: &NormalizedDocument) -> Result<EncodedAseprite, WriterError> {
    let width = u16_value("canvas width", document.canvas.0)?;
    let height = u16_value("canvas height", document.canvas.1)?;
    let mut file = AsepriteFile::new(width, height, ColorMode::Rgba);
    let mut warnings = Vec::new();
    collect_unmapped_animation_warnings(document, &mut warnings);

    if document.frames.is_empty() {
        return Err(WriterError::Aseprite(
            "normalized document has no frames".to_string(),
        ));
    }
    for (expected_index, frame) in document.frames.iter().enumerate() {
        if frame.index != expected_index as u32 {
            return Err(WriterError::InvalidFrameIndex {
                expected: expected_index,
                actual: frame.index,
            });
        }
        let duration = frame
            .duration_ms
            .unwrap_or(u32::from(DEFAULT_FRAME_DURATION_MS));
        file.add_frame(u16_value("frame duration", duration)?);
    }
    if let Some(loop_mode) = &document.loop_mode {
        let repeat = match loop_mode {
            NormalizedLoopMode::Infinite => 0,
            NormalizedLoopMode::Finite(count) => u16_value("loop repeat count", *count)?,
        };
        file.add_tag_with(
            "PSD Animation",
            0..=(document.frames.len() - 1),
            LoopDirection::Forward,
            repeat,
        )
        .map_err(|error| WriterError::Aseprite(error.to_string()))?;
    }

    let mut bindings = Vec::new();
    for layer in &document.root_layers {
        create_layer_tree(&mut file, layer, None, &mut bindings, &mut warnings)?;
    }

    for frame in &document.frames {
        let frame_index = frame.index as usize;
        let mut visible_ids = Vec::new();
        for layer in &document.root_layers {
            layer.collect_visible_pixel_layer_ids(frame_index, true, &mut visible_ids);
        }
        let visible_ids = visible_ids.into_iter().collect::<HashSet<_>>();
        for binding in &bindings {
            if !visible_ids.contains(&binding.layer.id) {
                continue;
            }
            let state = frame_state(binding.layer, frame_index)?;
            let pixels =
                binding
                    .layer
                    .pixels
                    .as_ref()
                    .ok_or_else(|| WriterError::InvalidPixels {
                        layer_id: binding.layer.id,
                        message: "pixel layer has no owned data".to_string(),
                    })?;
            let position = cel_position(pixels, state)?;
            let ase_pixels = aseprite_pixels(binding.layer.id, pixels)?;
            let opacity = normalized_opacity(
                state.opacity.or(binding.layer.opacity),
                format!("layer {} frame {frame_index}", binding.layer.id),
                &mut warnings,
            )?;
            if opacity == 255 {
                file.set_raw_cel(
                    binding.handle,
                    frame_index,
                    ase_pixels,
                    position.0,
                    position.1,
                )
                .map_err(|error| WriterError::Aseprite(error.to_string()))?;
            } else {
                file.set_cel_with(
                    binding.handle,
                    frame_index,
                    CelOptions {
                        pixels: ase_pixels,
                        x: position.0,
                        y: position.1,
                        opacity,
                        z_index: 0,
                    },
                )
                .map_err(|error| WriterError::Aseprite(error.to_string()))?;
            }
        }
    }

    let mut bytes = Vec::new();
    file.write_to(&mut bytes)
        .map_err(|error| WriterError::Aseprite(error.to_string()))?;
    Ok(EncodedAseprite { bytes, warnings })
}

/// Encodes a normalized document using an experimental logical-layer plan.
pub fn encode_with_plan(
    document: &NormalizedDocument,
    plan: &LayerWritePlan,
) -> Result<EncodedAseprite, WriterError> {
    let width = u16_value("canvas width", document.canvas.0)?;
    let height = u16_value("canvas height", document.canvas.1)?;
    let mut file = AsepriteFile::new(width, height, ColorMode::Rgba);
    let mut warnings = Vec::new();
    collect_unmapped_animation_warnings(document, &mut warnings);

    if document.frames.is_empty() {
        return Err(WriterError::Aseprite(
            "normalized document has no frames".to_string(),
        ));
    }
    for (expected_index, frame) in document.frames.iter().enumerate() {
        if frame.index != expected_index as u32 {
            return Err(WriterError::InvalidFrameIndex {
                expected: expected_index,
                actual: frame.index,
            });
        }
        let duration = frame
            .duration_ms
            .unwrap_or(u32::from(DEFAULT_FRAME_DURATION_MS));
        file.add_frame(u16_value("frame duration", duration)?);
    }
    if let Some(loop_mode) = &document.loop_mode {
        let repeat = match loop_mode {
            NormalizedLoopMode::Infinite => 0,
            NormalizedLoopMode::Finite(count) => u16_value("loop repeat count", *count)?,
        };
        file.add_tag_with(
            "PSD Animation",
            0..=(document.frames.len() - 1),
            LoopDirection::Forward,
            repeat,
        )
        .map_err(|error| WriterError::Aseprite(error.to_string()))?;
    }

    let mut bindings = Vec::new();
    for node in &plan.root_nodes {
        create_planned_tree(
            &mut file,
            node,
            None,
            document,
            plan,
            &mut bindings,
            &mut warnings,
        )?;
    }
    for frame_index in 0..document.frames.len() {
        for binding in &bindings {
            let Some(planned_cel) = binding.track.cels[frame_index] else {
                continue;
            };
            if planned_cel.source_frame_index as usize != frame_index {
                return Err(WriterError::InvalidFrameIndex {
                    expected: frame_index,
                    actual: planned_cel.source_frame_index,
                });
            }
            let source = find_layer(document, planned_cel.source_layer_id).ok_or_else(|| {
                WriterError::InvalidPixels {
                    layer_id: planned_cel.source_layer_id,
                    message: "planned source layer was not found".to_string(),
                }
            })?;
            let state = frame_state(source, frame_index)?;
            let pixels = source
                .pixels
                .as_ref()
                .ok_or_else(|| WriterError::InvalidPixels {
                    layer_id: source.id,
                    message: "pixel layer has no owned data".to_string(),
                })?;
            let position = cel_position(pixels, state)?;
            let ase_pixels = aseprite_pixels(source.id, pixels)?;
            let opacity = normalized_opacity(
                state.opacity.or(source.opacity),
                format!("layer {} frame {frame_index}", source.id),
                &mut warnings,
            )?;
            if opacity == 255 && planned_cel.z_index == 0 {
                file.set_raw_cel(
                    binding.handle,
                    frame_index,
                    ase_pixels,
                    position.0,
                    position.1,
                )
                .map_err(|error| WriterError::Aseprite(error.to_string()))?;
            } else {
                file.set_cel_with(
                    binding.handle,
                    frame_index,
                    CelOptions {
                        pixels: ase_pixels,
                        x: position.0,
                        y: position.1,
                        opacity,
                        z_index: planned_cel.z_index,
                    },
                )
                .map_err(|error| WriterError::Aseprite(error.to_string()))?;
            }
        }
    }

    let mut bytes = Vec::new();
    file.write_to(&mut bytes)
        .map_err(|error| WriterError::Aseprite(error.to_string()))?;
    Ok(EncodedAseprite { bytes, warnings })
}

/// Associates every normalized pixel layer with its newly-created Aseprite layer.
fn create_layer_tree<'a>(
    file: &mut AsepriteFile,
    layer: &'a NormalizedLayer,
    parent: Option<GroupRef>,
    bindings: &mut Vec<PixelBinding<'a>>,
    warnings: &mut Vec<String>,
) -> Result<(), WriterError> {
    let options = layer_options(layer, warnings)?;
    match layer.kind {
        NormalizedLayerKind::Group => {
            let group = match parent {
                Some(parent) => file.add_group_in_with(&layer.name, parent, options),
                None => file.add_group_with(&layer.name, options),
            };
            for child in &layer.children {
                create_layer_tree(file, child, Some(group), bindings, warnings)?;
            }
        }
        NormalizedLayerKind::Pixel => {
            let handle = match parent {
                Some(parent) => file.add_layer_in_with(&layer.name, parent, options),
                None => file.add_layer_with(&layer.name, options),
            };
            bindings.push(PixelBinding { layer, handle });
            if !layer.children.is_empty() {
                warnings.push(format!(
                    "pixel layer {} has children; children were not serialized",
                    layer.id
                ));
            }
        }
    }
    Ok(())
}

/// Stores a normalized pixel layer and its Aseprite handle for cel creation.
struct PixelBinding<'a> {
    layer: &'a NormalizedLayer,
    handle: LayerRef,
}

/// Associates one planned logical track with its output Aseprite layer.
struct PlannedBinding<'a> {
    track: &'a LogicalLayerTrack,
    handle: LayerRef,
}

/// Creates the output tree described by a logical-layer plan.
fn create_planned_tree<'a>(
    file: &mut AsepriteFile,
    node: &'a PlannedNode,
    parent: Option<GroupRef>,
    document: &NormalizedDocument,
    plan: &'a LayerWritePlan,
    bindings: &mut Vec<PlannedBinding<'a>>,
    warnings: &mut Vec<String>,
) -> Result<(), WriterError> {
    match node {
        PlannedNode::Group {
            name,
            source_layer_id,
            children,
        } => {
            let source = source_layer_id.and_then(|id| find_layer(document, id));
            let mut options = source
                .map(|layer| layer_options(layer, warnings))
                .transpose()?
                .unwrap_or_default();
            options.visible = true;
            let group = match parent {
                Some(parent) => file.add_group_in_with(name, parent, options),
                None => file.add_group_with(name, options),
            };
            for child in children {
                create_planned_tree(file, child, Some(group), document, plan, bindings, warnings)?;
            }
        }
        PlannedNode::Track { track_id } => {
            let track = plan.tracks.get(*track_id).ok_or_else(|| {
                WriterError::Aseprite(format!("logical track {track_id} is not present in plan"))
            })?;
            let source =
                find_layer(document, track.representative_source_layer_id).ok_or_else(|| {
                    WriterError::InvalidPixels {
                        layer_id: track.representative_source_layer_id,
                        message: "logical track representative layer was not found".to_string(),
                    }
                })?;
            let mut options = layer_options(source, warnings)?;
            options.visible = track.cels.iter().any(Option::is_some);
            let handle = match parent {
                Some(parent) => file.add_layer_in_with(&track.name, parent, options),
                None => file.add_layer_with(&track.name, options),
            };
            bindings.push(PlannedBinding { track, handle });
        }
    }
    Ok(())
}

/// Finds a normalized source layer by its stable source ID.
fn find_layer(document: &NormalizedDocument, id: u32) -> Option<&NormalizedLayer> {
    fn find(layers: &[NormalizedLayer], id: u32) -> Option<&NormalizedLayer> {
        for layer in layers {
            if layer.id == id {
                return Some(layer);
            }
            if let Some(found) = find(&layer.children, id) {
                return Some(found);
            }
        }
        None
    }
    find(&document.root_layers, id)
}

/// Maps base layer properties to Aseprite layer options.
fn layer_options(
    layer: &NormalizedLayer,
    warnings: &mut Vec<String>,
) -> Result<LayerOptions, WriterError> {
    Ok(LayerOptions {
        opacity: normalized_opacity(layer.opacity, format!("layer {}", layer.id), warnings)?,
        blend_mode: blend_mode(layer.blend_mode.as_deref(), layer.id, warnings),
        visible: layer.frame_states.iter().any(|state| state.enabled),
        ..LayerOptions::default()
    })
}

/// Maps an Aseprite-compatible blend-mode name, warning when it cannot be preserved.
fn blend_mode(value: Option<&str>, layer_id: u32, warnings: &mut Vec<String>) -> BlendMode {
    match value.unwrap_or("normal") {
        "normal" => BlendMode::Normal,
        "multiply" => BlendMode::Multiply,
        "screen" => BlendMode::Screen,
        "overlay" => BlendMode::Overlay,
        "darken" => BlendMode::Darken,
        "lighten" => BlendMode::Lighten,
        "color dodge" => BlendMode::ColorDodge,
        "color burn" => BlendMode::ColorBurn,
        "hard light" => BlendMode::HardLight,
        "soft light" => BlendMode::SoftLight,
        "difference" => BlendMode::Difference,
        "exclusion" => BlendMode::Exclusion,
        "hue" => BlendMode::Hue,
        "saturation" => BlendMode::Saturation,
        "color" => BlendMode::Color,
        "luminosity" => BlendMode::Luminosity,
        "subtract" | "subtraction" => BlendMode::Subtract,
        "divide" => BlendMode::Divide,
        other => {
            warnings.push(format!(
                "layer {layer_id} blend mode {other:?} mapped to normal"
            ));
            BlendMode::Normal
        }
    }
}

/// Converts normalized opacity in the 0.0..=1.0 range to Aseprite's 8-bit representation.
fn normalized_opacity(
    value: Option<f64>,
    field: String,
    warnings: &mut Vec<String>,
) -> Result<u8, WriterError> {
    let value = value.unwrap_or(1.0);
    let opacity = opacity_to_u8(Some(value), &field)?;
    if (value * 255.0) != f64::from(opacity) {
        warnings.push(format!(
            "{field} opacity {value} quantized to {opacity}/255 for Aseprite"
        ));
    }
    Ok(opacity)
}

/// Converts normalized opacity for shared writer and read-back validation logic.
pub(crate) fn opacity_to_u8(value: Option<f64>, field: &str) -> Result<u8, WriterError> {
    let value = value.unwrap_or(1.0);
    if !value.is_finite() || !(0.0..=1.0).contains(&value) {
        return Err(WriterError::InvalidOpacity {
            field: field.to_string(),
            value: value.to_string(),
        });
    }
    Ok((value * 255.0).round() as u8)
}

/// Validates and converts normalized RGBA8 data to the Aseprite pixel type.
fn aseprite_pixels(layer_id: u32, pixels: &NormalizedPixels) -> Result<Pixels, WriterError> {
    let width = u16_value("pixel width", pixels.width)?;
    let height = u16_value("pixel height", pixels.height)?;
    Pixels::new(pixels.data.clone(), width, height, ColorMode::Rgba).map_err(|error| {
        WriterError::InvalidPixels {
            layer_id,
            message: error.to_string(),
        }
    })
}

/// Returns the normalized state for one layer/frame, rejecting incomplete models.
fn frame_state(
    layer: &NormalizedLayer,
    frame_index: usize,
) -> Result<&NormalizedLayerFrameState, WriterError> {
    layer
        .frame_states
        .get(frame_index)
        .ok_or_else(|| WriterError::InvalidPixels {
            layer_id: layer.id,
            message: format!("missing frame state {frame_index}"),
        })
}

/// Converts a u32 model value to an Aseprite u16 value.
fn u16_value(field: &str, value: u32) -> Result<u16, WriterError> {
    u16::try_from(value).map_err(|_| WriterError::FormatLimit {
        field: field.to_string(),
        value: i64::from(value),
        max: i64::from(u16::MAX),
    })
}

/// Computes a cel origin from the base layer bounds and the frame-local PSD offset.
pub(crate) fn cel_position(
    pixels: &NormalizedPixels,
    state: &NormalizedLayerFrameState,
) -> Result<(i16, i16), WriterError> {
    let x = add_frame_offset("cel x", pixels.left, state.offset.map(|point| point.x))?;
    let y = add_frame_offset("cel y", pixels.top, state.offset.map(|point| point.y))?;
    Ok((i16_value("cel x", x)?, i16_value("cel y", y)?))
}

/// Adds one integral frame-local offset to a base model coordinate.
fn add_frame_offset(field: &str, base: i32, offset: Option<f64>) -> Result<i32, WriterError> {
    let Some(offset) = offset else {
        return Ok(base);
    };
    if !offset.is_finite()
        || offset.fract() != 0.0
        || offset < f64::from(i32::MIN)
        || offset > f64::from(i32::MAX)
    {
        return Err(WriterError::InvalidCoordinate {
            field: field.to_string(),
            value: offset.to_string(),
        });
    }
    base.checked_add(offset as i32)
        .ok_or_else(|| WriterError::InvalidCoordinate {
            field: field.to_string(),
            value: format!("{base} + {offset}"),
        })
}

/// Converts an i32 model coordinate to an Aseprite i16 coordinate.
fn i16_value(field: &str, value: i32) -> Result<i16, WriterError> {
    i16::try_from(value).map_err(|_| WriterError::FormatLimit {
        field: field.to_string(),
        value: i64::from(value),
        max: i64::from(i16::MAX),
    })
}

/// Records animation features that the first Aseprite mapping cannot represent directly.
fn collect_unmapped_animation_warnings(document: &NormalizedDocument, warnings: &mut Vec<String>) {
    let mut reference_point_count = 0;
    let mut group_opacity_count = 0;
    for layer in &document.root_layers {
        collect_layer_animation_warning_counts(
            layer,
            &mut reference_point_count,
            &mut group_opacity_count,
        );
    }
    if reference_point_count > 0 {
        warnings.push(format!(
            "{reference_point_count} reference points were not serialized"
        ));
    }
    if group_opacity_count > 0 {
        warnings.push(format!(
            "{group_opacity_count} group frame opacity overrides were not serialized"
        ));
    }
    if document.active_frame_index.is_some() {
        warnings.push("Photoshop active frame is not serialized as Aseprite UI state".to_string());
    }
}

/// Counts unsupported frame-local properties recursively.
fn collect_layer_animation_warning_counts(
    layer: &NormalizedLayer,
    reference_point_count: &mut usize,
    group_opacity_count: &mut usize,
) {
    for state in &layer.frame_states {
        *reference_point_count += usize::from(state.reference_point.is_some());
        if layer.kind == NormalizedLayerKind::Group {
            *group_opacity_count += usize::from(state.opacity.is_some());
        }
    }
    for child in &layer.children {
        collect_layer_animation_warning_counts(child, reference_point_count, group_opacity_count);
    }
}

#[cfg(test)]
mod tests {
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
}
