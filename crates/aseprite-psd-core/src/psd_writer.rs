//! PSD/PSB writer and read-back validator for normalized Aseprite exports.

use std::collections::HashMap;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::path::{Path, PathBuf};

use ag_psd::frame_animation::{LayerFrameTrack, replace_frame_animation, validate_frame_animation};
use ag_psd::psd::{
    AnimationDispose, AnimationFrame, AnimationFrameInfo, AnimationInfo, Animations, BlendMode,
    ColorMode, Compression, ImageResources, Layer, LayerAdditionalInfo, PixelData, PointF, Psd,
    ReadOptions, SectionDividerType, WriteOptions,
};
use ag_psd::writer::{
    create_writer_default, get_writer_buffer, write_bytes, write_section, write_signature,
};

use crate::aseprite_reader::{
    FrameSnapshot, FrameSnapshotLayer, is_writable_pixel_cel,
    read_aseprite_export_with_active_frame,
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
    /// Channel compression policy; `None` uses Photoshop-compatible RLE.
    pub compression: Option<ExportCompression>,
    /// Embed private metadata that allows this converter to recover cel relationships.
    pub embed_roundtrip_metadata: bool,
    /// Include pixel layers that contain no cels in the exported document.
    pub include_empty_layers: bool,
    /// Physical-content reuse policy for animated Aseprite exports.
    pub content_reuse: ExportContentReuse,
}

/// Controls how an animated export reuses physical Photoshop layer content.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExportContentReuse {
    /// Materialize every playback frame independently.
    None,
    /// Reuse only Aseprite cels that explicitly share a linked-cel source.
    Linked,
    /// Also reuse independently authored states whose complete data is equal.
    Aggressive,
}

impl ExportContentReuse {
    /// Returns the stable CLI token for this reuse policy.
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::None => "none",
            Self::Linked => "linked",
            Self::Aggressive => "aggressive",
        }
    }

    /// Parses a stable CLI token.
    pub fn parse(value: &str) -> Option<Self> {
        Some(match value {
            "none" => Self::None,
            "linked" => Self::Linked,
            "aggressive" => Self::Aggressive,
            _ => return None,
        })
    }
}

impl Default for ExportContentReuse {
    fn default() -> Self {
        Self::None
    }
}

/// Compression modes supported by the PSD/PSB writer.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExportCompression {
    /// Store channel bytes without compression.
    Raw,
    /// Pack channel rows with PackBits RLE.
    Rle,
    /// ZIP-compress channel bytes without prediction for diagnostics only.
    Zip,
    /// ZIP-compress channel bytes after horizontal prediction for diagnostics only.
    ZipPrediction,
}

impl Default for ExportCompression {
    fn default() -> Self {
        Self::Rle
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
            content_reuse: ExportContentReuse::None,
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
    /// Reuse policy requested by the caller.
    pub requested_content_reuse: ExportContentReuse,
    /// Reuse policy actually used for the physical layout.
    pub actual_content_reuse: ExportContentReuse,
    /// Number of physical PSD layer records in the frame-by-frame baseline layout.
    pub baseline_physical_layer_count: usize,
    /// Number of physical PSD layer records emitted by the selected layout.
    pub physical_layer_count: usize,
    /// Number of explicit Aseprite linked-cel states reused by the layout.
    pub explicit_link_reuse_count: usize,
    /// Number of independently authored exact states reused by the layout.
    pub exact_match_reuse_count: usize,
    /// Reasons the requested layout was conservatively unavailable.
    pub content_reuse_fallbacks: Vec<String>,
    /// Final encoded PSD/PSB byte count.
    pub output_bytes: usize,
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

    let mut source =
        read_aseprite_export_with_active_frame(input, composite, options.active_frame_index)?;
    let mut information_loss = source.information_loss;
    if !options.include_empty_layers {
        information_loss
            .entries
            .retain(|entry| entry.code != crate::InformationLossCode::EmptyPixelLayer);
    }
    let (model, metadata, frame_first, mut reuse_stats) = if let Some(snapshots) = source
        .frame_snapshots
        .as_deref()
        .filter(|snapshots| snapshots.len() > 1)
    {
        build_frame_first_psd(
            &source.document,
            snapshots,
            &source.composites,
            &mut information_loss,
            options.embed_roundtrip_metadata,
            options.include_empty_layers,
            options.content_reuse,
        )?
    } else {
        if !options.include_empty_layers {
            omit_empty_pixel_layers(&mut source.document.root_layers);
        }
        let (model, _frame_ids) =
            build_psd(&source.document, &source.composites, &mut information_loss)?;
        let metadata = animation_metadata(&source.document, options.embed_roundtrip_metadata)?;
        (
            model.clone(),
            metadata,
            false,
            ContentReuseStats {
                baseline_layers: count_psd_layer_records(&model),
                ..Default::default()
            },
        )
    };
    if options.content_reuse != ExportContentReuse::None {
        if information_loss
            .entries
            .iter()
            .any(|entry| entry.code == crate::InformationLossCode::Tilemap)
        {
            reuse_stats.fallbacks.push(
                "tilemap rasterization is not eligible for structural content reuse".to_string(),
            );
        }
        if !frame_first {
            reuse_stats
                .fallbacks
                .push("content reuse requires multiple playback frames".to_string());
        }
    }
    validate_frame_animation(&model).map_err(|error| {
        ExportError::Writer(format!(
            "generated frame animation failed validation: {error}"
        ))
    })?;
    let compression = options.compression.unwrap_or_default();
    let write_options = WriteOptions {
        no_background: Some(true),
        psb: Some(psb),
        compression: Some(compression.ag_psd()),
        trim_image_data: Some(false),
        ..Default::default()
    };
    let encoded = catch_unwind(AssertUnwindSafe(|| {
        ag_psd::write_psd(&model, &write_options)
    }))
    .map_err(|_| ExportError::Writer("ag-psd panicked while encoding the document".to_string()))?;
    let encoded = inject_roundtrip_metadata(encoded, &metadata, psb)?;
    validate_output(
        &encoded,
        &source.document,
        &source.composites,
        psb,
        Some(compression),
        frame_first,
        reuse_stats.local_layout,
    )?;
    commit_bytes(output, &encoded, options.overwrite).map_err(ExportError::OutputIo)?;

    Ok(ExportReport {
        input: input.to_path_buf(),
        composite: composite.to_path_buf(),
        output: output.to_path_buf(),
        information_loss,
        active_frame_index: source.document.active_frame_index,
        requested_content_reuse: options.content_reuse,
        actual_content_reuse: reuse_stats.actual,
        baseline_physical_layer_count: reuse_stats.baseline_layers,
        physical_layer_count: count_psd_layer_records(&model),
        explicit_link_reuse_count: reuse_stats.explicit_link_reuse_count,
        exact_match_reuse_count: reuse_stats.exact_match_reuse_count,
        content_reuse_fallbacks: reuse_stats.fallbacks,
        output_bytes: encoded.len(),
    })
}

#[derive(Debug, Default)]
struct ContentReuseStats {
    actual: ExportContentReuse,
    baseline_layers: usize,
    explicit_link_reuse_count: usize,
    exact_match_reuse_count: usize,
    fallbacks: Vec<String>,
    local_layout: bool,
}

/// Counts emitted Photoshop records, including each group closing divider.
fn count_psd_layer_records(psd: &Psd) -> usize {
    let mut count = 0;
    let mut stack = psd
        .children
        .as_deref()
        .unwrap_or_default()
        .iter()
        .collect::<Vec<_>>();
    while let Some(layer) = stack.pop() {
        count += 1;
        if layer.children.is_some() {
            count += 1;
        }
        stack.extend(layer.children.iter().flatten());
    }
    count
}

/// Removes pixel layers with no writable cel while pruning empty group branches.
fn omit_empty_pixel_layers(layers: &mut Vec<NormalizedLayer>) {
    for layer in layers.iter_mut() {
        omit_empty_pixel_layers(&mut layer.children);
    }
    layers.retain(|layer| match layer.kind {
        NormalizedLayerKind::Group => !layer.children.is_empty(),
        NormalizedLayerKind::Pixel => layer.frame_states.iter().any(|state| {
            let opacity = state.opacity.or_else(|| state.enabled.then_some(1.0));
            is_writable_pixel_cel(
                opacity,
                layer.pixels.as_ref().map(|pixels| pixels.data.as_slice()),
            )
        }),
    });
}

/// Retains only frame-local layers that have a cel, pruning empty group branches.
///
/// Frame snapshots are materialized independently for each playback frame. A pixel layer
/// without a cel in the current snapshot must not become a synthetic 1x1 PSD layer when the
/// caller selected the default omit-empty policy.
fn filter_frame_snapshot_layers(
    layers: &[FrameSnapshotLayer],
    include_empty_layers: bool,
) -> Vec<FrameSnapshotLayer> {
    if include_empty_layers {
        return layers.to_vec();
    }

    struct Node<'a> {
        source: &'a FrameSnapshotLayer,
        children: Vec<usize>,
    }

    let mut nodes = Vec::new();
    let mut roots = Vec::with_capacity(layers.len());
    for source in layers {
        roots.push(nodes.len());
        nodes.push(Node {
            source,
            children: Vec::new(),
        });
    }

    let mut pending = roots.clone();
    while let Some(index) = pending.pop() {
        if nodes[index].source.kind != NormalizedLayerKind::Group {
            continue;
        }
        let child_sources = nodes[index].source.children.iter().collect::<Vec<_>>();
        let mut children = Vec::with_capacity(child_sources.len());
        for source in child_sources {
            let child = nodes.len();
            nodes.push(Node {
                source,
                children: Vec::new(),
            });
            children.push(child);
        }
        nodes[index].children = children.clone();
        pending.extend(children.into_iter().rev());
    }

    let mut built = (0..nodes.len())
        .map(|_| None)
        .collect::<Vec<Option<FrameSnapshotLayer>>>();
    for index in (0..nodes.len()).rev() {
        let node = &nodes[index];
        built[index] = match node.source.kind {
            NormalizedLayerKind::Pixel => {
                snapshot_cel_is_writable(node.source.cel.as_ref()).then(|| node.source.clone())
            }
            NormalizedLayerKind::Group => {
                let children = node
                    .children
                    .iter()
                    .filter_map(|child| built[*child].take())
                    .collect::<Vec<_>>();
                (!children.is_empty()).then(|| {
                    let mut retained = node.source.clone();
                    retained.children = children;
                    retained
                })
            }
        };
    }

    roots
        .into_iter()
        .filter_map(|index| built[index].take())
        .collect()
}

