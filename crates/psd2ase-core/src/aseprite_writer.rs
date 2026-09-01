use std::collections::{HashMap, HashSet};

use aseprite::{
    AsepriteFile, BlendMode, CelOptions, ColorMode, GroupRef, LayerOptions, LayerRef,
    LinkedCelOptions, LoopDirection, Pixels,
};

use crate::aseprite_metadata::{active_frame_user_data, reference_point_user_data};
use crate::photoshop_metadata::has_meaningful_reference_point;
use crate::{
    InformationLocation, InformationLossCode, LayerWritePlan, LogicalLayerTrack, LossDisposition,
    NormalizedDocument, NormalizedLayer, NormalizedLayerFrameState, NormalizedLayerKind,
    NormalizedLoopMode, NormalizedPixels, PlannedNode,
    jitter::{JitterPlan, resolved_pixels},
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
    /// Structured warning metadata used to build the information-loss report.
    pub(crate) warning_details: Vec<WriterWarning>,
    /// Counts of ordinary and linked cels emitted by the serializer.
    pub cel_reuse: CelReuseReport,
}

/// A non-fatal writer warning with its source loss classification and location.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct WriterWarning {
    /// Human-readable warning detail retained in the conversion report.
    pub(crate) message: String,
    /// Stable report code for this warning.
    pub(crate) code: InformationLossCode,
    /// Report disposition for this warning.
    pub(crate) disposition: LossDisposition,
    /// Source location affected by this warning.
    pub(crate) location: InformationLocation,
    /// Whether the warning can change rendered pixels.
    pub(crate) visual_impact: bool,
    /// Whether the warning can change editability.
    pub(crate) editability_impact: bool,
}

/// Collects writer warnings while keeping their presentation and source metadata together.
#[derive(Debug, Default)]
struct WarningCollector {
    entries: Vec<WriterWarning>,
}

impl WarningCollector {
    /// Records one structured writer warning.
    fn push(
        &mut self,
        code: InformationLossCode,
        disposition: LossDisposition,
        location: InformationLocation,
        message: impl Into<String>,
        visual_impact: bool,
        editability_impact: bool,
    ) {
        self.entries.push(WriterWarning {
            message: message.into(),
            code,
            disposition,
            location,
            visual_impact,
            editability_impact,
        });
    }

    /// Splits structured warnings into legacy text and source-aware details.
    fn into_parts(self) -> (Vec<String>, Vec<WriterWarning>) {
        let messages = self
            .entries
            .iter()
            .map(|warning| warning.message.clone())
            .collect();
        (messages, self.entries)
    }
}

/// Summarizes identical-pixel cel reuse performed by the serializer.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct CelReuseReport {
    /// Number of cels containing their own pixel data.
    pub pixel_cel_count: usize,
    /// Number of cels linked to an earlier cel on the same layer.
    pub linked_cel_count: usize,
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

/// Initializes the shared canvas, timeline, loop tag, and warning collection.
fn initialize_file(
    document: &NormalizedDocument,
    preserve_photoshop_metadata: bool,
) -> Result<(AsepriteFile, WarningCollector), WriterError> {
    let width = u16_value("canvas width", document.canvas.0)?;
    let height = u16_value("canvas height", document.canvas.1)?;
    let mut file = AsepriteFile::new(width, height, ColorMode::Rgba);
    let mut warnings = WarningCollector::default();
    collect_unmapped_animation_warnings(document, &mut warnings, preserve_photoshop_metadata);

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
    if let Some(user_data) =
        active_frame_user_data(document.active_frame_index, document.frames.len())
    {
        file.set_sprite_user_data(user_data);
    }

    Ok((file, warnings))
}

/// Encodes one normalized document as an RGBA Aseprite file.
pub fn encode(document: &NormalizedDocument) -> Result<EncodedAseprite, WriterError> {
    encode_with_linked_cels(document, crate::LinkedCelMode::Off)
}

/// Encodes one normalized document with the selected linked-cel policy.
pub fn encode_with_linked_cels(
    document: &NormalizedDocument,
    linked_cels: crate::LinkedCelMode,
) -> Result<EncodedAseprite, WriterError> {
    encode_with_linked_cels_and_jitter(document, linked_cels, &JitterPlan::default())
}

