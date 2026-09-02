//! Format-independent conversion boundaries for Aseprite and Photoshop documents.
//!
//! The current implementation exposes a normalized reader and an experimental
//! Aseprite writer. Coordinate mapping remains provisional until visual review
//! of a generated file is complete.

mod aseprite_metadata;
pub mod aseprite_reader;
pub mod aseprite_writer;
mod atomic_output;
mod error;
pub mod information_loss;
pub mod jitter;
pub mod layer_names;
pub mod logical_layers;
mod model;
pub mod photoshop_animation;
mod photoshop_metadata;
pub mod psd_writer;
mod roundtrip;

pub use aseprite_writer::{
    CelReuseReport, DEFAULT_FRAME_DURATION_MS, EncodedAseprite, WriterError,
};
pub use error::{ConversionError, ExportError, InspectionError};
pub use information_loss::{
    InformationLocation, InformationLoss, InformationLossCode, InformationLossReport,
    LossDisposition, report_json, report_json_with_active_frame, write_report,
    write_report_with_active_frame,
};
pub use jitter::{
    JitterKind, JitterMode, JitterOptions, JitterPlan, JitterProfile, JitterReport,
    JitterThresholds, build_jitter_plan, resolved_pixels, stabilized_document,
};
pub use layer_names::{
    COPY_SUFFIX_CATALOG_VERSION, CopySuffixCatalog, CopySuffixKind, CopySuffixMatch,
    CopySuffixRule, MAX_COPY_SUFFIX_DEPTH, ParsedLayerName,
};
pub use logical_layers::{
    AssociationDecision, AssociationDecisionStatus, AssociationExclusionKind, AssociationPhase,
    AssociationReport, AssociationStrategy, AutoAssociationOptions, CandidateGroupReport,
    CandidateTrackRelation, CandidateTrackRelationReport, LayerAssociation, LayerWritePlan,
    LayerZOrderMode, LogicalLayerTrack, PlannedCel, PlannedNode, StableOrderMode,
    UncertainLayerMode, build_layer_write_plan,
};
pub use model::{
    DocumentInspection, NormalizedBounds, NormalizedDocument, NormalizedFrame, NormalizedLayer,
    NormalizedLayerFrameState, NormalizedLayerKind, NormalizedLoopMode, NormalizedPixels,
};
pub use photoshop_animation::{
    AnimationFlags, AnimationLayerInput, AnimationParseError, AnimationPoint, LayerAnimationState,
    LayerFrameState, LoopMode, PhotoshopAnimation, PhotoshopFrame, VisibleFrameLayers,
    parse_photoshop_animation,
};
pub use psd_writer::{ExportCompression, ExportOptions, ExportReport, export};

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
    /// Selects source-preserving output or a valid automatic association configuration.
    pub layer_association: LayerAssociation,
    /// Selects whether identical pixel cels may share Aseprite storage.
    pub linked_cels: LinkedCelMode,
    /// Selects conservative pixel stabilization before cel emission.
    pub jitter: JitterOptions,
    /// Preserve meaningful Photoshop-only metadata for a later PSD round trip.
    pub preserve_photoshop_metadata: bool,
    /// Selects how source layers become playback frames before association.
    pub frame_source: FrameSource,
}

/// Selects the source structure used to construct playback frames.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum FrameSource {
    /// Use a Photoshop timeline when present and otherwise preserve a static document.
    #[default]
    Auto,
    /// Preserve the PSD as one static frame even when other frame-like structures exist.
    Static,
    /// Treat each non-background top-level layer or group as one playback frame.
    TopLevel,
}

/// Selects whether identical cels are emitted as links to an earlier cel.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum LinkedCelMode {
    /// Store every visible cel with its own pixel data.
    #[default]
    Off,
    /// Link exact RGBA/size matches within each output layer.
    Identical,
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
    /// Structured source and output compatibility losses.
    pub information_loss: InformationLossReport,
    /// Automatic layer-association diagnostics, when auto mode was selected.
    pub association: Option<AssociationReport>,
    /// Counts of ordinary and linked cels in the committed output.
    pub cel_reuse: CelReuseReport,
    /// Pixel stabilization diagnostics, when enabled.
    pub jitter: Option<JitterReport>,
    /// Source active frame index for temporary import handoff and reports.
    pub active_frame_index: Option<u32>,
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
    let roundtrip = roundtrip::inspect(&bytes)?;

    Ok(DocumentInspection {
        width: psd.width as u32,
        height: psd.height as u32,
        bits_per_channel: psd.bits_per_channel.map(|value| value as u32),
        color_mode: psd.color_mode.map(|value| format!("{value:?}")),
        root_layer_count: psd.children.as_ref().map_or(0, Vec::len),
        roundtrip_marked: roundtrip.marked && roundtrip.valid,
    })
}

