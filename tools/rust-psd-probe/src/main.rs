use std::env;
use std::fs;
use std::path::PathBuf;
use std::process::ExitCode;

use ag_psd::psd::{AnimationDispose, BlendMode, ColorMode, Layer};
use serde::Serialize;
use sha2::{Digest, Sha256};

const DEFAULT_INPUT: &str = r"path\to\fixture.psd";
const SCHEMA_VERSION: u32 = 1;

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
    let root_layers = psd.children.as_deref().unwrap_or_default();
    for (index, layer) in root_layers.iter().enumerate() {
        collect_layer(layer, &[index.to_string()], &mut layers)?;
    }

    let animations = psd
        .image_resources
        .as_ref()
        .and_then(|resources| resources.animations.as_ref())
        .map(|value| AnimationSnapshot {
            frames: value
                .frames
                .iter()
                .map(|frame| AnimationFrameSnapshot {
                    id: frame.id,
                    delay: frame.delay,
                    dispose: frame.dispose.map(animation_dispose_name),
                })
                .collect(),
            animation_sets: value
                .animations
                .iter()
                .map(|animation| AnimationSetSnapshot {
                    id: animation.id,
                    frames: animation.frames.clone(),
                    repeats: animation.repeats,
                    active_frame: animation.active_frame,
                })
                .collect(),
        });

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
        animation: AnimationSummary {
            resource_4000_exposed: animations.is_some(),
            animations,
            timeline_information_exposed: psd
                .image_resources
                .as_ref()
                .and_then(|resources| resources.timeline_information.as_ref())
                .is_some(),
        },
    })
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
        animation_frame_count: layer
            .additional_info
            .animation_frames
            .as_ref()
            .map(Vec::len),
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

/// Normalizes a Photoshop animation disposal enum.
fn animation_dispose_name(value: AnimationDispose) -> String {
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
    resource_4000_exposed: bool,
    animations: Option<AnimationSnapshot>,
    timeline_information_exposed: bool,
}

#[derive(Debug, Serialize)]
struct AnimationSnapshot {
    frames: Vec<AnimationFrameSnapshot>,
    animation_sets: Vec<AnimationSetSnapshot>,
}

#[derive(Debug, Serialize)]
struct AnimationFrameSnapshot {
    id: f64,
    delay: f64,
    dispose: Option<String>,
}

#[derive(Debug, Serialize)]
struct AnimationSetSnapshot {
    id: f64,
    frames: Vec<f64>,
    repeats: Option<f64>,
    active_frame: Option<f64>,
}
