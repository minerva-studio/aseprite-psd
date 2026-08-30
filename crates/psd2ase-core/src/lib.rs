//! Format-independent conversion boundaries for PSD to Aseprite.
//!
//! The current implementation exposes a normalized reader and an experimental
//! Aseprite writer. Coordinate mapping remains provisional until visual review
//! of a generated file is complete.

pub mod aseprite_writer;
mod error;
mod model;
pub mod photoshop_animation;

pub use aseprite_writer::{DEFAULT_FRAME_DURATION_MS, EncodedAseprite, WriterError};
pub use error::{ConversionError, InspectionError};
pub use model::{
    DocumentInspection, NormalizedBounds, NormalizedDocument, NormalizedFrame, NormalizedLayer,
    NormalizedLayerFrameState, NormalizedLayerKind, NormalizedLoopMode, NormalizedPixels,
};
pub use photoshop_animation::{
    AnimationFlags, AnimationLayerInput, AnimationParseError, AnimationPoint, LayerAnimationState,
    LayerFrameState, LoopMode, PhotoshopAnimation, PhotoshopFrame, VisibleFrameLayers,
    parse_photoshop_animation,
};

use std::collections::{HashMap, HashSet};
use std::fs::{self, OpenOptions};
use std::io::{Cursor, Write};
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

/// The package version exposed to the CLI and reports.
pub const VERSION: &str = env!("CARGO_PKG_VERSION");

/// Options controlling a conversion transaction.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ConvertOptions {
    /// Allow replacing an existing output path after successful validation.
    pub overwrite: bool,
}

/// Summary produced after a conversion has committed its output.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConversionReport {
    /// The source PSD path supplied by the caller.
    pub input: PathBuf,
    /// The output path supplied by the caller.
    pub output: PathBuf,
    /// Warnings produced while mapping or validating the document.
    pub warnings: Vec<String>,
}

/// Reads PSD structure metadata without creating an output file.
pub fn inspect(input: &Path) -> Result<DocumentInspection, InspectionError> {
    let bytes = fs::read(input).map_err(InspectionError::InputIo)?;
    let options = ag_psd::psd::ReadOptions {
        skip_layer_image_data: Some(true),
        skip_composite_image_data: Some(true),
        skip_thumbnail: Some(true),
        ..Default::default()
    };
    let psd = ag_psd::read_psd(&bytes, &options)
        .map_err(|error| InspectionError::PsdRead(error.to_string()))?;

    Ok(DocumentInspection {
        width: psd.width as u32,
        height: psd.height as u32,
        bits_per_channel: psd.bits_per_channel.map(|value| value as u32),
        color_mode: psd.color_mode.map(|value| format!("{value:?}")),
        root_layer_count: psd.children.as_ref().map_or(0, Vec::len),
    })
}

/// Reads a PSD and converts it into the format-neutral intermediate model.
pub fn normalize(input: &Path) -> Result<NormalizedDocument, InspectionError> {
    let bytes = fs::read(input).map_err(InspectionError::InputIo)?;
    normalize_bytes(&bytes)
}