/// Reads a PSD and converts it into the format-neutral intermediate model.
pub fn normalize(input: &Path) -> Result<NormalizedDocument, InspectionError> {
    let bytes = fs::read(input).map_err(InspectionError::InputIo)?;
    normalize_bytes(&bytes).map(|(document, _)| document)
}

/// Converts one parser buffer without exposing ag-psd types to callers.
fn normalize_bytes(
    bytes: &[u8],
) -> Result<(NormalizedDocument, InformationLossReport), InspectionError> {
    let options = ag_psd::psd::ReadOptions {
        use_image_data: Some(true),
        skip_thumbnail: Some(true),
        ..Default::default()
    };
    let psd = ag_psd::read_psd(bytes, &options)
        .map_err(|error| InspectionError::PsdRead(error.to_string()))?;
    validate_normalization_bit_depth(psd.bits_per_channel)?;
    let mut information_loss = InformationLossReport::default();
    collect_source_losses(&psd, &mut information_loss);
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

    Ok((
        NormalizedDocument {
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
        },
        information_loss,
    ))
}

/// Records source features that the normalized model intentionally drops.
fn collect_source_losses(psd: &ag_psd::psd::Psd, report: &mut InformationLossReport) {
    if let Some(artboards) = &psd.artboards {
        if artboards.count > 0.0 {
            report.add(
                InformationLossCode::Artboards,
                LossDisposition::Dropped,
                InformationLocation {
                    layer_id: None,
                    path: "document".to_string(),
                    frame_index: None,
                },
                "artboards are not represented in the normalized model",
                true,
                true,
            );
        }
    }
    if let Some(resources) = &psd.image_resources {
        if resources
            .slices
            .as_ref()
            .is_some_and(|value| !value.is_empty())
        {
            report.add(
                InformationLossCode::Slices,
                LossDisposition::Dropped,
                InformationLocation {
                    layer_id: None,
                    path: "image_resources".to_string(),
                    frame_index: None,
                },
                "slices are not represented in the normalized model",
                false,
                true,
            );
        }
        if resources.layer_comps.is_some() {
            report.add(
                InformationLossCode::LayerComps,
                LossDisposition::Dropped,
                InformationLocation {
                    layer_id: None,
                    path: "image_resources".to_string(),
                    frame_index: None,
                },
                "layer comps are not represented in the normalized model",
                false,
                true,
            );
        }
    }
    if let Some(layers) = psd.children.as_deref() {
        for (index, layer) in layers.iter().enumerate() {
            collect_layer_losses(layer, &[index.to_string()], report);
        }
    }
}

