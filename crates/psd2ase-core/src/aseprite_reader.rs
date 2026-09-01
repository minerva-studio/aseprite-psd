//! Aseprite snapshot reader for the PSD export path.

use std::fs;
use std::path::Path;

use aseprite::{AsepriteFile, CelKind, ColorMode, LayerKind, LoopDirection, Pixels};

use crate::aseprite_metadata::read_reference_point_user_data;
use crate::{
    AnimationPoint, ExportError, InformationLocation, InformationLossCode, InformationLossReport,
    LossDisposition, NormalizedBounds, NormalizedDocument, NormalizedFrame, NormalizedLayer,
    NormalizedLayerFrameState, NormalizedLayerKind, NormalizedLoopMode, NormalizedPixels,
};

/// Normalized source and trusted flattened composites used by the PSD writer.
#[derive(Debug, Clone)]
pub struct AsepriteExportSource {
    /// Source layer/cel content represented by static normalized pixel layers.
    pub document: NormalizedDocument,
    /// Full-canvas RGBA8 composites in normalized playback order.
    pub composites: Vec<Vec<u8>>,
    /// Compatibility losses discovered while reading the source snapshots.
    pub information_loss: InformationLossReport,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct CelSample {
    pixels: Vec<u8>,
    width: u32,
    height: u32,
    x: i32,
    y: i32,
    opacity: u8,
    z_index: i16,
}

/// Reads an original Aseprite snapshot and its independently flattened composite snapshot.
pub fn read_aseprite_export(
    input: &Path,
    composite: &Path,
) -> Result<AsepriteExportSource, ExportError> {
    read_aseprite_export_with_active_frame(input, composite, None)
}

/// Reads export snapshots while applying an active frame supplied by the caller.
pub fn read_aseprite_export_with_active_frame(
    input: &Path,
    composite: &Path,
    active_frame_index: Option<u32>,
) -> Result<AsepriteExportSource, ExportError> {
    let file = read_file(input)?;
    let flattened = read_file(composite)?;
    if file.width() != flattened.width()
        || file.height() != flattened.height()
        || file.frames().len() != flattened.frames().len()
    {
        return Err(ExportError::AsepriteRead(
            "original and composite snapshots have different canvas or frame counts".to_string(),
        ));
    }
    if file.frames().is_empty() {
        return Err(ExportError::AsepriteRead(
            "Aseprite snapshot contains no frames".to_string(),
        ));
    }

    let mut information_loss = InformationLossReport::default();
    record_document_losses(&file, &mut information_loss);
    let (sequence, loop_mode) = playback_sequence(&file, &mut information_loss)?;
    let source_composites = read_flattened_composites(&flattened)?;
    let composites = sequence
        .iter()
        .map(|source_frame| source_composites[*source_frame].clone())
        .collect::<Vec<_>>();
    let has_tilemap = file
        .layers()
        .iter()
        .any(|layer| matches!(layer.kind, LayerKind::Tilemap { .. }));
    let mut document = if has_tilemap {
        record_tilemap_losses(&file, &mut information_loss);
        composite_document(&file, &sequence, &composites, loop_mode)
    } else {
        normalized_document(&file, &sequence, loop_mode, &mut information_loss)?
    };
    if let Some(active_frame_index) = active_frame_index {
        if active_frame_index as usize >= document.frames.len() {
            return Err(ExportError::AsepriteRead(format!(
                "active frame index {active_frame_index} is outside the {}-frame export",
                document.frames.len()
            )));
        }
        document.active_frame_index = Some(active_frame_index);
    }
    Ok(AsepriteExportSource {
        document,
        composites,
        information_loss,
    })
}

/// Parses one Aseprite file without exposing the dependency error type.
fn read_file(path: &Path) -> Result<AsepriteFile, ExportError> {
    if !path.is_file() {
        return Err(ExportError::InputMissing(path.to_path_buf()));
    }
    let bytes = fs::read(path).map_err(|error| ExportError::AsepriteRead(error.to_string()))?;
    AsepriteFile::from_reader(bytes.as_slice())
        .map_err(|error| ExportError::AsepriteRead(error.to_string()))
}

/// Records document-level representations that PSD cannot preserve as editable Aseprite data.
fn record_document_losses(file: &AsepriteFile, report: &mut InformationLossReport) {
    if file.color_mode() != ColorMode::Rgba {
        report.add(
            InformationLossCode::UnsupportedColor,
            LossDisposition::Rasterized,
            InformationLocation {
                layer_id: None,
                path: "document".to_string(),
                frame_index: None,
            },
            "Aseprite color data is converted to RGBA8 for Photoshop",
            false,
            true,
        );
    }
    if !file.slices().is_empty() {
        report.add(
            InformationLossCode::Slices,
            LossDisposition::Dropped,
            InformationLocation {
                layer_id: None,
                path: "document/slices".to_string(),
                frame_index: None,
            },
            "Aseprite slices are not represented by the PSD export",
            false,
            true,
        );
    }
    if file.color_profile().is_some() {
        report.add(
            InformationLossCode::EmbeddedColorProfile,
            LossDisposition::Dropped,
            InformationLocation {
                layer_id: None,
                path: "document/color-profile".to_string(),
                frame_index: None,
            },
            "Aseprite color-profile metadata is not embedded in the PSD export",
            false,
            true,
        );
    }
}

/// Expands authored tag directions into one deterministic Photoshop playback sequence.
fn playback_sequence(
    file: &AsepriteFile,
    report: &mut InformationLossReport,
) -> Result<(Vec<usize>, NormalizedLoopMode), ExportError> {
    if file.tags().is_empty() {
        return Ok((
            (0..file.frames().len()).collect(),
            NormalizedLoopMode::Infinite,
        ));
    }
    let mut sequence = Vec::new();
    for tag in file.tags() {
        if tag.from_frame > tag.to_frame || tag.to_frame >= file.frames().len() {
            return Err(ExportError::AsepriteRead(format!(
                "tag {:?} has an invalid frame range",
                tag.name
            )));
        }
        let forward = (tag.from_frame..=tag.to_frame).collect::<Vec<_>>();
        let reverse = (tag.from_frame..=tag.to_frame).rev().collect::<Vec<_>>();
        match tag.direction {
            LoopDirection::Forward => sequence.extend(forward),
            LoopDirection::Reverse => sequence.extend(reverse),
            LoopDirection::PingPong => {
                sequence.extend(&forward);
                sequence.extend(
                    forward
                        .iter()
                        .rev()
                        .skip(1)
                        .take(forward.len().saturating_sub(2)),
                );
            }
            LoopDirection::PingPongReverse => {
                sequence.extend(&reverse);
                sequence.extend(
                    reverse
                        .iter()
                        .rev()
                        .skip(1)
                        .take(reverse.len().saturating_sub(2)),
                );
            }
            _ => {
                return Err(ExportError::AsepriteRead(format!(
                    "tag {:?} uses an unsupported loop direction",
                    tag.name
                )));
            }
        }
    }
    report.add(
        InformationLossCode::AnimationTagName,
        LossDisposition::Degraded,
        InformationLocation {
            layer_id: None,
            path: "document/tags".to_string(),
            frame_index: None,
        },
        "Aseprite tag ranges are expanded into one deterministic Photoshop frame sequence; tag names and boundaries are not editable",
        false,
        true,
    );
    let loop_mode = if file.tags().len() == 1 {
        let repeat = file.tags()[0].repeat;
        if repeat == 0 {
            NormalizedLoopMode::Infinite
        } else {
            NormalizedLoopMode::Finite(u32::from(repeat))
        }
    } else {
        NormalizedLoopMode::Infinite
    };
    Ok((sequence, loop_mode))
}

/// Reads a flattened snapshot as one trusted, already-composited cel per source frame.
fn read_flattened_composites(file: &AsepriteFile) -> Result<Vec<Vec<u8>>, ExportError> {
    let mut output = Vec::with_capacity(file.frames().len());
    for frame_index in 0..file.frames().len() {
        let mut canvas = vec![0; usize::from(file.width()) * usize::from(file.height()) * 4];
        let mut cel_count = 0;
        for (layer_index, layer) in file.layers().iter().enumerate() {
            if !layer.visible || matches!(layer.kind, LayerKind::Group) {
                continue;
            }
            let Some(layer_ref) = file.layer_ref(layer_index) else {
                continue;
            };
            let Some(sample) = cel_sample(file, layer_ref, frame_index)? else {
                continue;
            };
            cel_count += 1;
            if cel_count > 1 {
                return Err(ExportError::AsepriteRead(format!(
                    "flattened snapshot frame {frame_index} contains more than one cel"
                )));
            }
            blit_single_flattened_cel(&mut canvas, file.width(), file.height(), &sample);
        }
        output.push(canvas);
    }
    Ok(output)
}

/// Copies one flattened cel into a transparent full-canvas buffer with clipping.
fn blit_single_flattened_cel(canvas: &mut [u8], width: u16, height: u16, sample: &CelSample) {
    for source_y in 0..sample.height as i32 {
        for source_x in 0..sample.width as i32 {
            let target_x = sample.x + source_x;
            let target_y = sample.y + source_y;
            if target_x < 0
                || target_y < 0
                || target_x >= i32::from(width)
                || target_y >= i32::from(height)
            {
                continue;
            }
            let source = ((source_y as u32 * sample.width + source_x as u32) * 4) as usize;
            let target = (target_y as usize * usize::from(width) + target_x as usize) * 4;
            canvas[target..target + 4].copy_from_slice(&sample.pixels[source..source + 4]);
            canvas[target + 3] =
                ((u16::from(canvas[target + 3]) * u16::from(sample.opacity)) / 255) as u8;
        }
    }
}

/// Builds the normalized layer tree when all source cels are directly readable.
fn normalized_document(
    file: &AsepriteFile,
    sequence: &[usize],
    loop_mode: NormalizedLoopMode,
    report: &mut InformationLossReport,
) -> Result<NormalizedDocument, ExportError> {
    let mut next_id = 1_u32;
    let mut roots = Vec::new();
    for index in 0..file.layers().len() {
        if file.layers()[index].parent.is_none() {
            roots.push(build_layer(file, index, sequence, report, &mut next_id)?);
        }
    }
    Ok(document_header(file, sequence, roots, loop_mode))
}

/// Recursively maps one Aseprite layer into normalized groups and static cel layers.
fn build_layer(
    file: &AsepriteFile,
    layer_index: usize,
    sequence: &[usize],
    report: &mut InformationLossReport,
    next_id: &mut u32,
) -> Result<NormalizedLayer, ExportError> {
    let layer = &file.layers()[layer_index];
    match layer.kind {
        LayerKind::Group => {
            let id = take_id(next_id);
            let mut children = Vec::new();
            for child_index in 0..file.layers().len() {
                if file.layers()[child_index].parent == Some(layer_index) {
                    children.push(build_layer(file, child_index, sequence, report, next_id)?);
                }
            }
            let source_points =
                read_reference_point_user_data(layer.user_data.as_ref(), file.frames().len());
            let reference_points = sequence
                .iter()
                .map(|source_frame| source_points.get(*source_frame).copied().flatten())
                .collect::<Vec<_>>();
            Ok(group_layer(
                id,
                layer,
                sequence.len(),
                children,
                Some(&reference_points),
            ))
        }
        LayerKind::Normal => build_cel_layers(file, layer_index, sequence, report, next_id),
        LayerKind::Tilemap { .. } => Err(ExportError::AsepriteRead(
            "tilemap reached the editable layer mapper after rasterization selection".to_string(),
        )),
        _ => Err(ExportError::AsepriteRead(format!(
            "layer {:?} uses an unsupported kind",
            layer.name
        ))),
    }
}

/// Builds one direct pixel layer or one wrapper group containing reusable static cels.
fn build_cel_layers(
    file: &AsepriteFile,
    layer_index: usize,
    sequence: &[usize],
    report: &mut InformationLossReport,
    next_id: &mut u32,
) -> Result<NormalizedLayer, ExportError> {
    let layer = &file.layers()[layer_index];
    let layer_ref = file
        .layer_ref(layer_index)
        .ok_or_else(|| ExportError::AsepriteRead("normal layer has no layer handle".to_string()))?;
    let source_points =
        read_reference_point_user_data(layer.user_data.as_ref(), file.frames().len());
    let reference_points = sequence
        .iter()
        .map(|source_frame| source_points.get(*source_frame).copied().flatten())
        .collect::<Vec<_>>();
    let mut variants: Vec<CelSample> = Vec::new();
    let mut occurrences = Vec::with_capacity(sequence.len());
    for (frame_index, source_frame) in sequence.iter().enumerate() {
        let sample = cel_sample(file, layer_ref, *source_frame)?;
        let Some(sample) = sample else {
            occurrences.push(None);
            continue;
        };
        if sample.z_index != 0 {
            report.add(
                InformationLossCode::CelZIndex,
                LossDisposition::Degraded,
                InformationLocation {
                    layer_id: Some((layer_index + 1) as u32),
                    path: layer.name.clone(),
                    frame_index: Some(frame_index as u32),
                },
                "Aseprite per-cel z-index is reduced to the static Photoshop layer order",
                true,
                true,
            );
        }
        let variant = variants
            .iter()
            .position(|existing| {
                existing.width == sample.width
                    && existing.height == sample.height
                    && existing.pixels == sample.pixels
            })
            .unwrap_or_else(|| {
                variants.push(sample.clone());
                variants.len() - 1
            });
        occurrences.push(Some((variant, sample.x, sample.y, sample.opacity)));
    }

    if variants.is_empty() {
        report.add(
            InformationLossCode::EmptyPixelLayer,
            LossDisposition::Degraded,
            InformationLocation {
                layer_id: Some((layer_index + 1) as u32),
                path: layer.name.clone(),
                frame_index: None,
            },
            "An empty Aseprite pixel layer is represented by a permanently hidden transparent PSD pixel",
            false,
            true,
        );
        return Ok(static_cel_layer(
            take_id(next_id),
            layer.name.clone(),
            &CelSample {
                pixels: vec![0, 0, 0, 0],
                width: 1,
                height: 1,
                x: 0,
                y: 0,
                opacity: 255,
                z_index: 0,
            },
            &vec![None; sequence.len()],
            0,
            Some(layer.opacity),
            blend_mode_name(layer.blend_mode),
            layer.visible,
            None,
        ));
    }
    if variants.len() == 1 {
        return Ok(static_cel_layer(
            take_id(next_id),
            layer.name.clone(),
            &variants[0],
            &occurrences,
            0,
            Some(layer.opacity),
            blend_mode_name(layer.blend_mode),
            layer.visible,
            Some(&reference_points),
        ));
    }

    let mut children = Vec::with_capacity(variants.len());
    for (variant_index, variant) in variants.iter().enumerate() {
        children.push(static_cel_layer(
            take_id(next_id),
            format!("{} — Cel {}", layer.name, variant_index + 1),
            variant,
            &occurrences,
            variant_index,
            Some(255),
            "normal".to_string(),
            true,
            None,
        ));
    }
    Ok(group_layer(
        take_id(next_id),
        layer,
        sequence.len(),
        children,
        Some(&reference_points),
    ))
}

/// Creates one normalized static pixel layer and its per-frame visibility/offset records.
#[allow(clippy::too_many_arguments)]
fn static_cel_layer(
    id: u32,
    name: String,
    variant: &CelSample,
    occurrences: &[Option<(usize, i32, i32, u8)>],
    variant_index: usize,
    layer_opacity: Option<u8>,
    blend_mode: String,
    visible: bool,
    reference_points: Option<&[Option<AnimationPoint>]>,
) -> NormalizedLayer {
    let states = occurrences
        .iter()
        .enumerate()
        .map(|(frame_index, occurrence)| {
            let matching = occurrence
                .as_ref()
                .filter(|(index, _, _, _)| *index == variant_index);
            NormalizedLayerFrameState {
                frame_index: frame_index as u32,
                record_present: true,
                enabled: visible && matching.is_some(),
                explicit_enable: true,
                offset: matching.map(|(_, x, y, _)| crate::AnimationPoint {
                    x: f64::from(*x - variant.x),
                    y: f64::from(*y - variant.y),
                }),
                reference_point: reference_points
                    .and_then(|points| points.get(frame_index).copied())
                    .flatten(),
                opacity: matching.map(|(_, _, _, opacity)| f64::from(*opacity) / 255.0),
            }
        })
        .collect();
    NormalizedLayer {
        id,
        name,
        kind: NormalizedLayerKind::Pixel,
        bounds: NormalizedBounds {
            left: variant.x,
            top: variant.y,
            right: variant.x + variant.width as i32,
            bottom: variant.y + variant.height as i32,
        },
        opacity: layer_opacity.map(|value| f64::from(value) / 255.0),
        blend_mode: Some(blend_mode),
        hidden: Some(!visible),
        pixels: Some(NormalizedPixels {
            width: variant.width,
            height: variant.height,
            left: variant.x,
            top: variant.y,
            data: variant.pixels.clone(),
        }),
        children: Vec::new(),
        frame_states: states,
    }
}

/// Creates one normalized group with static Aseprite layer properties.
fn group_layer(
    id: u32,
    layer: &aseprite::Layer,
    frame_count: usize,
    children: Vec<NormalizedLayer>,
    reference_points: Option<&[Option<AnimationPoint>]>,
) -> NormalizedLayer {
    NormalizedLayer {
        id,
        name: layer.name.clone(),
        kind: NormalizedLayerKind::Group,
        bounds: NormalizedBounds {
            left: 0,
            top: 0,
            right: 0,
            bottom: 0,
        },
        opacity: Some(f64::from(layer.opacity) / 255.0),
        blend_mode: Some(blend_mode_name(layer.blend_mode)),
        hidden: Some(!layer.visible),
        pixels: None,
        children,
        frame_states: (0..frame_count)
            .map(|frame_index| NormalizedLayerFrameState {
                frame_index: frame_index as u32,
                record_present: true,
                enabled: layer.visible,
                explicit_enable: true,
                offset: None,
                reference_point: reference_points
                    .and_then(|points| points.get(frame_index).copied())
                    .flatten(),
                opacity: None,
            })
            .collect(),
    }
}

/// Returns one cel as owned RGBA8 data, following linked cel pixels while preserving position.
fn cel_sample(
    file: &AsepriteFile,
    layer: aseprite::LayerRef,
    frame: usize,
) -> Result<Option<CelSample>, ExportError> {
    let Some(cel) = file.cel(layer, frame) else {
        return Ok(None);
    };
    let (pixels, x, y) = match &cel.kind {
        CelKind::Raw { pixels, x, y } | CelKind::Compressed { pixels, x, y, .. } => {
            (pixels, *x, *y)
        }
        CelKind::Linked { x, y, .. } => {
            let resolved = file.resolve_cel(layer, frame).ok_or_else(|| {
                ExportError::AsepriteRead(format!("linked cel at frame {frame} cannot be resolved"))
            })?;
            let pixels = match &resolved.kind {
                CelKind::Raw { pixels, .. } | CelKind::Compressed { pixels, .. } => pixels,
                _ => {
                    return Err(ExportError::AsepriteRead(format!(
                        "linked cel at frame {frame} resolves to a non-pixel cel"
                    )));
                }
            };
            (pixels, *x, *y)
        }
        CelKind::Tilemap { .. } => return Ok(None),
        _ => {
            return Err(ExportError::AsepriteRead(format!(
                "cel at frame {frame} uses an unsupported kind"
            )));
        }
    };
    Ok(Some(CelSample {
        pixels: rgba_pixels(file, pixels),
        width: u32::from(pixels.width),
        height: u32::from(pixels.height),
        x: i32::from(x),
        y: i32::from(y),
        opacity: cel.opacity,
        z_index: cel.z_index,
    }))
}

/// Converts all supported Aseprite color modes to owned RGBA8 pixels.
fn rgba_pixels(file: &AsepriteFile, pixels: &Pixels) -> Vec<u8> {
    match file.color_mode() {
        ColorMode::Rgba => pixels.data.clone(),
        ColorMode::Grayscale => pixels
            .data
            .chunks(2)
            .flat_map(|pixel| [pixel[0], pixel[0], pixel[0], pixel[1]])
            .collect(),
        ColorMode::Indexed => pixels
            .data
            .iter()
            .flat_map(|index| {
                let color = file.palette().get(usize::from(*index));
                let alpha = if *index == file.transparent_index() {
                    0
                } else {
                    color.map_or(255, |value| value.a)
                };
                [
                    color.map_or(0, |value| value.r),
                    color.map_or(0, |value| value.g),
                    color.map_or(0, |value| value.b),
                    alpha,
                ]
            })
            .collect(),
        _ => Vec::new(),
    }
}

/// Converts Aseprite blend modes to the normalized Photoshop spelling.
fn blend_mode_name(mode: aseprite::BlendMode) -> String {
    use aseprite::BlendMode;
    match mode {
        BlendMode::Normal => "normal",
        BlendMode::Multiply => "multiply",
        BlendMode::Screen => "screen",
        BlendMode::Overlay => "overlay",
        BlendMode::Darken => "darken",
        BlendMode::Lighten => "lighten",
        BlendMode::ColorDodge => "color dodge",
        BlendMode::ColorBurn => "color burn",
        BlendMode::HardLight => "hard light",
        BlendMode::SoftLight => "soft light",
        BlendMode::Difference => "difference",
        BlendMode::Exclusion => "exclusion",
        BlendMode::Hue => "hue",
        BlendMode::Saturation => "saturation",
        BlendMode::Color => "color",
        BlendMode::Luminosity => "luminosity",
        BlendMode::Addition => "linear dodge",
        BlendMode::Subtract => "subtract",
        BlendMode::Divide => "divide",
        _ => "normal",
    }
    .to_string()
}

/// Allocates one stable layer ID in traversal order.
fn take_id(next_id: &mut u32) -> u32 {
    let id = *next_id;
    *next_id += 1;
    id
}

/// Creates the shared normalized document header and playback frames.
fn document_header(
    file: &AsepriteFile,
    sequence: &[usize],
    root_layers: Vec<NormalizedLayer>,
    loop_mode: NormalizedLoopMode,
) -> NormalizedDocument {
    NormalizedDocument {
        canvas: (u32::from(file.width()), u32::from(file.height())),
        channels: Some(4),
        bits_per_channel: Some(8),
        color_mode: Some("rgba".to_string()),
        root_layers,
        frames: sequence
            .iter()
            .enumerate()
            .map(|(index, source_frame)| NormalizedFrame {
                index: index as u32,
                source_id: Some((index + 1) as u32),
                duration_ms: Some(u32::from(file.frames()[*source_frame].duration_ms)),
                dispose: Some("auto".to_string()),
            })
            .collect(),
        loop_mode: Some(loop_mode),
        active_frame_index: None,
        animation_resource_ids: vec![4000],
        animation_frame_flags: Some(crate::AnimationFlags {
            propagate_frame_one: false,
            unify_layer_position: false,
            unify_layer_style: false,
            unify_layer_visibility: false,
        }),
    }
}

/// Records editable tilemap loss locations before switching to trusted composites.
fn record_tilemap_losses(file: &AsepriteFile, report: &mut InformationLossReport) {
    for (index, layer) in file.layers().iter().enumerate() {
        if matches!(layer.kind, LayerKind::Tilemap { .. }) {
            report.add(
                InformationLossCode::Tilemap,
                LossDisposition::Rasterized,
                InformationLocation {
                    layer_id: Some((index + 1) as u32),
                    path: layer.name.clone(),
                    frame_index: None,
                },
                "Aseprite tilemap editability is rasterized using the independently flattened composite snapshot",
                false,
                true,
            );
        }
    }
}

/// Builds a composite-only normalized document for sources containing tilemaps.
fn composite_document(
    file: &AsepriteFile,
    sequence: &[usize],
    composites: &[Vec<u8>],
    loop_mode: NormalizedLoopMode,
) -> NormalizedDocument {
    let mut variants: Vec<Vec<u8>> = Vec::new();
    let mut occurrences = Vec::with_capacity(composites.len());
    for pixels in composites {
        let variant = variants
            .iter()
            .position(|value| value == pixels)
            .unwrap_or_else(|| {
                variants.push(pixels.clone());
                variants.len() - 1
            });
        occurrences.push(Some((variant, 0, 0, 255)));
    }
    let mut children = Vec::new();
    for (index, pixels) in variants.iter().enumerate() {
        children.push(static_cel_layer(
            (index + 2) as u32,
            format!("Composite — Frame {}", index + 1),
            &CelSample {
                pixels: pixels.clone(),
                width: u32::from(file.width()),
                height: u32::from(file.height()),
                x: 0,
                y: 0,
                opacity: 255,
                z_index: 0,
            },
            &occurrences,
            index,
            Some(255),
            "normal".to_string(),
            true,
            None,
        ));
    }
    let synthetic = aseprite::Layer {
        name: "Rasterized Composite".to_string(),
        kind: LayerKind::Group,
        parent: None,
        opacity: 255,
        blend_mode: aseprite::BlendMode::Normal,
        visible: true,
        editable: true,
        lock_movement: false,
        background: false,
        prefer_linked_cels: false,
        collapsed: false,
        reference_layer: false,
        user_data: None,
    };
    document_header(
        file,
        sequence,
        vec![group_layer(1, &synthetic, sequence.len(), children, None)],
        loop_mode,
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use aseprite::Color;

    #[test]
    fn ping_pong_expansion_does_not_duplicate_endpoints() {
        let mut file = AsepriteFile::new(1, 1, ColorMode::Rgba);
        file.add_frame(10);
        file.add_frame(20);
        file.add_frame(30);
        file.add_tag("loop", 0..=2, LoopDirection::PingPong)
            .expect("add tag");
        let mut report = InformationLossReport::default();
        assert_eq!(
            playback_sequence(&file, &mut report).expect("sequence").0,
            vec![0, 1, 2, 1]
        );
    }

    #[test]
    fn single_tag_repeat_count_becomes_the_document_loop_count() {
        let mut file = AsepriteFile::new(1, 1, ColorMode::Rgba);
        file.add_frame(10);
        file.add_tag_with("once more", 0..=0, LoopDirection::Forward, 2)
            .expect("add tag");
        let mut report = InformationLossReport::default();
        assert_eq!(
            playback_sequence(&file, &mut report).expect("sequence").1,
            NormalizedLoopMode::Finite(2)
        );
    }

    #[test]
    fn grayscale_pixels_expand_to_rgba() {
        let file = AsepriteFile::new(1, 1, ColorMode::Grayscale);
        let pixels =
            Pixels::new(vec![90, 128], 1, 1, ColorMode::Grayscale).expect("grayscale pixels");
        assert_eq!(rgba_pixels(&file, &pixels), vec![90, 90, 90, 128]);
    }

    #[test]
    fn indexed_pixels_use_palette_and_transparent_index() {
        let mut file = AsepriteFile::new(2, 1, ColorMode::Indexed);
        file.set_palette(&[
            Color {
                r: 10,
                g: 20,
                b: 30,
                a: 255,
                name: None,
            },
            Color {
                r: 40,
                g: 50,
                b: 60,
                a: 200,
                name: None,
            },
        ])
        .expect("indexed palette");
        let pixels = Pixels::new(vec![0, 1], 2, 1, ColorMode::Indexed).expect("indexed pixels");
        assert_eq!(
            rgba_pixels(&file, &pixels),
            vec![10, 20, 30, 0, 40, 50, 60, 200]
        );
    }
}