/// Converts one parser buffer without exposing ag-psd types to callers.
fn normalize_bytes(bytes: &[u8]) -> Result<NormalizedDocument, InspectionError> {
    let options = ag_psd::psd::ReadOptions {
        use_image_data: Some(true),
        skip_thumbnail: Some(true),
        ..Default::default()
    };
    let psd = ag_psd::read_psd(bytes, &options)
        .map_err(|error| InspectionError::PsdRead(error.to_string()))?;
    let canvas = (
        integral_u32(psd.width, "document width")?,
        integral_u32(psd.height, "document height")?,
    );
    let channels = psd
        .channels
        .map(|value| integral_u32(value, "document channel count"))
        .transpose()?;
    let bits_per_channel = psd
        .bits_per_channel
        .map(|value| integral_u32(value, "document bit depth"))
        .transpose()?;
    let root_layers = psd.children.as_deref().unwrap_or_default();

    let mut animation_inputs = Vec::new();
    let mut seen_ids = HashSet::new();
    for (index, layer) in root_layers.iter().enumerate() {
        collect_animation_inputs(
            layer,
            &[index.to_string()],
            &[],
            &mut animation_inputs,
            &mut seen_ids,
        )?;
    }
    let animation = parse_photoshop_animation(bytes, &animation_inputs)
        .map_err(|error| InspectionError::Normalization(format!("Photoshop animation: {error}")))?;

    let mut layers = Vec::with_capacity(root_layers.len());
    for (index, layer) in root_layers.iter().enumerate() {
        layers.push(build_layer(layer, &[index.to_string()])?);
    }

    let (frames, loop_mode, active_frame_index, resource_ids, frame_flags) =
        if let Some(animation) = &animation {
            let frames = animation
                .frames
                .iter()
                .enumerate()
                .map(|(index, frame)| NormalizedFrame {
                    index: index as u32,
                    source_id: Some(frame.id),
                    duration_ms: Some(frame.duration_ms),
                    dispose: frame.dispose.clone(),
                })
                .collect::<Vec<_>>();
            if frames.is_empty() {
                return Err(InspectionError::Normalization(
                    "animation metadata declared no frames".to_string(),
                ));
            }
            let mut states = HashMap::with_capacity(animation.layer_states.len());
            for state in &animation.layer_states {
                if states.insert(state.layer_id, state).is_some() {
                    return Err(InspectionError::Normalization(format!(
                        "duplicate animation state for layer {}",
                        state.layer_id
                    )));
                }
            }
            apply_animation_states(&mut layers, &states, &animation.frames)?;
            (
                frames,
                animation.loop_mode.as_ref().map(normalized_loop_mode),
                animation.active_frame_index,
                animation.resource_ids.clone(),
                animation.frame_flags.clone(),
            )
        } else {
            apply_static_states(&mut layers);
            (
                vec![NormalizedFrame {
                    index: 0,
                    source_id: None,
                    duration_ms: None,
                    dispose: None,
                }],
                None,
                None,
                Vec::new(),
                None,
            )
        };

    Ok(NormalizedDocument {
        canvas,
        channels,
        bits_per_channel,
        color_mode: psd
            .color_mode
            .map(|value| normalize_enum_name(&format!("{value:?}"))),
        root_layers: layers,
        frames,
        loop_mode,
        active_frame_index,
        animation_resource_ids: resource_ids,
        animation_frame_flags: frame_flags,
    })
}

/// Collects strict layer IDs and ancestry for the Photoshop metadata scanner.
fn collect_animation_inputs(
    layer: &ag_psd::psd::Layer,
    path: &[String],
    ancestors: &[u32],
    inputs: &mut Vec<AnimationLayerInput>,
    seen_ids: &mut HashSet<u32>,
) -> Result<(), InspectionError> {
    let path_string = path.join("/");
    let id = layer_id(layer.additional_info.id, &path_string)?;
    if !seen_ids.insert(id) {
        return Err(InspectionError::Normalization(format!(
            "duplicate layer id {id} at {path_string}"
        )));
    }
    inputs.push(AnimationLayerInput {
        id,
        path: path_string,
        is_group: layer.children.is_some(),
        hidden: layer.hidden.unwrap_or(false),
        ancestor_ids: ancestors.to_vec(),
    });
    let mut child_ancestors = ancestors.to_vec();
    if layer.children.is_some() {
        child_ancestors.push(id);
    }
    if let Some(children) = &layer.children {
        for (index, child) in children.iter().enumerate() {
            let mut child_path = path.to_vec();
            child_path.push(index.to_string());
            collect_animation_inputs(child, &child_path, &child_ancestors, inputs, seen_ids)?;
        }
    }
    Ok(())
}

