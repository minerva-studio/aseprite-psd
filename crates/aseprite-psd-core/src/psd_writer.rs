//! PSD/PSB writer and read-back validator for normalized Aseprite exports.

use std::collections::{HashMap, HashSet};
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::path::{Path, PathBuf};

use ag_psd::descriptor::{
    Descriptor, DescriptorValue, UnitDoubleValue, read_version_and_descriptor,
    write_version_and_descriptor,
};
use ag_psd::psd::{
    AnimationDispose, AnimationFrame, AnimationFrameFlags, AnimationFrameInfo, AnimationInfo,
    Animations, BlendMode, ColorMode, Compression, ImageResources, Layer, LayerAdditionalInfo,
    PixelData, PointF, Psd, ReadOptions, SectionDividerType, WriteOptions,
};
use ag_psd::reader::PsdReader;
use ag_psd::writer::{
    PsdWriter, create_writer_default, get_writer_buffer, write_bytes, write_section,
    write_signature, write_uint8, write_uint16, write_uint32, write_zeros,
};

use crate::aseprite_reader::{
    FrameSnapshot, FrameSnapshotLayer, read_aseprite_export_with_active_frame,
};
use crate::atomic_output::commit_bytes;
use crate::roundtrip::{LayerMarker, MarkerRole, encode_marker};
use crate::{
    ExportError, InformationLossReport, NormalizedDocument, NormalizedLayer, NormalizedLayerKind,
    NormalizedLoopMode,
};

/// Options controlling one validated PSD/PSB export transaction.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ExportOptions {
    /// Allow replacing an existing output only after the new bytes validate.
    pub overwrite: bool,
    /// Current Aseprite frame to write as Photoshop's zero-based active frame.
    pub active_frame_index: Option<u32>,
    /// Channel compression policy; `None` preserves the existing ZIP default.
    pub compression: Option<ExportCompression>,
    /// Embed private metadata that allows this converter to recover cel relationships.
    pub embed_roundtrip_metadata: bool,
    /// Include pixel layers that contain no cels in the exported document.
    pub include_empty_layers: bool,
}

/// Compression modes supported by the PSD/PSB writer.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExportCompression {
    /// Store channel bytes without compression.
    Raw,
    /// Pack channel rows with PackBits RLE.
    Rle,
    /// ZIP-compress channel bytes without prediction.
    Zip,
    /// ZIP-compress channel bytes after horizontal prediction.
    ZipPrediction,
}

impl Default for ExportCompression {
    fn default() -> Self {
        Self::Zip
    }
}

impl ExportCompression {
    /// Returns the stable CLI token for this compression mode.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Raw => "raw",
            Self::Rle => "rle",
            Self::Zip => "zip",
            Self::ZipPrediction => "zip-prediction",
        }
    }

    /// Parses the stable CLI token.
    pub fn parse(value: &str) -> Option<Self> {
        Some(match value {
            "raw" => Self::Raw,
            "rle" => Self::Rle,
            "zip" => Self::Zip,
            "zip-prediction" => Self::ZipPrediction,
            _ => return None,
        })
    }

    fn ag_psd(self) -> Compression {
        match self {
            Self::Raw => Compression::RawData,
            Self::Rle => Compression::RleCompressed,
            Self::Zip => Compression::ZipWithoutPrediction,
            Self::ZipPrediction => Compression::ZipWithPrediction,
        }
    }
}

impl Default for ExportOptions {
    /// Enables round-trip metadata while keeping replacement and active-frame defaults safe.
    fn default() -> Self {
        Self {
            overwrite: false,
            active_frame_index: None,
            compression: None,
            embed_roundtrip_metadata: true,
            include_empty_layers: false,
        }
    }
}

/// Result of one committed Aseprite-to-Photoshop export.
#[derive(Debug, Clone)]
pub struct ExportReport {
    /// Original Aseprite snapshot path.
    pub input: PathBuf,
    /// Independently flattened Aseprite snapshot path.
    pub composite: PathBuf,
    /// Committed PSD or PSB path.
    pub output: PathBuf,
    /// Structured compatibility losses from the export mapping.
    pub information_loss: InformationLossReport,
    /// Active frame index written to the Photoshop document, when supplied.
    pub active_frame_index: Option<u32>,
}

/// Exports one original/composite Aseprite snapshot pair to a validated PSD or PSB.
pub fn export(
    input: &Path,
    composite: &Path,
    output: &Path,
    options: &ExportOptions,
) -> Result<ExportReport, ExportError> {
    if !input.is_file() {
        return Err(ExportError::InputMissing(input.to_path_buf()));
    }
    if !composite.is_file() {
        return Err(ExportError::InputMissing(composite.to_path_buf()));
    }
    if output.exists() && !options.overwrite {
        return Err(ExportError::OutputExists(output.to_path_buf()));
    }
    let psb = match output
        .extension()
        .and_then(|value| value.to_str())
        .map(str::to_ascii_lowercase)
        .as_deref()
    {
        Some("psd") => false,
        Some("psb") => true,
        _ => {
            return Err(ExportError::InvalidPath(format!(
                "output extension must be .psd or .psb: {}",
                output.display()
            )));
        }
    };

    let source =
        read_aseprite_export_with_active_frame(input, composite, options.active_frame_index)?;
    let mut information_loss = source.information_loss;
    if !options.include_empty_layers {
        information_loss
            .entries
            .retain(|entry| entry.code != crate::InformationLossCode::EmptyPixelLayer);
    }
    let (model, metadata, frame_first) = if let Some(snapshots) = source.frame_snapshots.as_deref()
    {
        let snapshots = if options.include_empty_layers {
            snapshots.to_vec()
        } else {
            omit_empty_pixel_layers(snapshots)
        };
        let (model, metadata) = build_frame_first_psd(
            &source.document,
            &snapshots,
            &source.composites,
            &mut information_loss,
            options.embed_roundtrip_metadata,
        )?;
        (model, metadata, true)
    } else {
        (
            build_psd(&source.document, &source.composites, &mut information_loss)?,
            animation_metadata(&source.document, options.embed_roundtrip_metadata),
            false,
        )
    };
    let write_options = WriteOptions {
        no_background: Some(true),
        psb: Some(psb),
        compress: (options.compression.is_none()).then_some(true),
        compression: options.compression.map(ExportCompression::ag_psd),
        trim_image_data: Some(false),
        ..Default::default()
    };
    let encoded = catch_unwind(AssertUnwindSafe(|| {
        ag_psd::write_psd(&model, &write_options)
    }))
    .map_err(|_| ExportError::Writer("ag-psd panicked while encoding the document".to_string()))?;
    let mut encoded = inject_animation_metadata(encoded, &metadata, psb)?;
    if options.active_frame_index.is_none() {
        encoded = omit_active_frame_descriptor(encoded)?;
    }
    validate_output(
        &encoded,
        &source.document,
        &source.composites,
        psb,
        options.compression,
        frame_first,
    )?;
    commit_bytes(output, &encoded, options.overwrite).map_err(ExportError::OutputIo)?;

    Ok(ExportReport {
        input: input.to_path_buf(),
        composite: composite.to_path_buf(),
        output: output.to_path_buf(),
        information_loss,
        active_frame_index: source.document.active_frame_index,
    })
}

/// Removes pixel layers that have no cel in any exported frame while preserving frame topology.
fn omit_empty_pixel_layers(snapshots: &[FrameSnapshot]) -> Vec<FrameSnapshot> {
    let non_empty_ids = snapshots
        .iter()
        .flat_map(|snapshot| snapshot.layers.iter())
        .flat_map(collect_non_empty_layer_ids)
        .collect::<HashSet<_>>();
    snapshots
        .iter()
        .map(|snapshot| FrameSnapshot {
            layers: snapshot
                .layers
                .iter()
                .filter_map(|layer| filter_empty_pixel_layer(layer, &non_empty_ids))
                .collect(),
        })
        .collect()
}

/// Collects source IDs for pixel layers that contain at least one cel.
fn collect_non_empty_layer_ids(layer: &FrameSnapshotLayer) -> Vec<u32> {
    let mut ids = layer
        .cel
        .as_ref()
        .map(|_| vec![layer.source_layer_id])
        .unwrap_or_default();
    for child in &layer.children {
        ids.extend(collect_non_empty_layer_ids(child));
    }
    ids
}

/// Filters one frame snapshot without removing groups or frame-local empty placeholders.
fn filter_empty_pixel_layer(
    layer: &FrameSnapshotLayer,
    non_empty_ids: &HashSet<u32>,
) -> Option<FrameSnapshotLayer> {
    if layer.kind == NormalizedLayerKind::Pixel && !non_empty_ids.contains(&layer.source_layer_id) {
        return None;
    }
    Some(FrameSnapshotLayer {
        children: layer
            .children
            .iter()
            .filter_map(|child| filter_empty_pixel_layer(child, non_empty_ids))
            .collect(),
        ..layer.clone()
    })
}

/// Builds a PSD whose root layers are one complete snapshot group per playback frame.
fn build_frame_first_psd(
    document: &NormalizedDocument,
    snapshots: &[FrameSnapshot],
    composites: &[Vec<u8>],
    report: &mut InformationLossReport,
    embed_roundtrip_metadata: bool,
) -> Result<(Psd, HashMap<u32, LayerAnimationMetadata>), ExportError> {
    if snapshots.len() != document.frames.len() || snapshots.is_empty() {
        return Err(ExportError::Writer(
            "frame snapshot count differs from normalized document".to_string(),
        ));
    }
    let expected = document.canvas.0 as usize * document.canvas.1 as usize * 4;
    if composites
        .iter()
        .any(|composite| composite.len() != expected)
    {
        return Err(ExportError::Writer(
            "composite pixel size differs from normalized document".to_string(),
        ));
    }
    let active_frame = document.active_frame_index.unwrap_or(0) as usize;
    if active_frame >= snapshots.len() {
        return Err(ExportError::Writer(
            "active frame index is outside the frame snapshots".to_string(),
        ));
    }
    let frame_ids = document
        .frames
        .iter()
        .enumerate()
        .map(|(index, frame)| f64::from(frame.source_id.unwrap_or((index + 1) as u32)))
        .collect::<Vec<_>>();
    let frames = document
        .frames
        .iter()
        .enumerate()
        .map(|(index, frame)| AnimationFrameInfo {
            id: frame_ids[index],
            delay: f64::from(frame.duration_ms.unwrap_or(100)) / 1000.0,
            dispose: Some(match frame.dispose.as_deref() {
                Some("none") => AnimationDispose::None,
                Some("dispose") => AnimationDispose::Dispose,
                _ => AnimationDispose::Auto,
            }),
        })
        .collect();
    let repeats = match document.loop_mode {
        Some(NormalizedLoopMode::Infinite) | None => Some(0.0),
        Some(NormalizedLoopMode::Finite(value)) => Some(f64::from(value)),
    };
    let active_frame_id = document.active_frame_index.map(f64::from);
    let mut next_id = 1_u32;
    let mut metadata = HashMap::new();
    let mut children = Vec::with_capacity(snapshots.len());
    for (frame_index, snapshot) in snapshots.iter().enumerate() {
        let id = take_export_id(&mut next_id);
        let frame_states = (0..snapshots.len())
            .map(|index| AnimationFrame {
                frames: vec![f64::from(index as u32 + 1)],
                enable: Some(index == frame_index),
                offset: None,
                reference_point: None,
                opacity: None,
                effects: None,
            })
            .collect::<Vec<_>>();
        metadata.insert(
            id,
            LayerAnimationMetadata {
                frames: frame_states,
                flags: default_animation_flags(),
                marker: embed_roundtrip_metadata.then_some(LayerMarker {
                    version: 2,
                    role: MarkerRole::FrameGroup,
                    logical_layer_id: 0xFFFF_FFFF,
                    variant_index: frame_index as u32 + 1,
                    variant_count: snapshots.len() as u32,
                }),
            },
        );
        let layers = snapshot
            .layers
            .iter()
            .map(|layer| {
                build_frame_snapshot_layer(
                    layer,
                    frame_index,
                    snapshots.len(),
                    &mut next_id,
                    report,
                    &mut metadata,
                    embed_roundtrip_metadata,
                    String::new(),
                )
            })
            .collect::<Result<Vec<_>, _>>()?;
        children.push(Layer {
            additional_info: LayerAdditionalInfo {
                name: Some(format!("Frame {}", frame_index + 1)),
                id: Some(f64::from(id)),
                ..Default::default()
            },
            children: Some(layers),
            opened: Some(true),
            hidden: Some(frame_index != active_frame),
            blend_mode: Some(BlendMode::Normal),
            ..Default::default()
        });
    }
    Ok((
        Psd {
            width: f64::from(document.canvas.0),
            height: f64::from(document.canvas.1),
            channels: Some(4.0),
            bits_per_channel: Some(8.0),
            color_mode: Some(ColorMode::Rgb),
            children: Some(children),
            image_data: Some(PixelData {
                width: document.canvas.0,
                height: document.canvas.1,
                data: composites[active_frame].clone(),
            }),
            image_resources: Some(ImageResources {
                animations: Some(Animations {
                    frames,
                    animations: vec![AnimationInfo {
                        id: 1.0,
                        frames: frame_ids,
                        repeats,
                        active_frame: active_frame_id,
                    }],
                }),
                ..Default::default()
            }),
            ..Default::default()
        },
        metadata,
    ))
}