/// Encodes a normalized document using resolved jitter pixels.
pub fn encode_with_linked_cels_and_jitter(
    document: &NormalizedDocument,
    linked_cels: crate::LinkedCelMode,
    jitter: &JitterPlan,
) -> Result<EncodedAseprite, WriterError> {
    encode_with_linked_cels_and_jitter_and_metadata(document, linked_cels, jitter, false)
}

/// Encodes normalized layers with optional Photoshop round-trip metadata.
pub(crate) fn encode_with_linked_cels_and_jitter_and_metadata(
    document: &NormalizedDocument,
    linked_cels: crate::LinkedCelMode,
    jitter: &JitterPlan,
    preserve_photoshop_metadata: bool,
) -> Result<EncodedAseprite, WriterError> {
    let (mut file, mut warnings) = initialize_file(document, preserve_photoshop_metadata)?;

    let mut bindings = Vec::new();
    for layer in &document.root_layers {
        let path = layer_path(None, layer);
        create_layer_tree(
            &mut file,
            layer,
            None,
            path,
            &mut bindings,
            &mut warnings,
            preserve_photoshop_metadata,
        )?;
    }

    let mut reuse = (0..bindings.len())
        .map(|_| CelReuseTracker::new(linked_cels))
        .collect::<Vec<_>>();
    for frame in &document.frames {
        let frame_index = frame.index as usize;
        let mut visible_ids = Vec::new();
        for layer in &document.root_layers {
            layer.collect_visible_pixel_layer_ids(frame_index, true, &mut visible_ids);
        }
        let visible_ids = visible_ids.into_iter().collect::<HashSet<_>>();
        for (binding_index, binding) in bindings.iter().enumerate() {
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
            if pixels.width == 0 || pixels.height == 0 {
                continue;
            }
            let position = cel_position(pixels, state)?;
            let resolved = resolved_pixels(document, jitter, binding.layer.id)
                .unwrap_or_else(|| pixels.clone());
            let ase_pixels = aseprite_pixels(binding.layer.id, &resolved)?;
            let opacity = normalized_opacity(
                state.opacity.or(binding.layer.opacity),
                format!("layer {} frame {frame_index}", binding.layer.id),
                with_frame(&binding.location, frame_index as u32),
                &mut warnings,
            )?;
            emit_cel(
                &mut file,
                binding.handle,
                frame_index,
                PreparedCel {
                    pixels: ase_pixels,
                    x: position.0,
                    y: position.1,
                    opacity,
                    z_index: 0,
                },
                &mut reuse[binding_index],
            )?;
        }
    }

    let mut bytes = Vec::new();
    file.write_to(&mut bytes)
        .map_err(|error| WriterError::Aseprite(error.to_string()))?;
    let (warnings, warning_details) = warnings.into_parts();
    Ok(EncodedAseprite {
        bytes,
        warnings,
        warning_details,
        cel_reuse: reuse.into_iter().map(|tracker| tracker.report).fold(
            CelReuseReport::default(),
            |mut total, report| {
                total.pixel_cel_count += report.pixel_cel_count;
                total.linked_cel_count += report.linked_cel_count;
                total
            },
        ),
    })
}

/// Encodes a normalized document using an experimental logical-layer plan.
pub fn encode_with_plan(
    document: &NormalizedDocument,
    plan: &LayerWritePlan,
) -> Result<EncodedAseprite, WriterError> {
    encode_with_plan_and_linked_cels(document, plan, crate::LinkedCelMode::Off)
}

/// Encodes a logical-layer plan with the selected linked-cel policy.
pub fn encode_with_plan_and_linked_cels(
    document: &NormalizedDocument,
    plan: &LayerWritePlan,
    linked_cels: crate::LinkedCelMode,
) -> Result<EncodedAseprite, WriterError> {
    encode_with_plan_and_linked_cels_and_jitter(document, plan, linked_cels, &JitterPlan::default())
}

/// Encodes a logical-layer plan using resolved jitter pixels.
pub fn encode_with_plan_and_linked_cels_and_jitter(
    document: &NormalizedDocument,
    plan: &LayerWritePlan,
    linked_cels: crate::LinkedCelMode,
    jitter: &JitterPlan,
) -> Result<EncodedAseprite, WriterError> {
    encode_with_plan_and_linked_cels_and_jitter_and_metadata(
        document,
        plan,
        linked_cels,
        jitter,
        false,
    )
}