/// Converts one ag-psd layer into an owned normalized layer tree.
fn build_layer(
    layer: &ag_psd::psd::Layer,
    path: &[String],
) -> Result<NormalizedLayer, InspectionError> {
    let path_string = path.join("/");
    let bounds = normalized_bounds(layer, &path_string)?;
    let is_group = layer.children.is_some();
    let pixels = if is_group {
        None
    } else {
        let pixel = layer
            .image_data
            .as_ref()
            .or(layer.canvas.as_ref())
            .ok_or_else(|| {
                InspectionError::Normalization(format!(
                    "pixel layer has no RGBA8 data at {path_string}"
                ))
            })?;
        Some(copy_rgba8_pixels(pixel, bounds, &path_string)?)
    };
    let mut children = Vec::new();
    if let Some(source_children) = &layer.children {
        children.reserve(source_children.len());
        for (index, child) in source_children.iter().enumerate() {
            let mut child_path = path.to_vec();
            child_path.push(index.to_string());
            children.push(build_layer(child, &child_path)?);
        }
    }
    Ok(NormalizedLayer {
        id: layer_id(layer.additional_info.id, &path_string)?,
        name: layer.additional_info.name.clone().unwrap_or_default(),
        kind: if is_group {
            NormalizedLayerKind::Group
        } else {
            NormalizedLayerKind::Pixel
        },
        bounds,
        opacity: layer.opacity,
        blend_mode: layer
            .blend_mode
            .map(|value| normalize_enum_name(&format!("{value:?}"))),
        hidden: layer.hidden,
        pixels,
        children,
        frame_states: Vec::new(),
    })
}

/// Applies source animation states recursively to the normalized tree.
fn apply_animation_states(
    layers: &mut [NormalizedLayer],
    states: &HashMap<u32, &LayerAnimationState>,
    frames: &[PhotoshopFrame],
) -> Result<(), InspectionError> {
    for layer in layers {
        let source = states.get(&layer.id).ok_or_else(|| {
            InspectionError::Normalization(format!(
                "animation state missing for normalized layer {}",
                layer.id
            ))
        })?;
        if source.frames.len() != frames.len() {
            return Err(InspectionError::Normalization(format!(
                "animation state length mismatch for layer {}: expected {}, got {}",
                layer.id,
                frames.len(),
                source.frames.len()
            )));
        }
        layer.frame_states = source
            .frames
            .iter()
            .enumerate()
            .map(|(frame_index, state)| NormalizedLayerFrameState {
                frame_index: frame_index as u32,
                enabled: state.enabled,
                explicit_enable: state.explicit_enable,
                offset: state.offset,
                reference_point: state.reference_point,
                opacity: state.opacity,
            })
            .collect();
        apply_animation_states(&mut layer.children, states, frames)?;
    }
    Ok(())
}

/// Adds one base state to every layer of a static document.
fn apply_static_states(layers: &mut [NormalizedLayer]) {
    for layer in layers {
        layer.frame_states = vec![NormalizedLayerFrameState {
            frame_index: 0,
            enabled: !layer.hidden.unwrap_or(false),
            explicit_enable: false,
            offset: None,
            reference_point: None,
            opacity: None,
        }];
        apply_static_states(&mut layer.children);
    }
}

/// Converts an animation loop policy without retaining parser types in the model.
fn normalized_loop_mode(value: &LoopMode) -> NormalizedLoopMode {
    match value {
        LoopMode::Infinite => NormalizedLoopMode::Infinite,
        LoopMode::Finite(count) => NormalizedLoopMode::Finite(*count),
    }
}

/// Validates a Photoshop layer ID before it enters the normalized model.
fn layer_id(value: Option<f64>, path: &str) -> Result<u32, InspectionError> {
    value
        .filter(|value| {
            value.is_finite() && *value >= 1.0 && value.fract() == 0.0 && *value <= u32::MAX as f64
        })
        .map(|value| value as u32)
        .ok_or_else(|| InspectionError::Normalization(format!("layer at {path} has an invalid ID")))
}

/// Converts an integral finite PSD number to a u32 model field.
fn integral_u32(value: f64, field: &str) -> Result<u32, InspectionError> {
    if value.is_finite() && value >= 0.0 && value.fract() == 0.0 && value <= u32::MAX as f64 {
        Ok(value as u32)
    } else {
        Err(InspectionError::Normalization(format!(
            "{field} must be a finite integer in the u32 range"
        )))
    }
}