/// Builds a PSD whose top-level groups are the editable Aseprite playback frames.
///
/// Frame folders are deliberately retained: they are the user-visible animation model
/// for ordinary Aseprite documents and must not be replaced with one static hierarchy.
fn build_frame_first_psd(
    document: &NormalizedDocument,
    snapshots: &[FrameSnapshot],
    composites: &[Vec<u8>],
    report: &mut InformationLossReport,
    embed_roundtrip_metadata: bool,
    include_empty_layers: bool,
    content_reuse: ExportContentReuse,
) -> Result<(Psd, HashMap<u32, LayerMarker>, bool, ContentReuseStats), ExportError> {
    if snapshots.len() != document.frames.len() || snapshots.is_empty() {
        return Err(ExportError::Writer(
            "frame snapshot count differs from normalized document".to_string(),
        ));
    }
    let expected = usize::try_from(document.canvas.0)
        .ok()
        .and_then(|width| {
            usize::try_from(document.canvas.1)
                .ok()
                .and_then(|height| width.checked_mul(height))
        })
        .and_then(|area| area.checked_mul(4))
        .ok_or_else(|| {
            ExportError::Writer("composite dimensions exceed memory limits".to_string())
        })?;
    if composites
        .iter()
        .any(|composite| composite.len() != expected)
    {
        return Err(ExportError::Writer(
            "composite pixel size differs from normalized document".to_string(),
        ));
    }
    let active_frame = usize::try_from(document.active_frame_index.unwrap_or(0)).map_err(|_| {
        ExportError::Writer("active frame index exceeds platform limits".to_string())
    })?;
    if active_frame >= snapshots.len() {
        return Err(ExportError::Writer(
            "active frame index is outside the frame snapshots".to_string(),
        ));
    }
    let filtered_layers = snapshots
        .iter()
        .map(|snapshot| filter_frame_snapshot_layers(&snapshot.layers, include_empty_layers))
        .collect::<Vec<_>>();
    if content_reuse != ExportContentReuse::None {
        if let Some(local) = try_build_local_reuse_psd(
            document,
            snapshots,
            composites,
            report,
            embed_roundtrip_metadata,
            include_empty_layers,
            content_reuse,
        )? {
            return Ok(local);
        }
    }
    let mut frame_owners = Vec::with_capacity(snapshots.len());
    let mut physical_frames = Vec::<usize>::new();
    let mut explicit_link_reuse_count = 0;
    let mut exact_match_reuse_count = 0;
    for frame_index in 0..snapshots.len() {
        let owner = if content_reuse == ExportContentReuse::None {
            None
        } else {
            physical_frames.iter().position(|candidate| {
                snapshots_equal(
                    &filtered_layers[frame_index],
                    &filtered_layers[*candidate],
                    content_reuse,
                )
            })
        };
        if let Some(owner) = owner {
            frame_owners.push(owner);
            if content_reuse == ExportContentReuse::Linked {
                explicit_link_reuse_count += 1;
            } else {
                exact_match_reuse_count += 1;
            }
        } else {
            frame_owners.push(physical_frames.len());
            physical_frames.push(frame_index);
        }
    }
    let actual_content_reuse = if physical_frames.len() < snapshots.len() {
        content_reuse
    } else {
        ExportContentReuse::None
    };
    let mut physical_layer_count = physical_frames.len();
    for frame_index in &physical_frames {
        let layers = &filtered_layers[*frame_index];
        physical_layer_count = physical_layer_count
            .checked_add(count_frame_snapshot_layers(layers)?)
            .ok_or_else(|| ExportError::Writer("PSD layer count exceeds ID limits".to_string()))?;
    }
    let first_frame_id = u32::try_from(physical_layer_count)
        .map_err(|_| ExportError::Writer("PSD layer count exceeds ID limits".to_string()))?
        .checked_add(1)
        .ok_or_else(|| ExportError::Writer("frame ID allocation overflow".to_string()))?;
    let frame_ids = (0..snapshots.len())
        .map(|index| {
            let index = u32::try_from(index).map_err(|_| {
                ExportError::Writer("frame count exceeds PSD ID limits".to_string())
            })?;
            first_frame_id
                .checked_add(index)
                .map(f64::from)
                .ok_or_else(|| ExportError::Writer("frame ID allocation overflow".to_string()))
        })
        .collect::<Result<Vec<_>, _>>()?;
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
        .collect::<Vec<_>>();
    let repeats = match document.loop_mode {
        Some(NormalizedLoopMode::Infinite) | None => Some(0.0),
        Some(NormalizedLoopMode::Finite(value)) => Some(f64::from(value)),
    };
    let mut next_id = 1_u32;
    let mut metadata = HashMap::new();
    let mut layer_availability = HashMap::new();
    let mut children = Vec::with_capacity(physical_frames.len());
    for (physical_index, frame_index) in physical_frames.iter().copied().enumerate() {
        let id = take_export_id(&mut next_id)?;
        layer_availability.insert(id, true);
        if embed_roundtrip_metadata {
            metadata.insert(
                id,
                LayerMarker {
                    version: if actual_content_reuse == ExportContentReuse::None {
                        2
                    } else {
                        3
                    },
                    role: MarkerRole::FrameGroup,
                    logical_layer_id: if actual_content_reuse == ExportContentReuse::Linked {
                        u32::MAX
                    } else if actual_content_reuse == ExportContentReuse::Aggressive {
                        u32::MAX - 1
                    } else {
                        u32::MAX
                    },
                    variant_index: u32::try_from(physical_index + 1).map_err(|_| {
                        ExportError::Writer("frame count exceeds PSD ID limits".to_string())
                    })?,
                    variant_count: u32::try_from(snapshots.len()).map_err(|_| {
                        ExportError::Writer("frame count exceeds PSD ID limits".to_string())
                    })?,
                },
            );
        }
        let layers = build_frame_snapshot_layers(
            &filtered_layers[frame_index],
            frame_index,
            snapshots.len(),
            &mut next_id,
            report,
            &mut metadata,
            &mut layer_availability,
            embed_roundtrip_metadata,
            if actual_content_reuse == ExportContentReuse::None {
                2
            } else {
                3
            },
        )?;
        children.push(Layer {
            additional_info: LayerAdditionalInfo {
                name: Some(if actual_content_reuse == ExportContentReuse::None {
                    format!("Frame {}", physical_index + 1)
                } else {
                    format!("State {}", physical_index + 1)
                }),
                id: Some(f64::from(id)),
                ..Default::default()
            },
            children: Some(layers),
            opened: Some(true),
            // Frame-folder visibility is static layer presentation, not the per-frame
            // animation state. Keep every frame folder visible so Photoshop can apply
            // the corresponding mlst entry when the timeline changes frames.
            hidden: Some(false),
            blend_mode: Some(BlendMode::Normal),
            ..Default::default()
        });
    }
    let mut model = Psd {
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
            ids_seed_number: Some(f64::from(next_id.saturating_sub(1))),
            animations: Some(Animations {
                frames,
                animations: vec![AnimationInfo {
                    id: 0.0,
                    frames: frame_ids.clone(),
                    repeats,
                    active_frame: document.active_frame_index.map(f64::from),
                }],
            }),
            ..Default::default()
        }),
        ..Default::default()
    };
    let animations = model
        .image_resources
        .as_mut()
        .and_then(|resources| resources.animations.take())
        .ok_or_else(|| {
            ExportError::Writer("frame animation directory was not created".to_string())
        })?;
    let tracks =
        collect_generated_frame_tracks(&model, &frame_ids, &layer_availability, &frame_owners)?;
    replace_frame_animation(&mut model, animations, tracks.clone()).map_err(|error| {
        ExportError::Writer(format!("invalid generated frame animation: {error}"))
    })?;
    apply_static_visibility_from_tracks(&mut model, &tracks)?;
    let baseline_layers = snapshots
        .iter()
        .map(|snapshot| {
            count_frame_snapshot_layers(&filter_frame_snapshot_layers(
                &snapshot.layers,
                include_empty_layers,
            ))
        })
        .collect::<Result<Vec<_>, _>>()?
        .into_iter()
        .sum::<usize>()
        + snapshots.len();
    Ok((
        model,
        metadata,
        true,
        ContentReuseStats {
            actual: actual_content_reuse,
            baseline_layers,
            explicit_link_reuse_count,
            exact_match_reuse_count,
            fallbacks: Vec::new(),
            local_layout: false,
        },
    ))
}

#[derive(Debug, Clone)]
struct LocalReusePlan {
    source: FrameSnapshotLayer,
    cels: Vec<Option<crate::aseprite_reader::FrameSnapshotCel>>,
    state_indices: Vec<Option<usize>>,
    children: Vec<LocalReusePlan>,
}

/// Attempts a logical-layer layout that shares repeated pixel-layer states across frames.
///
/// This planner is intentionally conservative: it requires stable source IDs, names, kinds,
/// child order, and base display properties across every playback frame. Unsupported topology
/// falls back to the frame-folder layout selected by the caller.
#[allow(clippy::too_many_arguments)]
fn try_build_local_reuse_psd(
    document: &NormalizedDocument,
    snapshots: &[FrameSnapshot],
    composites: &[Vec<u8>],
    report: &mut InformationLossReport,
    embed_roundtrip_metadata: bool,
    include_empty_layers: bool,
    content_reuse: ExportContentReuse,
) -> Result<Option<(Psd, HashMap<u32, LayerMarker>, bool, ContentReuseStats)>, ExportError> {
    let mut plans = Vec::new();
    for template in &snapshots[0].layers {
        match build_local_reuse_plan(template, snapshots, content_reuse, include_empty_layers) {
            Some(plan) => plans.push(plan),
            None if !include_empty_layers
                && !snapshot_layer_has_writable_cel(template, snapshots) => {}
            None => return Ok(None),
        }
    }
    if plans.is_empty() {
        return Ok(None);
    }
    let baseline_layers = snapshots
        .iter()
        .map(|snapshot| {
            count_frame_snapshot_layers(&filter_frame_snapshot_layers(
                &snapshot.layers,
                include_empty_layers,
            ))
        })
        .collect::<Result<Vec<_>, _>>()?
        .into_iter()
        .sum::<usize>()
        + snapshots.len();
    let local_layers = plans.iter().map(count_local_reuse_records).sum::<usize>();
    if local_layers + 1 >= baseline_layers {
        return Ok(None);
    }

    let active_frame = usize::try_from(document.active_frame_index.unwrap_or(0)).map_err(|_| {
        ExportError::Writer("active frame index exceeds platform limits".to_string())
    })?;
    let mut next_id = 1_u32;
    let mut metadata = HashMap::new();
    let mut availability = HashMap::<u32, Vec<bool>>::new();
    let mut roots = Vec::with_capacity(plans.len());
    for plan in &plans {
        roots.push(build_local_reuse_layer(
            plan,
            snapshots.len(),
            &mut next_id,
            report,
            &mut metadata,
            &mut availability,
            embed_roundtrip_metadata,
        )?);
    }
    if let Some(root) = roots.first() {
        if let Some(id) = root.additional_info.id.and_then(exact_export_id) {
            if embed_roundtrip_metadata {
                metadata.insert(
                    id,
                    LayerMarker {
                        version: 3,
                        role: MarkerRole::FrameGroup,
                        logical_layer_id: if content_reuse == ExportContentReuse::Linked {
                            crate::roundtrip::LOCAL_LINKED_CONTENT_REUSE_MARKER_ID
                        } else {
                            crate::roundtrip::LOCAL_CONTENT_REUSE_MARKER_ID
                        },
                        variant_index: 1,
                        variant_count: u32::try_from(snapshots.len()).map_err(|_| {
                            ExportError::Writer("frame count exceeds PSD limits".to_string())
                        })?,
                    },
                );
            }
        }
    }
    let frame_ids = (0..snapshots.len())
        .map(|_| Ok(f64::from(take_export_id(&mut next_id)?)))
        .collect::<Result<Vec<_>, ExportError>>()?;
    let frames = document
        .frames
        .iter()
        .enumerate()
        .map(|(index, frame)| AnimationFrameInfo {
            id: frame_ids[index],
            delay: f64::from(frame.duration_ms.unwrap_or(100)) / 1000.0,
            dispose: Some(AnimationDispose::Auto),
        })
        .collect::<Vec<_>>();
    let repeats = match document.loop_mode {
        Some(NormalizedLoopMode::Infinite) | None => Some(0.0),
        Some(NormalizedLoopMode::Finite(value)) => Some(f64::from(value)),
    };
    let mut model = Psd {
        width: f64::from(document.canvas.0),
        height: f64::from(document.canvas.1),
        channels: Some(4.0),
        bits_per_channel: Some(8.0),
        color_mode: Some(ColorMode::Rgb),
        children: Some(roots),
        image_data: Some(PixelData {
            width: document.canvas.0,
            height: document.canvas.1,
            data: composites[active_frame].clone(),
        }),
        image_resources: Some(ImageResources {
            ids_seed_number: Some(f64::from(next_id.saturating_sub(1))),
            animations: Some(Animations {
                frames,
                animations: vec![AnimationInfo {
                    id: 0.0,
                    frames: frame_ids.clone(),
                    repeats,
                    active_frame: document.active_frame_index.map(f64::from),
                }],
            }),
            ..Default::default()
        }),
        ..Default::default()
    };
    let animations = model
        .image_resources
        .as_mut()
        .and_then(|resources| resources.animations.take())
        .ok_or_else(|| {
            ExportError::Writer("frame animation directory was not created".to_string())
        })?;
    let tracks = collect_local_frame_tracks(&model, &frame_ids, &availability)?;
    replace_frame_animation(&mut model, animations, tracks.clone()).map_err(|error| {
        ExportError::Writer(format!("invalid generated frame animation: {error}"))
    })?;
    apply_static_visibility_from_tracks(&mut model, &tracks)?;
    let (explicit_link_reuse_count, exact_match_reuse_count) =
        count_local_reuse_matches(&plans, content_reuse);
    Ok(Some((
        model,
        metadata,
        true,
        ContentReuseStats {
            actual: content_reuse,
            baseline_layers,
            explicit_link_reuse_count,
            exact_match_reuse_count,
            fallbacks: Vec::new(),
            local_layout: true,
        },
    )))
}

fn build_local_reuse_plan(
    template: &FrameSnapshotLayer,
    snapshots: &[FrameSnapshot],
    mode: ExportContentReuse,
    include_empty_layers: bool,
) -> Option<LocalReusePlan> {
    let matching = snapshots
        .iter()
        .map(|snapshot| find_snapshot_layer(&snapshot.layers, template.source_layer_id))
        .collect::<Option<Vec<_>>>()?;
    if matching.iter().any(|layer| {
        layer.name != template.name
            || layer.kind != template.kind
            || layer.opacity != template.opacity
            || layer.blend_mode != template.blend_mode
            || layer
                .children
                .iter()
                .map(|child| child.source_layer_id)
                .collect::<Vec<_>>()
                != template
                    .children
                    .iter()
                    .map(|child| child.source_layer_id)
                    .collect::<Vec<_>>()
    }) {
        return None;
    }
    if template.kind == NormalizedLayerKind::Pixel {
        let cels = matching
            .iter()
            .map(|layer| layer.cel.clone())
            .collect::<Vec<_>>();
        if !include_empty_layers
            && cels
                .iter()
                .all(|cel| !snapshot_cel_is_writable(cel.as_ref()))
        {
            return None;
        }
        let mut unique = Vec::<Option<crate::aseprite_reader::FrameSnapshotCel>>::new();
        let mut state_indices = Vec::with_capacity(cels.len());
        for cel in &cels {
            if !include_empty_layers && !snapshot_cel_is_writable(cel.as_ref()) {
                state_indices.push(None);
                continue;
            }
            let state = unique.iter().position(|candidate| match (candidate, cel) {
                (None, None) => true,
                (Some(left), Some(right)) => snapshot_cel_equal(left, right, mode),
                _ => false,
            });
            let index = state.unwrap_or_else(|| {
                unique.push(cel.clone());
                unique.len() - 1
            });
            state_indices.push(Some(index));
        }
        return Some(LocalReusePlan {
            source: template.clone(),
            cels: unique,
            state_indices,
            children: Vec::new(),
        });
    }
    let mut children = Vec::new();
    for child in &template.children {
        match build_local_reuse_plan(child, snapshots, mode, include_empty_layers) {
            Some(plan) => children.push(plan),
            None if !include_empty_layers && !snapshot_layer_has_writable_cel(child, snapshots) => {
            }
            None => return None,
        }
    }
    if !include_empty_layers && !template.children.is_empty() && children.is_empty() {
        return None;
    }
    Some(LocalReusePlan {
        source: template.clone(),
        cels: vec![None],
        state_indices: vec![Some(0); snapshots.len()],
        children,
    })
}