/// Encodes a logical-layer plan with optional Photoshop round-trip metadata.
pub(crate) fn encode_with_plan_and_linked_cels_and_jitter_and_metadata(
    document: &NormalizedDocument,
    plan: &LayerWritePlan,
    linked_cels: crate::LinkedCelMode,
    jitter: &JitterPlan,
    preserve_photoshop_metadata: bool,
) -> Result<EncodedAseprite, WriterError> {
    let (mut file, mut warnings) = initialize_file(document, preserve_photoshop_metadata)?;

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
            preserve_photoshop_metadata,
        )?;
    }
    let mut reuse = (0..bindings.len())
        .map(|_| CelReuseTracker::new(linked_cels))
        .collect::<Vec<_>>();
    for frame_index in 0..document.frames.len() {
        for (binding_index, binding) in bindings.iter().enumerate() {
            let Some(planned_cel) = binding.track.cels[frame_index] else {
                continue;
            };
            if planned_cel.source_frame_index as usize != frame_index {
                return Err(WriterError::InvalidFrameIndex {
                    expected: frame_index,
                    actual: planned_cel.source_frame_index,
                });
            }
            let source = document
                .find_layer(planned_cel.source_layer_id)
                .ok_or_else(|| WriterError::InvalidPixels {
                    layer_id: planned_cel.source_layer_id,
                    message: "planned source layer was not found".to_string(),
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
            let resolved =
                resolved_pixels(document, jitter, source.id).unwrap_or_else(|| pixels.clone());
            let ase_pixels = aseprite_pixels(source.id, &resolved)?;
            let opacity = normalized_opacity(
                state.opacity.or(source.opacity),
                format!("layer {} frame {frame_index}", source.id),
                with_frame(&binding.location, frame_index as u32),
                &mut warnings,
            )?;
            emit_cel(
                &mut file,
                binding.handle,
                frame_index,
                PreparedCel {
                    pixels: ase_pixels,
                    x: position.0,
                    y: position.1,
                    opacity,
                    z_index: planned_cel.z_index,
                },
                &mut reuse[binding_index],
            )?;
        }
    }

    let mut bytes = Vec::new();
    file.write_to(&mut bytes)
        .map_err(|error| WriterError::Aseprite(error.to_string()))?;
    let (warnings, warning_details) = warnings.into_parts();
    Ok(EncodedAseprite {
        bytes,
        warnings,
        warning_details,
        cel_reuse: reuse.into_iter().map(|tracker| tracker.report).fold(
            CelReuseReport::default(),
            |mut total, report| {
                total.pixel_cel_count += report.pixel_cel_count;
                total.linked_cel_count += report.linked_cel_count;
                total
            },
        ),
    })
}

/// Input attributes needed to emit one ordinary or linked cel.
struct PreparedCel {
    pixels: Pixels,
    x: i16,
    y: i16,
    opacity: u8,
    z_index: i16,
}

/// Identifies pixel data before the full byte comparison confirms a match.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
struct PixelFingerprint {
    width: u16,
    height: u16,
    hash: u64,
}

/// A prior ordinary cel that can be used as a linked-cel source.
struct ReusableCel {
    source_frame: usize,
    width: u16,
    height: u16,
    rgba: Vec<u8>,
}

/// Tracks reusable ordinary cels independently for one output layer.
struct CelReuseTracker {
    mode: crate::LinkedCelMode,
    sources: HashMap<PixelFingerprint, Vec<ReusableCel>>,
    report: CelReuseReport,
}

impl CelReuseTracker {
    /// Creates an empty tracker for one output layer.
    fn new(mode: crate::LinkedCelMode) -> Self {
        Self {
            mode,
            sources: HashMap::new(),
            report: CelReuseReport::default(),
        }
    }
}

/// Describes how one cel was stored.
enum CelEmission {
    Pixel,
    Linked,
}

/// Emits one cel and optionally links it to the first identical ordinary cel.
fn emit_cel(
    file: &mut AsepriteFile,
    layer: LayerRef,
    frame: usize,
    cel: PreparedCel,
    reuse: &mut CelReuseTracker,
) -> Result<CelEmission, WriterError> {
    let width = cel.pixels.width;
    let height = cel.pixels.height;
    let fingerprint = PixelFingerprint {
        width,
        height,
        hash: fnv1a(&cel.pixels.data),
    };
    if reuse.mode == crate::LinkedCelMode::Identical
        && let Some(source) = reuse.sources.get(&fingerprint).and_then(|sources| {
            sources.iter().find(|source| {
                source.width == width && source.height == height && source.rgba == cel.pixels.data
            })
        })
    {
        let source_frame = source.source_frame;
        file.set_linked_cel_with(
            layer,
            frame,
            source_frame,
            LinkedCelOptions {
                x: cel.x,
                y: cel.y,
                opacity: cel.opacity,
                z_index: cel.z_index,
                ..Default::default()
            },
        )
        .map_err(|error| WriterError::Aseprite(error.to_string()))?;
        reuse.report.linked_cel_count += 1;
        return Ok(CelEmission::Linked);
    }

    if cel.opacity == 255 && cel.z_index == 0 {
        file.set_raw_cel(layer, frame, cel.pixels, cel.x, cel.y)
            .map_err(|error| WriterError::Aseprite(error.to_string()))?;
    } else {
        file.set_cel_with(
            layer,
            frame,
            CelOptions {
                pixels: cel.pixels,
                x: cel.x,
                y: cel.y,
                opacity: cel.opacity,
                z_index: cel.z_index,
            },
        )
        .map_err(|error| WriterError::Aseprite(error.to_string()))?;
    }
    reuse.report.pixel_cel_count += 1;
    if reuse.mode == crate::LinkedCelMode::Identical {
        reuse
            .sources
            .entry(fingerprint)
            .or_default()
            .push(ReusableCel {
                source_frame: frame,
                width,
                height,
                rgba: file
                    .resolve_cel(layer, frame)
                    .and_then(|cel| match &cel.kind {
                        aseprite::CelKind::Raw { pixels, .. }
                        | aseprite::CelKind::Compressed { pixels, .. } => Some(pixels.data.clone()),
                        _ => None,
                    })
                    .unwrap_or_default(),
            });
    }
    Ok(CelEmission::Pixel)
}

/// Computes a deterministic FNV-1a hash for a pixel buffer.
fn fnv1a(bytes: &[u8]) -> u64 {
    bytes.iter().fold(0xcbf29ce484222325, |hash, byte| {
        (hash ^ u64::from(*byte)).wrapping_mul(0x100000001b3)
    })
}

/// Associates every normalized pixel layer with its newly-created Aseprite layer.
fn create_layer_tree<'a>(
    file: &mut AsepriteFile,
    layer: &'a NormalizedLayer,
    parent: Option<GroupRef>,
    path: String,
    bindings: &mut Vec<PixelBinding<'a>>,
    warnings: &mut WarningCollector,
    preserve_photoshop_metadata: bool,
) -> Result<(), WriterError> {
    let location = layer_location(&path, layer.id);
    let options = layer_options(layer, &location, warnings)?;
    match layer.kind {
        NormalizedLayerKind::Group => {
            let group = match parent {
                Some(parent) => file.add_group_in_with(&layer.name, parent, options),
                None => file.add_group_with(&layer.name, options),
            };
            if preserve_photoshop_metadata {
                if let Some(user_data) = reference_point_user_data(layer, layer.frame_states.len())
                {
                    file.set_group_user_data(group, user_data);
                }
            }
            for child in &layer.children {
                create_layer_tree(
                    file,
                    child,
                    Some(group),
                    layer_path(Some(&path), child),
                    bindings,
                    warnings,
                    preserve_photoshop_metadata,
                )?;
            }
        }
        NormalizedLayerKind::Pixel => {
            let handle = match parent {
                Some(parent) => file.add_layer_in_with(&layer.name, parent, options),
                None => file.add_layer_with(&layer.name, options),
            };
            if preserve_photoshop_metadata {
                if let Some(user_data) = reference_point_user_data(layer, layer.frame_states.len())
                {
                    file.set_layer_user_data(handle, user_data);
                }
            }
            bindings.push(PixelBinding {
                layer,
                handle,
                location: location.clone(),
            });
            if !layer.children.is_empty() {
                warnings.push(
                    InformationLossCode::PixelLayerChildren,
                    LossDisposition::Dropped,
                    location,
                    format!(
                        "pixel layer {} has children; children were not serialized",
                        layer.id
                    ),
                    true,
                    true,
                );
            }
        }
    }
    Ok(())
}