/// Converts an integral finite PSD coordinate to an i32 model field.
fn integral_i32(value: Option<f64>, field: &str) -> Result<i32, InspectionError> {
    let value =
        value.ok_or_else(|| InspectionError::Normalization(format!("{field} is missing")))?;
    if value.is_finite()
        && value.fract() == 0.0
        && value >= i32::MIN as f64
        && value <= i32::MAX as f64
    {
        Ok(value as i32)
    } else {
        Err(InspectionError::Normalization(format!(
            "{field} must be a finite integer in the i32 range"
        )))
    }
}

/// Validates and converts a parser layer rectangle.
fn normalized_bounds(
    layer: &ag_psd::psd::Layer,
    path: &str,
) -> Result<NormalizedBounds, InspectionError> {
    let bounds = NormalizedBounds {
        left: integral_i32(layer.left, &format!("layer {path} left"))?,
        top: integral_i32(layer.top, &format!("layer {path} top"))?,
        right: integral_i32(layer.right, &format!("layer {path} right"))?,
        bottom: integral_i32(layer.bottom, &format!("layer {path} bottom"))?,
    };
    if bounds.right < bounds.left || bounds.bottom < bounds.top {
        return Err(InspectionError::Normalization(format!(
            "layer {path} bounds are inverted"
        )));
    }
    Ok(bounds)
}

/// Copies and validates one parser pixel buffer as owned RGBA8 data.
fn copy_rgba8_pixels(
    pixel: &ag_psd::psd::PixelData,
    bounds: NormalizedBounds,
    path: &str,
) -> Result<NormalizedPixels, InspectionError> {
    let expected = usize::try_from(
        u64::from(pixel.width)
            .checked_mul(u64::from(pixel.height))
            .and_then(|value| value.checked_mul(4))
            .ok_or_else(|| {
                InspectionError::Normalization(format!(
                    "pixel dimensions overflow RGBA8 size at {path}"
                ))
            })?,
    )
    .map_err(|_| {
        InspectionError::Normalization(format!(
            "pixel dimensions exceed addressable memory at {path}"
        ))
    })?;
    if pixel.data.len() != expected {
        return Err(InspectionError::Normalization(format!(
            "pixel buffer length mismatch at {path}: expected {expected}, got {}",
            pixel.data.len()
        )));
    }
    Ok(NormalizedPixels {
        width: pixel.width,
        height: pixel.height,
        left: bounds.left,
        top: bounds.top,
        data: pixel.data.clone(),
    })
}