fn snapshot_layer_has_writable_cel(
    template: &FrameSnapshotLayer,
    snapshots: &[FrameSnapshot],
) -> bool {
    snapshots.iter().any(|snapshot| {
        find_snapshot_layer(&snapshot.layers, template.source_layer_id).is_some_and(|layer| {
            snapshot_cel_is_writable(layer.cel.as_ref())
                || layer
                    .children
                    .iter()
                    .any(|child| snapshot_layer_has_writable_cel(child, snapshots))
        })
    })
}

/// Applies the shared cel predicate to one frame-local snapshot cel.
fn snapshot_cel_is_writable(cel: Option<&crate::aseprite_reader::FrameSnapshotCel>) -> bool {
    cel.is_some_and(|cel| {
        is_writable_pixel_cel(
            Some(f64::from(cel.opacity) / 255.0),
            Some(cel.pixels.as_slice()),
        )
    })
}

fn find_snapshot_layer<'a>(
    layers: &'a [FrameSnapshotLayer],
    id: u32,
) -> Option<&'a FrameSnapshotLayer> {
    layers.iter().find_map(|layer| {
        (layer.source_layer_id == id)
            .then_some(layer)
            .or_else(|| find_snapshot_layer(&layer.children, id))
    })
}

fn snapshot_cel_equal(
    left: &crate::aseprite_reader::FrameSnapshotCel,
    right: &crate::aseprite_reader::FrameSnapshotCel,
    mode: ExportContentReuse,
) -> bool {
    let same_display = left.width == right.width
        && left.height == right.height
        && left.x == right.x
        && left.y == right.y
        && left.opacity == right.opacity
        && left.pixels == right.pixels;
    if !same_display {
        return false;
    }
    match mode {
        ExportContentReuse::Aggressive => true,
        ExportContentReuse::Linked => {
            left.linked_source_frame == right.linked_source_frame
                && ((left.explicitly_linked && right.explicitly_linked)
                    || (!left.explicitly_linked
                        && !right.explicitly_linked
                        && left.source_frame == right.source_frame)
                    || (left.explicitly_linked && right.source_frame == right.linked_source_frame)
                    || (right.explicitly_linked && left.source_frame == left.linked_source_frame))
        }
        ExportContentReuse::None => false,
    }
}

fn count_local_reuse_records(plan: &LocalReusePlan) -> usize {
    if plan.source.kind == NormalizedLayerKind::Pixel {
        if plan.cels.len() <= 1 {
            1
        } else {
            plan.cels.len() + 2
        }
    } else {
        2 + plan
            .children
            .iter()
            .map(count_local_reuse_records)
            .sum::<usize>()
    }
}

fn count_local_reuse_matches(plans: &[LocalReusePlan], mode: ExportContentReuse) -> (usize, usize) {
    fn walk(
        plan: &LocalReusePlan,
        mode: ExportContentReuse,
        explicit: &mut usize,
        exact: &mut usize,
    ) {
        if plan.source.kind == NormalizedLayerKind::Pixel {
            let mut first = HashMap::<usize, usize>::new();
            for (frame, state) in plan.state_indices.iter().enumerate() {
                if let Some(state) = state {
                    if first.insert(*state, frame).is_some() {
                        if mode == ExportContentReuse::Linked {
                            *explicit += 1;
                        }
                        if mode == ExportContentReuse::Aggressive {
                            *exact += 1;
                        }
                    }
                }
            }
        }
        for child in &plan.children {
            walk(child, mode, explicit, exact);
        }
    }
    let mut explicit = 0;
    let mut exact = 0;
    for plan in plans {
        walk(plan, mode, &mut explicit, &mut exact);
    }
    (explicit, exact)
}

#[allow(clippy::too_many_arguments)]
fn build_local_reuse_layer(
    plan: &LocalReusePlan,
    frame_count: usize,
    next_id: &mut u32,
    report: &mut InformationLossReport,
    metadata: &mut HashMap<u32, LayerMarker>,
    availability: &mut HashMap<u32, Vec<bool>>,
    embed_roundtrip_metadata: bool,
) -> Result<Layer, ExportError> {
    let source = &plan.source;
    let id = take_export_id(next_id)?;
    let (blend_mode, unknown_blend) = psd_blend_mode(source.blend_mode.as_deref());
    if unknown_blend {
        report.add(
            crate::InformationLossCode::UnknownBlendMode,
            crate::LossDisposition::Degraded,
            crate::InformationLocation {
                layer_id: Some(source.source_layer_id),
                path: source.name.clone(),
                frame_index: None,
            },
            "A blend mode that is not supported by the PSD writer was written as Normal",
            true,
            true,
        );
    }
    if source.kind == NormalizedLayerKind::Group {
        let children = plan
            .children
            .iter()
            .map(|child| {
                build_local_reuse_layer(
                    child,
                    frame_count,
                    next_id,
                    report,
                    metadata,
                    availability,
                    embed_roundtrip_metadata,
                )
            })
            .collect::<Result<Vec<_>, _>>()?;
        let divider_id = take_export_id(next_id)?;
        availability.insert(id, vec![source.visible; frame_count]);
        availability.insert(divider_id, vec![source.visible; frame_count]);
        if embed_roundtrip_metadata {
            insert_frame_snapshot_marker(
                metadata,
                id,
                source.source_layer_id,
                0,
                frame_count,
                true,
                3,
            )?;
        }
        return Ok(Layer {
            additional_info: LayerAdditionalInfo {
                name: Some(source.name.clone()),
                id: Some(f64::from(id)),
                ..Default::default()
            },
            children: Some(children),
            bounding_divider_additional_info: Some(LayerAdditionalInfo {
                name: Some("</Layer group>".to_string()),
                id: Some(f64::from(divider_id)),
                ..Default::default()
            }),
            opened: Some(true),
            hidden: Some(!source.visible),
            blend_mode: Some(blend_mode),
            opacity: source.opacity.map(|value| f64::from(value) / 255.0),
            ..Default::default()
        });
    }

    let build_pixel =
        |name: String, cel: Option<&crate::aseprite_reader::FrameSnapshotCel>, id: u32| -> Layer {
            let mut layer = Layer {
                additional_info: LayerAdditionalInfo {
                    name: Some(name),
                    id: Some(f64::from(id)),
                    ..Default::default()
                },
                blend_mode: Some(blend_mode),
                opacity: source.opacity.map(|value| f64::from(value) / 255.0),
                hidden: Some(false),
                ..Default::default()
            };
            if let Some(cel) = cel {
                layer.opacity = Some(f64::from(cel.opacity) / 255.0);
                layer.top = Some(f64::from(cel.y));
                layer.left = Some(f64::from(cel.x));
                layer.bottom = Some(f64::from(cel.y) + f64::from(cel.height));
                layer.right = Some(f64::from(cel.x) + f64::from(cel.width));
                layer.image_data = Some(PixelData {
                    width: cel.width,
                    height: cel.height,
                    data: cel.pixels.clone(),
                });
            } else {
                layer.top = Some(0.0);
                layer.left = Some(0.0);
                layer.bottom = Some(1.0);
                layer.right = Some(1.0);
                layer.image_data = Some(PixelData {
                    width: 1,
                    height: 1,
                    data: vec![0, 0, 0, 0],
                });
            }
            layer
        };
    let mut variants = Vec::with_capacity(plan.cels.len());
    let has_wrapper = plan.cels.len() > 1;
    for (variant_index, cel) in plan.cels.iter().enumerate() {
        let variant_id = if !has_wrapper && variant_index == 0 {
            id
        } else {
            take_export_id(next_id)?
        };
        let name = if plan.cels.len() == 1 {
            source.name.clone()
        } else {
            format!("State {}", variant_index + 1)
        };
        let layer = build_pixel(name, cel.as_ref(), variant_id);
        let enabled = plan
            .state_indices
            .iter()
            .map(|state| source.visible && state == &Some(variant_index))
            .collect::<Vec<_>>();
        availability.insert(variant_id, enabled);
        if embed_roundtrip_metadata {
            let representative = plan
                .state_indices
                .iter()
                .position(|state| state == &Some(variant_index))
                .unwrap_or(0);
            insert_frame_snapshot_marker(
                metadata,
                variant_id,
                source.source_layer_id,
                representative,
                frame_count,
                true,
                3,
            )?;
        }
        variants.push(layer);
    }
    if variants.len() == 1 {
        return Ok(variants.remove(0));
    }
    availability.insert(id, vec![source.visible; frame_count]);
    let divider_id = take_export_id(next_id)?;
    availability.insert(divider_id, vec![source.visible; frame_count]);
    Ok(Layer {
        additional_info: LayerAdditionalInfo {
            name: Some(source.name.clone()),
            id: Some(f64::from(id)),
            ..Default::default()
        },
        children: Some(variants),
        bounding_divider_additional_info: Some(LayerAdditionalInfo {
            name: Some("</Layer group>".to_string()),
            id: Some(f64::from(divider_id)),
            ..Default::default()
        }),
        opened: Some(true),
        hidden: Some(!source.visible),
        blend_mode: Some(blend_mode),
        opacity: source.opacity.map(|value| f64::from(value) / 255.0),
        ..Default::default()
    })
}

fn collect_local_frame_tracks(
    psd: &Psd,
    frame_ids: &[f64],
    availability: &HashMap<u32, Vec<bool>>,
) -> Result<Vec<LayerFrameTrack>, ExportError> {
    let mut tracks = Vec::new();
    let mut stack = psd
        .children
        .as_deref()
        .unwrap_or_default()
        .iter()
        .collect::<Vec<_>>();
    while let Some(layer) = stack.pop() {
        let id = layer
            .additional_info
            .id
            .and_then(exact_export_id)
            .ok_or_else(|| ExportError::Writer("generated layer has no valid id".to_string()))?;
        let states = availability.get(&id).ok_or_else(|| {
            ExportError::Writer(format!("generated layer {id} has no availability state"))
        })?;
        if states.len() != frame_ids.len() {
            return Err(ExportError::Writer(
                "local reuse track length differs from timeline".to_string(),
            ));
        }
        tracks.push(LayerFrameTrack {
            layer_id: id,
            states: frame_ids
                .iter()
                .enumerate()
                .map(|(index, frame_id)| AnimationFrame {
                    frames: vec![*frame_id],
                    enable: Some(states[index]),
                    offset: None,
                    reference_point: None,
                    opacity: None,
                    effects: None,
                })
                .collect(),
            flags: None,
        });
        if let Some(divider) = layer.bounding_divider_additional_info.as_ref() {
            let divider_id = divider.id.and_then(exact_export_id).ok_or_else(|| {
                ExportError::Writer("generated divider has no valid id".to_string())
            })?;
            let states = availability.get(&divider_id).ok_or_else(|| {
                ExportError::Writer(format!(
                    "generated divider {divider_id} has no availability state"
                ))
            })?;
            tracks.push(LayerFrameTrack {
                layer_id: divider_id,
                states: frame_ids
                    .iter()
                    .enumerate()
                    .map(|(index, frame_id)| AnimationFrame {
                        frames: vec![*frame_id],
                        enable: Some(states[index]),
                        offset: None,
                        reference_point: None,
                        opacity: None,
                        effects: None,
                    })
                    .collect(),
                flags: None,
            });
        }
        stack.extend(layer.children.iter().flatten());
    }
    Ok(tracks)
}

/// Compares complete frame-local layer trees without crossing source-layer boundaries.
fn snapshots_equal(
    left: &[FrameSnapshotLayer],
    right: &[FrameSnapshotLayer],
    mode: ExportContentReuse,
) -> bool {
    left.len() == right.len()
        && left
            .iter()
            .zip(right)
            .all(|(left, right)| snapshot_layer_equal(left, right, mode))
}