/// Builds one frame-local layer while allocating a unique PSD layer id.
#[allow(clippy::too_many_arguments)]
fn build_frame_snapshot_layer(
    source: &FrameSnapshotLayer,
    frame_index: usize,
    frame_count: usize,
    next_id: &mut u32,
    report: &mut InformationLossReport,
    metadata: &mut HashMap<u32, LayerAnimationMetadata>,
    embed_roundtrip_metadata: bool,
    parent_path: String,
) -> Result<Layer, ExportError> {
    let id = take_export_id(next_id);
    let path = if parent_path.is_empty() {
        source.name.clone()
    } else {
        format!("{parent_path}/{}", source.name)
    };
    let (blend_mode, unknown_blend) = psd_blend_mode(source.blend_mode.as_deref());
    if unknown_blend {
        report.add(
            crate::InformationLossCode::UnknownBlendMode,
            crate::LossDisposition::Degraded,
            crate::InformationLocation {
                layer_id: Some(source.source_layer_id),
                path: path.clone(),
                frame_index: Some(frame_index as u32),
            },
            "A blend mode that is not supported by the PSD writer was written as Normal",
            true,
            true,
        );
    }
    let base_opacity = source.opacity.map(|value| f64::from(value) / 255.0);
    let mut layer = Layer {
        additional_info: LayerAdditionalInfo {
            name: Some(source.name.clone()),
            id: Some(f64::from(id)),
            ..Default::default()
        },
        blend_mode: Some(blend_mode),
        opacity: base_opacity,
        hidden: Some(!source.visible),
        ..Default::default()
    };
    match source.kind {
        NormalizedLayerKind::Group => {
            layer.opened = Some(true);
            layer.children = Some(
                source
                    .children
                    .iter()
                    .map(|child| {
                        build_frame_snapshot_layer(
                            child,
                            frame_index,
                            frame_count,
                            next_id,
                            report,
                            metadata,
                            embed_roundtrip_metadata,
                            path.clone(),
                        )
                    })
                    .collect::<Result<Vec<_>, _>>()?,
            );
        }
        NormalizedLayerKind::Pixel => {
            let Some(cel) = source.cel.as_ref() else {
                layer.hidden = Some(true);
                layer.top = Some(0.0);
                layer.left = Some(0.0);
                layer.bottom = Some(1.0);
                layer.right = Some(1.0);
                layer.image_data = Some(PixelData {
                    width: 1,
                    height: 1,
                    data: vec![0, 0, 0, 0],
                });
                if embed_roundtrip_metadata {
                    metadata.insert(
                        id,
                        LayerAnimationMetadata {
                            frames: Vec::new(),
                            flags: default_animation_flags(),
                            marker: Some(LayerMarker {
                                version: 2,
                                role: MarkerRole::LayerCopy,
                                logical_layer_id: source.source_layer_id,
                                variant_index: frame_index as u32 + 1,
                                variant_count: frame_count as u32,
                            }),
                        },
                    );
                }
                return Ok(layer);
            };
            layer.opacity = Some(f64::from(cel.opacity) / 255.0);
            layer.top = Some(f64::from(cel.y));
            layer.left = Some(f64::from(cel.x));
            layer.bottom = Some(f64::from(cel.y + cel.height as i32));
            layer.right = Some(f64::from(cel.x + cel.width as i32));
            layer.image_data = Some(PixelData {
                width: cel.width,
                height: cel.height,
                data: cel.pixels.clone(),
            });
        }
    }
    if embed_roundtrip_metadata {
        metadata.insert(
            id,
            LayerAnimationMetadata {
                frames: Vec::new(),
                flags: default_animation_flags(),
                marker: Some(LayerMarker {
                    version: 2,
                    role: MarkerRole::LayerCopy,
                    logical_layer_id: source.source_layer_id,
                    variant_index: frame_index as u32 + 1,
                    variant_count: frame_count as u32,
                }),
            },
        );
    }
    Ok(layer)
}

/// Allocates a non-zero PSD layer identifier for one exported record.
fn take_export_id(next_id: &mut u32) -> u32 {
    let id = *next_id;
    *next_id = next_id.saturating_add(1);
    id
}

/// Returns the animation flag defaults used by frame-group metadata.
fn default_animation_flags() -> AnimationFrameFlags {
    AnimationFrameFlags {
        propagate_frame_one: Some(false),
        unify_layer_position: Some(false),
        unify_layer_style: Some(false),
        unify_layer_visibility: Some(false),
    }
}

/// Builds the ag-psd document while keeping NormalizedDocument as the sole domain model.
fn build_psd(
    document: &NormalizedDocument,
    composites: &[Vec<u8>],
    report: &mut InformationLossReport,
) -> Result<Psd, ExportError> {
    let composite = composites.first().ok_or_else(|| {
        ExportError::Writer("normalized export has no composite frames".to_string())
    })?;
    let expected = document.canvas.0 as usize * document.canvas.1 as usize * 4;
    if composite.len() != expected {
        return Err(ExportError::Writer(format!(
            "composite pixel size differs: expected {expected}, got {}",
            composite.len()
        )));
    }
    let frame_ids = document
        .frames
        .iter()
        .enumerate()
        .map(|(index, frame)| f64::from(frame.source_id.unwrap_or((index + 1) as u32)))
        .collect::<Vec<_>>();
    let frames = document
        .frames
        .iter()
        .enumerate()
        .map(|(index, frame)| AnimationFrameInfo {
            id: frame_ids[index],
            delay: f64::from(frame.duration_ms.unwrap_or(100)) / 1000.0,
            dispose: Some(match frame.dispose.as_deref() {
                Some("none") => AnimationDispose::None,
                Some("dispose") => AnimationDispose::Dispose,
                _ => AnimationDispose::Auto,
            }),
        })
        .collect();
    let repeats = match document.loop_mode {
        Some(NormalizedLoopMode::Infinite) | None => Some(0.0),
        Some(NormalizedLoopMode::Finite(value)) => Some(f64::from(value)),
    };
    Ok(Psd {
        width: f64::from(document.canvas.0),
        height: f64::from(document.canvas.1),
        channels: Some(4.0),
        bits_per_channel: Some(8.0),
        color_mode: Some(ColorMode::Rgb),
        children: Some(
            document
                .root_layers
                .iter()
                .map(|layer| {
                    let path = export_layer_path(None, layer);
                    build_psd_layer(layer, &path, report)
                })
                .collect::<Result<Vec<_>, _>>()?,
        ),
        image_data: Some(PixelData {
            width: document.canvas.0,
            height: document.canvas.1,
            data: composite.clone(),
        }),
        image_resources: Some(ImageResources {
            animations: Some(Animations {
                frames,
                animations: vec![AnimationInfo {
                    id: 1.0,
                    frames: frame_ids,
                    repeats,
                    active_frame: document.active_frame_index.map(f64::from),
                }],
            }),
            ..Default::default()
        }),
        ..Default::default()
    })
}

/// Converts one normalized group or static cel layer into the ag-psd tree.
fn build_psd_layer(
    source: &NormalizedLayer,
    path: &str,
    report: &mut InformationLossReport,
) -> Result<Layer, ExportError> {
    let animation_frames = source
        .frame_states
        .iter()
        .map(|state| AnimationFrame {
            frames: vec![f64::from(state.frame_index + 1)],
            enable: Some(state.enabled),
            offset: state.offset.map(|point| PointF {
                x: point.x,
                y: point.y,
            }),
            reference_point: state.reference_point.map(|point| PointF {
                x: point.x,
                y: point.y,
            }),
            opacity: state.opacity,
            effects: None,
        })
        .collect::<Vec<_>>();
    let flags = AnimationFrameFlags {
        propagate_frame_one: Some(false),
        unify_layer_position: Some(false),
        unify_layer_style: Some(false),
        unify_layer_visibility: Some(false),
    };
    let blend_mode = psd_blend_mode(source.blend_mode.as_deref());
    if blend_mode.1 {
        report.add(
            crate::InformationLossCode::UnknownBlendMode,
            crate::LossDisposition::Degraded,
            crate::InformationLocation {
                layer_id: Some(source.id),
                path: path.to_string(),
                frame_index: None,
            },
            "A blend mode that is not supported by the PSD writer was written as Normal",
            true,
            true,
        );
    }
    let mut layer = Layer {
        additional_info: LayerAdditionalInfo {
            name: Some(source.name.clone()),
            id: Some(f64::from(source.id)),
            animation_frames: Some(animation_frames),
            animation_frame_flags: Some(flags),
            ..Default::default()
        },
        blend_mode: Some(blend_mode.0),
        opacity: source.opacity,
        hidden: source.hidden,
        ..Default::default()
    };
    match source.kind {
        NormalizedLayerKind::Group => {
            layer.opened = Some(true);
            layer.children = Some(
                source
                    .children
                    .iter()
                    .map(|child| {
                        let child_path = export_layer_path(Some(path), child);
                        build_psd_layer(child, &child_path, report)
                    })
                    .collect::<Result<Vec<_>, _>>()?,
            );
        }
        NormalizedLayerKind::Pixel => {
            let pixels = source.pixels.as_ref().ok_or_else(|| {
                ExportError::Writer(format!("pixel layer {} has no pixels", source.id))
            })?;
            layer.top = Some(f64::from(pixels.top));
            layer.left = Some(f64::from(pixels.left));
            layer.bottom = Some(f64::from(pixels.top) + f64::from(pixels.height));
            layer.right = Some(f64::from(pixels.left) + f64::from(pixels.width));
            layer.image_data = Some(PixelData {
                width: pixels.width,
                height: pixels.height,
                data: pixels.data.clone(),
            });
        }
    }
    Ok(layer)
}

/// Maps every supported normalized/Aseprite blend spelling to ag-psd.
fn psd_blend_mode(value: Option<&str>) -> (BlendMode, bool) {
    let value = value.unwrap_or("normal");
    let mode = match value {
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
        "linear dodge" | "addition" => BlendMode::LinearDodge,
        "subtract" => BlendMode::Subtract,
        "divide" => BlendMode::Divide,
        _ => BlendMode::Normal,
    };
    (
        mode,
        !matches!(
            value,
            "normal"
                | "multiply"
                | "screen"
                | "overlay"
                | "darken"
                | "lighten"
                | "color dodge"
                | "color burn"
                | "hard light"
                | "soft light"
                | "difference"
                | "exclusion"
                | "hue"
                | "saturation"
                | "color"
                | "luminosity"
                | "linear dodge"
                | "addition"
                | "subtract"
                | "divide"
        ),
    )
}

