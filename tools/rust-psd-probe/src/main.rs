use std::env;
use std::fs;
use std::path::PathBuf;
use std::process::ExitCode;

use psd2ase_core::{
    AnimationFlags, AnimationPoint, NormalizedDocument, NormalizedLayer, NormalizedLayerFrameState,
    NormalizedLayerKind, NormalizedLoopMode, normalize,
};
use serde::Serialize;
use sha2::{Digest, Sha256};

const DEFAULT_INPUT: &str = r"path\to\fixture.psd";
const SCHEMA_VERSION: u32 = 3;

/// Runs the Rust PSD probe and writes its normalized snapshot.
fn main() -> ExitCode {
    match run(env::args().skip(1).collect()) {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("error: {error}");
            ExitCode::FAILURE
        }
    }
}

/// Parses probe arguments, reads the PSD, and serializes the snapshot.
fn run(arguments: Vec<String>) -> Result<(), String> {
    let input = argument_value(&arguments, "--input")
        .or_else(|| env::var_os("PSD2ASE_FIXTURE").map(PathBuf::from))
        .unwrap_or_else(|| PathBuf::from(DEFAULT_INPUT));
    let output = argument_value(&arguments, "--output")
        .unwrap_or_else(|| PathBuf::from("target/probe/rust-snapshot.json"));
    if !input.is_file() {
        return Err(format!("input is not a file: {}", input.display()));
    }
    let bytes = fs::read(&input).map_err(|error| format!("could not read input: {error}"))?;
    let document = normalize(&input).map_err(|error| error.to_string())?;
    let snapshot = build_snapshot(&bytes, &document)?;
    let serialized = serde_json::to_vec_pretty(&snapshot)
        .map_err(|error| format!("could not serialize snapshot: {error}"))?;
    if let Some(parent) = output.parent() {
        fs::create_dir_all(parent)
            .map_err(|error| format!("could not create output directory: {error}"))?;
    }
    fs::write(&output, serialized).map_err(|error| format!("could not write snapshot: {error}"))?;
    println!("wrote {}", output.display());
    Ok(())
}

/// Returns the value after a two-token command-line option.
fn argument_value(arguments: &[String], option: &str) -> Option<PathBuf> {
    arguments
        .iter()
        .position(|argument| argument == option)
        .and_then(|index| arguments.get(index + 1))
        .map(PathBuf::from)
}

/// Builds a probe snapshot exclusively from the normalized document model.
fn build_snapshot(bytes: &[u8], document: &NormalizedDocument) -> Result<ProbeSnapshot, String> {
    let mut layers = Vec::new();
    let mut normalized_layers = Vec::new();
    for (index, layer) in document.root_layers.iter().enumerate() {
        collect_layer(
            layer,
            &[index.to_string()],
            &mut layers,
            &mut normalized_layers,
        )?;
    }
    let animated = !document.animation_resource_ids.is_empty();
    Ok(ProbeSnapshot {
        schema_version: SCHEMA_VERSION,
        source: SourceSnapshot {
            byte_length: bytes.len(),
            sha256: sha256_hex(bytes),
        },
        document: DocumentSnapshot {
            width: document.canvas.0,
            height: document.canvas.1,
            channels: document.channels,
            bits_per_channel: document.bits_per_channel,
            color_mode: document.color_mode.clone(),
            root_layer_count: document.root_layers.len(),
            group_count: layers.iter().filter(|layer| layer.kind == "group").count(),
            pixel_layer_count: layers.iter().filter(|layer| layer.kind == "pixel").count(),
        },
        layers,
        animation: if animated {
            animation_snapshot(document, &normalized_layers)
        } else {
            AnimationSummary::default()
        },
        normalized_document: normalized_document_snapshot(document, &normalized_layers),
    })
}

/// Recursively converts one normalized layer into base and model snapshots.
fn collect_layer(
    layer: &NormalizedLayer,
    path: &[String],
    snapshots: &mut Vec<LayerSnapshot>,
    normalized_layers: &mut Vec<NormalizedLayerStateSnapshot>,
) -> Result<(), String> {
    let path_string = path.join("/");
    let is_group = layer.kind == NormalizedLayerKind::Group;
    let pixel = layer.pixels.as_ref().map(pixel_snapshot).transpose()?;
    snapshots.push(LayerSnapshot {
        path: path_string.clone(),
        id: layer.id,
        kind: if is_group { "group" } else { "pixel" }.to_string(),
        name: layer.name.clone(),
        top: layer.bounds.top,
        left: layer.bounds.left,
        bottom: layer.bounds.bottom,
        right: layer.bounds.right,
        opacity: layer.opacity,
        blend_mode: layer.blend_mode.clone(),
        hidden: layer.hidden,
        pixel,
        animation_frame_count: if layer.frame_states.len() > 1 {
            Some(layer.frame_states.len())
        } else {
            None
        },
    });
    normalized_layers.push(NormalizedLayerStateSnapshot {
        layer_id: layer.id,
        path: path_string,
        frames: layer
            .frame_states
            .iter()
            .map(normalized_layer_frame_snapshot)
            .collect(),
    });
    for (index, child) in layer.children.iter().enumerate() {
        let mut child_path = path.to_vec();
        child_path.push(index.to_string());
        collect_layer(child, &child_path, snapshots, normalized_layers)?;
    }
    Ok(())
}