fn snapshot_layer_equal(
    left: &FrameSnapshotLayer,
    right: &FrameSnapshotLayer,
    mode: ExportContentReuse,
) -> bool {
    left.source_layer_id == right.source_layer_id
        && left.name == right.name
        && left.kind == right.kind
        && left.opacity == right.opacity
        && left.blend_mode == right.blend_mode
        && left.visible == right.visible
        && match (&left.cel, &right.cel) {
            (None, None) => true,
            (Some(left), Some(right)) => {
                let same_display = left.width == right.width
                    && left.height == right.height
                    && left.x == right.x
                    && left.y == right.y
                    && left.opacity == right.opacity
                    && left.pixels == right.pixels;
                let same_link = left.linked_source_frame == right.linked_source_frame
                    && ((left.explicitly_linked && right.explicitly_linked)
                        || (!left.explicitly_linked
                            && !right.explicitly_linked
                            && left.source_frame == right.source_frame)
                        || left.explicitly_linked
                            && right.source_frame == right.linked_source_frame
                        || right.explicitly_linked
                            && left.source_frame == left.linked_source_frame);
                match mode {
                    ExportContentReuse::Linked => same_link && same_display,
                    ExportContentReuse::Aggressive => same_display,
                    ExportContentReuse::None => false,
                }
            }
            _ => false,
        }
        && left.children.len() == right.children.len()
        && left
            .children
            .iter()
            .zip(&right.children)
            .all(|(left, right)| snapshot_layer_equal(left, right, mode))
}

/// Counts physical layer records in a frame snapshot without recursing through user depth.
fn count_frame_snapshot_layers(roots: &[FrameSnapshotLayer]) -> Result<usize, ExportError> {
    let mut count = 0_usize;
    let mut stack = roots.iter().collect::<Vec<_>>();
    while let Some(layer) = stack.pop() {
        count = count
            .checked_add(1)
            .ok_or_else(|| ExportError::Writer("PSD layer count exceeds ID limits".to_string()))?;
        stack.extend(layer.children.iter());
        if layer.kind == NormalizedLayerKind::Group {
            count = count.checked_add(1).ok_or_else(|| {
                ExportError::Writer("PSD layer count exceeds ID limits".to_string())
            })?;
        }
    }
    Ok(count)
}

/// Creates one typed Photoshop frame track for every generated physical record.
fn collect_generated_frame_tracks(
    psd: &Psd,
    frame_ids: &[f64],
    layer_availability: &HashMap<u32, bool>,
    frame_owners: &[usize],
) -> Result<Vec<LayerFrameTrack>, ExportError> {
    let mut tracks = Vec::new();
    let mut stack = psd
        .children
        .as_deref()
        .unwrap_or_default()
        .iter()
        .enumerate()
        .map(|(index, layer)| (layer, index))
        .collect::<Vec<_>>();
    while let Some((layer, owner_frame)) = stack.pop() {
        let layer_id = layer
            .additional_info
            .id
            .and_then(exact_export_id)
            .ok_or_else(|| ExportError::Writer("generated layer has no valid id".to_string()))?;
        let enabled = *layer_availability.get(&layer_id).ok_or_else(|| {
            ExportError::Writer(format!(
                "generated layer {layer_id} has no availability state"
            ))
        })?;
        let states = frame_ids
            .iter()
            .enumerate()
            .map(|(index, frame_id)| AnimationFrame {
                frames: vec![*frame_id],
                enable: Some(enabled && frame_owners[index] == owner_frame),
                offset: None,
                reference_point: None,
                opacity: None,
                effects: None,
            })
            .collect();
        tracks.push(LayerFrameTrack {
            layer_id,
            states,
            flags: None,
        });
        if let Some(divider) = layer.bounding_divider_additional_info.as_ref() {
            let divider_id = divider.id.and_then(exact_export_id).ok_or_else(|| {
                ExportError::Writer("generated divider has no valid id".to_string())
            })?;
            let divider_enabled = *layer_availability.get(&divider_id).ok_or_else(|| {
                ExportError::Writer(format!(
                    "generated divider {divider_id} has no availability state"
                ))
            })?;
            tracks.push(LayerFrameTrack {
                layer_id: divider_id,
                states: frame_ids
                    .iter()
                    .enumerate()
                    .map(|(index, frame_id)| AnimationFrame {
                        frames: vec![*frame_id],
                        enable: Some(divider_enabled && frame_owners[index] == owner_frame),
                        offset: None,
                        reference_point: None,
                        opacity: None,
                        effects: None,
                    })
                    .collect(),
                flags: None,
            });
        }
        stack.extend(
            layer
                .children
                .iter()
                .flatten()
                .map(|child| (child, owner_frame)),
        );
    }
    Ok(tracks)
}

/// Applies the first typed frame state as Photoshop's static visibility baseline.
fn apply_static_visibility_from_tracks(
    psd: &mut Psd,
    tracks: &[LayerFrameTrack],
) -> Result<(), ExportError> {
    let initial_visibility = tracks
        .iter()
        .map(|track| {
            let enabled = track
                .states
                .first()
                .and_then(|state| state.enable)
                .ok_or_else(|| {
                    ExportError::Writer(format!(
                        "generated animation track {} has no explicit first-frame state",
                        track.layer_id
                    ))
                })?;
            Ok::<_, ExportError>((track.layer_id, enabled))
        })
        .collect::<Result<HashMap<_, _>, _>>()?;

    let mut paths = Vec::new();
    if let Some(children) = psd.children.as_deref() {
        paths.extend((0..children.len()).map(|index| vec![index]));
    }
    while let Some(path) = paths.pop() {
        let layer = layer_at_path_mut(psd.children.as_mut(), &path)?;
        let layer_id = layer
            .additional_info
            .id
            .and_then(exact_export_id)
            .ok_or_else(|| ExportError::Writer("generated layer has no valid id".to_string()))?;
        let enabled = initial_visibility.get(&layer_id).copied().ok_or_else(|| {
            ExportError::Writer(format!("generated layer {layer_id} has no animation track"))
        })?;
        layer.hidden = Some(!enabled);
        let child_count = layer.children.as_ref().map_or(0, Vec::len);
        paths.extend((0..child_count).map(|index| {
            let mut child_path = path.clone();
            child_path.push(index);
            child_path
        }));
    }
    Ok(())
}

/// Resolves one generated layer by an index path without recursive traversal.
fn layer_at_path_mut<'a>(
    roots: Option<&'a mut Vec<Layer>>,
    path: &[usize],
) -> Result<&'a mut Layer, ExportError> {
    let layers = roots.ok_or_else(|| ExportError::Writer("PSD has no layer roots".to_string()))?;
    let first = *path
        .first()
        .ok_or_else(|| ExportError::Writer("generated layer path is empty".to_string()))?;
    let mut layer = layers
        .get_mut(first)
        .ok_or_else(|| ExportError::Writer("generated layer path is invalid".to_string()))?;
    for index in &path[1..] {
        layer = layer
            .children
            .as_mut()
            .and_then(|children| children.get_mut(*index))
            .ok_or_else(|| ExportError::Writer("generated layer path is invalid".to_string()))?;
    }
    Ok(layer)
}

fn exact_export_id(value: f64) -> Option<u32> {
    (value.is_finite() && value > 0.0 && value.fract() == 0.0 && value <= u32::MAX as f64)
        .then_some(value as u32)
}

/// Builds each frame folder's nested layers bottom-up without recursing through user depth.
#[allow(clippy::too_many_arguments)]
fn build_frame_snapshot_layers(
    roots: &[FrameSnapshotLayer],
    frame_index: usize,
    frame_count: usize,
    next_id: &mut u32,
    report: &mut InformationLossReport,
    metadata: &mut HashMap<u32, LayerMarker>,
    layer_availability: &mut HashMap<u32, bool>,
    embed_roundtrip_metadata: bool,
    marker_version: u16,
) -> Result<Vec<Layer>, ExportError> {
    struct Node<'a> {
        source: &'a FrameSnapshotLayer,
        path: String,
        children: Vec<usize>,
    }

    let mut nodes = Vec::new();
    let mut root_nodes = Vec::new();
    for source in roots {
        root_nodes.push(nodes.len());
        nodes.push(Node {
            source,
            path: source.name.clone(),
            children: Vec::new(),
        });
    }
    let mut pending = root_nodes.clone();
    while let Some(parent) = pending.pop() {
        let parent_path = nodes[parent].path.clone();
        let mut children = Vec::with_capacity(nodes[parent].source.children.len());
        for source in &nodes[parent].source.children {
            let child = nodes.len();
            nodes.push(Node {
                source,
                path: format!("{parent_path}/{}", source.name),
                children: Vec::new(),
            });
            children.push(child);
        }
        nodes[parent].children = children.clone();
        pending.extend(children.into_iter().rev());
    }

    let mut built = (0..nodes.len())
        .map(|_| None)
        .collect::<Vec<Option<Layer>>>();
    for index in (0..nodes.len()).rev() {
        let node = &nodes[index];
        let children = node
            .children
            .iter()
            .map(|child| {
                built[*child].take().ok_or_else(|| {
                    ExportError::Writer(
                        "frame-folder PSD post-order construction failed".to_string(),
                    )
                })
            })
            .collect::<Result<Vec<_>, _>>()?;
        built[index] = Some(build_frame_snapshot_layer(
            node.source,
            frame_index,
            frame_count,
            next_id,
            report,
            metadata,
            layer_availability,
            embed_roundtrip_metadata,
            marker_version,
            &node.path,
            children,
        )?);
    }
    root_nodes
        .into_iter()
        .map(|index| {
            built[index].take().ok_or_else(|| {
                ExportError::Writer("frame-folder PSD root construction failed".to_string())
            })
        })
        .collect()
}

/// Converts one original Aseprite layer for a specific editable frame folder.
#[allow(clippy::too_many_arguments)]
fn build_frame_snapshot_layer(
    source: &FrameSnapshotLayer,
    frame_index: usize,
    frame_count: usize,
    next_id: &mut u32,
    report: &mut InformationLossReport,
    metadata: &mut HashMap<u32, LayerMarker>,
    layer_availability: &mut HashMap<u32, bool>,
    embed_roundtrip_metadata: bool,
    marker_version: u16,
    parent_path: &str,
    children: Vec<Layer>,
) -> Result<Layer, ExportError> {
    let id = take_export_id(next_id)?;
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
                frame_index: Some(u32::try_from(frame_index).map_err(|_| {
                    ExportError::Writer("frame index exceeds PSD limits".to_string())
                })?),
            },
            "A blend mode that is not supported by the PSD writer was written as Normal",
            true,
            true,
        );
    }
    let mut layer = Layer {
        additional_info: LayerAdditionalInfo {
            name: Some(source.name.clone()),
            id: Some(f64::from(id)),
            ..Default::default()
        },
        blend_mode: Some(blend_mode),
        opacity: source.opacity.map(|value| f64::from(value) / 255.0),
        hidden: Some(false),
        ..Default::default()
    };
    let available = match source.kind {
        NormalizedLayerKind::Group => source.visible,
        NormalizedLayerKind::Pixel => source.visible && source.cel.is_some(),
    };
    layer_availability.insert(id, available);
    match source.kind {
        NormalizedLayerKind::Group => {
            layer.opened = Some(true);
            layer.children = Some(children);
            layer.bounding_divider_additional_info = Some(LayerAdditionalInfo {
                name: Some("</Layer group>".to_string()),
                id: Some(f64::from(take_export_id(next_id)?)),
                ..Default::default()
            });
            let divider_id = layer
                .bounding_divider_additional_info
                .as_ref()
                .and_then(|info| info.id)
                .and_then(exact_export_id)
                .ok_or_else(|| {
                    ExportError::Writer("generated divider has no valid id".to_string())
                })?;
            layer_availability.insert(divider_id, source.visible);
        }
        NormalizedLayerKind::Pixel => {
            let Some(cel) = source.cel.as_ref() else {
                layer.top = Some(0.0);
                layer.left = Some(0.0);
                layer.bottom = Some(1.0);
                layer.right = Some(1.0);
                layer.image_data = Some(PixelData {
                    width: 1,
                    height: 1,
                    data: vec![0, 0, 0, 0],
                });
                insert_frame_snapshot_marker(
                    metadata,
                    id,
                    source.source_layer_id,
                    frame_index,
                    frame_count,
                    embed_roundtrip_metadata,
                    marker_version,
                )?;
                return Ok(layer);
            };
            layer.opacity = Some(f64::from(cel.opacity) / 255.0);
            layer.top = Some(f64::from(cel.y));
            layer.left = Some(f64::from(cel.x));
            layer.bottom = Some(f64::from(cel.y) + f64::from(cel.height));
            layer.right = Some(f64::from(cel.x) + f64::from(cel.width));
            layer.image_data = Some(PixelData {
                width: cel.width,
                height: cel.height,
                data: cel.pixels.clone(),
            });
        }
    }
    insert_frame_snapshot_marker(
        metadata,
        id,
        source.source_layer_id,
        frame_index,
        frame_count,
        embed_roundtrip_metadata,
        marker_version,
    )?;
    Ok(layer)
}

/// Associates one physical frame-local layer with its original Aseprite layer when enabled.
fn insert_frame_snapshot_marker(
    metadata: &mut HashMap<u32, LayerMarker>,
    id: u32,
    logical_layer_id: u32,
    frame_index: usize,
    frame_count: usize,
    embed_roundtrip_metadata: bool,
    marker_version: u16,
) -> Result<(), ExportError> {
    let marker = embed_roundtrip_metadata
        .then(|| {
            Ok(LayerMarker {
                version: marker_version,
                role: MarkerRole::LayerCopy,
                logical_layer_id,
                variant_index: u32::try_from(frame_index + 1).map_err(|_| {
                    ExportError::Writer("frame count exceeds PSD ID limits".to_string())
                })?,
                variant_count: u32::try_from(frame_count).map_err(|_| {
                    ExportError::Writer("frame count exceeds PSD ID limits".to_string())
                })?,
            })
        })
        .transpose()?;
    if let Some(marker) = marker {
        metadata.insert(id, marker);
    }
    Ok(())
}