/// Builds a stable human-readable path for one exported normalized layer.
fn export_layer_path(parent: Option<&str>, layer: &NormalizedLayer) -> String {
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

/// Injects the shmd records missing from ag-psd 0.2.0 into its encoded layer records.
fn inject_animation_metadata(
    mut bytes: Vec<u8>,
    metadata: &HashMap<u32, LayerAnimationMetadata>,
    psb: bool,
) -> Result<Vec<u8>, ExportError> {
    let layout = layer_record_layout(&bytes, psb)?;
    let mut insertions = Vec::new();
    for record in layout.records {
        let Some(id) = record.layer_id else {
            continue;
        };
        let Some(payload) = metadata.get(&id) else {
            continue;
        };
        let mut block = if payload.frames.is_empty() {
            Vec::new()
        } else {
            shmd_block(id, payload)
        };
        if let Some(marker) = payload.marker {
            block.extend(roundtrip_block(marker));
        }
        let new_extra = record
            .extra_length
            .checked_add(block.len())
            .ok_or_else(|| ExportError::Writer("layer extra-data length overflow".to_string()))?;
        write_be_u32(&mut bytes, record.extra_length_offset, new_extra as u32)?;
        insertions.push((record.extra_end, block));
    }
    let added = insertions
        .iter()
        .map(|(_, value)| value.len())
        .sum::<usize>();
    write_be_length(
        &mut bytes,
        layout.layer_info_length_offset,
        layout.layer_info_length + added,
        psb,
    )?;
    write_be_length(
        &mut bytes,
        layout.layer_mask_length_offset,
        layout.layer_mask_length + added,
        psb,
    )?;
    insertions.sort_by_key(|(offset, _)| *offset);
    for (offset, block) in insertions.into_iter().rev() {
        bytes.splice(offset..offset, block);
    }
    Ok(bytes)
}

/// Removes ag-psd's implicit AFrm=0 when the caller did not request an active frame.
fn omit_active_frame_descriptor(mut bytes: Vec<u8>) -> Result<Vec<u8>, ExportError> {
    let color_data_length = read_be_u32(&bytes, 26)? as usize;
    let resources_length_offset = 30;
    let resources_start = 34usize
        .checked_add(color_data_length)
        .ok_or_else(|| ExportError::Writer("PSD resource offset overflow".to_string()))?;
    let resources_length = read_be_u32(&bytes, resources_length_offset)? as usize;
    let resources_end = resources_start
        .checked_add(resources_length)
        .ok_or_else(|| ExportError::Writer("PSD resource length overflow".to_string()))?;
    if resources_end > bytes.len() {
        return Err(ExportError::Writer(
            "PSD image resources are truncated".to_string(),
        ));
    }

    let resources = &bytes[resources_start..resources_end];
    let rewritten = rewrite_image_resources_without_active_frame(resources)?;
    if rewritten.len() == resources.len() {
        return Ok(bytes);
    }

    bytes.splice(resources_start..resources_end, rewritten.iter().copied());
    write_be_u32(
        &mut bytes,
        resources_length_offset,
        u32::try_from(rewritten.len())
            .map_err(|_| ExportError::Writer("PSD image resources exceed 4 GiB".to_string()))?,
    )?;
    Ok(bytes)
}

/// Rewrites animation image resources after removing the optional active-frame field.
fn rewrite_image_resources_without_active_frame(resources: &[u8]) -> Result<Vec<u8>, ExportError> {
    let mut cursor = 0usize;
    let mut rewritten = Vec::with_capacity(resources.len());
    let mut changed = false;

    while cursor < resources.len() {
        let entry_start = cursor;
        if resources.get(cursor..cursor + 4) != Some(b"8BIM") {
            return Err(ExportError::Writer(
                "invalid image resource signature while removing AFrm".to_string(),
            ));
        }
        cursor += 4;
        let id = read_be_u16(resources, cursor)?;
        cursor += 2;
        let name_length = *resources.get(cursor).ok_or_else(|| {
            ExportError::Writer("truncated image resource name length".to_string())
        })? as usize;
        cursor += 1;
        cursor = cursor
            .checked_add(name_length)
            .ok_or_else(|| ExportError::Writer("image resource name overflow".to_string()))?;
        if !(1 + name_length).is_multiple_of(2) {
            cursor = cursor
                .checked_add(1)
                .ok_or_else(|| ExportError::Writer("image resource name overflow".to_string()))?;
        }
        let data_length_offset = cursor;
        let data_length = read_be_u32(resources, cursor)? as usize;
        cursor += 4;
        let data_start = cursor;
        let data_end = data_start
            .checked_add(data_length)
            .ok_or_else(|| ExportError::Writer("image resource data overflow".to_string()))?;
        let data = resources
            .get(data_start..data_end)
            .ok_or_else(|| ExportError::Writer("truncated image resource data".to_string()))?;
        cursor = data_end;
        if !data_length.is_multiple_of(2) {
            cursor = cursor.checked_add(1).ok_or_else(|| {
                ExportError::Writer("image resource padding overflow".to_string())
            })?;
        }
        if cursor > resources.len() {
            return Err(ExportError::Writer(
                "truncated image resource padding".to_string(),
            ));
        }
        let entry_end = cursor;

        let replacement = if id == 4000 || id == 4003 {
            rewrite_animation_resource_without_active_frame(data)?
        } else {
            None
        };
        let Some(data) = replacement else {
            rewritten.extend_from_slice(&resources[entry_start..entry_end]);
            continue;
        };

        changed = true;
        rewritten.extend_from_slice(&resources[entry_start..data_length_offset]);
        rewritten.extend_from_slice(
            &u32::try_from(data.len())
                .map_err(|_| ExportError::Writer("animation resource exceeds 4 GiB".to_string()))?
                .to_be_bytes(),
        );
        rewritten.extend_from_slice(&data);
        if !data.len().is_multiple_of(2) {
            rewritten.push(0);
        }
    }

    if changed {
        Ok(rewritten)
    } else {
        Ok(resources.to_vec())
    }
}

/// Removes AFrm from every animation set in one `mani` image resource.
fn rewrite_animation_resource_without_active_frame(
    data: &[u8],
) -> Result<Option<Vec<u8>>, ExportError> {
    if data.len() < 12 || &data[..8] != b"maniIRFR" {
        return Ok(None);
    }
    let section_length = read_be_u32(data, 8)? as usize;
    let section_start = 12usize;
    let section_end = section_start
        .checked_add(section_length)
        .ok_or_else(|| ExportError::Writer("animation resource section overflow".to_string()))?;
    let section = data.get(section_start..section_end).ok_or_else(|| {
        ExportError::Writer("animation resource section is truncated".to_string())
    })?;

    let mut cursor = 0usize;
    let mut rewritten_section = Vec::with_capacity(section.len());
    let mut changed = false;
    while cursor < section.len() {
        let entry_start = cursor;
        if section.get(cursor..cursor + 4) != Some(b"8BIM") {
            return Err(ExportError::Writer(
                "invalid animation subresource signature".to_string(),
            ));
        }
        cursor += 4;
        let key = section.get(cursor..cursor + 4).ok_or_else(|| {
            ExportError::Writer("truncated animation subresource key".to_string())
        })?;
        cursor += 4;
        let payload_length_offset = cursor;
        let payload_length = read_be_u32(section, cursor)? as usize;
        cursor += 4;
        let payload_start = cursor;
        let payload_end = payload_start
            .checked_add(payload_length)
            .ok_or_else(|| ExportError::Writer("animation descriptor overflow".to_string()))?;
        let payload = section
            .get(payload_start..payload_end)
            .ok_or_else(|| ExportError::Writer("animation descriptor is truncated".to_string()))?;
        cursor = payload_end;
        if !payload_length.is_multiple_of(2) {
            cursor = cursor.checked_add(1).ok_or_else(|| {
                ExportError::Writer("animation descriptor padding overflow".to_string())
            })?;
        }
        if cursor > section.len() {
            return Err(ExportError::Writer(
                "truncated animation descriptor padding".to_string(),
            ));
        }
        let entry_end = cursor;

        let replacement = if key == b"AnDs" {
            rewrite_animation_descriptor_without_active_frame(payload)?
        } else {
            None
        };
        let Some(payload) = replacement else {
            rewritten_section.extend_from_slice(&section[entry_start..entry_end]);
            continue;
        };

        changed = true;
        rewritten_section.extend_from_slice(&section[entry_start..payload_length_offset]);
        rewritten_section.extend_from_slice(
            &u32::try_from(payload.len())
                .map_err(|_| ExportError::Writer("animation descriptor exceeds 4 GiB".to_string()))?
                .to_be_bytes(),
        );
        rewritten_section.extend_from_slice(&payload);
        if !payload.len().is_multiple_of(2) {
            rewritten_section.push(0);
        }
    }

    if !changed {
        return Ok(None);
    }
    let mut rewritten = Vec::with_capacity(data.len());
    rewritten.extend_from_slice(b"maniIRFR");
    rewritten.extend_from_slice(
        &u32::try_from(rewritten_section.len())
            .map_err(|_| ExportError::Writer("animation resource exceeds 4 GiB".to_string()))?
            .to_be_bytes(),
    );
    rewritten.extend_from_slice(&rewritten_section);
    rewritten.extend_from_slice(&data[section_end..]);
    Ok(Some(rewritten))
}

/// Removes AFrm fields from an encoded animation descriptor.
fn rewrite_animation_descriptor_without_active_frame(
    payload: &[u8],
) -> Result<Option<Vec<u8>>, ExportError> {
    let mut reader = PsdReader::new(payload, None, None);
    let mut descriptor = read_version_and_descriptor(&mut reader).map_err(|error| {
        ExportError::Writer(format!("cannot read animation descriptor: {error}"))
    })?;
    let mut changed = false;
    if let Some(DescriptorValue::List(sets)) = descriptor
        .items
        .iter_mut()
        .find_map(|(key, value)| (key == "FSts").then_some(value))
    {
        for value in sets {
            let DescriptorValue::Descriptor(set) = value else {
                continue;
            };
            let before = set.items.len();
            set.items.retain(|(key, _)| key != "AFrm");
            changed |= set.items.len() != before;
        }
    }
    if !changed {
        return Ok(None);
    }

    let mut writer = create_writer_default();
    write_version_and_descriptor(&mut writer, &descriptor);
    Ok(Some(get_writer_buffer(&writer)))
}

#[derive(Debug)]
struct LayerAnimationMetadata {
    frames: Vec<AnimationFrame>,
    flags: AnimationFrameFlags,
    marker: Option<LayerMarker>,
}

/// Indexes normalized per-layer animation records by the PSD layer ID.
fn animation_metadata(
    document: &NormalizedDocument,
    embed_roundtrip_metadata: bool,
) -> HashMap<u32, LayerAnimationMetadata> {
    fn collect(
        layer: &NormalizedLayer,
        parent: Option<&NormalizedLayer>,
        output: &mut HashMap<u32, LayerAnimationMetadata>,
        embed_roundtrip_metadata: bool,
    ) {
        let marker = if embed_roundtrip_metadata {
            parent
                .filter(|parent| is_materialized_cel_wrapper(parent))
                .map(|parent| LayerMarker {
                    version: 1,
                    role: MarkerRole::Variant,
                    logical_layer_id: parent.id,
                    variant_index: parent
                        .children
                        .iter()
                        .position(|child| child.id == layer.id)
                        .map_or(0, |index| index as u32 + 1),
                    variant_count: parent.children.len() as u32,
                })
                .or_else(|| {
                    is_materialized_cel_wrapper(layer).then(|| LayerMarker {
                        version: 1,
                        role: MarkerRole::Wrapper,
                        logical_layer_id: layer.id,
                        variant_index: 0,
                        variant_count: layer.children.len() as u32,
                    })
                })
        } else {
            None
        };
        output.insert(
            layer.id,
            LayerAnimationMetadata {
                frames: layer
                    .frame_states
                    .iter()
                    .map(|state| AnimationFrame {
                        frames: vec![f64::from(state.frame_index + 1)],
                        enable: Some(state.enabled),
                        offset: state.offset.map(|point| PointF {
                            x: point.x,
                            y: point.y,
                        }),
                        reference_point: state.reference_point.map(|point| PointF {
                            x: point.x,
                            y: point.y,
                        }),
                        opacity: state.opacity,
                        effects: None,
                    })
                    .collect(),
                flags: AnimationFrameFlags {
                    propagate_frame_one: Some(false),
                    unify_layer_position: Some(false),
                    unify_layer_style: Some(false),
                    unify_layer_visibility: Some(false),
                },
                marker,
            },
        );
        for child in &layer.children {
            collect(child, Some(layer), output, embed_roundtrip_metadata);
        }
    }
    let mut output = HashMap::new();
    for layer in &document.root_layers {
        collect(layer, None, &mut output, embed_roundtrip_metadata);
    }
    output
}

/// Recognizes the wrapper shape created when one Aseprite layer has multiple cel variants.
fn is_materialized_cel_wrapper(layer: &NormalizedLayer) -> bool {
    if layer.kind != NormalizedLayerKind::Group || layer.children.len() < 2 {
        return false;
    }
    let child_ids = layer
        .children
        .iter()
        .map(|child| child.id)
        .collect::<Vec<_>>();
    if child_ids
        .windows(2)
        .any(|pair| pair[1] != pair[0].saturating_add(1))
        || child_ids
            .last()
            .is_none_or(|last_id| last_id.saturating_add(1) != layer.id)
    {
        return false;
    }
    if !layer
        .children
        .iter()
        .all(|child| child.kind == NormalizedLayerKind::Pixel && child.name == layer.name)
    {
        return false;
    }
    let active_sets = layer
        .children
        .iter()
        .map(|child| {
            child
                .frame_states
                .iter()
                .enumerate()
                .filter_map(|(index, state)| state.enabled.then_some(index))
                .collect::<std::collections::BTreeSet<_>>()
        })
        .collect::<Vec<_>>();
    active_sets.iter().all(|set| !set.is_empty())
        && active_sets.iter().enumerate().all(|(index, left)| {
            active_sets
                .iter()
                .skip(index + 1)
                .all(|right| left.is_disjoint(right))
        })
}

/// Serializes one shmd additional-info block using ag-psd descriptor primitives.
fn shmd_block(id: u32, metadata: &LayerAnimationMetadata) -> Vec<u8> {
    let mut payload = create_writer_default();
    write_uint32(&mut payload, 2);
    write_signature(&mut payload, "8BIM");
    write_signature(&mut payload, "mlst");
    write_uint8(&mut payload, 0);
    write_zeros(&mut payload, 3);
    write_section(
        &mut payload,
        2,
        |writer| write_mlst(writer, id, &metadata.frames),
        true,
        false,
    );
    write_signature(&mut payload, "8BIM");
    write_signature(&mut payload, "mdyn");
    write_uint8(&mut payload, 0);
    write_zeros(&mut payload, 3);
    write_section(
        &mut payload,
        2,
        |writer| {
            write_uint16(writer, 0);
            write_uint8(
                writer,
                if metadata.flags.propagate_frame_one == Some(true) {
                    0
                } else {
                    0x0f
                },
            );
            write_uint8(
                writer,
                u8::from(metadata.flags.unify_layer_position == Some(true))
                    | (u8::from(metadata.flags.unify_layer_style == Some(true)) << 1)
                    | (u8::from(metadata.flags.unify_layer_visibility == Some(true)) << 2),
            );
        },
        false,
        false,
    );
    let payload = get_writer_buffer(&payload);

    let mut block = create_writer_default();
    write_signature(&mut block, "8BIM");
    write_signature(&mut block, "shmd");
    write_section(
        &mut block,
        2,
        |writer| write_bytes(writer, Some(&payload)),
        false,
        false,
    );
    get_writer_buffer(&block)
}

/// Serializes one private round-trip marker as a layer additional-info block.
fn roundtrip_block(marker: LayerMarker) -> Vec<u8> {
    let mut block = create_writer_default();
    write_signature(&mut block, "8BIM");
    write_signature(&mut block, "p2rt");
    write_section(
        &mut block,
        2,
        |writer| write_bytes(writer, Some(&encode_marker(marker))),
        false,
        false,
    );
    get_writer_buffer(&block)
}

/// Writes the upstream FrameListDescriptor shape consumed by Photoshop and this importer.
fn write_mlst(writer: &mut PsdWriter, id: u32, frames: &[AnimationFrame]) {
    let mut descriptor = Descriptor::new("", "null");
    descriptor.set("LaID", DescriptorValue::Integer(id as i32));
    descriptor.set(
        "LaSt",
        DescriptorValue::List(
            frames
                .iter()
                .map(|frame| DescriptorValue::Descriptor(frame_descriptor(frame)))
                .collect(),
        ),
    );
    write_version_and_descriptor(writer, &descriptor);
}

/// Builds one frame-state action descriptor.
fn frame_descriptor(frame: &AnimationFrame) -> Descriptor {
    let mut descriptor = Descriptor::new("", "null");
    if let Some(enable) = frame.enable {
        descriptor.set("enab", DescriptorValue::Boolean(enable));
    }
    descriptor.set(
        "FrLs",
        DescriptorValue::List(
            frame
                .frames
                .iter()
                .map(|value| DescriptorValue::Integer(*value as i32))
                .collect(),
        ),
    );
    if let Some(point) = frame.offset {
        descriptor.set("Ofst", DescriptorValue::Descriptor(point_descriptor(point)));
    }
    if let Some(point) = frame.reference_point {
        descriptor.set("FXRf", DescriptorValue::Descriptor(point_descriptor(point)));
    }
    if let Some(opacity) = frame.opacity {
        let mut blend = Descriptor::new("", "null");
        blend.set(
            "Opct",
            DescriptorValue::UnitDouble(UnitDoubleValue {
                units: "Percent".to_string(),
                value: opacity * 100.0,
            }),
        );
        descriptor.set("blendOptions", DescriptorValue::Descriptor(blend));
    }
    descriptor
}

/// Builds one Photoshop horizontal/vertical point descriptor.
fn point_descriptor(point: PointF) -> Descriptor {
    let mut descriptor = Descriptor::new("", "Pnt ");
    descriptor.set("Hrzn", DescriptorValue::Double(point.x));
    descriptor.set("Vrtc", DescriptorValue::Double(point.y));
    descriptor
}

#[derive(Debug)]
struct LayerRecordLayout {
    layer_mask_length_offset: usize,
    layer_mask_length: usize,
    layer_info_length_offset: usize,
    layer_info_length: usize,
    records: Vec<LayerRecord>,
    channel_payloads: Vec<ChannelPayload>,
    composite_start: usize,
    composite_compression: u16,
}

#[derive(Debug)]
struct LayerRecord {
    extra_length_offset: usize,
    extra_length: usize,
    extra_end: usize,
    layer_id: Option<u32>,
    section_divider_type: Option<u32>,
}

#[derive(Debug)]
struct ChannelPayload {
    compression: u16,
    encoded_length: usize,
    data_start: usize,
    expected_decoded_length: Option<usize>,
}

/// Locates all encoded layer records and their lyid values without interpreting pixels.
fn layer_record_layout(bytes: &[u8], psb: bool) -> Result<LayerRecordLayout, ExportError> {
    if bytes.len() < 30 || &bytes[..4] != b"8BPS" {
        return Err(ExportError::Writer(
            "ag-psd returned an invalid container".to_string(),
        ));
    }
    let mut cursor = 26;
    cursor = skip_u32_section(bytes, cursor)?;
    cursor = skip_u32_section(bytes, cursor)?;
    let layer_mask_length_offset = cursor;
    let layer_mask_length = read_be_length(bytes, cursor, psb)?;
    cursor += if psb { 8 } else { 4 };
    let layer_info_length_offset = cursor;
    let layer_info_length = read_be_length(bytes, cursor, psb)?;
    cursor += if psb { 8 } else { 4 };
    let count = read_be_i16(bytes, cursor)?.unsigned_abs() as usize;
    cursor += 2;
    let mut records = Vec::with_capacity(count);
    let mut channel_lengths_by_record = Vec::with_capacity(count);
    for _ in 0..count {
        let top = read_be_i32(bytes, cursor)?;
        let left = read_be_i32(bytes, cursor + 4)?;
        let bottom = read_be_i32(bytes, cursor + 8)?;
        let right = read_be_i32(bytes, cursor + 12)?;
        checked_advance(bytes, &mut cursor, 16)?;
        let layer_pixels = Some(rectangle_area(top, left, bottom, right)?);
        let channels = read_be_u16(bytes, cursor)? as usize;
        cursor += 2;
        let mut channel_lengths = Vec::with_capacity(channels);
        for _ in 0..channels {
            let channel_id = read_be_i16(bytes, cursor)?;
            cursor += 2;
            let length = read_be_length(bytes, cursor, psb)?;
            cursor += if psb { 8 } else { 4 };
            channel_lengths.push((channel_id, length, layer_pixels));
        }
        if bytes.get(cursor..cursor + 4) != Some(b"8BIM") {
            return Err(ExportError::Writer(
                "layer record has an invalid blend-mode signature".to_string(),
            ));
        }
        checked_advance(bytes, &mut cursor, 12)?;
        let extra_length_offset = cursor;
        let extra_length = read_be_u32(bytes, cursor)? as usize;
        cursor += 4;
        let extra_start = cursor;
        let extra_end = extra_start
            .checked_add(extra_length)
            .filter(|end| *end <= bytes.len())
            .ok_or_else(|| ExportError::Writer("layer extra data exceeds output".to_string()))?;
        let (layer_id, section_divider_type) =
            find_layer_metadata(&bytes[extra_start..extra_end], psb)?;
        records.push(LayerRecord {
            extra_length_offset,
            extra_length,
            extra_end,
            layer_id,
            section_divider_type,
        });
        channel_lengths_by_record.push(channel_lengths);
        cursor = extra_end;
    }
    let layer_info_end = layer_info_length_offset
        .checked_add(if psb { 8 } else { 4 })
        .and_then(|start| start.checked_add(layer_info_length))
        .filter(|end| *end <= bytes.len())
        .ok_or_else(|| ExportError::Writer("layer info exceeds output".to_string()))?;
    let mut channel_payloads = Vec::new();
    for channel_lengths in channel_lengths_by_record {
        for (channel_id, length, layer_pixels) in channel_lengths {
            if length < 2 {
                return Err(ExportError::Writer(
                    "layer channel is shorter than its compression field".to_string(),
                ));
            }
            let compression = read_be_u16(bytes, cursor)?;
            if compression > Compression::ZipWithPrediction as u16 {
                return Err(ExportError::Writer(format!(
                    "layer channel uses unknown compression code {compression}"
                )));
            }
            channel_payloads.push(ChannelPayload {
                compression,
                encoded_length: length,
                data_start: cursor + 2,
                expected_decoded_length: (!matches!(channel_id, -2 | -3))
                    .then_some(layer_pixels)
                    .flatten(),
            });
            cursor = cursor
                .checked_add(length)
                .filter(|end| *end <= layer_info_end)
                .ok_or_else(|| {
                    ExportError::Writer("layer channel data exceeds output".to_string())
                })?;
        }
    }
    let composite_start = layer_mask_length_offset
        .checked_add(if psb { 8 } else { 4 })
        .and_then(|start| start.checked_add(layer_mask_length))
        .filter(|start| start.checked_add(2).is_some_and(|end| end <= bytes.len()))
        .ok_or_else(|| ExportError::Writer("composite image data is truncated".to_string()))?;
    let composite_compression = read_be_u16(bytes, composite_start)?;
    if composite_compression > Compression::ZipWithPrediction as u16 {
        return Err(ExportError::Writer(format!(
            "composite image uses unknown compression code {composite_compression}"
        )));
    }
    Ok(LayerRecordLayout {
        layer_mask_length_offset,
        layer_mask_length,
        layer_info_length_offset,
        layer_info_length,
        records,
        channel_payloads,
        composite_start,
        composite_compression,
    })
}

/// Finds layer ID and section-divider metadata inside one layer's extra-data payload.
fn find_layer_metadata(extra: &[u8], psb: bool) -> Result<(Option<u32>, Option<u32>), ExportError> {
    let mut cursor = 0;
    cursor = skip_local_u32_section(extra, cursor)?;
    cursor = skip_local_u32_section(extra, cursor)?;
    let name_length = *extra
        .get(cursor)
        .ok_or_else(|| ExportError::Writer("layer Pascal name is truncated".to_string()))?
        as usize;
    cursor += 1 + name_length;
    cursor = (cursor + 3) & !3;
    let mut layer_id = None;
    let mut section_divider_type = None;
    while cursor < extra.len() {
        if extra.len() - cursor < 12 {
            return Err(ExportError::Writer(
                "layer additional-info header is truncated".to_string(),
            ));
        }
        let signature = &extra[cursor..cursor + 4];
        let key = &extra[cursor + 4..cursor + 8];
        if signature != b"8BIM" && signature != b"8B64" {
            return Err(ExportError::Writer(
                "layer additional-info signature is invalid".to_string(),
            ));
        }
        cursor += 8;
        let length = if psb && additional_info_uses_u64_length(key) {
            let value = read_be_u64(extra, cursor)? as usize;
            cursor += 8;
            value
        } else {
            let value = read_be_u32(extra, cursor)? as usize;
            cursor += 4;
            value
        };
        let end = cursor
            .checked_add(length)
            .filter(|end| *end <= extra.len())
            .ok_or_else(|| {
                ExportError::Writer("layer additional-info exceeds record".to_string())
            })?;
        if key == b"lyid" {
            layer_id = Some(read_be_u32(extra, cursor)?);
        } else if matches!(key, b"lsct" | b"lsdk") && length >= 4 {
            let divider = read_be_u32(extra, cursor)?;
            if divider > SectionDividerType::BoundingSectionDivider as u32 {
                return Err(ExportError::Writer(format!(
                    "layer section divider uses unknown type {divider}"
                )));
            }
            section_divider_type = Some(divider);
        }
        cursor = (end + 1) & !1;
    }
    Ok((layer_id, section_divider_type))
}

/// Returns whether a PSB additional-info key uses an eight-byte length field.
fn additional_info_uses_u64_length(key: &[u8]) -> bool {
    matches!(
        key,
        b"LMsk"
            | b"Lr16"
            | b"Lr32"
            | b"Layr"
            | b"Mt16"
            | b"Mt32"
            | b"Mtrn"
            | b"Alph"
            | b"FMsk"
            | b"lnk2"
            | b"FEid"
            | b"FXid"
            | b"PxSD"
    )
}

/// Calculates a checked PSD rectangle area and rejects inverted coordinates.
fn rectangle_area(top: i32, left: i32, bottom: i32, right: i32) -> Result<usize, ExportError> {
    let height = i64::from(bottom) - i64::from(top);
    let width = i64::from(right) - i64::from(left);
    if height < 0 || width < 0 {
        return Err(ExportError::OutputValidation(
            "layer record has inverted bounds".to_string(),
        ));
    }
    let height = usize::try_from(height)
        .map_err(|_| ExportError::OutputValidation("layer height exceeds memory".to_string()))?;
    let width = usize::try_from(width)
        .map_err(|_| ExportError::OutputValidation("layer width exceeds memory".to_string()))?;
    width
        .checked_mul(height)
        .ok_or_else(|| ExportError::OutputValidation("layer area exceeds memory".to_string()))
}

/// Validates the container, export contract, normalized semantics, and composite.
fn validate_output(
    bytes: &[u8],
    expected: &NormalizedDocument,
    composites: &[Vec<u8>],
    psb: bool,
    compression: Option<ExportCompression>,
    frame_first: bool,
) -> Result<(), ExportError> {
    let layout = validate_container_structure(bytes, psb)?;
    let options = ReadOptions {
        use_image_data: Some(true),
        skip_thumbnail: Some(true),
        ..Default::default()
    };
    let parsed = ag_psd::read_psd(bytes, &options)
        .map_err(|error| ExportError::OutputValidation(error.to_string()))?;
    validate_export_contract(&parsed, &layout, expected, compression)?;
    validate_export_semantics(bytes, &parsed, expected, composites, frame_first)?;
    Ok(())
}

/// Validates only PSD/PSB container invariants required by the emitted RGB8 subset.
fn validate_container_structure(bytes: &[u8], psb: bool) -> Result<LayerRecordLayout, ExportError> {
    let version = read_be_u16(bytes, 4)?;
    if version != if psb { 2 } else { 1 } {
        return Err(ExportError::OutputValidation(
            "PSD/PSB container version differs from output extension".to_string(),
        ));
    }
    if bytes.get(6..12) != Some(&[0; 6]) {
        return Err(ExportError::OutputValidation(
            "PSD header reserved bytes are not zero".to_string(),
        ));
    }
    let layout = layer_record_layout(bytes, psb)?;
    for record in &layout.records {
        if record
            .section_divider_type
            .is_some_and(|divider| divider > SectionDividerType::BoundingSectionDivider as u32)
        {
            return Err(ExportError::OutputValidation(
                "layer record has an unknown section-divider type".to_string(),
            ));
        }
    }
    for payload in &layout.channel_payloads {
        if matches!(
            payload.compression,
            value if value == Compression::ZipWithoutPrediction as u16
                || value == Compression::ZipWithPrediction as u16
        ) {
            let end = payload
                .data_start
                .checked_add(payload.encoded_length - 2)
                .filter(|end| *end <= bytes.len())
                .ok_or_else(|| {
                    ExportError::OutputValidation(
                        "ZIP layer channel exceeds the output".to_string(),
                    )
                })?;
            validate_zlib_payload(
                &bytes[payload.data_start..end],
                payload.expected_decoded_length,
                "layer channel",
            )?;
        }
    }
    validate_composite_payload(bytes, psb, &layout)?;
    Ok(layout)
}

/// Validates the choices promised by the Aseprite export interface.
fn validate_export_contract(
    parsed: &Psd,
    layout: &LayerRecordLayout,
    expected: &NormalizedDocument,
    compression: Option<ExportCompression>,
) -> Result<(), ExportError> {
    if !matches!(parsed.channels, Some(3.0 | 4.0))
        || parsed.bits_per_channel != Some(8.0)
        || parsed.color_mode != Some(ColorMode::Rgb)
    {
        return Err(ExportError::OutputValidation(format!(
            "exported document is not RGBA8/RGB as required (channels={:?}, bits={:?}, mode={:?})",
            parsed.channels, parsed.bits_per_channel, parsed.color_mode
        )));
    }
    if parsed.width != f64::from(expected.canvas.0) || parsed.height != f64::from(expected.canvas.1)
    {
        return Err(ExportError::OutputValidation(
            "canvas dimensions differ after ag-psd read-back".to_string(),
        ));
    }
    if let Some(compression) = compression {
        validate_channel_compression(layout, compression.ag_psd() as u16)?;
    }
    Ok(())
}

/// Validates normalized document semantics and the independently supplied composite.
fn validate_export_semantics(
    bytes: &[u8],
    parsed: &Psd,
    expected: &NormalizedDocument,
    composites: &[Vec<u8>],
    frame_first: bool,
) -> Result<(), ExportError> {
    if frame_first {
        validate_frame_group_roots(bytes, expected)?;
    } else {
        let (normalized, _) = crate::normalize_bytes(bytes)
            .map_err(|error| ExportError::OutputValidation(error.to_string()))?;
        compare_normalized(expected, &normalized)?;
    }
    let actual_composite = parsed
        .image_data
        .as_ref()
        .or(parsed.canvas.as_ref())
        .ok_or_else(|| {
            ExportError::OutputValidation("read-back PSD has no composite image".to_string())
        })?;
    let active_frame = expected.active_frame_index.unwrap_or(0) as usize;
    let expected_composite = composites
        .get(active_frame)
        .or_else(|| composites.first())
        .ok_or_else(|| {
            ExportError::OutputValidation("source has no composite frames".to_string())
        })?;
    let expected_composite = expected_composite.as_slice();
    if actual_composite.width != expected.canvas.0 || actual_composite.height != expected.canvas.1 {
        return Err(ExportError::OutputValidation(
            "flattened composite dimensions differ after ag-psd read-back".to_string(),
        ));
    }
    if actual_composite.data != expected_composite {
        let difference = actual_composite
            .data
            .iter()
            .zip(expected_composite)
            .position(|(actual, expected)| actual != expected)
            .unwrap_or(0);
        return Err(ExportError::OutputValidation(format!(
            "flattened composite differs after ag-psd read-back at byte {difference}: expected {}, got {}",
            expected_composite
                .get(difference)
                .copied()
                .unwrap_or_default(),
            actual_composite
                .data
                .get(difference)
                .copied()
                .unwrap_or_default()
        )));
    }
    Ok(())
}

/// Validates the frame-group root contract without flattening the duplicated snapshots.
fn validate_frame_group_roots(
    bytes: &[u8],
    expected: &NormalizedDocument,
) -> Result<(), ExportError> {
    let parsed = ag_psd::read_psd(
        bytes,
        &ReadOptions {
            use_image_data: Some(false),
            skip_thumbnail: Some(true),
            ..Default::default()
        },
    )
    .map_err(|error| ExportError::OutputValidation(error.to_string()))?;
    let roots = parsed.children.as_ref().ok_or_else(|| {
        ExportError::OutputValidation("frame-group PSD has no root layers".to_string())
    })?;
    if roots.len() != expected.frames.len() {
        return Err(ExportError::OutputValidation(format!(
            "frame-group root count differs: expected {}, got {}",
            expected.frames.len(),
            roots.len()
        )));
    }
    for (index, root) in roots.iter().enumerate() {
        if root.additional_info.name.as_deref() != Some(format!("Frame {}", index + 1).as_str()) {
            return Err(ExportError::OutputValidation(format!(
                "frame-group root {index} has an unexpected name"
            )));
        }
    }
    Ok(())
}

/// Verifies that layer and composite channel headers use the requested mode.
fn validate_channel_compression(
    layout: &LayerRecordLayout,
    expected: u16,
) -> Result<(), ExportError> {
    for payload in &layout.channel_payloads {
        // ag-psd intentionally stores an empty channel as a two-byte raw payload.
        if payload.compression != expected
            && !(payload.compression == Compression::RawData as u16 && payload.encoded_length == 2)
        {
            return Err(ExportError::OutputValidation(format!(
                "encoded channel compression differs: expected {expected}, got {}",
                payload.compression
            )));
        }
    }
    if layout.composite_compression != expected {
        return Err(ExportError::OutputValidation(format!(
            "encoded composite compression differs: expected {expected}, got {}",
            layout.composite_compression
        )));
    }
    Ok(())
}

/// Validates one complete zlib stream and its optional decoded byte length.
fn validate_zlib_payload(
    payload: &[u8],
    expected_length: Option<usize>,
    owner: &str,
) -> Result<(), ExportError> {
    use flate2::read::ZlibDecoder;
    use std::io::Read;

    let mut decoder = ZlibDecoder::new(payload);
    let mut decoded = Vec::new();
    decoder.read_to_end(&mut decoded).map_err(|error| {
        ExportError::OutputValidation(format!("{owner} has invalid ZIP data: {error}"))
    })?;
    if decoder.total_in() as usize != payload.len() {
        return Err(ExportError::OutputValidation(format!(
            "{owner} contains bytes after its ZIP stream"
        )));
    }
    if let Some(expected) = expected_length
        && decoded.len() != expected
    {
        return Err(ExportError::OutputValidation(format!(
            "{owner} ZIP output length differs: expected {expected}, got {}",
            decoded.len()
        )));
    }
    Ok(())
}

/// Validates the composite channel payload using its declared compression method.
fn validate_composite_payload(
    bytes: &[u8],
    psb: bool,
    layout: &LayerRecordLayout,
) -> Result<(), ExportError> {
    use flate2::{Decompress, FlushDecompress, Status};

    let channels = read_be_u16(bytes, 12)? as usize;
    let height = read_be_u32(bytes, 14)? as usize;
    let width = read_be_u32(bytes, 18)? as usize;
    let channel_size = width.checked_mul(height).ok_or_else(|| {
        ExportError::OutputValidation("composite dimensions overflow memory".to_string())
    })?;
    let payload_start = layout.composite_start + 2;
    let payload = &bytes[payload_start..];
    match layout.composite_compression {
        value if value == Compression::RawData as u16 => {
            let expected = channel_size.checked_mul(channels).ok_or_else(|| {
                ExportError::OutputValidation("composite byte length overflows memory".to_string())
            })?;
            if payload.len() != expected {
                return Err(ExportError::OutputValidation(format!(
                    "raw composite length differs: expected {expected}, got {}",
                    payload.len()
                )));
            }
        }
        value if value == Compression::RleCompressed as u16 => {
            let count_width = if psb { 4 } else { 2 };
            let rows = height.checked_mul(channels).ok_or_else(|| {
                ExportError::OutputValidation("composite RLE row count overflows".to_string())
            })?;
            let table_length = rows.checked_mul(count_width).ok_or_else(|| {
                ExportError::OutputValidation("composite RLE table overflows".to_string())
            })?;
            if payload.len() < table_length {
                return Err(ExportError::OutputValidation(
                    "composite RLE row table is truncated".to_string(),
                ));
            }
            let mut encoded_length = 0usize;
            for row in 0..rows {
                let offset = row * count_width;
                let length = if psb {
                    read_be_u32(payload, offset)? as usize
                } else {
                    read_be_u16(payload, offset)? as usize
                };
                encoded_length = encoded_length.checked_add(length).ok_or_else(|| {
                    ExportError::OutputValidation("composite RLE payload overflows".to_string())
                })?;
            }
            if table_length + encoded_length != payload.len() {
                return Err(ExportError::OutputValidation(
                    "composite RLE row lengths do not cover the payload".to_string(),
                ));
            }
        }
        value
            if value == Compression::ZipWithoutPrediction as u16
                || value == Compression::ZipWithPrediction as u16 =>
        {
            let mut cursor = 0usize;
            for _ in 0..channels {
                let mut decompressor = Decompress::new(true);
                let mut decoded = vec![0; channel_size.saturating_add(1)];
                let status = decompressor
                    .decompress(&payload[cursor..], &mut decoded, FlushDecompress::Finish)
                    .map_err(|error| {
                        ExportError::OutputValidation(format!(
                            "composite channel has invalid ZIP data: {error}"
                        ))
                    })?;
                if status != Status::StreamEnd || decompressor.total_out() as usize != channel_size
                {
                    return Err(ExportError::OutputValidation(
                        "composite ZIP channel did not decode to the canvas size".to_string(),
                    ));
                }
                cursor = cursor
                    .checked_add(decompressor.total_in() as usize)
                    .filter(|cursor| *cursor <= payload.len())
                    .ok_or_else(|| {
                        ExportError::OutputValidation(
                            "composite ZIP channel exceeds the payload".to_string(),
                        )
                    })?;
            }
            if cursor != payload.len() {
                return Err(ExportError::OutputValidation(
                    "composite image contains bytes after its ZIP channels".to_string(),
                ));
            }
        }
        _ => unreachable!("compression code was checked while parsing the layout"),
    }
    Ok(())
}

/// Compares the enduring normalized contracts written into the PSD.
fn compare_normalized(
    expected: &NormalizedDocument,
    actual: &NormalizedDocument,
) -> Result<(), ExportError> {
    if expected.canvas != actual.canvas
        || expected.frames != actual.frames
        || expected.loop_mode != actual.loop_mode
        || expected.active_frame_index != actual.active_frame_index
    {
        return Err(ExportError::OutputValidation(format!(
            "canvas, frames, loop mode, or active frame differ: expected {:?} {:?} {:?} {:?}, got {:?} {:?} {:?} {:?}",
            expected.canvas,
            expected.frames,
            expected.loop_mode,
            expected.active_frame_index,
            actual.canvas,
            actual.frames,
            actual.loop_mode,
            actual.active_frame_index,
        )));
    }
    compare_layers(&expected.root_layers, &actual.root_layers)
}

/// Recursively compares layer tree, pixels, visibility, offsets, and opacity.
fn compare_layers(
    expected: &[NormalizedLayer],
    actual: &[NormalizedLayer],
) -> Result<(), ExportError> {
    if expected.len() != actual.len() {
        return Err(ExportError::OutputValidation(
            "layer tree child count differs".to_string(),
        ));
    }
    for (expected, actual) in expected.iter().zip(actual) {
        if expected.id != actual.id
            || expected.name != actual.name
            || expected.kind != actual.kind
            || expected.bounds != actual.bounds
            || !same_base_opacity(expected.opacity, actual.opacity)
            || !same_base_blend_mode(expected.blend_mode.as_deref(), actual.blend_mode.as_deref())
            || expected.hidden != actual.hidden
            || expected.pixels != actual.pixels
            || expected.frame_states.len() != actual.frame_states.len()
        {
            return Err(ExportError::OutputValidation(format!(
                "layer {} structure or pixel dimensions differ",
                expected.id
            )));
        }
        for (expected_state, actual_state) in expected.frame_states.iter().zip(&actual.frame_states)
        {
            if expected_state.frame_index != actual_state.frame_index
                || expected_state.enabled != actual_state.enabled
                || expected_state.offset != actual_state.offset
                || !same_optional_point(
                    expected_state.reference_point,
                    actual_state.reference_point,
                )
                || !same_optional_float(expected_state.opacity, actual_state.opacity)
            {
                return Err(ExportError::OutputValidation(format!(
                    "layer {} frame {} visibility, position, or opacity differs",
                    expected.id, expected_state.frame_index
                )));
            }
        }
        compare_layers(&expected.children, &actual.children)?;
    }
    Ok(())
}

/// Compares optional normalized floating-point values with descriptor tolerance.
fn same_optional_float(left: Option<f64>, right: Option<f64>) -> bool {
    match (left, right) {
        (None, None) => true,
        (Some(left), Some(right)) => (left - right).abs() < 1e-9,
        _ => false,
    }
}

/// Compares base layer opacity where PSD serializes an implicit default as 1.0.
fn same_base_opacity(left: Option<f64>, right: Option<f64>) -> bool {
    same_optional_float(Some(left.unwrap_or(1.0)), Some(right.unwrap_or(1.0)))
}

/// Compares base layer blend mode where PSD serializes an implicit default as Normal.
fn same_base_blend_mode(left: Option<&str>, right: Option<&str>) -> bool {
    left.unwrap_or("normal") == right.unwrap_or("normal")
}

/// Compares optional animation points with descriptor-level float tolerance.
fn same_optional_point(
    left: Option<crate::AnimationPoint>,
    right: Option<crate::AnimationPoint>,
) -> bool {
    match (left, right) {
        (None, None) => true,
        (Some(left), Some(right)) => {
            (left.x - right.x).abs() < 1e-9 && (left.y - right.y).abs() < 1e-9
        }
        _ => false,
    }
}

/// Skips one top-level u32-length-prefixed section.
fn skip_u32_section(bytes: &[u8], offset: usize) -> Result<usize, ExportError> {
    let length = read_be_u32(bytes, offset)? as usize;
    offset
        .checked_add(4 + length)
        .filter(|end| *end <= bytes.len())
        .ok_or_else(|| ExportError::Writer("PSD section exceeds output".to_string()))
}

/// Skips one local u32-length-prefixed section.
fn skip_local_u32_section(bytes: &[u8], offset: usize) -> Result<usize, ExportError> {
    skip_u32_section(bytes, offset)
}

/// Advances a checked parser cursor.
fn checked_advance(bytes: &[u8], cursor: &mut usize, count: usize) -> Result<(), ExportError> {
    *cursor = cursor
        .checked_add(count)
        .filter(|end| *end <= bytes.len())
        .ok_or_else(|| ExportError::Writer("PSD layer record is truncated".to_string()))?;
    Ok(())
}

/// Reads a big-endian u16 from a checked byte offset.
fn read_be_u16(bytes: &[u8], offset: usize) -> Result<u16, ExportError> {
    let value = bytes
        .get(offset..offset + 2)
        .ok_or_else(|| ExportError::Writer("PSD u16 is truncated".to_string()))?;
    Ok(u16::from_be_bytes([value[0], value[1]]))
}

/// Reads a big-endian i16 from a checked byte offset.
fn read_be_i16(bytes: &[u8], offset: usize) -> Result<i16, ExportError> {
    Ok(read_be_u16(bytes, offset)? as i16)
}

/// Reads a big-endian i32 from a checked byte offset.
fn read_be_i32(bytes: &[u8], offset: usize) -> Result<i32, ExportError> {
    Ok(read_be_u32(bytes, offset)? as i32)
}

/// Reads a big-endian u32 from a checked byte offset.
fn read_be_u32(bytes: &[u8], offset: usize) -> Result<u32, ExportError> {
    let value = bytes
        .get(offset..offset + 4)
        .ok_or_else(|| ExportError::Writer("PSD u32 is truncated".to_string()))?;
    Ok(u32::from_be_bytes([value[0], value[1], value[2], value[3]]))
}

/// Reads a big-endian u64 from a checked byte offset.
fn read_be_u64(bytes: &[u8], offset: usize) -> Result<u64, ExportError> {
    let value = bytes
        .get(offset..offset + 8)
        .ok_or_else(|| ExportError::Writer("PSD u64 is truncated".to_string()))?;
    Ok(u64::from_be_bytes([
        value[0], value[1], value[2], value[3], value[4], value[5], value[6], value[7],
    ]))
}

/// Reads one PSD/PSB variable-width section length.
fn read_be_length(bytes: &[u8], offset: usize, large: bool) -> Result<usize, ExportError> {
    let value = if large {
        read_be_u64(bytes, offset)?
    } else {
        u64::from(read_be_u32(bytes, offset)?)
    };
    usize::try_from(value).map_err(|_| ExportError::Writer("PSD length exceeds memory".to_string()))
}

/// Writes a checked big-endian u32 field.
fn write_be_u32(bytes: &mut [u8], offset: usize, value: u32) -> Result<(), ExportError> {
    let target = bytes
        .get_mut(offset..offset + 4)
        .ok_or_else(|| ExportError::Writer("PSD u32 field is truncated".to_string()))?;
    target.copy_from_slice(&value.to_be_bytes());
    Ok(())
}

/// Writes one PSD/PSB variable-width section length.
fn write_be_length(
    bytes: &mut [u8],
    offset: usize,
    value: usize,
    large: bool,
) -> Result<(), ExportError> {
    if large {
        let target = bytes
            .get_mut(offset..offset + 8)
            .ok_or_else(|| ExportError::Writer("PSB u64 field is truncated".to_string()))?;
        target.copy_from_slice(&(value as u64).to_be_bytes());
        Ok(())
    } else {
        let value = u32::try_from(value)
            .map_err(|_| ExportError::Writer("PSD section exceeds 4 GiB".to_string()))?;
        write_be_u32(bytes, offset, value)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::aseprite_reader::FrameSnapshotCel;
    use std::fs;
    use std::time::{SystemTime, UNIX_EPOCH};

    use crate::{
        AutoAssociationOptions, ConvertOptions, JitterKind, JitterMode, JitterOptions,
        JitterProfile, LayerAssociation,
    };
    use aseprite::{
        AsepriteFile, BlendMode as AseBlendMode, ColorMode as AseColorMode, LayerOptions, Pixels,
        Tileset, TilesetData, TilesetFlags,
    };

    /// Builds the mandatory prefix of one layer extra-data payload.
    fn empty_layer_extra() -> Vec<u8> {
        vec![0; 12]
    }

    /// Appends one even-padded additional-info block to layer extra data.
    fn push_additional_info(extra: &mut Vec<u8>, signature: &[u8; 4], key: &[u8; 4], data: &[u8]) {
        extra.extend_from_slice(signature);
        extra.extend_from_slice(key);
        extra.extend_from_slice(&(data.len() as u32).to_be_bytes());
        extra.extend_from_slice(data);
        if !data.len().is_multiple_of(2) {
            extra.push(0);
        }
    }

    #[test]
    fn optional_layer_metadata_accepts_legal_absence_and_unknown_blocks() {
        assert_eq!(
            find_layer_metadata(&empty_layer_extra(), false).expect("empty layer extra"),
            (None, None)
        );

        let mut extra = empty_layer_extra();
        push_additional_info(&mut extra, b"8BIM", b"zzzz", &[1, 2, 3]);
        assert_eq!(
            find_layer_metadata(&extra, false).expect("unknown additional info"),
            (None, None)
        );

        let mut psb_extra = empty_layer_extra();
        psb_extra.extend_from_slice(b"8B64Lr16");
        psb_extra.extend_from_slice(&0u64.to_be_bytes());
        assert_eq!(
            find_layer_metadata(&psb_extra, true).expect("PSB large-length block"),
            (None, None)
        );
    }

    #[test]
    fn section_divider_accepts_every_specified_type_and_optional_tail() {
        for divider in 0u32..=3 {
            for length in [4usize, 12, 16] {
                let mut data = vec![0; length];
                data[..4].copy_from_slice(&divider.to_be_bytes());
                if length >= 12 {
                    data[4..8].copy_from_slice(b"8BIM");
                    data[8..12].copy_from_slice(b"pass");
                }
                let mut extra = empty_layer_extra();
                push_additional_info(&mut extra, b"8BIM", b"lsct", &data);
                assert_eq!(
                    find_layer_metadata(&extra, false).expect("valid section divider"),
                    (None, Some(divider))
                );
            }
        }
    }

    #[test]
    fn malformed_additional_info_is_rejected_without_requiring_layer_id() {
        let mut invalid_signature = empty_layer_extra();
        push_additional_info(&mut invalid_signature, b"NOPE", b"zzzz", &[1]);
        assert!(find_layer_metadata(&invalid_signature, false).is_err());

        let mut truncated = empty_layer_extra();
        truncated.extend_from_slice(b"8BIMlyid");
        truncated.extend_from_slice(&4u32.to_be_bytes());
        truncated.extend_from_slice(&[0, 1]);
        assert!(find_layer_metadata(&truncated, false).is_err());

        let mut unknown_divider = empty_layer_extra();
        push_additional_info(&mut unknown_divider, b"8BIM", b"lsdk", &4u32.to_be_bytes());
        assert!(find_layer_metadata(&unknown_divider, false).is_err());
    }

    #[test]
    fn strict_zip_validation_rejects_raw_deflate_and_corruption() {
        use flate2::Compression as FlateCompression;
        use flate2::write::{DeflateEncoder, ZlibEncoder};
        use std::io::Write;

        let source = [1, 2, 3, 4];
        let mut zlib = ZlibEncoder::new(Vec::new(), FlateCompression::default());
        zlib.write_all(&source).expect("zlib write");
        let encoded = zlib.finish().expect("zlib finish");
        validate_zlib_payload(&encoded, Some(source.len()), "test channel")
            .expect("valid zlib payload");

        let mut raw = DeflateEncoder::new(Vec::new(), FlateCompression::default());
        raw.write_all(&source).expect("deflate write");
        let raw = raw.finish().expect("deflate finish");
        assert!(validate_zlib_payload(&raw, Some(source.len()), "test channel").is_err());

        let mut corrupted = encoded;
        let last = corrupted.len() - 1;
        corrupted[last] ^= 0xff;
        assert!(validate_zlib_payload(&corrupted, Some(source.len()), "test channel").is_err());
    }

    #[test]
    fn exports_animation_and_rejects_unapproved_replacement() {
        let directory = std::env::temp_dir().join(format!(
            "aseprite-psd-export-{}",
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .expect("clock")
                .as_nanos()
        ));
        fs::create_dir_all(&directory).expect("create test directory");
        let input = directory.join("source.aseprite");
        let composite = directory.join("composite.aseprite");
        let output = directory.join("output.psd");
        let active_output = directory.join("active-output.psd");
        let roundtrip = directory.join("roundtrip.aseprite");
        let psb_output = directory.join("output.psb");

        let mut source = AsepriteFile::new(2, 1, AseColorMode::Rgba);
        let layer = source.add_layer_with(
            "动画层",
            LayerOptions {
                opacity: 200,
                blend_mode: AseBlendMode::Multiply,
                ..Default::default()
            },
        );
        let first = source.add_frame(120);
        let second = source.add_frame(80);
        let third = source.add_frame(60);
        source
            .set_cel(
                layer,
                first,
                Pixels::new(vec![255, 0, 0, 255], 1, 1, AseColorMode::Rgba).expect("first pixels"),
                0,
                0,
            )
            .expect("first cel");
        source
            .set_cel(
                layer,
                second,
                Pixels::new(vec![0, 0, 255, 255], 1, 1, AseColorMode::Rgba).expect("second pixels"),
                1,
                0,
            )
            .expect("second cel");
        source
            .set_linked_cel(layer, third, first)
            .expect("linked third cel");
        source.add_layer_with(
            "Empty Pixel Layer",
            LayerOptions {
                visible: false,
                ..Default::default()
            },
        );
        write_aseprite(&input, &source);

        let mut flattened = AsepriteFile::new(2, 1, AseColorMode::Rgba);
        let flat = flattened.add_layer("Composite");
        let first = flattened.add_frame(120);
        let second = flattened.add_frame(80);
        let third = flattened.add_frame(60);
        flattened
            .set_cel(
                flat,
                first,
                Pixels::new(vec![255, 0, 0, 255, 0, 0, 0, 0], 2, 1, AseColorMode::Rgba)
                    .expect("first composite"),
                0,
                0,
            )
            .expect("first composite cel");
        flattened
            .set_cel(
                flat,
                second,
                Pixels::new(vec![0, 0, 0, 0, 0, 0, 255, 255], 2, 1, AseColorMode::Rgba)
                    .expect("second composite"),
                0,
                0,
            )
            .expect("second composite cel");
        flattened
            .set_cel(
                flat,
                third,
                Pixels::new(vec![255, 0, 0, 255, 0, 0, 0, 0], 2, 1, AseColorMode::Rgba)
                    .expect("third composite"),
                0,
                0,
            )
            .expect("third composite cel");
        write_aseprite(&composite, &flattened);

        export(&input, &composite, &output, &ExportOptions::default()).expect("export PSD");
        let bytes = fs::read(&output).expect("read PSD");
        assert_eq!(&bytes[..6], b"8BPS\0\x01");
        assert_eq!(
            crate::roundtrip::inspect(&bytes).expect("inspect round-trip metadata"),
            crate::roundtrip::RoundTripStatus {
                marked: true,
                valid: true,
            }
        );
        let mut corrupted = bytes.clone();
        let marker_offset = corrupted
            .windows(4)
            .position(|window| window == b"P2RT")
            .expect("round-trip marker payload");
        corrupted[marker_offset + 5] = 9;
        assert_eq!(
            crate::roundtrip::inspect(&corrupted).expect("inspect corrupted metadata"),
            crate::roundtrip::RoundTripStatus {
                marked: true,
                valid: false,
            }
        );
        let corrupted_input = directory.join("corrupted.psd");
        fs::write(&corrupted_input, &corrupted).expect("write corrupted PSD");
        let recovery_output = directory.join("recovery.aseprite");
        assert!(matches!(
            crate::convert(
                &corrupted_input,
                &recovery_output,
                &crate::ConvertOptions {
                    layer_association: crate::LayerAssociation::AutoForRoundTrip,
                    ..Default::default()
                }
            ),
            Err(crate::ConversionError::RoundTripRecoveryRequired(_))
        ));
        let normalized = crate::normalize(&output).expect("normalize written PSD");
        assert_eq!(normalized.frames.len(), 3);
        assert_eq!(normalized.frames[0].duration_ms, Some(120));
        assert_eq!(normalized.frames[1].duration_ms, Some(80));
        assert_eq!(normalized.frames[2].duration_ms, Some(60));
        assert_eq!(normalized.active_frame_index, None);
        assert_eq!(normalized.root_layers.len(), 3);
        assert_eq!(
            normalized
                .root_layers
                .iter()
                .map(|layer| layer.name.as_str())
                .collect::<Vec<_>>(),
            vec!["Frame 1", "Frame 2", "Frame 3"]
        );

        let omit_empty_output = directory.join("omit-empty.psd");
        let omit_report = export(
            &input,
            &composite,
            &omit_empty_output,
            &ExportOptions {
                include_empty_layers: false,
                ..Default::default()
            },
        )
        .expect("export PSD without empty layers");
        assert!(
            !omit_report
                .information_loss
                .entries
                .iter()
                .any(|entry| entry.code == crate::InformationLossCode::EmptyPixelLayer)
        );
        let normalized_without_empty =
            crate::normalize(&omit_empty_output).expect("normalize PSD without empty layers");
        assert!(
            normalized_without_empty
                .root_layers
                .iter()
                .all(|layer| layer.children.len() == 1)
        );

        crate::convert(
            &output,
            &roundtrip,
            &crate::ConvertOptions {
                layer_association: crate::LayerAssociation::AutoForRoundTrip,
                ..Default::default()
            },
        )
        .expect("import exported PSD");
        let roundtrip_file =
            AsepriteFile::from_reader(fs::read(&roundtrip).expect("read roundtrip ASE").as_slice())
                .expect("parse roundtrip ASE");
        assert_eq!(
            roundtrip_file
                .frames()
                .iter()
                .map(|frame| frame.duration_ms)
                .collect::<Vec<_>>(),
            vec![120, 80, 60]
        );

        let unmarked_output = directory.join("unmarked.psd");
        export(
            &input,
            &composite,
            &unmarked_output,
            &ExportOptions {
                embed_roundtrip_metadata: false,
                ..Default::default()
            },
        )
        .expect("export PSD without round-trip metadata");
        assert_eq!(
            crate::roundtrip::inspect(&fs::read(&unmarked_output).expect("read unmarked PSD"))
                .expect("inspect unmarked PSD"),
            crate::roundtrip::RoundTripStatus {
                marked: false,
                valid: true,
            }
        );

        export(&input, &composite, &psb_output, &ExportOptions::default()).expect("export PSB");
        let psb_bytes = fs::read(&psb_output).expect("read PSB");
        assert_eq!(&psb_bytes[..6], b"8BPS\0\x02");
        assert_eq!(
            crate::normalize(&psb_output)
                .expect("normalize PSB")
                .frames
                .len(),
            3
        );

        for (index, compression) in [
            ExportCompression::Raw,
            ExportCompression::Rle,
            ExportCompression::Zip,
            ExportCompression::ZipPrediction,
        ]
        .into_iter()
        .enumerate()
        {
            let mode_output = directory.join(format!("compression-{index}.psd"));
            let report = export(
                &input,
                &composite,
                &mode_output,
                &ExportOptions {
                    compression: Some(compression),
                    ..Default::default()
                },
            )
            .expect("export selected compression");
            assert_eq!(report.output, mode_output);
            assert_eq!(
                crate::normalize(&mode_output)
                    .expect("normalize selected compression")
                    .frames
                    .len(),
                3
            );
        }

        let active_report = export(
            &input,
            &composite,
            &active_output,
            &ExportOptions {
                active_frame_index: Some(1),
                ..Default::default()
            },
        )
        .expect("export PSD with active frame");
        assert_eq!(active_report.active_frame_index, Some(1));
        assert_eq!(
            crate::normalize(&active_output)
                .expect("normalize active-frame PSD")
                .active_frame_index,
            Some(1)
        );

        let invalid_output = directory.join("invalid-active-output.psd");
        let error = export(
            &input,
            &composite,
            &invalid_output,
            &ExportOptions {
                active_frame_index: Some(3),
                ..Default::default()
            },
        )
        .expect_err("out-of-range active frame must be rejected");
        assert!(matches!(error, ExportError::AsepriteRead(_)));

        let original = bytes.clone();
        let error = export(&input, &composite, &output, &ExportOptions::default())
            .expect_err("existing output must be rejected");
        assert!(matches!(error, ExportError::OutputExists(_)));
        assert_eq!(fs::read(&output).expect("read preserved PSD"), original);

        let bad_composite = directory.join("bad-composite.aseprite");
        let mut bad = AsepriteFile::new(1, 1, AseColorMode::Rgba);
        bad.add_layer("Composite");
        bad.add_frame(100);
        write_aseprite(&bad_composite, &bad);
        let error = export(
            &input,
            &bad_composite,
            &output,
            &ExportOptions {
                overwrite: true,
                ..Default::default()
            },
        )
        .expect_err("invalid replacement must fail before commit");
        assert!(matches!(error, ExportError::AsepriteRead(_)));
        assert_eq!(fs::read(&output).expect("read rollback PSD"), original);
        fs::remove_dir_all(directory).expect("remove test directory");
    }

    #[test]
    fn empty_pixel_layer_policy_omits_only_layers_without_any_cel() {
        assert!(!ExportOptions::default().include_empty_layers);

        let empty = |id: u32| FrameSnapshotLayer {
            source_layer_id: id,
            name: format!("empty-{id}"),
            kind: NormalizedLayerKind::Pixel,
            opacity: None,
            blend_mode: None,
            visible: true,
            cel: None,
            children: Vec::new(),
        };
        let populated = |id: u32, has_cel: bool| FrameSnapshotLayer {
            source_layer_id: id,
            name: format!("populated-{id}"),
            kind: NormalizedLayerKind::Pixel,
            opacity: None,
            blend_mode: None,
            visible: true,
            cel: has_cel.then_some(FrameSnapshotCel {
                width: 1,
                height: 1,
                x: 0,
                y: 0,
                opacity: 255,
                pixels: vec![255, 0, 0, 255],
            }),
            children: Vec::new(),
        };
        let snapshots = vec![
            FrameSnapshot {
                layers: vec![empty(1), populated(2, true)],
            },
            FrameSnapshot {
                layers: vec![empty(1), populated(2, false)],
            },
        ];

        let filtered = omit_empty_pixel_layers(&snapshots);
        assert_eq!(filtered.len(), 2);
        assert_eq!(filtered[0].layers.len(), 1);
        assert_eq!(filtered[1].layers.len(), 1);
        assert_eq!(filtered[0].layers[0].source_layer_id, 2);
        assert_eq!(filtered[1].layers[0].source_layer_id, 2);
    }

    /// Generates a deterministic PSD fixture and verifies the complete Jitter import path.
    #[test]
    fn generated_jitter_fixture_reports_and_repairs_known_pixels() {
        let directory = std::env::temp_dir().join(format!(
            "aseprite-psd-jitter-fixture-{}",
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .expect("clock")
                .as_nanos()
        ));
        fs::create_dir_all(&directory).expect("create jitter fixture directory");
        let input = directory.join("jitter-source.aseprite");
        let composite = directory.join("jitter-composite.aseprite");
        let psd = directory.join("jitter-positive.psd");
        let output = directory.join("jitter-repair.aseprite");
        let width = 16usize;
        let height = 16usize;
        let transparent = vec![0; width * height * 4];
        let alpha_pixels = |speck: bool| {
            let mut pixels = transparent.clone();
            let opaque = (4 * width + 4) * 4;
            pixels[opaque..opaque + 4].copy_from_slice(&[220, 80, 40, 255]);
            if speck {
                let isolated = (10 * width + 10) * 4;
                pixels[isolated..isolated + 4].copy_from_slice(&[12, 34, 56, 4]);
            }
            pixels
        };
        let color_pixels = |variant: u8| {
            let mut pixels = transparent.clone();
            let base = (7 * width + 7) * 4;
            pixels[base..base + 4].copy_from_slice(&[80 + variant, 120, 180, 255]);
            pixels
        };

        let mut source = AsepriteFile::new(width as u16, height as u16, AseColorMode::Rgba);
        let alpha_layer = source.add_layer("Jitter Alpha");
        let color_layer = source.add_layer("Jitter Color");
        let frames = [
            source.add_frame(100),
            source.add_frame(100),
            source.add_frame(100),
        ];
        for (index, frame) in frames.iter().copied().enumerate() {
            source
                .set_cel(
                    alpha_layer,
                    frame,
                    Pixels::new(
                        alpha_pixels(index == 0),
                        width as u16,
                        height as u16,
                        AseColorMode::Rgba,
                    )
                    .expect("alpha fixture pixels"),
                    0,
                    0,
                )
                .expect("alpha fixture cel");
            source
                .set_cel(
                    color_layer,
                    frame,
                    Pixels::new(
                        color_pixels(if index == 1 { 4 } else { 0 }),
                        width as u16,
                        height as u16,
                        AseColorMode::Rgba,
                    )
                    .expect("color fixture pixels"),
                    0,
                    0,
                )
                .expect("color fixture cel");
        }
        write_aseprite(&input, &source);

        let mut flattened = AsepriteFile::new(width as u16, height as u16, AseColorMode::Rgba);
        let composite_layer = flattened.add_layer("Composite");
        let composite_frames = [
            flattened.add_frame(100),
            flattened.add_frame(100),
            flattened.add_frame(100),
        ];
        for frame in composite_frames {
            flattened
                .set_cel(
                    composite_layer,
                    frame,
                    Pixels::new(
                        transparent.clone(),
                        width as u16,
                        height as u16,
                        AseColorMode::Rgba,
                    )
                    .expect("composite fixture pixels"),
                    0,
                    0,
                )
                .expect("composite fixture cel");
        }
        write_aseprite(&composite, &flattened);

        export(&input, &composite, &psd, &ExportOptions::default()).expect("export jitter PSD");
        let report = crate::convert(
            &psd,
            &output,
            &ConvertOptions {
                layer_association: LayerAssociation::Auto(AutoAssociationOptions::default()),
                jitter: JitterOptions {
                    mode: JitterMode::Repair,
                    kind: JitterKind::All,
                    profile: JitterProfile::Conservative,
                    ..Default::default()
                },
                overwrite: true,
                ..Default::default()
            },
        )
        .expect("convert generated jitter PSD");
        let jitter = report.jitter.expect("jitter report");
        assert_eq!(jitter.alpha_candidates, 1);
        assert_eq!(jitter.alpha_repairs, 1);
        assert_eq!(jitter.color_candidates, 2);
        assert_eq!(jitter.color_repairs, 2);

        let keep = std::env::var_os("ASEPRITE_PSD_KEEP_JITTER_FIXTURE").is_some();
        if keep {
            let kept = Path::new(env!("CARGO_MANIFEST_DIR")).join("target");
            fs::create_dir_all(&kept).expect("create kept fixture directory");
            for file in [&input, &composite, &psd, &output] {
                fs::copy(file, kept.join(file.file_name().expect("fixture filename")))
                    .expect("copy kept jitter fixture");
            }
        } else {
            fs::remove_dir_all(directory).expect("remove jitter fixture directory");
        }
    }

    #[test]
    fn tilemap_export_uses_the_trusted_flattened_snapshot_and_reports_rasterization() {
        let directory = std::env::temp_dir().join(format!(
            "aseprite-psd-tilemap-export-{}",
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .expect("clock")
                .as_nanos()
        ));
        fs::create_dir_all(&directory).expect("create tilemap test directory");
        let input = directory.join("source.aseprite");
        let composite = directory.join("composite.aseprite");
        let output = directory.join("output.psd");

        let mut source = AsepriteFile::new(2, 2, AseColorMode::Rgba);
        source.add_tileset(Tileset {
            id: 0,
            flags: TilesetFlags(2),
            name: "tiles".to_string(),
            tile_count: 1,
            tile_width: 1,
            tile_height: 1,
            base_index: 0,
            data: TilesetData::Embedded {
                pixels: vec![255, 0, 0, 255],
                original_compressed: None,
            },
            user_data: None,
            tile_user_data: Vec::new(),
        });
        let tilemap = source.add_tilemap_layer("Terrain", 0);
        source.add_frame(100);
        source
            .set_tilemap_cel(tilemap, 0, vec![0, 0, 0, 0], 2, 2, 0, 0)
            .expect("tilemap cel");
        write_aseprite(&input, &source);

        let mut flattened = AsepriteFile::new(2, 2, AseColorMode::Rgba);
        let flat = flattened.add_layer("Composite");
        let frame = flattened.add_frame(100);
        flattened
            .set_cel(
                flat,
                frame,
                Pixels::new([255, 0, 0, 255].repeat(4), 2, 2, AseColorMode::Rgba)
                    .expect("tilemap composite pixels"),
                0,
                0,
            )
            .expect("tilemap composite cel");
        write_aseprite(&composite, &flattened);

        let report = export(&input, &composite, &output, &ExportOptions::default())
            .expect("export tilemap fallback");
        assert!(
            report
                .information_loss
                .entries
                .iter()
                .any(|loss| loss.code == crate::InformationLossCode::Tilemap)
        );
        let normalized = crate::normalize(&output).expect("normalize tilemap PSD");
        assert_eq!(normalized.root_layers[0].name, "Rasterized Composite");
        assert_eq!(normalized.root_layers[0].children.len(), 1);
        assert_eq!(
            normalized.root_layers[0].children[0]
                .pixels
                .as_ref()
                .expect("composite pixels")
                .data,
            [255, 0, 0, 255].repeat(4)
        );
        fs::remove_dir_all(directory).expect("remove tilemap test directory");
    }

    #[test]
    fn unknown_blend_mode_is_reported_when_writer_falls_back_to_normal() {
        let document = NormalizedDocument {
            canvas: (1, 1),
            channels: Some(4),
            bits_per_channel: Some(8),
            color_mode: Some("rgba".to_string()),
            root_layers: vec![NormalizedLayer {
                id: 7,
                name: "Future blend".to_string(),
                kind: NormalizedLayerKind::Pixel,
                bounds: crate::NormalizedBounds {
                    left: 0,
                    top: 0,
                    right: 1,
                    bottom: 1,
                },
                opacity: Some(0.5),
                blend_mode: Some("future-blend".to_string()),
                hidden: Some(false),
                pixels: Some(crate::NormalizedPixels {
                    width: 1,
                    height: 1,
                    left: 0,
                    top: 0,
                    data: vec![255, 0, 0, 255],
                }),
                children: Vec::new(),
                frame_states: vec![crate::NormalizedLayerFrameState {
                    frame_index: 0,
                    record_present: true,
                    enabled: true,
                    explicit_enable: true,
                    offset: None,
                    reference_point: None,
                    opacity: None,
                }],
            }],
            frames: vec![crate::NormalizedFrame {
                index: 0,
                source_id: Some(1),
                duration_ms: Some(100),
                dispose: Some("auto".to_string()),
            }],
            loop_mode: None,
            active_frame_index: None,
            animation_resource_ids: vec![4000],
            animation_frame_flags: None,
        };
        let mut report = InformationLossReport::default();
        let psd = build_psd(&document, &[vec![255, 0, 0, 255]], &mut report)
            .expect("future blend mode should fall back safely");

        assert_eq!(
            psd.children.as_ref().expect("layer")[0].blend_mode,
            Some(BlendMode::Normal)
        );
        assert_eq!(report.entries.len(), 1);
        let loss = &report.entries[0];
        assert_eq!(loss.code, crate::InformationLossCode::UnknownBlendMode);
        assert_eq!(loss.disposition, crate::LossDisposition::Degraded);
        assert_eq!(loss.count, 1);
        assert_eq!(loss.locations[0].path, "Future blend");
        assert!(loss.visual_impact);
        assert!(loss.editability_impact);
    }

    #[test]
    fn export_compression_tokens_are_stable_and_distinct() {
        let modes = [
            ExportCompression::Raw,
            ExportCompression::Rle,
            ExportCompression::Zip,
            ExportCompression::ZipPrediction,
        ];
        let tokens = modes.map(ExportCompression::as_str);
        assert_eq!(tokens, ["raw", "rle", "zip", "zip-prediction"]);
        for mode in modes {
            assert_eq!(ExportCompression::parse(mode.as_str()), Some(mode));
        }
        assert_eq!(ExportCompression::parse("unsupported"), None);
    }

    /// Writes one test Aseprite file through its authentic serializer.
    fn write_aseprite(path: &Path, file: &AsepriteFile) {
        let mut bytes = Vec::new();
        file.write_to(&mut bytes).expect("serialize Aseprite");
        fs::write(path, bytes).expect("write Aseprite");
    }
}