/// Converts owned RGBA8 data into dimensions, origin, and a digest.
fn pixel_snapshot(pixel: &psd2ase_core::NormalizedPixels) -> Result<PixelSnapshot, String> {
    let expected = pixel
        .width
        .checked_mul(pixel.height)
        .and_then(|value| value.checked_mul(4))
        .ok_or_else(|| "pixel dimensions overflow RGBA8 size".to_string())?;
    if pixel.data.len() != expected as usize {
        return Err(format!(
            "pixel buffer length mismatch: expected {expected}, got {}",
            pixel.data.len()
        ));
    }
    Ok(PixelSnapshot {
        width: pixel.width,
        height: pixel.height,
        left: pixel.left,
        top: pixel.top,
        byte_length: pixel.data.len(),
        sha256: sha256_hex(&pixel.data),
    })
}

/// Returns the stable SHA-256 hex digest used by both probes.
fn sha256_hex(bytes: &[u8]) -> String {
    let digest = Sha256::digest(bytes);
    digest.iter().map(|byte| format!("{byte:02x}")).collect()
}

/// Converts the normalized model's authored animation into the stage gate shape.
fn animation_snapshot(
    document: &NormalizedDocument,
    layers: &[NormalizedLayerStateSnapshot],
) -> AnimationSummary {
    AnimationSummary {
        resource_ids: document.animation_resource_ids.clone(),
        frames: document
            .frames
            .iter()
            .filter_map(|frame| {
                Some(AnimationFrameSnapshot {
                    id: frame.source_id?,
                    duration_ms: frame.duration_ms?,
                    dispose: frame.dispose.clone(),
                })
            })
            .collect(),
        loop_mode: document.loop_mode.as_ref().map(loop_mode_name),
        active_frame: document.active_frame_index,
        layer_states: layers
            .iter()
            .map(|layer| LayerAnimationSnapshot {
                layer_id: layer.layer_id,
                path: layer.path.clone(),
                frames: layer
                    .frames
                    .iter()
                    .map(|frame| LayerFrameSnapshot {
                        frame_id: document.frames[frame.frame_index as usize]
                            .source_id
                            .expect("animated frame must have a source ID"),
                        enabled: frame.enabled,
                        explicit_enable: frame.explicit_enable,
                        offset: frame.offset.clone(),
                        reference_point: frame.reference_point.clone(),
                        opacity: frame.opacity,
                    })
                    .collect(),
            })
            .collect(),
        visible_pixel_layers: document
            .frames
            .iter()
            .enumerate()
            .map(|(frame_index, frame)| {
                let mut layer_ids = Vec::new();
                for layer in &document.root_layers {
                    layer.collect_visible_pixel_layer_ids(frame_index, true, &mut layer_ids);
                }
                VisibleFrameLayersSnapshot {
                    frame_id: frame
                        .source_id
                        .expect("animated frame must have a source ID"),
                    layer_ids,
                }
            })
            .collect(),
        frame_flags: document
            .animation_frame_flags
            .as_ref()
            .map(animation_flags_snapshot),
    }
}

/// Converts the complete normalized model animation section, including static fallback frames.
fn normalized_document_snapshot(
    document: &NormalizedDocument,
    layers: &[NormalizedLayerStateSnapshot],
) -> NormalizedDocumentSnapshot {
    NormalizedDocumentSnapshot {
        frames: document
            .frames
            .iter()
            .map(|frame| NormalizedFrameSnapshot {
                index: frame.index,
                source_id: frame.source_id,
                duration_ms: frame.duration_ms,
                dispose: frame.dispose.clone(),
            })
            .collect(),
        loop_mode: document.loop_mode.as_ref().map(loop_mode_name),
        active_frame: document.active_frame_index,
        resource_ids: document.animation_resource_ids.clone(),
        layer_states: layers.to_vec(),
    }
}

/// Converts one normalized frame state into probe JSON.
fn normalized_layer_frame_snapshot(
    state: &NormalizedLayerFrameState,
) -> NormalizedLayerFrameSnapshot {
    NormalizedLayerFrameSnapshot {
        frame_index: state.frame_index,
        enabled: state.enabled,
        explicit_enable: state.explicit_enable,
        offset: state.offset.map(point_snapshot),
        reference_point: state.reference_point.map(point_snapshot),
        opacity: state.opacity,
    }
}

/// Converts a normalized animation point into probe JSON.
fn point_snapshot(point: AnimationPoint) -> PointSnapshot {
    PointSnapshot {
        x: point.x,
        y: point.y,
    }
}