/// Allocates a non-zero PSD layer identifier without wrapping.
fn take_export_id(next_id: &mut u32) -> Result<u32, ExportError> {
    let id = *next_id;
    *next_id = next_id
        .checked_add(1)
        .ok_or_else(|| ExportError::Writer("PSD layer ID allocation overflow".to_string()))?;
    Ok(id)
}

/// Builds the ag-psd document while keeping NormalizedDocument as the sole domain model.
fn build_psd(
    document: &NormalizedDocument,
    composites: &[Vec<u8>],
    report: &mut InformationLossReport,
) -> Result<(Psd, Option<Vec<f64>>), ExportError> {
    let active_frame = usize::try_from(document.active_frame_index.unwrap_or(0)).map_err(|_| {
        ExportError::Writer("active frame index exceeds platform limits".to_string())
    })?;
    let composite = composites
        .get(active_frame)
        .or_else(|| composites.first())
        .ok_or_else(|| {
            ExportError::Writer("normalized export has no composite frames".to_string())
        })?;
    let expected = usize::try_from(document.canvas.0)
        .ok()
        .and_then(|width| {
            usize::try_from(document.canvas.1)
                .ok()
                .and_then(|height| width.checked_mul(height))
        })
        .and_then(|area| area.checked_mul(4))
        .ok_or_else(|| {
            ExportError::Writer("composite dimensions exceed memory limits".to_string())
        })?;
    if composite.len() != expected {
        return Err(ExportError::Writer(format!(
            "composite pixel size differs: expected {expected}, got {}",
            composite.len()
        )));
    }
    let max_layer_id = max_normalized_layer_id(&document.root_layers)?;
    let timeline = document.frames.len() > 1;
    let frame_ids = timeline
        .then(|| {
            let first = max_layer_id
                .checked_add(1)
                .ok_or_else(|| ExportError::Writer("frame ID allocation overflow".to_string()))?;
            document
                .frames
                .iter()
                .enumerate()
                .map(|(index, _)| {
                    let index = u32::try_from(index).map_err(|_| {
                        ExportError::Writer("frame count exceeds PSD ID limits".to_string())
                    })?;
                    first.checked_add(index).map(f64::from).ok_or_else(|| {
                        ExportError::Writer("frame ID allocation overflow".to_string())
                    })
                })
                .collect::<Result<Vec<_>, _>>()
        })
        .transpose()?;
    let frames = frame_ids.as_ref().map(|frame_ids| {
        document
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
            .collect::<Vec<_>>()
    });
    let repeats = match document.loop_mode {
        Some(NormalizedLoopMode::Infinite) | None => Some(0.0),
        Some(NormalizedLoopMode::Finite(value)) => Some(f64::from(value)),
    };
    Ok((
        Psd {
            width: f64::from(document.canvas.0),
            height: f64::from(document.canvas.1),
            channels: Some(4.0),
            bits_per_channel: Some(8.0),
            color_mode: Some(ColorMode::Rgb),
            children: Some(build_psd_layers(
                &document.root_layers,
                report,
                frame_ids.as_deref(),
            )?),
            image_data: Some(PixelData {
                width: document.canvas.0,
                height: document.canvas.1,
                data: composite.clone(),
            }),
            image_resources: Some(ImageResources {
                ids_seed_number: Some(f64::from(max_layer_id)),
                animations: frame_ids.as_ref().map(|frame_ids| Animations {
                    frames: frames.expect("timeline frames were built with frame IDs"),
                    animations: vec![AnimationInfo {
                        id: 0.0,
                        frames: frame_ids.clone(),
                        repeats,
                        active_frame: document.active_frame_index.map(f64::from),
                    }],
                }),
                ..Default::default()
            }),
            ..Default::default()
        },
        frame_ids,
    ))
}

/// Finds the highest concrete layer ID before allocating a disjoint Photoshop frame range.
fn max_normalized_layer_id(layers: &[NormalizedLayer]) -> Result<u32, ExportError> {
    let mut maximum = 0_u32;
    let mut stack = layers.iter().collect::<Vec<_>>();
    while let Some(layer) = stack.pop() {
        if layer.id == 0 {
            return Err(ExportError::Writer(
                "PSD layer ID zero is not valid for animation".to_string(),
            ));
        }
        maximum = maximum.max(layer.id);
        stack.extend(layer.children.iter());
    }
    Ok(maximum)
}

/// One normalized layer staged for iterative PSD tree construction.
struct PsdLayerNode<'a> {
    source: &'a NormalizedLayer,
    path: String,
    children: Vec<usize>,
}

/// Builds the PSD layer tree bottom-up so user-controlled hierarchy depth cannot exhaust the stack.
fn build_psd_layers(
    roots: &[NormalizedLayer],
    report: &mut InformationLossReport,
    frame_ids: Option<&[f64]>,
) -> Result<Vec<Layer>, ExportError> {
    let mut nodes = Vec::new();
    let mut root_indices = Vec::new();
    for layer in roots {
        root_indices.push(nodes.len());
        nodes.push(PsdLayerNode {
            source: layer,
            path: export_layer_path(None, layer),
            children: Vec::new(),
        });
    }
    let mut pending = root_indices.clone();
    while let Some(parent_index) = pending.pop() {
        let parent_path = nodes[parent_index].path.clone();
        let children = &nodes[parent_index].source.children;
        let mut child_indices = Vec::with_capacity(children.len());
        for child in children {
            let index = nodes.len();
            nodes.push(PsdLayerNode {
                source: child,
                path: export_layer_path(Some(&parent_path), child),
                children: Vec::new(),
            });
            child_indices.push(index);
        }
        nodes[parent_index].children = child_indices.clone();
        pending.extend(child_indices.into_iter().rev());
    }

    let mut built = (0..nodes.len())
        .map(|_| None)
        .collect::<Vec<Option<Layer>>>();
    for index in (0..nodes.len()).rev() {
        let node = &nodes[index];
        let children = node
            .children
            .iter()
            .map(|child| {
                built[*child].take().ok_or_else(|| {
                    ExportError::Writer("PSD layer tree post-order construction failed".to_string())
                })
            })
            .collect::<Result<Vec<_>, _>>()?;
        built[index] = Some(build_psd_layer(
            node.source,
            &node.path,
            report,
            frame_ids,
            children,
        )?);
    }
    root_indices
        .into_iter()
        .map(|index| {
            built[index].take().ok_or_else(|| {
                ExportError::Writer("PSD root layer construction failed".to_string())
            })
        })
        .collect()
}