/// Normalizes parser enum debug names to a stable lowercase string.
fn normalize_enum_name(value: &str) -> String {
    let mut spaced = String::with_capacity(value.len() + 4);
    for (index, character) in value.chars().enumerate() {
        if index > 0 && character.is_ascii_uppercase() {
            spaced.push(' ');
        }
        spaced.push(character);
    }
    spaced.replace('_', " ").to_ascii_lowercase()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn state(frame_index: u32, enabled: bool) -> NormalizedLayerFrameState {
        NormalizedLayerFrameState {
            frame_index,
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
}

/// Converts a PSD into an Aseprite file after validation and mapping.
pub fn convert(
    input: &Path,
    output: &Path,
    options: &ConvertOptions,
) -> Result<ConversionReport, ConversionError> {
    if !input.is_file() {
        return Err(ConversionError::InputMissing(input.to_path_buf()));
    }

    if output.exists() && !options.overwrite {
        return Err(ConversionError::OutputExists(output.to_path_buf()));
    }

    let document =
        normalize(input).map_err(|error| ConversionError::InputInspection(error.to_string()))?;
    let mut encoded = aseprite_writer::encode(&document)
        .map_err(|error| ConversionError::Writer(error.to_string()))?;
    encoded.warnings.insert(
        0,
        "coordinate policy: provisional pixels.left/top plus frame offset cel origin".to_string(),
    );
    validate_aseprite_output(&encoded.bytes, &document)?;
    commit_output(output, &encoded.bytes, options.overwrite)?;

    Ok(ConversionReport {
        input: input.to_path_buf(),
        output: output.to_path_buf(),
        warnings: encoded.warnings,
    })
}

/// Validates the encoded Aseprite structure against the normalized source model.
fn validate_aseprite_output(
    bytes: &[u8],
    document: &NormalizedDocument,
) -> Result<(), ConversionError> {
    let file = aseprite::AsepriteFile::from_reader(Cursor::new(bytes))
        .map_err(|error| ConversionError::OutputValidation(error.to_string()))?;
    if file.width() != u16::try_from(document.canvas.0).unwrap_or(u16::MAX)
        || file.height() != u16::try_from(document.canvas.1).unwrap_or(u16::MAX)
    {
        return Err(ConversionError::OutputValidation(
            "canvas dimensions differ from normalized document".to_string(),
        ));
    }
    if file.frames().len() != document.frames.len() {
        return Err(ConversionError::OutputValidation(format!(
            "frame count differs: expected {}, got {}",
            document.frames.len(),
            file.frames().len()
        )));
    }
    for (index, frame) in document.frames.iter().enumerate() {
        let expected = frame
            .duration_ms
            .unwrap_or(u32::from(DEFAULT_FRAME_DURATION_MS));
        if u32::from(file.frames()[index].duration_ms) != expected {
            return Err(ConversionError::OutputValidation(format!(
                "frame {index} duration differs: expected {expected}, got {}",
                file.frames()[index].duration_ms
            )));
        }
    }

    let mut layers = Vec::new();
    flatten_layers(&document.root_layers, None, &mut layers);
    if file.layers().len() != layers.len() {
        return Err(ConversionError::OutputValidation(format!(
            "layer count differs: expected {}, got {}",
            layers.len(),
            file.layers().len()
        )));
    }
    for (layer_index, (source, expected_parent)) in layers.iter().enumerate() {
        let output_layer = &file.layers()[layer_index];
        if output_layer.name != source.name
            || output_layer.opacity
                != aseprite_writer::opacity_to_u8(source.opacity, &format!("layer {}", source.id))
                    .map_err(|error| ConversionError::OutputValidation(error.to_string()))?
            || output_layer.parent != *expected_parent
            || output_layer.visible != source.frame_states.iter().any(|state| state.enabled)
        {
            return Err(ConversionError::OutputValidation(format!(
                "layer {layer_index} attributes differ for {}",
                source.id
            )));
        }
        match source.kind {
            NormalizedLayerKind::Group => {
                if output_layer.kind != aseprite::LayerKind::Group {
                    return Err(ConversionError::OutputValidation(format!(
                        "layer {layer_index} should be a group"
                    )));
                }
            }
            NormalizedLayerKind::Pixel => {
                let output_handle = file.layer_ref(layer_index).ok_or_else(|| {
                    ConversionError::OutputValidation(format!(
                        "pixel layer {layer_index} cannot be addressed"
                    ))
                })?;
                if output_layer.kind != aseprite::LayerKind::Normal {
                    return Err(ConversionError::OutputValidation(format!(
                        "layer {layer_index} should be a pixel layer"
                    )));
                }
                for frame_index in 0..document.frames.len() {
                    let visible = is_visible_pixel(document, source.id, frame_index);
                    let cel = file.cel(output_handle, frame_index);
                    if visible != cel.is_some() {
                        return Err(ConversionError::OutputValidation(format!(
                            "layer {} frame {frame_index} cel visibility differs",
                            source.id
                        )));
                    }
                    if let Some(cel) = cel {
                        validate_cel(cel, source, frame_index)?;
                    }
                }
            }
        }
    }
    Ok(())
}

/// Flattens the normalized tree in the same order used by the writer.
fn flatten_layers<'a>(
    layers: &'a [NormalizedLayer],
    parent: Option<usize>,
    output: &mut Vec<(&'a NormalizedLayer, Option<usize>)>,
) {
    for layer in layers {
        let index = output.len();
        output.push((layer, parent));
        flatten_layers(&layer.children, Some(index), output);
    }
}

/// Checks whether a normalized pixel layer is effectively visible in a frame.
fn is_visible_pixel(document: &NormalizedDocument, layer_id: u32, frame_index: usize) -> bool {
    let mut visible = Vec::new();
    for layer in &document.root_layers {
        layer.collect_visible_pixel_layer_ids(frame_index, true, &mut visible);
    }
    visible.contains(&layer_id)
}

/// Validates one read-back cel's dimensions, position, opacity, and bytes.
fn validate_cel(
    cel: &aseprite::Cel,
    source: &NormalizedLayer,
    frame_index: usize,
) -> Result<(), ConversionError> {
    let expected_state = source.frame_states.get(frame_index).ok_or_else(|| {
        ConversionError::OutputValidation(format!("missing source frame state {frame_index}"))
    })?;
    let pixels = source.pixels.as_ref().ok_or_else(|| {
        ConversionError::OutputValidation(format!("pixel layer {} has no source pixels", source.id))
    })?;
    let expected_opacity = aseprite_writer::opacity_to_u8(
        expected_state.opacity.or(source.opacity),
        &format!("layer {} frame {frame_index}", source.id),
    )
    .map_err(|error| ConversionError::OutputValidation(error.to_string()))?;
    if cel.opacity != expected_opacity {
        return Err(ConversionError::OutputValidation(format!(
            "layer {} frame {frame_index} opacity differs",
            source.id
        )));
    }
    let expected_position = aseprite_writer::cel_position(pixels, expected_state)
        .map_err(|error| ConversionError::OutputValidation(error.to_string()))?;
    let (output_pixels, x, y) = match &cel.kind {
        aseprite::CelKind::Raw { pixels, x, y }
        | aseprite::CelKind::Compressed { pixels, x, y, .. } => (pixels, *x, *y),
        _ => {
            return Err(ConversionError::OutputValidation(format!(
                "layer {} frame {frame_index} is not a pixel cel",
                source.id
            )));
        }
    };
    if output_pixels.width != u16::try_from(pixels.width).unwrap_or(u16::MAX)
        || output_pixels.height != u16::try_from(pixels.height).unwrap_or(u16::MAX)
        || (x, y) != expected_position
        || output_pixels.data != pixels.data
    {
        return Err(ConversionError::OutputValidation(format!(
            "layer {} frame {frame_index} pixel data or position differs",
            source.id
        )));
    }
    Ok(())
}

/// Writes validated bytes through a same-directory temporary transaction.
fn commit_output(output: &Path, bytes: &[u8], overwrite: bool) -> Result<(), ConversionError> {
    let parent = output.parent().unwrap_or_else(|| Path::new("."));
    fs::create_dir_all(parent).map_err(ConversionError::OutputIo)?;
    let file_name = output
        .file_name()
        .and_then(|value| value.to_str())
        .unwrap_or("output.aseprite");
    let stamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|error| ConversionError::OutputIo(std::io::Error::other(error)))?
        .as_nanos();
    let temporary = parent.join(format!(".{file_name}.{stamp}.tmp"));
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&temporary)
        .map_err(ConversionError::OutputIo)?;
    let result = (|| {
        file.write_all(bytes).map_err(ConversionError::OutputIo)?;
        file.sync_all().map_err(ConversionError::OutputIo)?;
        if !overwrite || !output.exists() {
            fs::rename(&temporary, output).map_err(ConversionError::OutputIo)?;
            return Ok(());
        }
        let backup = parent.join(format!(".{file_name}.{stamp}.bak"));
        fs::rename(output, &backup).map_err(ConversionError::OutputIo)?;
        match fs::rename(&temporary, output) {
            Ok(()) => {
                fs::remove_file(backup).map_err(ConversionError::OutputIo)?;
                Ok(())
            }
            Err(error) => {
                let _ = fs::rename(&backup, output);
                Err(ConversionError::OutputIo(error))
            }
        }
    })();
    if result.is_err() {
        let _ = fs::remove_file(&temporary);
    }
    result
}