/// Stores a normalized pixel layer and its Aseprite handle for cel creation.
struct PixelBinding<'a> {
    layer: &'a NormalizedLayer,
    handle: LayerRef,
    location: InformationLocation,
}

/// Associates one planned logical track with its output Aseprite layer.
struct PlannedBinding<'a> {
    track: &'a LogicalLayerTrack,
    handle: LayerRef,
    location: InformationLocation,
}

/// Creates the output tree described by a logical-layer plan.
fn create_planned_tree<'a>(
    file: &mut AsepriteFile,
    node: &'a PlannedNode,
    parent: Option<GroupRef>,
    document: &NormalizedDocument,
    plan: &'a LayerWritePlan,
    bindings: &mut Vec<PlannedBinding<'a>>,
    warnings: &mut WarningCollector,
    preserve_photoshop_metadata: bool,
) -> Result<(), WriterError> {
    match node {
        PlannedNode::Group {
            name,
            source_layer_id,
            children,
        } => {
            let source = source_layer_id.and_then(|id| document.find_layer(id));
            let source_location = source.map(|layer| find_layer_location(document, layer.id));
            let mut options = source
                .zip(source_location.as_ref())
                .map(|(layer, location)| layer_options(layer, location, warnings))
                .transpose()?
                .unwrap_or_default();
            options.visible = true;
            let group = match parent {
                Some(parent) => file.add_group_in_with(name, parent, options),
                None => file.add_group_with(name, options),
            };
            if preserve_photoshop_metadata {
                if let Some(source) = source {
                    if let Some(user_data) =
                        reference_point_user_data(source, document.frames.len())
                    {
                        file.set_group_user_data(group, user_data);
                    }
                }
            }
            for child in children {
                create_planned_tree(
                    file,
                    child,
                    Some(group),
                    document,
                    plan,
                    bindings,
                    warnings,
                    preserve_photoshop_metadata,
                )?;
            }
        }
        PlannedNode::Track { track_id } => {
            let track = plan.tracks.get(*track_id).ok_or_else(|| {
                WriterError::Aseprite(format!("logical track {track_id} is not present in plan"))
            })?;
            let source = document
                .find_layer(track.representative_source_layer_id)
                .ok_or_else(|| WriterError::InvalidPixels {
                    layer_id: track.representative_source_layer_id,
                    message: "logical track representative layer was not found".to_string(),
                })?;
            let location = find_layer_location(document, source.id);
            let mut options = layer_options(source, &location, warnings)?;
            options.visible = track.cels.iter().any(Option::is_some);
            let handle = match parent {
                Some(parent) => file.add_layer_in_with(&track.name, parent, options),
                None => file.add_layer_with(&track.name, options),
            };
            if preserve_photoshop_metadata {
                if let Some(user_data) = reference_point_user_data(source, document.frames.len()) {
                    file.set_layer_user_data(handle, user_data);
                }
            }
            bindings.push(PlannedBinding {
                track,
                handle,
                location,
            });
        }
    }
    Ok(())
}