/// Recursively records layer-level features outside the normalized contract.
fn collect_layer_losses(
    layer: &ag_psd::psd::Layer,
    path: &[String],
    report: &mut InformationLossReport,
) {
    let path = path.join("/");
    let location = |layer_id| InformationLocation {
        layer_id,
        path: path.clone(),
        frame_index: None,
    };
    let id = layer
        .additional_info
        .id
        .and_then(|value| u32::try_from(value as u64).ok());
    if layer.additional_info.mask.is_some() || layer.additional_info.real_mask.is_some() {
        report.add(
            InformationLossCode::PixelMask,
            LossDisposition::Dropped,
            location(id),
            "pixel mask is not represented in the normalized model",
            true,
            true,
        );
    }
    if layer.additional_info.vector_mask.is_some() {
        report.add(
            InformationLossCode::VectorMask,
            LossDisposition::Dropped,
            location(id),
            "vector mask is not represented in the normalized model",
            true,
            true,
        );
    }
    if layer.clipping == Some(true) {
        report.add(
            InformationLossCode::Clipping,
            LossDisposition::Dropped,
            location(id),
            "clipping is not represented in the normalized model",
            true,
            true,
        );
    }
    if layer.additional_info.text.is_some() {
        report.add(
            InformationLossCode::TextLayer,
            LossDisposition::Dropped,
            location(id),
            "text layer is rasterized or dropped to pixel data",
            true,
            true,
        );
    }
    if layer.additional_info.adjustment.is_some() {
        report.add(
            InformationLossCode::AdjustmentLayer,
            LossDisposition::Dropped,
            location(id),
            "adjustment layer is not represented in the normalized model",
            true,
            true,
        );
    }
    if layer.additional_info.effects.is_some() {
        report.add(
            InformationLossCode::LayerEffects,
            LossDisposition::Dropped,
            location(id),
            "layer effects are not represented in the normalized model",
            true,
            true,
        );
    }
    if layer.additional_info.placed_layer.is_some() {
        report.add(
            InformationLossCode::SmartObject,
            LossDisposition::Dropped,
            location(id),
            "smart object is not represented in the normalized model",
            true,
            true,
        );
    }
    if let Some(children) = &layer.children {
        for (index, child) in children.iter().enumerate() {
            let mut child_path = path.split('/').map(str::to_string).collect::<Vec<_>>();
            child_path.push(index.to_string());
            collect_layer_losses(child, &child_path, report);
        }
    }
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
        is_container_group: layer.children.as_ref().is_some_and(|children| {
            !children.is_empty() && children.iter().all(|child| child.children.is_some())
        }),
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
        match layer.image_data.as_ref().or(layer.canvas.as_ref()) {
            Some(pixel) => Some(copy_rgba8_pixels(pixel, bounds, &path_string)?),
            None if bounds.right == bounds.left || bounds.bottom == bounds.top => {
                Some(empty_pixels(bounds, &path_string)?)
            }
            None => {
                return Err(InspectionError::Normalization(format!(
                    "non-empty pixel layer has no RGBA8 data at {path_string}"
                )));
            }
        }
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

/// Creates the zero-length pixel buffer owned by a genuinely empty layer.
fn empty_pixels(bounds: NormalizedBounds, path: &str) -> Result<NormalizedPixels, InspectionError> {
    let width = u32::try_from(bounds.right - bounds.left)
        .map_err(|_| InspectionError::Normalization(format!("invalid pixel width at {path}")))?;
    let height = u32::try_from(bounds.bottom - bounds.top)
        .map_err(|_| InspectionError::Normalization(format!("invalid pixel height at {path}")))?;
    let byte_len = u64::from(width)
        .checked_mul(u64::from(height))
        .and_then(|value| value.checked_mul(4))
        .and_then(|value| usize::try_from(value).ok())
        .ok_or_else(|| {
            InspectionError::Normalization(format!("pixel dimensions overflow at {path}"))
        })?;
    Ok(NormalizedPixels {
        width,
        height,
        left: bounds.left,
        top: bounds.top,
        data: vec![0; byte_len],
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
                record_present: state.record_present,
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
            record_present: false,
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
    if let Some(value) = value
        .filter(|value| {
            value.is_finite() && *value >= 1.0 && value.fract() == 0.0 && *value <= u32::MAX as f64
        })
        .map(|value| value as u32)
    {
        return Ok(value);
    }
    // Static PSDs may omit Photoshop's optional layer ID. A path-based
    // identity is stable and independent of the user-facing layer name.
    let mut hash = 0x811c9dc5_u32;
    for byte in path.as_bytes() {
        hash ^= u32::from(*byte);
        hash = hash.wrapping_mul(0x01000193);
    }
    Ok(hash | 0x8000_0000)
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

/// Rejects source depths that cannot be represented faithfully by normalization.
fn validate_normalization_bit_depth(bits_per_channel: Option<f64>) -> Result<(), InspectionError> {
    if bits_per_channel == Some(32.0) {
        return Err(InspectionError::Normalization(
            "32-bit PSD input is not supported for conversion".to_string(),
        ));
    }
    Ok(())
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

/// Applies an explicit non-timeline frame interpretation before logical association.
fn apply_frame_source(
    document: &mut NormalizedDocument,
    frame_source: FrameSource,
) -> Result<Vec<String>, String> {
    match frame_source {
        FrameSource::Auto => return Ok(Vec::new()),
        FrameSource::Static => {
            apply_static_states(&mut document.root_layers);
            document.frames = vec![NormalizedFrame {
                index: 0,
                source_id: None,
                duration_ms: None,
                dispose: None,
            }];
            document.loop_mode = None;
            document.active_frame_index = None;
            document.animation_resource_ids.clear();
            document.animation_frame_flags = None;
            return Ok(vec!["frame source: static document".to_string()]);
        }
        FrameSource::TopLevel if !document.animation_resource_ids.is_empty() => {
            return Err(
                "top-level frame source cannot replace a Photoshop timeline; use --frame-source auto or static"
                    .to_string(),
            );
        }
        FrameSource::TopLevel => {}
    }

    let frame_roots = document
        .root_layers
        .iter()
        .enumerate()
        .filter(|(_, layer)| !is_shared_background(layer))
        .map(|(index, _)| index)
        .collect::<Vec<_>>();
    if frame_roots.is_empty() {
        return Err("top-level frame source found no frame layers".to_string());
    }
    let frame_count = frame_roots.len();
    for (root_index, layer) in document.root_layers.iter_mut().enumerate() {
        let active_frame = frame_roots.iter().position(|index| *index == root_index);
        apply_top_level_frame_states(
            layer,
            frame_count,
            active_frame,
            active_frame.is_some(),
            true,
        );
    }
    document.frames = (0..frame_count)
        .map(|index| NormalizedFrame {
            index: index as u32,
            source_id: None,
            duration_ms: Some(u32::from(DEFAULT_FRAME_DURATION_MS)),
            dispose: None,
        })
        .collect();
    document.loop_mode = Some(NormalizedLoopMode::Infinite);
    document.active_frame_index = Some(0);
    document.animation_resource_ids.clear();
    document.animation_frame_flags = None;
    let shared = document
        .root_layers
        .iter()
        .filter(|layer| is_shared_background(layer))
        .map(|layer| layer.name.as_str())
        .collect::<Vec<_>>();
    Ok(vec![format!(
        "frame source: {frame_count} top-level frames; shared layers: {}",
        if shared.is_empty() {
            "none".to_string()
        } else {
            shared.join(", ")
        }
    )])
}

/// Returns whether a top-level layer is the explicit shared Procreate background.
fn is_shared_background(layer: &NormalizedLayer) -> bool {
    layer.name.trim().eq_ignore_ascii_case("background")
}

/// Replaces one static layer subtree with states for a top-level frame interpretation.
fn apply_top_level_frame_states(
    layer: &mut NormalizedLayer,
    frame_count: usize,
    active_frame: Option<usize>,
    force_selected_root: bool,
    ancestors_active: bool,
) {
    let source_enabled = !layer.hidden.unwrap_or(false);
    layer.frame_states = (0..frame_count)
        .map(|frame_index| NormalizedLayerFrameState {
            frame_index: frame_index as u32,
            record_present: false,
            enabled: ancestors_active
                && (force_selected_root || source_enabled)
                && active_frame.is_none_or(|active| active == frame_index),
            explicit_enable: false,
            offset: None,
            reference_point: None,
            opacity: None,
        })
        .collect();
    for child in &mut layer.children {
        apply_top_level_frame_states(child, frame_count, active_frame, false, ancestors_active);
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

    let bytes =
        fs::read(input).map_err(|error| ConversionError::InputInspection(error.to_string()))?;
    let (exact_roundtrip, layer_association) = match options.layer_association {
        LayerAssociation::AutoForRoundTrip => {
            let layout = roundtrip::inspect_detailed(&bytes)
                .map_err(|error| ConversionError::InputInspection(error.to_string()))?;
            resolve_roundtrip_association(layout)?
        }
        association => (false, association),
    };
    let (mut document, mut information_loss) = normalize_bytes(&bytes)
        .map_err(|error| ConversionError::InputInspection(error.to_string()))?;
    let frame_source_warnings = apply_frame_source(&mut document, options.frame_source)
        .map_err(ConversionError::InputInspection)?;
    if document.bits_per_channel != Some(8)
        || !matches!(document.color_mode.as_deref(), Some("rgb" | "rgba"))
    {
        information_loss.add(
            InformationLossCode::UnsupportedColor,
            LossDisposition::Degraded,
            InformationLocation {
                layer_id: None,
                path: "document".to_string(),
                frame_index: None,
            },
            format!(
                "source color mode {:?} at {:?} bits per channel is normalized to RGBA8",
                document.color_mode, document.bits_per_channel
            ),
            true,
            true,
        );
    }
    let allow_inferred_cross_source_matches =
        !matches!(options.frame_source, FrameSource::TopLevel)
            || !matches!(
                layer_association,
                LayerAssociation::Auto(AutoAssociationOptions {
                    strategy: AssociationStrategy::Conservative { .. },
                    ..
                })
            );
    let initial_plan = if exact_roundtrip {
        merge_frame_group_states(&mut document)
            .map_err(ConversionError::RoundTripRecoveryRequired)?;
        Some(
            build_frame_group_roundtrip_plan(&document)
                .map_err(ConversionError::RoundTripRecoveryRequired)?,
        )
    } else {
        match layer_association {
            LayerAssociation::Preserve => None,
            LayerAssociation::Auto(auto_options) => Some(
                logical_layers::build_layer_write_plan_with_context(
                    &document,
                    auto_options,
                    options.preserve_photoshop_metadata,
                    allow_inferred_cross_source_matches,
                )
                .map_err(|error| ConversionError::Writer(error.to_string()))?,
            ),
            LayerAssociation::AutoForRoundTrip => None,
        }
    };
    let initial_jitter_plan = build_jitter_plan(&document, initial_plan.as_ref(), options.jitter)
        .map_err(|error| ConversionError::Writer(error.to_string()))?;
    let plan = if options.jitter.mode == crate::JitterMode::Assist {
        if let (Some(auto_options), Some(_initial_plan)) = (
            match layer_association {
                LayerAssociation::Auto(value) => Some(value),
                LayerAssociation::Preserve => None,
                LayerAssociation::AutoForRoundTrip => None,
            },
            initial_plan.as_ref(),
        ) {
            let stabilized = stabilized_document(&document, &initial_jitter_plan);
            Some(
                logical_layers::build_layer_write_plan_with_context(
                    &stabilized,
                    auto_options,
                    options.preserve_photoshop_metadata,
                    allow_inferred_cross_source_matches,
                )
                .map_err(|error| ConversionError::Writer(error.to_string()))?,
            )
        } else {
            initial_plan
        }
    } else {
        initial_plan
    };
    let jitter_plan = if options.jitter.mode == crate::JitterMode::Assist {
        JitterPlan {
            report: initial_jitter_plan.report.clone(),
            ..JitterPlan::default()
        }
    } else {
        initial_jitter_plan
    };
    let jitter =
        (options.jitter.mode != crate::JitterMode::Off).then(|| jitter_plan.report.clone());
    let association = plan.as_ref().map(|plan| plan.report.clone());
    let encoded = match plan.as_ref() {
        None => aseprite_writer::encode_with_linked_cels_and_jitter_and_metadata(
            &document,
            options.linked_cels,
            &jitter_plan,
            options.preserve_photoshop_metadata,
        ),
        Some(plan) => aseprite_writer::encode_with_plan_and_linked_cels_and_jitter_and_metadata(
            &document,
            plan,
            options.linked_cels,
            &jitter_plan,
            options.preserve_photoshop_metadata,
        ),
    }
    .map_err(|error| ConversionError::Writer(error.to_string()))?;
    let mut retained_warnings = vec![
        "coordinate policy: provisional pixels.left/top plus frame offset cel origin".to_string(),
    ];
    retained_warnings.extend(frame_source_warnings);
    for warning in encoded.warning_details {
        information_loss.add(
            warning.code,
            warning.disposition,
            warning.location,
            warning.message,
            warning.visual_impact,
            warning.editability_impact,
        );
    }
    match plan.as_ref() {
        None => {
            validate_aseprite_output(&encoded.bytes, &document, options.linked_cels, &jitter_plan)?
        }
        Some(plan) => validate_planned_aseprite_output(
            &encoded.bytes,
            &document,
            plan,
            options.linked_cels,
            &jitter_plan,
        )?,
    }
    commit_output(output, &encoded.bytes, options.overwrite)?;

    Ok(ConversionReport {
        input: input.to_path_buf(),
        output: output.to_path_buf(),
        warnings: retained_warnings,
        information_loss,
        association,
        cel_reuse: encoded.cel_reuse,
        jitter,
        active_frame_index: document.active_frame_index,
    })
}

/// Resolves the internal round-trip preset while preserving automatic fallback semantics.
fn resolve_roundtrip_association(
    layout: roundtrip::RoundTripLayout,
) -> Result<(bool, LayerAssociation), ConversionError> {
    if layout.status.marked && !layout.status.valid {
        return Err(ConversionError::RoundTripRecoveryRequired(
            "converter-owned frame-group metadata is missing, damaged, or inconsistent".to_string(),
        ));
    }
    if layout.version == Some(2) {
        return Ok((true, LayerAssociation::Preserve));
    }
    Ok((
        false,
        LayerAssociation::Auto(AutoAssociationOptions::default()),
    ))
}

/// Parses output bytes and validates the format-independent document header.
fn read_and_validate_output_header(
    bytes: &[u8],
    document: &NormalizedDocument,
) -> Result<aseprite::AsepriteFile, ConversionError> {
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
        let actual = u32::from(file.frames()[index].duration_ms);
        if canonical_frame_duration_ms(actual) != canonical_frame_duration_ms(expected) {
            return Err(ConversionError::OutputValidation(format!(
                "frame {index} duration differs: expected {expected}, got {}",
                file.frames()[index].duration_ms
            )));
        }
    }
    Ok(file)
}

/// Returns the ten-millisecond duration quantum used by Photoshop animation data.
fn canonical_frame_duration_ms(duration_ms: u32) -> u32 {
    duration_ms / 10 * 10
}

/// Merges frame-local visibility from duplicated Frame groups into representatives.
fn merge_frame_group_states(document: &mut NormalizedDocument) -> Result<(), String> {
    let snapshots = document.root_layers.clone();
    let first = snapshots
        .first()
        .ok_or_else(|| "frame-group document has no roots".to_string())?;
    for child_index in 0..first.children.len() {
        let sources = snapshots
            .iter()
            .map(|root| root.children.get(child_index))
            .collect::<Option<Vec<_>>>()
            .ok_or_else(|| "frame-group topology differs between frames".to_string())?;
        let target = document
            .root_layers
            .get_mut(0)
            .and_then(|root| root.children.get_mut(child_index))
            .ok_or_else(|| "frame-group topology differs between frames".to_string())?;
        merge_frame_group_layer_states(target, &sources)?;
    }
    Ok(())
}

/// Recursively merges one representative's frame-local visibility states.
fn merge_frame_group_layer_states(
    target: &mut NormalizedLayer,
    sources: &[&NormalizedLayer],
) -> Result<(), String> {
    if sources
        .iter()
        .any(|source| source.name != target.name || source.kind != target.kind)
    {
        return Err("frame-group layer names or kinds differ between frames".to_string());
    }
    target.frame_states = sources
        .iter()
        .enumerate()
        .map(|(frame_index, source)| NormalizedLayerFrameState {
            frame_index: frame_index as u32,
            record_present: true,
            enabled: source
                .frame_states
                .first()
                .map_or(!source.hidden.unwrap_or(false), |state| state.enabled),
            explicit_enable: true,
            offset: source.frame_states.first().and_then(|state| state.offset),
            reference_point: source
                .frame_states
                .first()
                .and_then(|state| state.reference_point),
            opacity: source.frame_states.first().and_then(|state| state.opacity),
        })
        .collect();
    if target.kind == NormalizedLayerKind::Group {
        for child_index in 0..target.children.len() {
            let child_sources = sources
                .iter()
                .map(|source| source.children.get(child_index))
                .collect::<Option<Vec<_>>>()
                .ok_or_else(|| "frame-group topology differs between frames".to_string())?;
            merge_frame_group_layer_states(&mut target.children[child_index], &child_sources)?;
        }
    }
    Ok(())
}

/// Builds an exact write plan from converter-owned Frame group snapshots.
fn build_frame_group_roundtrip_plan(
    document: &NormalizedDocument,
) -> Result<LayerWritePlan, String> {
    if document.frames.is_empty() || document.root_layers.len() != document.frames.len() {
        return Err("frame-group root count does not match the animation frame count".to_string());
    }
    for (index, layer) in document.root_layers.iter().enumerate() {
        if layer.kind != NormalizedLayerKind::Group || layer.name != format!("Frame {}", index + 1)
        {
            return Err("frame-group roots are missing or out of order".to_string());
        }
    }
    let mut tracks = Vec::new();
    let mut root_nodes = Vec::new();
    let first_children = &document.root_layers[0].children;
    for child_index in 0..first_children.len() {
        let layers = document
            .root_layers
            .iter()
            .map(|root| root.children.get(child_index))
            .collect::<Option<Vec<_>>>()
            .ok_or_else(|| "frame-group topology differs between frames".to_string())?;
        root_nodes.push(build_frame_group_node(
            &layers,
            &mut tracks,
            &mut Vec::new(),
        )?);
    }
    let track_count = tracks.len();
    Ok(LayerWritePlan {
        root_nodes,
        tracks,
        report: AssociationReport {
            observation_count: track_count,
            track_count,
            omitted_source_layer_ids: Vec::new(),
            z_order_mode: LayerZOrderMode::Stable,
            stable_order_mode: StableOrderMode::Consensus,
            uncertain_layer_mode: UncertainLayerMode::Group,
            strategy: AssociationStrategy::Compact,
            name_catalog_version: COPY_SUFFIX_CATALOG_VERSION,
            z_order_diagnostics: Vec::new(),
            stable_order_diagnostics: vec!["exact converter-owned frame-group mapping".to_string()],
            candidate_groups: Vec::new(),
            decisions: Vec::new(),
            warnings: Vec::new(),
        },
    })
}

/// Recursively maps corresponding layers from every Frame group into one plan node.
fn build_frame_group_node(
    layers: &[&NormalizedLayer],
    tracks: &mut Vec<LogicalLayerTrack>,
    group_path: &mut Vec<String>,
) -> Result<PlannedNode, String> {
    let first = layers
        .first()
        .ok_or_else(|| "frame-group layer is missing".to_string())?;
    if layers.iter().any(|layer| {
        layer.name != first.name
            || layer.kind != first.kind
            || layer.children.len() != first.children.len()
    }) {
        return Err("frame-group layer names or kinds differ between frames".to_string());
    }
    match first.kind {
        NormalizedLayerKind::Group => {
            let mut children = Vec::new();
            group_path.push(first.name.clone());
            for child_index in 0..first.children.len() {
                let child_layers = layers
                    .iter()
                    .map(|layer| layer.children.get(child_index))
                    .collect::<Option<Vec<_>>>()
                    .ok_or_else(|| "frame-group topology differs between frames".to_string())?;
                children.push(build_frame_group_node(&child_layers, tracks, group_path)?);
            }
            group_path.pop();
            Ok(PlannedNode::Group {
                name: first.name.clone(),
                source_layer_id: Some(first.id),
                children,
            })
        }
        NormalizedLayerKind::Pixel => {
            let id = tracks.len();
            let cels = layers
                .iter()
                .enumerate()
                .map(|(frame_index, layer)| {
                    let enabled = layer
                        .frame_states
                        .first()
                        .map_or(!layer.hidden.unwrap_or(false), |state| state.enabled);
                    enabled.then_some(PlannedCel {
                        source_layer_id: layer.id,
                        source_frame_index: frame_index as u32,
                        z_index: 0,
                    })
                })
                .collect();
            tracks.push(LogicalLayerTrack {
                id,
                name: first.name.clone(),
                representative_source_layer_id: first.id,
                group_path: group_path.clone(),
                cels,
            });
            Ok(PlannedNode::Track { track_id: id })
        }
    }
}

/// Validates an Aseprite file produced from the experimental logical plan.
fn validate_planned_aseprite_output(
    bytes: &[u8],
    document: &NormalizedDocument,
    plan: &LayerWritePlan,
    linked_cels: LinkedCelMode,
    jitter: &JitterPlan,
) -> Result<(), ConversionError> {
    let file = read_and_validate_output_header(bytes, document)?;

    let mut layers = Vec::new();
    flatten_planned_nodes(&plan.root_nodes, None, &mut layers);
    if file.layers().len() != layers.len() {
        return Err(ConversionError::OutputValidation(format!(
            "logical layer count differs: expected {}, got {}",
            layers.len(),
            file.layers().len()
        )));
    }
    for (layer_index, (node, expected_parent)) in layers.iter().enumerate() {
        let output_layer = &file.layers()[layer_index];
        if output_layer.parent != *expected_parent {
            return Err(ConversionError::OutputValidation(format!(
                "logical layer {layer_index} parent differs"
            )));
        }
        match node {
            PlannedNode::Group { name, .. } => {
                if output_layer.name != *name || output_layer.kind != aseprite::LayerKind::Group {
                    return Err(ConversionError::OutputValidation(format!(
                        "logical group {layer_index} attributes differ"
                    )));
                }
            }
            PlannedNode::Track { track_id } => {
                let track = plan.tracks.get(*track_id).ok_or_else(|| {
                    ConversionError::OutputValidation(format!(
                        "logical track {track_id} is not present in plan"
                    ))
                })?;
                if output_layer.name != track.name
                    || output_layer.kind != aseprite::LayerKind::Normal
                    || output_layer.visible != track.cels.iter().any(Option::is_some)
                {
                    return Err(ConversionError::OutputValidation(format!(
                        "logical track {track_id} attributes differ"
                    )));
                }
                let output_handle = file.layer_ref(layer_index).ok_or_else(|| {
                    ConversionError::OutputValidation(format!(
                        "logical track {track_id} cannot be addressed"
                    ))
                })?;
                for frame_index in 0..document.frames.len() {
                    let expected = track.cels[frame_index];
                    let actual = file.cel(output_handle, frame_index);
                    if expected.is_some() != actual.is_some() {
                        return Err(ConversionError::OutputValidation(format!(
                            "logical track {track_id} frame {frame_index} cel visibility differs"
                        )));
                    }
                    if let (Some(expected), Some(actual)) = (expected, actual) {
                        let source =
                            document
                                .find_layer(expected.source_layer_id)
                                .ok_or_else(|| {
                                    ConversionError::OutputValidation(format!(
                                        "source layer {} is missing",
                                        expected.source_layer_id
                                    ))
                                })?;
                        validate_cel(
                            &file,
                            output_handle,
                            actual,
                            document,
                            source,
                            frame_index,
                            linked_cels,
                            jitter,
                        )?;
                        if actual.z_index != expected.z_index {
                            return Err(ConversionError::OutputValidation(format!(
                                "logical track {track_id} frame {frame_index} z-index differs"
                            )));
                        }
                    }
                }
            }
        }
    }
    Ok(())
}

/// Flattens a logical plan in the same order used to create Aseprite layers.
fn flatten_planned_nodes<'a>(
    nodes: &'a [PlannedNode],
    parent: Option<usize>,
    output: &mut Vec<(&'a PlannedNode, Option<usize>)>,
) {
    for node in nodes {
        let index = output.len();
        output.push((node, parent));
        if let PlannedNode::Group { children, .. } = node {
            flatten_planned_nodes(children, Some(index), output);
        }
    }
}

/// Validates the encoded Aseprite structure against the normalized source model.
fn validate_aseprite_output(
    bytes: &[u8],
    document: &NormalizedDocument,
    linked_cels: LinkedCelMode,
    jitter: &JitterPlan,
) -> Result<(), ConversionError> {
    let file = read_and_validate_output_header(bytes, document)?;

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
                    let has_pixels = source
                        .pixels
                        .as_ref()
                        .is_some_and(|pixels| pixels.width > 0 && pixels.height > 0);
                    let should_have_cel =
                        has_pixels && is_visible_pixel(document, source.id, frame_index);
                    let cel = file.cel(output_handle, frame_index);
                    if should_have_cel != cel.is_some() {
                        return Err(ConversionError::OutputValidation(format!(
                            "layer {} frame {frame_index} cel visibility differs",
                            source.id
                        )));
                    }
                    if let Some(cel) = cel {
                        validate_cel(
                            &file,
                            output_handle,
                            cel,
                            document,
                            source,
                            frame_index,
                            linked_cels,
                            jitter,
                        )?;
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
#[allow(clippy::too_many_arguments)]
fn validate_cel(
    file: &aseprite::AsepriteFile,
    layer: aseprite::LayerRef,
    cel: &aseprite::Cel,
    document: &NormalizedDocument,
    source: &NormalizedLayer,
    frame_index: usize,
    linked_cels: LinkedCelMode,
    jitter: &JitterPlan,
) -> Result<(), ConversionError> {
    let expected_state = source.frame_states.get(frame_index).ok_or_else(|| {
        ConversionError::OutputValidation(format!("missing source frame state {frame_index}"))
    })?;
    let pixels = resolved_pixels(document, jitter, source.id).ok_or_else(|| {
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
    let expected_position = aseprite_writer::cel_position(&pixels, expected_state)
        .map_err(|error| ConversionError::OutputValidation(error.to_string()))?;
    let (output_pixels, x, y) = match &cel.kind {
        aseprite::CelKind::Raw { pixels, x, y }
        | aseprite::CelKind::Compressed { pixels, x, y, .. } => (pixels, *x, *y),
        aseprite::CelKind::Linked { source_frame, x, y } => {
            if linked_cels != LinkedCelMode::Identical {
                return Err(ConversionError::OutputValidation(format!(
                    "layer {} frame {frame_index} unexpectedly contains a linked cel",
                    source.id
                )));
            }
            if *source_frame >= frame_index {
                return Err(ConversionError::OutputValidation(format!(
                    "layer {} frame {frame_index} linked cel does not point backward",
                    source.id
                )));
            }
            let source_cel = file.cel(layer, *source_frame).ok_or_else(|| {
                ConversionError::OutputValidation(format!(
                    "layer {} frame {frame_index} linked cel source is missing",
                    source.id
                ))
            })?;
            if matches!(source_cel.kind, aseprite::CelKind::Linked { .. }) {
                return Err(ConversionError::OutputValidation(format!(
                    "layer {} frame {frame_index} linked cel points to another linked cel",
                    source.id
                )));
            }
            let resolved = file.resolve_cel(layer, frame_index).ok_or_else(|| {
                ConversionError::OutputValidation(format!(
                    "layer {} frame {frame_index} linked cel cannot be resolved",
                    source.id
                ))
            })?;
            let output_pixels = match &resolved.kind {
                aseprite::CelKind::Raw { pixels, .. }
                | aseprite::CelKind::Compressed { pixels, .. } => pixels,
                _ => {
                    return Err(ConversionError::OutputValidation(format!(
                        "layer {} frame {frame_index} linked cel resolves to a non-pixel cel",
                        source.id
                    )));
                }
            };
            (output_pixels, *x, *y)
        }
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

#[cfg(test)]
#[path = "tests/core.rs"]
mod tests;