/// Converts one normalized layer after its child PSD layers have already been constructed.
fn build_psd_layer(
    source: &NormalizedLayer,
    path: &str,
    report: &mut InformationLossReport,
    frame_ids: Option<&[f64]>,
    children: Vec<Layer>,
) -> Result<Layer, ExportError> {
    let animation_frames = frame_ids
        .map(|frame_ids| {
            source
                .frame_states
                .iter()
                .map(|state| {
                    let frame_id = frame_ids.get(state.frame_index as usize).ok_or_else(|| {
                        ExportError::Writer(format!(
                            "layer {} refers to missing frame {}",
                            source.id, state.frame_index
                        ))
                    })?;
                    Ok(AnimationFrame {
                        frames: vec![*frame_id],
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
                })
                .collect::<Result<Vec<_>, ExportError>>()
        })
        .transpose()?;
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
            animation_frames,
            animation_frame_flags: None,
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
            layer.children = Some(children);
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

/// Injects private converter markers after the public PSD writer completes.
fn inject_roundtrip_metadata(
    mut bytes: Vec<u8>,
    metadata: &HashMap<u32, LayerMarker>,
    psb: bool,
) -> Result<Vec<u8>, ExportError> {
    if metadata.is_empty() {
        return Ok(bytes);
    }
    let layout = layer_record_layout(&bytes, psb)?;
    let mut insertions = Vec::new();
    for record in layout.records {
        let Some(id) = record.layer_id else {
            continue;
        };
        let Some(payload) = metadata.get(&id) else {
            continue;
        };
        let block = roundtrip_block(*payload);
        let new_extra = record
            .extra_length
            .checked_add(block.len())
            .ok_or_else(|| ExportError::Writer("layer extra-data length overflow".to_string()))?;
        write_be_u32(
            &mut bytes,
            record.extra_length_offset,
            u32::try_from(new_extra).map_err(|_| {
                ExportError::Writer("layer extra-data length exceeds PSD limits".to_string())
            })?,
        )?;
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

/// Indexes normalized per-layer animation records by the PSD layer ID.
fn animation_metadata(
    document: &NormalizedDocument,
    embed_roundtrip_metadata: bool,
) -> Result<HashMap<u32, LayerMarker>, ExportError> {
    fn collect(
        layer: &NormalizedLayer,
        parent: Option<&NormalizedLayer>,
        output: &mut HashMap<u32, LayerMarker>,
        embed_roundtrip_metadata: bool,
    ) -> Result<(), ExportError> {
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
        if let Some(marker) = marker {
            output.insert(layer.id, marker);
        }
        for child in &layer.children {
            collect(child, Some(layer), output, embed_roundtrip_metadata)?;
        }
        Ok(())
    }
    let mut output = HashMap::new();
    for layer in &document.root_layers {
        collect(layer, None, &mut output, embed_roundtrip_metadata)?;
    }
    Ok(output)
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
    local_layout: bool,
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
    validate_export_semantics(
        bytes,
        &parsed,
        expected,
        composites,
        frame_first,
        local_layout,
    )?;
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
    local_layout: bool,
) -> Result<(), ExportError> {
    if frame_first {
        if local_layout {
            validate_local_reuse_roots(parsed, expected)?;
        } else {
            validate_frame_group_roots(parsed, expected)?;
        }
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

/// Verifies the logical-layer root contract for a local content-reuse export.
fn validate_local_reuse_roots(
    parsed: &Psd,
    expected: &NormalizedDocument,
) -> Result<(), ExportError> {
    let roots = parsed.children.as_deref().ok_or_else(|| {
        ExportError::OutputValidation("local reuse export has no root layers".to_string())
    })?;
    if roots.is_empty() || roots.len() > expected.root_layers.len() {
        return Err(ExportError::OutputValidation(format!(
            "local reuse export has {} physical roots for {} logical roots",
            roots.len(),
            expected.root_layers.len()
        )));
    }
    for root in roots {
        if root
            .additional_info
            .name
            .as_deref()
            .is_none_or(|name| name.starts_with("Frame ") || name.starts_with("State "))
            || root.additional_info.animation_frames.is_none()
        {
            return Err(ExportError::OutputValidation(
                "local reuse export root is missing its logical name or animation track"
                    .to_string(),
            ));
        }
    }
    Ok(())
}

/// Verifies that the editable export retains one root folder for each Aseprite frame.
fn validate_frame_group_roots(
    parsed: &Psd,
    expected: &NormalizedDocument,
) -> Result<(), ExportError> {
    let roots = parsed.children.as_deref().ok_or_else(|| {
        ExportError::OutputValidation("frame-folder export has no root layers".to_string())
    })?;
    if roots.is_empty() || roots.len() > expected.frames.len() {
        return Err(ExportError::OutputValidation(format!(
            "frame-folder export has {} physical root layers for {} timeline frames",
            roots.len(),
            expected.frames.len()
        )));
    }
    for (index, root) in roots.iter().enumerate() {
        let expected_name = format!("Frame {}", index + 1);
        let state_name = format!("State {}", index + 1);
        if !matches!(
            root.additional_info.name.as_deref(),
            Some(name) if name == expected_name || name == state_name
        ) || root.children.as_ref().is_none_or(Vec::is_empty)
        {
            return Err(ExportError::OutputValidation(format!(
                "frame-folder export is missing populated root {expected_name}"
            )));
        }
        let first_enable = root
            .additional_info
            .animation_frames
            .as_ref()
            .and_then(|states| states.first())
            .and_then(|state| state.enable)
            .ok_or_else(|| {
                ExportError::OutputValidation(format!(
                    "frame-folder root {expected_name} is missing an explicit first-frame state"
                ))
            })?;
        if root.hidden != Some(!first_enable) {
            return Err(ExportError::OutputValidation(format!(
                "frame-folder root {expected_name} static visibility disagrees with its first mlst state"
            )));
        }
    }
    Ok(())
}

/// Validates the frame-group root contract without flattening the duplicated snapshots.
/// Verifies that every emitted frame-group layer has one state for every global frame.
#[cfg(test)]
fn validate_frame_layer_states(
    layers: &[NormalizedLayer],
    frame_count: usize,
) -> Result<(), ExportError> {
    for layer in layers {
        if layer.frame_states.len() != frame_count {
            return Err(ExportError::OutputValidation(format!(
                "layer {} has {} animation states for {frame_count} global frames",
                layer.id,
                layer.frame_states.len()
            )));
        }
        for (index, state) in layer.frame_states.iter().enumerate() {
            if state.frame_index != index as u32 {
                return Err(ExportError::OutputValidation(format!(
                    "layer {} animation state order differs at frame {index}",
                    layer.id
                )));
            }
        }
        validate_frame_layer_states(&layer.children, frame_count)?;
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
            let expected = channel_size.checked_mul(channels).ok_or_else(|| {
                ExportError::OutputValidation("composite ZIP length overflows memory".to_string())
            })?;
            validate_zlib_payload(payload, Some(expected), "composite image")?;
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
    let static_equivalent = expected.frames.len() == 1
        && actual.frames.len() == 1
        && actual.frames[0].source_id.is_none()
        && actual.frames[0].duration_ms.is_none();
    let same_frames = static_equivalent
        || (expected.frames.len() == actual.frames.len()
            && expected
                .frames
                .iter()
                .zip(&actual.frames)
                .all(|(left, right)| {
                    left.index == right.index
                        && left.duration_ms == right.duration_ms
                        && left.dispose == right.dispose
                }));
    let same_loop_mode = static_equivalent || expected.loop_mode == actual.loop_mode;
    let same_active_frame =
        static_equivalent || expected.active_frame_index == actual.active_frame_index;
    if expected.canvas != actual.canvas || !same_frames || !same_loop_mode || !same_active_frame {
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
    compare_layers(
        &expected.root_layers,
        &actual.root_layers,
        static_equivalent,
    )
}

/// Recursively compares layer tree, pixels, visibility, offsets, and opacity.
fn compare_layers(
    expected: &[NormalizedLayer],
    actual: &[NormalizedLayer],
    ignore_static_frame_states: bool,
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
            || (!ignore_static_frame_states
                && expected.frame_states.len() != actual.frame_states.len())
        {
            return Err(ExportError::OutputValidation(format!(
                "layer {} structure or pixel dimensions differ",
                expected.id
            )));
        }
        if !ignore_static_frame_states {
            for (expected_state, actual_state) in
                expected.frame_states.iter().zip(&actual.frame_states)
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
        }
        compare_layers(
            &expected.children,
            &actual.children,
            ignore_static_frame_states,
        )?;
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
    use std::fs;
    use std::time::{SystemTime, UNIX_EPOCH};

    use crate::{
        AutoAssociationOptions, ConvertOptions, JitterKind, JitterMode, JitterOptions,
        JitterProfile, LayerAssociation, NormalizedBounds,
    };
    use aseprite::{
        AsepriteFile, BlendMode as AseBlendMode, CelOptions, ColorMode as AseColorMode,
        LayerOptions, Pixels, Tileset, TilesetData, TilesetFlags,
    };

    fn assert_physical_tracks(layers: &[ag_psd::psd::Layer], frame_count: usize) -> usize {
        let mut physical_count = 0;
        for layer in layers {
            assert!(layer.additional_info.id.is_some());
            let states = layer
                .additional_info
                .animation_frames
                .as_ref()
                .expect("ordinary layer frame track");
            assert_eq!(states.len(), frame_count);
            let first_enable = states
                .first()
                .and_then(|state| state.enable)
                .expect("explicit first-frame state");
            assert_eq!(layer.hidden, Some(!first_enable));
            physical_count += 1;
            if layer.bounding_divider_additional_info.is_some() {
                physical_count += 1;
            }
            if let Some(children) = layer.children.as_deref() {
                physical_count += assert_physical_tracks(children, frame_count);
            }
        }
        physical_count
    }

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

        let mut concatenated = encoded.clone();
        concatenated.extend_from_slice(&encoded);
        assert!(
            validate_zlib_payload(&concatenated, Some(source.len() * 2), "test channel").is_err()
        );

        let mut trailing = encoded.clone();
        trailing.push(0);
        assert!(validate_zlib_payload(&trailing, Some(source.len()), "test channel").is_err());
        assert!(validate_zlib_payload(&encoded, Some(source.len() + 1), "test channel").is_err());

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
        let reused_output = directory.join("reused-output.psd");
        let aggressive_output = directory.join("aggressive-output.psd");
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
        let parsed = ag_psd::read_psd(&bytes, &ag_psd::psd::ReadOptions::default())
            .expect("read typed frame animation from exported PSD");
        let parsed_animations = parsed
            .image_resources
            .as_ref()
            .and_then(|resources| resources.animations.as_ref())
            .expect("global typed animation directory");
        assert_eq!(parsed_animations.frames.len(), 3);
        assert_eq!(parsed_animations.animations[0].frames.len(), 3);
        assert_eq!(parsed_animations.animations[0].active_frame, Some(0.0));
        assert_eq!(
            assert_physical_tracks(parsed.children.as_deref().unwrap_or_default(), 3),
            9,
            "every generated non-empty layer and divider must have a complete typed track"
        );
        let default_layout = layer_record_layout(&bytes, false).expect("inspect default PSD");
        validate_channel_compression(&default_layout, ExportCompression::Rle.ag_psd() as u16)
            .expect("default export should use RLE");
        assert_eq!(
            crate::roundtrip::inspect(&bytes).expect("inspect round-trip metadata"),
            crate::roundtrip::RoundTripStatus {
                marked: true,
                valid: true,
            }
        );

        let reused_report = export(
            &input,
            &composite,
            &reused_output,
            &ExportOptions {
                content_reuse: ExportContentReuse::Linked,
                ..Default::default()
            },
        )
        .expect("export linked-content PSD");
        assert_eq!(
            reused_report.actual_content_reuse,
            ExportContentReuse::Linked
        );
        assert_eq!(reused_report.explicit_link_reuse_count, 1);
        assert_eq!(reused_report.physical_layer_count, 4);
        let reused_bytes = fs::read(&reused_output).expect("read linked-content PSD");
        assert_eq!(
            crate::roundtrip::inspect(&reused_bytes).expect("inspect v3 reuse metadata"),
            crate::roundtrip::RoundTripStatus {
                marked: true,
                valid: true,
            }
        );
        let reused = ag_psd::read_psd(&reused_bytes, &ag_psd::psd::ReadOptions::default())
            .expect("read linked-content PSD");
        assert_eq!(
            reused
                .image_resources
                .as_ref()
                .and_then(|resources| resources.animations.as_ref())
                .expect("linked-content animation directory")
                .frames
                .len(),
            3
        );
        assert_eq!(reused.children.as_ref().map_or(0, Vec::len), 1);
        let reused_root = &reused.children.as_ref().expect("local reuse roots")[0];
        assert_eq!(reused_root.additional_info.name.as_deref(), Some("动画层"));
        assert_eq!(reused_root.children.as_ref().map_or(0, Vec::len), 2);
        assert!(
            reused_root
                .children
                .as_ref()
                .expect("local reuse state variants")
                .iter()
                .all(|layer| layer
                    .additional_info
                    .name
                    .as_deref()
                    .is_some_and(|name| name.starts_with("State ")))
        );
        let reused_roundtrip = directory.join("reused-roundtrip.aseprite");
        crate::convert(
            &reused_output,
            &reused_roundtrip,
            &crate::ConvertOptions {
                layer_association: crate::LayerAssociation::AutoForRoundTrip,
                ..Default::default()
            },
        )
        .expect("v3 content-reuse metadata should restore the logical timeline");
        let reused_file = AsepriteFile::from_reader(
            fs::read(&reused_roundtrip)
                .expect("read v3 content-reuse roundtrip")
                .as_slice(),
        )
        .expect("parse v3 content-reuse roundtrip");
        assert_eq!(reused_file.frames().len(), 3);
        assert_eq!(reused_file.layers().len(), 1);
        assert_eq!(reused_file.layers()[0].name, "动画层");
        let reused_layer = reused_file.layer_ref(0).expect("roundtrip logical layer");
        assert!(matches!(
            reused_file
                .cel(reused_layer, 2)
                .expect("linked roundtrip cel")
                .kind,
            aseprite::CelKind::Linked { .. }
        ));

        let aggressive_report = export(
            &input,
            &composite,
            &aggressive_output,
            &ExportOptions {
                content_reuse: ExportContentReuse::Aggressive,
                ..Default::default()
            },
        )
        .expect("export aggressive-content PSD");
        assert_eq!(
            aggressive_report.actual_content_reuse,
            ExportContentReuse::Aggressive
        );
        assert_eq!(aggressive_report.exact_match_reuse_count, 1);
        assert_eq!(aggressive_report.physical_layer_count, 4);
        let aggressive_bytes = fs::read(&aggressive_output).expect("read aggressive-content PSD");
        assert_eq!(
            crate::roundtrip::inspect(&aggressive_bytes).expect("inspect aggressive metadata"),
            crate::roundtrip::RoundTripStatus {
                marked: true,
                valid: true,
            }
        );
        let aggressive_roundtrip = directory.join("aggressive-roundtrip.aseprite");
        crate::convert(
            &aggressive_output,
            &aggressive_roundtrip,
            &crate::ConvertOptions {
                layer_association: crate::LayerAssociation::AutoForRoundTrip,
                ..Default::default()
            },
        )
        .expect("aggressive v3 metadata should restore the logical timeline");
        let aggressive_file = AsepriteFile::from_reader(
            fs::read(&aggressive_roundtrip)
                .expect("read aggressive roundtrip")
                .as_slice(),
        )
        .expect("parse aggressive roundtrip");
        assert_eq!(aggressive_file.frames().len(), 3);
        assert!(matches!(
            aggressive_file
                .cel(
                    aggressive_file
                        .layer_ref(0)
                        .expect("aggressive logical layer"),
                    2
                )
                .expect("aggressive roundtrip cel")
                .kind,
            aseprite::CelKind::Raw { .. } | aseprite::CelKind::Compressed { .. }
        ));
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
        assert_eq!(normalized.animation_resource_ids, vec![4000]);
        assert_eq!(normalized.frames.len(), 3);
        assert_eq!(normalized.frames[0].duration_ms, Some(120));
        assert_eq!(normalized.frames[1].duration_ms, Some(80));
        assert_eq!(normalized.frames[2].duration_ms, Some(60));
        assert_eq!(normalized.active_frame_index, Some(0));
        let max_layer_id = default_layout
            .records
            .iter()
            .filter_map(|record| record.layer_id)
            .max()
            .expect("export should contain layer IDs");
        let frame_ids = normalized
            .frames
            .iter()
            .map(|frame| frame.source_id.expect("exported frame ID"))
            .collect::<Vec<_>>();
        assert!(frame_ids.iter().all(|id| *id > max_layer_id));
        assert!(frame_ids.windows(2).all(|pair| pair[0] < pair[1]));
        assert!(!bytes.windows(4).any(|window| window == b"mdyn"));
        assert_eq!(normalized.root_layers.len(), 3);
        assert_eq!(
            normalized
                .root_layers
                .iter()
                .map(|layer| layer.name.as_str())
                .collect::<Vec<_>>(),
            vec!["Frame 1", "Frame 2", "Frame 3"]
        );
        assert!(
            normalized
                .root_layers
                .iter()
                .enumerate()
                .all(|(index, layer)| layer.hidden == Some(index != 0)),
            "static visibility must initialize the first frame; mlst controls later playback"
        );
        assert_eq!(
            normalized.root_layers[1].frame_states[1].enabled, true,
            "a statically hidden later frame must be enabled by its mlst state"
        );
        validate_frame_layer_states(&normalized.root_layers, 3)
            .expect("every frame-local layer should have complete animation states");

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
        assert_eq!(normalized_without_empty.root_layers.len(), 3);
        assert!(
            normalized_without_empty
                .root_layers
                .iter()
                .all(|layer| layer.children.len() == 1),
            "omit policy must not materialize a frame-local empty pixel layer"
        );

        let include_empty_output = directory.join("include-empty.psd");
        export(
            &input,
            &composite,
            &include_empty_output,
            &ExportOptions {
                include_empty_layers: true,
                ..Default::default()
            },
        )
        .expect("export PSD with empty layers");
        let included = ag_psd::read_psd(
            &fs::read(&include_empty_output).expect("read PSD with empty layers"),
            &ag_psd::psd::ReadOptions::default(),
        )
        .expect("read PSD with included empty layers");
        assert_eq!(
            assert_physical_tracks(included.children.as_deref().unwrap_or_default(), 3),
            12,
            "include policy must retain frame-local empty pixel layers"
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
        let unmarked_bytes = fs::read(&unmarked_output).expect("read unmarked PSD bytes");
        let unmarked_normalized =
            crate::normalize(&unmarked_output).expect("normalize unmarked PSD");
        assert_eq!(unmarked_normalized.frames, normalized.frames);
        assert_eq!(unmarked_normalized.loop_mode, normalized.loop_mode);
        assert_eq!(
            unmarked_normalized.active_frame_index,
            normalized.active_frame_index
        );
        assert!(bytes.windows(8).any(|window| window == b"maniIRFR"));
        assert!(
            unmarked_bytes
                .windows(8)
                .any(|window| window == b"maniIRFR")
        );
        assert!(bytes.windows(4).any(|window| window == b"shmd"));
        assert!(unmarked_bytes.windows(4).any(|window| window == b"shmd"));
        assert!(bytes.windows(4).any(|window| window == b"p2rt"));
        assert!(!unmarked_bytes.windows(4).any(|window| window == b"p2rt"));

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
            let mode_bytes = fs::read(&mode_output).expect("read selected compression");
            let layout = layer_record_layout(&mode_bytes, false).expect("inspect PSD layout");
            let expected_code = compression.ag_psd() as u16;
            validate_channel_compression(&layout, expected_code)
                .expect("selected compression should be used for every non-empty channel");

            let mode_normalized =
                crate::normalize(&mode_output).expect("normalize selected compression");
            compare_normalized(&normalized, &mode_normalized)
                .expect("compression must not change normalized structure or pixels");

            let mode_roundtrip = directory.join(format!("compression-{index}.aseprite"));
            crate::convert(
                &mode_output,
                &mode_roundtrip,
                &crate::ConvertOptions {
                    layer_association: crate::LayerAssociation::AutoForRoundTrip,
                    ..Default::default()
                },
            )
            .expect("selected compression should remain importable");
            let mode_roundtrip_file = AsepriteFile::from_reader(
                fs::read(&mode_roundtrip)
                    .expect("read selected compression roundtrip")
                    .as_slice(),
            )
            .expect("parse selected compression roundtrip");
            assert_eq!(
                mode_roundtrip_file
                    .frames()
                    .iter()
                    .map(|frame| frame.duration_ms)
                    .collect::<Vec<_>>(),
                vec![120, 80, 60]
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

    /// Verifies Unicode layer names through Aseprite-to-PSD and PSD-to-Aseprite round trips.
    #[test]
    fn unicode_layer_names_survive_psd_roundtrip() {
        let directory = std::env::temp_dir().join(format!(
            "aseprite-psd-unicode-layer-names-{}",
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .expect("clock")
                .as_nanos()
        ));
        fs::create_dir_all(&directory).expect("create Unicode fixture directory");
        let input = directory.join("unicode-source.aseprite");
        let composite = directory.join("unicode-composite.aseprite");
        let psd = directory.join("unicode-layer-names.psd");
        let reimported = directory.join("unicode-reimported.aseprite");

        let layer_specs = [
            ("中文", [255, 0, 0, 255]),
            ("é", [0, 255, 0, 255]),
            ("e\u{301}", [0, 0, 255, 255]),
            ("", [255, 255, 0, 255]),
            ("相同", [255, 0, 255, 255]),
            ("相同", [0, 255, 255, 255]),
        ];
        let mut source = AsepriteFile::new(layer_specs.len() as u16, 1, AseColorMode::Rgba);
        let group = source.add_group("组😀");
        let frame = source.add_frame(100);
        let mut composite_pixels = Vec::new();
        for (x, (name, pixels)) in layer_specs.iter().enumerate() {
            let layer = source.add_layer_in(name, group);
            source
                .set_cel(
                    layer,
                    frame,
                    Pixels::new(pixels.to_vec(), 1, 1, AseColorMode::Rgba)
                        .expect("Unicode layer pixels"),
                    x as i16,
                    0,
                )
                .expect("Unicode layer cel");
            composite_pixels.extend_from_slice(pixels);
        }
        write_aseprite(&input, &source);

        let mut flattened = AsepriteFile::new(layer_specs.len() as u16, 1, AseColorMode::Rgba);
        let flattened_layer = flattened.add_layer("Composite");
        let flattened_frame = flattened.add_frame(100);
        flattened
            .set_cel(
                flattened_layer,
                flattened_frame,
                Pixels::new(
                    composite_pixels.clone(),
                    layer_specs.len() as u16,
                    1,
                    AseColorMode::Rgba,
                )
                .expect("Unicode composite pixels"),
                0,
                0,
            )
            .expect("Unicode composite cel");
        write_aseprite(&composite, &flattened);

        export(&input, &composite, &psd, &ExportOptions::default())
            .expect("export Unicode layer-name PSD");
        let normalized = crate::normalize(&psd).expect("normalize Unicode layer-name PSD");
        assert_eq!(normalized.canvas, (layer_specs.len() as u32, 1));
        assert_eq!(normalized.frames.len(), 1);
        assert_eq!(normalized.frames[0].duration_ms, None);

        let normalized_group = normalized.root_layers.first().expect("root group");
        assert_eq!(normalized_group.name, "组😀");
        assert_eq!(normalized_group.kind, NormalizedLayerKind::Group);
        assert_eq!(normalized_group.children.len(), layer_specs.len());
        let expected_names = layer_specs
            .iter()
            .map(|(name, _)| *name)
            .collect::<Vec<_>>();
        assert_eq!(
            normalized_group
                .children
                .iter()
                .map(|layer| layer.name.as_str())
                .collect::<Vec<_>>(),
            expected_names
        );
        assert_ne!(
            normalized_group.children[4].id, normalized_group.children[5].id,
            "duplicate names must retain distinct layer identities"
        );
        for (index, (_, pixels)) in layer_specs.iter().enumerate() {
            let layer = &normalized_group.children[index];
            assert_eq!(
                layer.bounds,
                NormalizedBounds {
                    left: index as i32,
                    top: 0,
                    right: index as i32 + 1,
                    bottom: 1,
                }
            );
            assert_eq!(
                layer.pixels.as_ref().expect("normalized layer pixels").data,
                pixels.to_vec()
            );
        }

        crate::convert(
            &psd,
            &reimported,
            &crate::ConvertOptions {
                layer_association: crate::LayerAssociation::AutoForRoundTrip,
                ..Default::default()
            },
        )
        .expect("reimport Unicode layer-name PSD");
        let reimported_file = AsepriteFile::from_reader(
            fs::read(&reimported)
                .expect("read Unicode layer-name reimport")
                .as_slice(),
        )
        .expect("parse Unicode layer-name reimport");
        assert_eq!(reimported_file.frames().len(), 1);
        assert_eq!(reimported_file.frames()[0].duration_ms, 100);
        let reimported_names = reimported_file
            .layers()
            .iter()
            .map(|layer| layer.name.as_str())
            .collect::<Vec<_>>();
        assert_eq!(
            reimported_names,
            vec!["中文", "é", "e\u{301}", "", "相同", "相同"]
        );
        assert!(
            reimported_file
                .layers()
                .iter()
                .all(|layer| layer.parent.is_none())
        );
        fs::remove_dir_all(directory).expect("remove Unicode fixture directory");
    }

    #[test]
    fn empty_pixel_layer_policy_omits_non_writable_layers_and_empty_groups() {
        assert!(!ExportOptions::default().include_empty_layers);
        let state = |enabled, opacity| crate::NormalizedLayerFrameState {
            frame_index: 0,
            record_present: true,
            enabled,
            explicit_enable: true,
            offset: None,
            reference_point: None,
            opacity,
        };
        let pixel = |id, enabled, opacity, data| NormalizedLayer {
            id,
            name: format!("layer-{id}"),
            kind: NormalizedLayerKind::Pixel,
            bounds: crate::NormalizedBounds {
                left: 0,
                top: 0,
                right: 1,
                bottom: 1,
            },
            opacity: None,
            blend_mode: None,
            hidden: None,
            pixels: Some(crate::NormalizedPixels {
                width: 1,
                height: 1,
                left: 0,
                top: 0,
                data,
            }),
            children: Vec::new(),
            frame_states: vec![state(enabled, opacity)],
        };
        let empty_group = NormalizedLayer {
            id: 6,
            name: "empty-group".to_string(),
            kind: NormalizedLayerKind::Group,
            bounds: crate::NormalizedBounds {
                left: 0,
                top: 0,
                right: 0,
                bottom: 0,
            },
            opacity: None,
            blend_mode: None,
            hidden: None,
            pixels: None,
            children: vec![pixel(7, false, Some(1.0), vec![0; 4])],
            frame_states: vec![state(true, None)],
        };
        let mut layers = vec![
            pixel(1, false, None, vec![255, 0, 0, 255]),
            pixel(2, true, Some(0.0), vec![255, 0, 0, 255]),
            pixel(3, true, Some(1.0), vec![0; 4]),
            pixel(4, true, Some(1.0), vec![255, 0, 0, 255]),
            pixel(5, false, Some(1.0), vec![255, 0, 0, 255]),
            empty_group,
        ];
        omit_empty_pixel_layers(&mut layers);
        assert_eq!(
            layers.iter().map(|layer| layer.id).collect::<Vec<_>>(),
            vec![4, 5],
            "missing cels, zero-opacity cels, transparent cels, and empty groups are omitted"
        );
    }

    #[test]
    fn writable_pixel_cel_predicate_covers_opacity_alpha_and_hidden_content() {
        assert!(!is_writable_pixel_cel(None, Some(&[255, 0, 0, 255])));
        assert!(!is_writable_pixel_cel(Some(0.0), Some(&[255, 0, 0, 255])));
        assert!(!is_writable_pixel_cel(Some(1.0), Some(&[255, 0, 0, 0])));
        assert!(is_writable_pixel_cel(Some(1.0), Some(&[255, 0, 0, 255])));
        assert!(
            is_writable_pixel_cel(Some(1.0), Some(&[255, 0, 0, 255])),
            "layer visibility is intentionally outside the cel predicate"
        );
    }

    #[test]
    fn frame_snapshot_empty_policy_is_sparse_per_frame() {
        let pixel = |id: u32, cel: Option<(u8, Vec<u8>)>| FrameSnapshotLayer {
            source_layer_id: id,
            name: format!("layer-{id}"),
            kind: NormalizedLayerKind::Pixel,
            opacity: None,
            blend_mode: None,
            visible: true,
            cel: cel.map(
                |(opacity, pixels)| crate::aseprite_reader::FrameSnapshotCel {
                    source_frame: 0,
                    linked_source_frame: 0,
                    explicitly_linked: false,
                    width: 1,
                    height: 1,
                    x: 0,
                    y: 0,
                    opacity,
                    pixels,
                },
            ),
            children: Vec::new(),
        };
        let snapshots = [
            vec![
                pixel(1, Some((255, vec![255, 0, 0, 255]))),
                pixel(2, None),
                pixel(3, Some((0, vec![255, 0, 0, 255]))),
                pixel(4, Some((255, vec![255, 0, 0, 0]))),
                pixel(5, Some((255, vec![255, 0, 0, 255]))),
            ],
            vec![
                pixel(1, None),
                pixel(2, Some((255, vec![255, 0, 0, 255]))),
                pixel(3, None),
                pixel(4, None),
                pixel(5, Some((255, vec![255, 0, 0, 255]))),
            ],
            vec![
                pixel(1, None),
                pixel(2, None),
                pixel(3, Some((255, vec![255, 0, 0, 255]))),
                pixel(4, None),
                pixel(5, Some((255, vec![255, 0, 0, 255]))),
            ],
        ];

        let omitted = snapshots
            .iter()
            .map(|layers| filter_frame_snapshot_layers(layers, false))
            .collect::<Vec<_>>();
        assert_eq!(
            omitted
                .iter()
                .map(|layers| layers.len())
                .collect::<Vec<_>>(),
            vec![2, 2, 2]
        );
        assert_eq!(
            omitted
                .iter()
                .map(|layers| layers[0].source_layer_id)
                .collect::<Vec<_>>(),
            vec![1, 2, 3]
        );
        assert!(
            omitted
                .iter()
                .all(|layers| layers.iter().any(|layer| { layer.source_layer_id == 5 }))
        );

        let included = snapshots
            .iter()
            .map(|layers| filter_frame_snapshot_layers(layers, true))
            .collect::<Vec<_>>();
        assert!(included.iter().all(|layers| layers.len() == 5));
    }

    #[test]
    fn frame_folder_export_filters_non_writable_cels_and_roundtrips_frames() {
        let directory = std::env::temp_dir().join(format!(
            "aseprite-psd-empty-cel-export-{}",
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .expect("clock")
                .as_nanos()
        ));
        fs::create_dir_all(&directory).expect("create empty-cel fixture directory");
        let input = directory.join("source.aseprite");
        let composite = directory.join("composite.aseprite");
        let omitted_output = directory.join("omitted.psd");
        let included_output = directory.join("included.psd");
        let roundtrip = directory.join("roundtrip.aseprite");
        let linked_output = directory.join("linked.psd");
        let linked_roundtrip = directory.join("linked-roundtrip.aseprite");
        let aggressive_output = directory.join("aggressive.psd");
        let aggressive_roundtrip = directory.join("aggressive-roundtrip.aseprite");

        let opaque_pixels = || {
            Pixels::new(vec![255, 0, 0, 255, 0, 0, 0, 0], 2, 1, AseColorMode::Rgba)
                .expect("opaque fixture pixels")
        };
        let transparent_pixels = || {
            Pixels::new(vec![0; 8], 2, 1, AseColorMode::Rgba).expect("transparent fixture pixels")
        };
        let mut source = AsepriteFile::new(2, 1, AseColorMode::Rgba);
        let content = source.add_layer("Content");
        let no_cel = source.add_layer("No Cel");
        let zero_opacity = source.add_layer_with("Zero Opacity", LayerOptions::default());
        let transparent = source.add_layer("Transparent");
        let hidden = source.add_layer_with(
            "Hidden Content",
            LayerOptions {
                visible: false,
                ..Default::default()
            },
        );
        let first = source.add_frame(120);
        let second = source.add_frame(80);
        source
            .set_cel(content, first, opaque_pixels(), 0, 0)
            .expect("content first cel");
        source
            .set_linked_cel(content, second, first)
            .expect("content linked cel");
        for frame in [first, second] {
            source
                .set_cel_with(
                    zero_opacity,
                    frame,
                    CelOptions {
                        pixels: opaque_pixels(),
                        opacity: 0,
                        ..Default::default()
                    },
                )
                .expect("zero-opacity cel");
            source
                .set_cel(transparent, frame, transparent_pixels(), 0, 0)
                .expect("transparent cel");
        }
        source
            .set_cel(hidden, first, opaque_pixels(), 0, 0)
            .expect("hidden first cel");
        source
            .set_linked_cel(hidden, second, first)
            .expect("hidden linked cel");
        let _ = no_cel;
        write_aseprite(&input, &source);

        let mut flattened = AsepriteFile::new(2, 1, AseColorMode::Rgba);
        let composite_layer = flattened.add_layer("Composite");
        let first = flattened.add_frame(120);
        let second = flattened.add_frame(80);
        for frame in [first, second] {
            flattened
                .set_cel(composite_layer, frame, opaque_pixels(), 0, 0)
                .expect("composite cel");
        }
        write_aseprite(&composite, &flattened);

        export(
            &input,
            &composite,
            &omitted_output,
            &ExportOptions::default(),
        )
        .expect("export omitted empty cels");
        let omitted = ag_psd::read_psd(
            &fs::read(&omitted_output).expect("read omitted PSD"),
            &ag_psd::psd::ReadOptions::default(),
        )
        .expect("parse omitted PSD");
        let omitted_animations = omitted
            .image_resources
            .as_ref()
            .and_then(|resources| resources.animations.as_ref())
            .expect("omitted animation directory");
        assert_eq!(omitted_animations.frames.len(), 2);
        assert_eq!(omitted.children.as_ref().map_or(0, Vec::len), 2);
        assert!(
            omitted
                .children
                .as_ref()
                .expect("omitted frame folders")
                .iter()
                .all(|frame| {
                    frame
                        .children
                        .as_ref()
                        .map(|children| {
                            children
                                .iter()
                                .map(|child| child.additional_info.name.as_deref())
                                .collect::<Vec<_>>()
                        })
                        == Some(vec![Some("Content"), Some("Hidden Content")])
                })
        );

        export(
            &input,
            &composite,
            &included_output,
            &ExportOptions {
                include_empty_layers: true,
                ..Default::default()
            },
        )
        .expect("export included empty cels");
        let included = ag_psd::read_psd(
            &fs::read(&included_output).expect("read included PSD"),
            &ag_psd::psd::ReadOptions::default(),
        )
        .expect("parse included PSD");
        assert_eq!(
            included
                .children
                .as_ref()
                .expect("included frame folders")
                .iter()
                .map(|frame| frame.children.as_ref().map_or(0, Vec::len))
                .collect::<Vec<_>>(),
            vec![5, 5]
        );

        let linked_report = export(
            &input,
            &composite,
            &linked_output,
            &ExportOptions {
                content_reuse: ExportContentReuse::Linked,
                ..Default::default()
            },
        )
        .expect("export linked sparse cels");
        assert_eq!(
            linked_report.actual_content_reuse,
            ExportContentReuse::Linked
        );
        assert!(linked_report.explicit_link_reuse_count > 0);
        let linked = ag_psd::read_psd(
            &fs::read(&linked_output).expect("read linked sparse PSD"),
            &ag_psd::psd::ReadOptions {
                use_image_data: Some(true),
                ..Default::default()
            },
        )
        .expect("parse linked sparse PSD");
        assert_eq!(
            linked.children.as_ref().expect("linked sparse roots").len(),
            2
        );
        assert!(
            linked
                .children
                .as_ref()
                .expect("linked sparse roots")
                .iter()
                .all(|layer| layer
                    .image_data
                    .as_ref()
                    .is_some_and(|pixels| pixels.width == 2 && pixels.height == 1))
        );
        crate::convert(
            &linked_output,
            &linked_roundtrip,
            &crate::ConvertOptions {
                layer_association: crate::LayerAssociation::AutoForRoundTrip,
                ..Default::default()
            },
        )
        .expect("roundtrip linked sparse PSD");
        let linked_file = AsepriteFile::from_reader(
            fs::read(&linked_roundtrip)
                .expect("read linked sparse roundtrip")
                .as_slice(),
        )
        .expect("parse linked sparse roundtrip");
        assert_eq!(linked_file.frames().len(), 2);
        let linked_content = linked_file
            .layers()
            .iter()
            .position(|layer| layer.name == "Content")
            .and_then(|index| linked_file.layer_ref(index))
            .expect("linked roundtrip content layer");
        assert!(matches!(
            linked_file
                .cel(linked_content, 1)
                .expect("linked roundtrip content cel")
                .kind,
            aseprite::CelKind::Linked { .. }
        ));

        let aggressive_report = export(
            &input,
            &composite,
            &aggressive_output,
            &ExportOptions {
                content_reuse: ExportContentReuse::Aggressive,
                ..Default::default()
            },
        )
        .expect("export aggressive sparse cels");
        assert_eq!(
            aggressive_report.actual_content_reuse,
            ExportContentReuse::Aggressive
        );
        assert!(aggressive_report.exact_match_reuse_count > 0);
        crate::convert(
            &aggressive_output,
            &aggressive_roundtrip,
            &crate::ConvertOptions {
                layer_association: crate::LayerAssociation::AutoForRoundTrip,
                ..Default::default()
            },
        )
        .expect("roundtrip aggressive sparse PSD");
        let aggressive_file = AsepriteFile::from_reader(
            fs::read(&aggressive_roundtrip)
                .expect("read aggressive sparse roundtrip")
                .as_slice(),
        )
        .expect("parse aggressive sparse roundtrip");
        assert_eq!(aggressive_file.frames().len(), 2);
        let aggressive_content = aggressive_file
            .layers()
            .iter()
            .position(|layer| layer.name == "Content")
            .and_then(|index| aggressive_file.layer_ref(index))
            .expect("aggressive roundtrip content layer");
        assert!(matches!(
            aggressive_file
                .cel(aggressive_content, 1)
                .expect("aggressive roundtrip content cel")
                .kind,
            aseprite::CelKind::Raw { .. } | aseprite::CelKind::Compressed { .. }
        ));

        crate::convert(
            &omitted_output,
            &roundtrip,
            &crate::ConvertOptions {
                layer_association: crate::LayerAssociation::AutoForRoundTrip,
                ..Default::default()
            },
        )
        .expect("roundtrip omitted PSD");
        let roundtrip_file = AsepriteFile::from_reader(
            fs::read(&roundtrip)
                .expect("read omitted roundtrip")
                .as_slice(),
        )
        .expect("parse omitted roundtrip");
        assert_eq!(
            roundtrip_file
                .frames()
                .iter()
                .map(|frame| frame.duration_ms)
                .collect::<Vec<_>>(),
            vec![120, 80]
        );

        fs::remove_dir_all(directory).expect("remove empty-cel fixture directory");
    }

    #[test]
    fn local_reuse_omits_transparent_states_without_placeholder_layers() {
        let cel = |source_frame: u32, opacity: u8, pixels: Vec<u8>| {
            crate::aseprite_reader::FrameSnapshotCel {
                source_frame,
                linked_source_frame: source_frame,
                explicitly_linked: false,
                width: 2,
                height: 1,
                x: 0,
                y: 0,
                opacity,
                pixels,
            }
        };
        let pixel = |id: u32, visible: bool, cel| FrameSnapshotLayer {
            source_layer_id: id,
            name: format!("layer-{id}"),
            kind: NormalizedLayerKind::Pixel,
            opacity: None,
            blend_mode: Some("normal".to_string()),
            visible,
            cel,
            children: Vec::new(),
        };
        let transparent = vec![0, 0, 0, 0, 0, 0, 0, 0];
        let opaque = vec![255, 0, 0, 255, 0, 0, 0, 0];
        let linked = |source_frame, pixels| {
            let mut cel = cel(source_frame, 255, pixels);
            cel.linked_source_frame = 0;
            cel.explicitly_linked = true;
            cel
        };
        let snapshots = vec![
            FrameSnapshot {
                layers: vec![
                    pixel(1, true, Some(cel(0, 255, opaque.clone()))),
                    pixel(2, true, Some(cel(0, 255, transparent.clone()))),
                    pixel(3, false, Some(linked(0, opaque.clone()))),
                ],
            },
            FrameSnapshot {
                layers: vec![
                    pixel(1, true, None),
                    pixel(2, true, Some(cel(1, 0, opaque.clone()))),
                    pixel(3, false, Some(linked(1, opaque.clone()))),
                ],
            },
            FrameSnapshot {
                layers: vec![
                    pixel(1, true, Some(cel(2, 255, opaque.clone()))),
                    pixel(2, true, None),
                    pixel(3, false, Some(linked(2, opaque.clone()))),
                ],
            },
        ];
        let document = NormalizedDocument {
            canvas: (2, 1),
            frames: (0..3)
                .map(|index| crate::NormalizedFrame {
                    index,
                    source_id: Some(index),
                    duration_ms: Some(100),
                    dispose: None,
                })
                .collect(),
            loop_mode: Some(NormalizedLoopMode::Infinite),
            active_frame_index: Some(0),
            ..Default::default()
        };
        let composites = vec![opaque.clone(), opaque.clone(), opaque];

        for mode in [ExportContentReuse::Linked, ExportContentReuse::Aggressive] {
            let mut report = InformationLossReport::default();
            let Some((psd, _, _, stats)) = try_build_local_reuse_psd(
                &document,
                &snapshots,
                &composites,
                &mut report,
                true,
                false,
                mode,
            )
            .expect("sparse local reuse plan") else {
                panic!("sparse local reuse should be selected for {mode:?}");
            };
            assert!(stats.local_layout);
            let roots = psd.children.as_ref().expect("local reuse roots");
            assert_eq!(roots.len(), 2, "transparent-only layer must be removed");
            assert!(roots.iter().all(|layer| {
                layer
                    .image_data
                    .as_ref()
                    .is_none_or(|pixels| pixels.width != 1 || pixels.height != 1)
            }));
            let sparse_track = roots
                .iter()
                .find(|layer| layer.additional_info.name.as_deref() == Some("layer-1"))
                .expect("sparse logical layer");
            let state_layer = sparse_track
                .children
                .as_ref()
                .and_then(|children| children.first())
                .unwrap_or(sparse_track);
            let states = state_layer
                .additional_info
                .animation_frames
                .as_ref()
                .expect("sparse logical animation track");
            assert_eq!(states.len(), 3);
            assert_eq!(states[1].enable, Some(false));
        }

        let mut report = InformationLossReport::default();
        let Some((included, _, _, _)) = try_build_local_reuse_psd(
            &document,
            &snapshots,
            &composites,
            &mut report,
            true,
            true,
            ExportContentReuse::Aggressive,
        )
        .expect("included local reuse plan") else {
            panic!("include policy should preserve transparent states");
        };
        assert!(
            included
                .children
                .as_ref()
                .expect("included roots")
                .iter()
                .any(|layer| layer
                    .children
                    .as_ref()
                    .is_some_and(|children| children.iter().any(|child| {
                        child
                            .image_data
                            .as_ref()
                            .is_some_and(|pixels| pixels.width == 1 && pixels.height == 1)
                    }))),
            "include policy keeps transparent placeholder state"
        );
    }

    /// Verifies content-reuse matching keeps source identity and display attributes in scope.
    #[test]
    fn content_reuse_matches_explicit_links_but_not_position_changes() {
        let layer = |x: i32, linked: bool| FrameSnapshotLayer {
            source_layer_id: 7,
            name: "Body".to_string(),
            kind: NormalizedLayerKind::Pixel,
            opacity: Some(255),
            blend_mode: Some("normal".to_string()),
            visible: true,
            cel: Some(crate::aseprite_reader::FrameSnapshotCel {
                source_frame: 2,
                linked_source_frame: 2,
                explicitly_linked: linked,
                width: 1,
                height: 1,
                x,
                y: 0,
                opacity: 255,
                pixels: vec![255, 0, 0, 255],
            }),
            children: Vec::new(),
        };
        assert!(snapshots_equal(
            &[layer(0, true)],
            &[layer(0, true)],
            ExportContentReuse::Linked
        ));
        assert!(!snapshots_equal(
            &[layer(0, true)],
            &[layer(1, true)],
            ExportContentReuse::Linked
        ));
        assert!(snapshots_equal(
            &[layer(0, false)],
            &[layer(0, true)],
            ExportContentReuse::Linked
        ));
        assert!(snapshots_equal(
            &[layer(0, false)],
            &[layer(0, true)],
            ExportContentReuse::Aggressive
        ));
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
            slices: Vec::new(),
            animation_tags: Vec::new(),
        };
        let mut report = InformationLossReport::default();
        let (psd, _) = build_psd(&document, &[vec![255, 0, 0, 255]], &mut report)
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
        assert_eq!(ExportCompression::default(), ExportCompression::Rle);
    }

    /// Writes one test Aseprite file through its authentic serializer.
    fn write_aseprite(path: &Path, file: &AsepriteFile) {
        let mut bytes = Vec::new();
        file.write_to(&mut bytes).expect("serialize Aseprite");
        fs::write(path, bytes).expect("write Aseprite");
    }
}