/// Maps base layer properties to Aseprite layer options.
fn layer_options(
    layer: &NormalizedLayer,
    location: &InformationLocation,
    warnings: &mut WarningCollector,
) -> Result<LayerOptions, WriterError> {
    Ok(LayerOptions {
        opacity: normalized_opacity(
            layer.opacity,
            format!("layer {}", layer.id),
            location.clone(),
            warnings,
        )?,
        blend_mode: blend_mode(layer.blend_mode.as_deref(), location.clone(), warnings),
        visible: layer.frame_states.iter().any(|state| state.enabled),
        ..LayerOptions::default()
    })
}

/// Maps an Aseprite-compatible blend-mode name, warning when it cannot be preserved.
fn blend_mode(
    value: Option<&str>,
    location: InformationLocation,
    warnings: &mut WarningCollector,
) -> BlendMode {
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
            let layer_id = location.layer_id.unwrap_or_default();
            warnings.push(
                InformationLossCode::UnknownBlendMode,
                LossDisposition::Degraded,
                location,
                format!("layer {layer_id} blend mode {other:?} mapped to normal"),
                true,
                true,
            );
            BlendMode::Normal
        }
    }
}

/// Converts normalized opacity in the 0.0..=1.0 range to Aseprite's 8-bit representation.
fn normalized_opacity(
    value: Option<f64>,
    field: String,
    location: InformationLocation,
    warnings: &mut WarningCollector,
) -> Result<u8, WriterError> {
    let value = value.unwrap_or(1.0);
    let opacity = opacity_to_u8(Some(value), &field)?;
    if (value * 255.0) != f64::from(opacity) {
        warnings.push(
            InformationLossCode::OpacityQuantization,
            LossDisposition::Degraded,
            location,
            format!("{field} opacity {value} quantized to {opacity}/255 for Aseprite"),
            true,
            true,
        );
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
fn collect_unmapped_animation_warnings(
    document: &NormalizedDocument,
    warnings: &mut WarningCollector,
    preserve_photoshop_metadata: bool,
) {
    for layer in &document.root_layers {
        collect_layer_animation_warnings(layer, None, warnings, preserve_photoshop_metadata);
    }
    // Active frame is stored as sprite metadata and applied by the Lua adapter.
}

/// Counts unsupported frame-local properties recursively.
fn collect_layer_animation_warnings(
    layer: &NormalizedLayer,
    parent_path: Option<&str>,
    warnings: &mut WarningCollector,
    preserve_photoshop_metadata: bool,
) {
    let path = layer_path(parent_path, layer);
    let location = layer_location(&path, layer.id);
    for state in &layer.frame_states {
        if !preserve_photoshop_metadata
            && state.reference_point.is_some()
            && has_meaningful_reference_point(layer, state.frame_index as usize)
        {
            warnings.push(
                InformationLossCode::ReferencePoint,
                LossDisposition::Dropped,
                with_frame(&location, state.frame_index),
                format!(
                    "layer {} frame {} reference point was not serialized",
                    layer.id, state.frame_index
                ),
                false,
                true,
            );
        }
        if layer.kind == NormalizedLayerKind::Group && state.opacity.is_some() {
            warnings.push(
                InformationLossCode::GroupFrameOpacity,
                LossDisposition::Dropped,
                with_frame(&location, state.frame_index),
                format!(
                    "layer {} frame {} group frame opacity override was not serialized",
                    layer.id, state.frame_index
                ),
                true,
                true,
            );
        }
    }
    for child in &layer.children {
        collect_layer_animation_warnings(child, Some(&path), warnings, preserve_photoshop_metadata);
    }
}

/// Builds a human-readable source path from normalized layer names.
fn layer_path(parent: Option<&str>, layer: &NormalizedLayer) -> String {
    let segment = if layer.name.is_empty() {
        "<unnamed>"
    } else {
        layer.name.as_str()
    };
    match parent {
        Some(parent) if !parent.is_empty() => format!("{parent}/{segment}"),
        _ => segment.to_string(),
    }
}

/// Builds a location for a normalized source layer.
fn layer_location(path: &str, layer_id: u32) -> InformationLocation {
    InformationLocation {
        layer_id: Some(layer_id),
        path: path.to_string(),
        frame_index: None,
    }
}

/// Adds a normalized frame index to an existing source location.
fn with_frame(location: &InformationLocation, frame_index: u32) -> InformationLocation {
    InformationLocation {
        frame_index: Some(frame_index),
        ..location.clone()
    }
}

/// Finds a normalized source layer location by its stable layer identifier.
fn find_layer_location(document: &NormalizedDocument, layer_id: u32) -> InformationLocation {
    fn find(
        layers: &[NormalizedLayer],
        parent_path: Option<&str>,
        layer_id: u32,
    ) -> Option<InformationLocation> {
        for layer in layers {
            let path = layer_path(parent_path, layer);
            if layer.id == layer_id {
                return Some(layer_location(&path, layer.id));
            }
            if let Some(found) = find(&layer.children, Some(&path), layer_id) {
                return Some(found);
            }
        }
        None
    }

    find(&document.root_layers, None, layer_id)
        .unwrap_or_else(|| layer_location(&format!("<unknown:{layer_id}>"), layer_id))
}

#[cfg(test)]
#[path = "tests/aseprite_writer.rs"]
mod tests;
