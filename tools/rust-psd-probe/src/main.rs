use std::env;
use std::fs;
use std::path::PathBuf;
use std::process::ExitCode;

use ag_psd::psd::{BlendMode, ColorMode, Layer};
use psd2ase_core::{
    AnimationFlags, AnimationLayerInput, AnimationParseError, AnimationPoint, LayerAnimationState,
    LayerFrameState, LoopMode, PhotoshopAnimation, parse_photoshop_animation,
};
use serde::Serialize;
use sha2::{Digest, Sha256};

const DEFAULT_INPUT: &str = r"path\to\fixture.psd";
const SCHEMA_VERSION: u32 = 2;

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
    let snapshot = build_snapshot(&bytes)?;
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

/// Builds a format-independent snapshot from one PSD byte buffer.
fn build_snapshot(bytes: &[u8]) -> Result<ProbeSnapshot, String> {
    let options = ag_psd::psd::ReadOptions {
        use_image_data: Some(true),
        skip_thumbnail: Some(true),
        ..Default::default()
    };
    let psd = ag_psd::read_psd(bytes, &options).map_err(|error| error.to_string())?;

    let mut layers = Vec::new();
    let mut animation_inputs = Vec::new();
    let root_layers = psd.children.as_deref().unwrap_or_default();
    for (index, layer) in root_layers.iter().enumerate() {
        collect_layer(layer, &[index.to_string()], &mut layers)?;
        collect_animation_input(layer, &[index.to_string()], &[], &mut animation_inputs)?;
    }

    let animation = parse_photoshop_animation(bytes, &animation_inputs).map_err(animation_error)?;
    for layer in &mut layers {
        layer.animation_frame_count = animation.as_ref().map(|value| {
            value
                .layer_states
                .iter()
                .find(|state| state.path == layer.path)
                .map_or(0, |state| state.frames.len())
        });
    }

    Ok(ProbeSnapshot {
        schema_version: SCHEMA_VERSION,
        source: SourceSnapshot {
            byte_length: bytes.len(),
            sha256: sha256_hex(bytes),
        },
        document: DocumentSnapshot {
            width: psd.width,
            height: psd.height,
            channels: psd.channels,
            bits_per_channel: psd.bits_per_channel,
            color_mode: psd.color_mode.map(color_mode_name),
            root_layer_count: root_layers.len(),
            group_count: layers.iter().filter(|layer| layer.kind == "group").count(),
            pixel_layer_count: layers.iter().filter(|layer| layer.kind == "pixel").count(),
        },
        layers,
        animation: animation
            .as_ref()
            .map(animation_snapshot)
            .unwrap_or_default(),
    })
}

/// Converts a normalized parser error to the probe's string error boundary.
fn animation_error(error: AnimationParseError) -> String {
    format!("Photoshop animation metadata: {error}")
}

/// Recursively builds the format-independent layer input for animation parsing.
fn collect_animation_input(
    layer: &Layer,
    path: &[String],
    ancestors: &[u32],
    inputs: &mut Vec<AnimationLayerInput>,
) -> Result<(), String> {
    let id = layer
        .additional_info
        .id
        .map(number_to_layer_id)
        .unwrap_or(0);
    inputs.push(AnimationLayerInput {
        id,
        path: path.join("/"),
        is_group: layer.children.is_some(),
        hidden: layer.hidden.unwrap_or(false),
        ancestor_ids: ancestors.to_vec(),
    });
    let mut child_ancestors = ancestors.to_vec();
    if id != 0 && layer.children.is_some() {
        child_ancestors.push(id);
    }
    if let Some(children) = &layer.children {
        for (index, child) in children.iter().enumerate() {
            let mut child_path = path.to_vec();
            child_path.push(index.to_string());
            collect_animation_input(child, &child_path, &child_ancestors, inputs)?;
        }
    }
    Ok(())
}

/// Converts ag-psd's numeric layer ID into the strict normalized ID type.
fn number_to_layer_id(value: f64) -> u32 {
    if value.is_finite() && value >= 1.0 && value.fract() == 0.0 && value <= u32::MAX as f64 {
        value as u32
    } else {
        0
    }
}

/// Recursively converts one parser layer into a normalized layer snapshot.
fn collect_layer(
    layer: &Layer,
    path: &[String],
    snapshots: &mut Vec<LayerSnapshot>,
) -> Result<(), String> {
    let is_group = layer.children.is_some();
    let pixel = if is_group {
        None
    } else {
        layer
            .image_data
            .as_ref()
            .or(layer.canvas.as_ref())
            .map(pixel_snapshot)
            .transpose()?
            .ok_or_else(|| format!("pixel layer has no RGBA8 data: {}", path.join("/")))
            .map(Some)?
    };

    snapshots.push(LayerSnapshot {
        path: path.join("/"),
        id: layer.additional_info.id,
        kind: if is_group { "group" } else { "pixel" }.to_string(),
        name: layer.additional_info.name.clone().unwrap_or_default(),
        top: layer.top,
        left: layer.left,
        bottom: layer.bottom,
        right: layer.right,
        opacity: layer.opacity,
        blend_mode: layer.blend_mode.map(blend_mode_name),
        hidden: layer.hidden,
        pixel,
        animation_frame_count: None,
    });

    if let Some(children) = &layer.children {
        for (index, child) in children.iter().enumerate() {
            let mut child_path = path.to_vec();
            child_path.push(index.to_string());
            collect_layer(child, &child_path, snapshots)?;
        }
    }
    Ok(())
}