/// Converts a normalized loop policy into its stable string name.
fn loop_mode_name(value: &NormalizedLoopMode) -> String {
    match value {
        NormalizedLoopMode::Infinite => "infinite".to_string(),
        NormalizedLoopMode::Finite(count) => format!("finite:{count}"),
    }
}

/// Converts normalized animation flags into probe JSON.
fn animation_flags_snapshot(value: &AnimationFlags) -> AnimationFlagsSnapshot {
    AnimationFlagsSnapshot {
        propagate_frame_one: value.propagate_frame_one,
        unify_layer_position: value.unify_layer_position,
        unify_layer_style: value.unify_layer_style,
        unify_layer_visibility: value.unify_layer_visibility,
    }
}

#[derive(Debug, Serialize)]
struct ProbeSnapshot {
    schema_version: u32,
    source: SourceSnapshot,
    document: DocumentSnapshot,
    layers: Vec<LayerSnapshot>,
    animation: AnimationSummary,
    normalized_document: NormalizedDocumentSnapshot,
}

#[derive(Debug, Serialize)]
struct SourceSnapshot {
    byte_length: usize,
    sha256: String,
}

#[derive(Debug, Serialize)]
struct DocumentSnapshot {
    width: u32,
    height: u32,
    channels: Option<u32>,
    bits_per_channel: Option<u32>,
    color_mode: Option<String>,
    root_layer_count: usize,
    group_count: usize,
    pixel_layer_count: usize,
}

#[derive(Debug, Serialize)]
struct LayerSnapshot {
    path: String,
    id: u32,
    kind: String,
    name: String,
    top: i32,
    left: i32,
    bottom: i32,
    right: i32,
    opacity: Option<f64>,
    blend_mode: Option<String>,
    hidden: Option<bool>,
    pixel: Option<PixelSnapshot>,
    animation_frame_count: Option<usize>,
}

#[derive(Debug, Serialize)]
struct PixelSnapshot {
    width: u32,
    height: u32,
    left: i32,
    top: i32,
    byte_length: usize,
    sha256: String,
}

#[derive(Debug, Serialize)]
struct NormalizedDocumentSnapshot {
    frames: Vec<NormalizedFrameSnapshot>,
    loop_mode: Option<String>,
    active_frame: Option<u32>,
    resource_ids: Vec<u16>,
    layer_states: Vec<NormalizedLayerStateSnapshot>,
}

#[derive(Debug, Clone, Serialize)]
struct NormalizedFrameSnapshot {
    index: u32,
    source_id: Option<u32>,
    duration_ms: Option<u32>,
    dispose: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
struct NormalizedLayerStateSnapshot {
    layer_id: u32,
    path: String,
    frames: Vec<NormalizedLayerFrameSnapshot>,
}

#[derive(Debug, Clone, Serialize)]
struct NormalizedLayerFrameSnapshot {
    frame_index: u32,
    enabled: bool,
    explicit_enable: bool,
    offset: Option<PointSnapshot>,
    reference_point: Option<PointSnapshot>,
    opacity: Option<f64>,
}

#[derive(Debug, Serialize)]
struct AnimationSummary {
    resource_ids: Vec<u16>,
    frames: Vec<AnimationFrameSnapshot>,
    loop_mode: Option<String>,
    active_frame: Option<u32>,
    layer_states: Vec<LayerAnimationSnapshot>,
    visible_pixel_layers: Vec<VisibleFrameLayersSnapshot>,
    frame_flags: Option<AnimationFlagsSnapshot>,
}

#[derive(Debug, Serialize)]
struct AnimationFrameSnapshot {
    id: u32,
    duration_ms: u32,
    dispose: Option<String>,
}

#[derive(Debug, Serialize)]
struct LayerAnimationSnapshot {
    layer_id: u32,
    path: String,
    frames: Vec<LayerFrameSnapshot>,
}

#[derive(Debug, Serialize)]
struct LayerFrameSnapshot {
    frame_id: u32,
    enabled: bool,
    explicit_enable: bool,
    offset: Option<PointSnapshot>,
    reference_point: Option<PointSnapshot>,
    opacity: Option<f64>,
}

#[derive(Debug, Clone, Serialize)]
struct PointSnapshot {
    x: f64,
    y: f64,
}

#[derive(Debug, Serialize)]
struct VisibleFrameLayersSnapshot {
    frame_id: u32,
    layer_ids: Vec<u32>,
}

#[derive(Debug, Serialize)]
struct AnimationFlagsSnapshot {
    propagate_frame_one: bool,
    unify_layer_position: bool,
    unify_layer_style: bool,
    unify_layer_visibility: bool,
}

impl Default for AnimationSummary {
    fn default() -> Self {
        Self {
            resource_ids: Vec::new(),
            frames: Vec::new(),
            loop_mode: None,
            active_frame: None,
            layer_states: Vec::new(),
            visible_pixel_layers: Vec::new(),
            frame_flags: None,
        }
    }
}