/// Converts one decoded pixel buffer into its dimensions and digest.
fn pixel_snapshot(pixel: &ag_psd::PixelData) -> Result<PixelSnapshot, String> {
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
        byte_length: pixel.data.len(),
        sha256: sha256_hex(&pixel.data),
    })
}

/// Returns the stable SHA-256 hex digest used by both probes.
fn sha256_hex(bytes: &[u8]) -> String {
    let digest = Sha256::digest(bytes);
    digest.iter().map(|byte| format!("{byte:02x}")).collect()
}

/// Normalizes parser enum debug names to the oracle's lowercase string form.
fn normalize_name(value: &str) -> String {
    let mut spaced = String::with_capacity(value.len() + 4);
    for (index, character) in value.chars().enumerate() {
        if index > 0 && character.is_ascii_uppercase() {
            spaced.push(' ');
        }
        spaced.push(character);
    }
    spaced.replace('_', " ").to_ascii_lowercase()
}

/// Converts the ag-psd color-mode enum into the probe's stable string form.
fn color_mode_name(value: ColorMode) -> String {
    normalize_name(&format!("{value:?}"))
}

/// Normalizes an Aseprite-compatible PSD blend mode name.
fn blend_mode_name(value: BlendMode) -> String {
    normalize_name(&format!("{value:?}"))
}

#[derive(Debug, Serialize)]
struct ProbeSnapshot {
    schema_version: u32,
    source: SourceSnapshot,
    document: DocumentSnapshot,
    layers: Vec<LayerSnapshot>,
    animation: AnimationSummary,
}

#[derive(Debug, Serialize)]
struct SourceSnapshot {
    byte_length: usize,
    sha256: String,
}

#[derive(Debug, Serialize)]
struct DocumentSnapshot {
    width: f64,
    height: f64,
    channels: Option<f64>,
    bits_per_channel: Option<f64>,
    color_mode: Option<String>,
    root_layer_count: usize,
    group_count: usize,
    pixel_layer_count: usize,
}

#[derive(Debug, Serialize)]
struct LayerSnapshot {
    path: String,
    id: Option<f64>,
    kind: String,
    name: String,
    top: Option<f64>,
    left: Option<f64>,
    bottom: Option<f64>,
    right: Option<f64>,
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
    byte_length: usize,
    sha256: String,
}

#[derive(Debug, Serialize)]
struct AnimationSummary {
    #[serde(default)]
    resource_ids: Vec<u16>,
    #[serde(default)]
    frames: Vec<AnimationFrameSnapshot>,
    #[serde(default)]
    loop_mode: Option<String>,
    #[serde(default)]
    active_frame: Option<u32>,
    #[serde(default)]
    layer_states: Vec<LayerAnimationSnapshot>,
    #[serde(default)]
    visible_pixel_layers: Vec<VisibleFrameLayersSnapshot>,
    #[serde(default)]
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

#[derive(Debug, Serialize)]
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

/// Converts the core animation model into the stable probe JSON shape.
fn animation_snapshot(animation: &PhotoshopAnimation) -> AnimationSummary {
    AnimationSummary {
        resource_ids: animation.resource_ids.clone(),
        frames: animation
            .frames
            .iter()
            .map(|frame| AnimationFrameSnapshot {
                id: frame.id,
                duration_ms: frame.duration_ms,
                dispose: frame.dispose.as_deref().map(dispose_name),
            })
            .collect(),
        loop_mode: animation.loop_mode.as_ref().map(loop_mode_name),
        active_frame: animation.active_frame_index,
        layer_states: animation
            .layer_states
            .iter()
            .map(layer_animation_snapshot)
            .collect(),
        visible_pixel_layers: animation
            .visible_pixel_layers
            .iter()
            .map(|frame| VisibleFrameLayersSnapshot {
                frame_id: frame.frame_id,
                layer_ids: frame.layer_ids.clone(),
            })
            .collect(),
        frame_flags: animation.frame_flags.as_ref().map(animation_flags_snapshot),
    }
}

/// Converts one normalized layer animation state into probe JSON.
fn layer_animation_snapshot(state: &LayerAnimationState) -> LayerAnimationSnapshot {
    LayerAnimationSnapshot {
        layer_id: state.layer_id,
        path: state.path.clone(),
        frames: state.frames.iter().map(layer_frame_snapshot).collect(),
    }
}

/// Converts one normalized frame state into probe JSON.
fn layer_frame_snapshot(state: &LayerFrameState) -> LayerFrameSnapshot {
    LayerFrameSnapshot {
        frame_id: state.frame_id,
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
fn loop_mode_name(value: &LoopMode) -> String {
    match value {
        LoopMode::Infinite => "infinite".to_string(),
        LoopMode::Finite(count) => format!("finite:{count}"),
    }
}

/// Converts the descriptor's enum spelling into the oracle's disposal name.
fn dispose_name(value: &str) -> String {
    value
        .rsplit('.')
        .next()
        .unwrap_or(value)
        .to_ascii_lowercase()
}

/// Converts normalized mdyn flags into probe JSON.
fn animation_flags_snapshot(value: &AnimationFlags) -> AnimationFlagsSnapshot {
    AnimationFlagsSnapshot {
        propagate_frame_one: value.propagate_frame_one,
        unify_layer_position: value.unify_layer_position,
        unify_layer_style: value.unify_layer_style,
        unify_layer_visibility: value.unify_layer_visibility,
    }
}
