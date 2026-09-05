use std::env;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use ag_psd::psd::{Compression, Layer, Psd, ReadOptions, WriteOptions};

/// Produces the Step 8A lossless-metadata Photoshop round-trip candidate.
fn main() -> ExitCode {
    match run(env::args().skip(1).collect()) {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("error: {error}");
            ExitCode::FAILURE
        }
    }
}

/// Reads the Photoshop original, writes RLE, and verifies retained metadata records.
fn run(arguments: Vec<String>) -> Result<(), String> {
    let input = argument_value(&arguments, "--input")
        .ok_or_else(|| "missing --input Photoshop PSD".to_string())?;
    let output =
        argument_value(&arguments, "--output").ok_or_else(|| "missing --output PSD".to_string())?;
    let reencode_pixels = arguments
        .iter()
        .any(|argument| argument == "--reencode-pixels");
    let source_bytes = fs::read(&input).map_err(|error| format!("{}: {error}", input.display()))?;
    let mut source = read_document(&source_bytes, &input, !reencode_pixels)?;
    reject_private_metadata(&source)?;
    if reencode_pixels {
        clear_raw_layer_channels(&mut source);
    }

    let encoded = ag_psd::write_psd(
        &source,
        &WriteOptions {
            compression: reencode_pixels.then_some(Compression::RleCompressed),
            ..Default::default()
        },
    );
    let readback = read_document(&encoded, &output, true)?;
    verify_preserved_metadata(&source, &readback)?;
    reject_private_metadata(&readback)?;

    if let Some(parent) = output.parent() {
        fs::create_dir_all(parent).map_err(|error| error.to_string())?;
    }
    fs::write(&output, encoded).map_err(|error| format!("{}: {error}", output.display()))?;
    println!("wrote {}", output.display());
    Ok(())
}

/// Clears retained layer-channel payloads so decoded pixels are encoded again.
fn clear_raw_layer_channels(document: &mut Psd) {
    fn visit(layers: &mut [Layer]) {
        for layer in layers {
            layer.raw_data = None;
            if let Some(children) = layer.children.as_mut() {
                visit(children);
            }
        }
    }
    if let Some(children) = document.children.as_mut() {
        visit(children);
    }
}

/// Returns the path following a two-token command-line option.
fn argument_value(arguments: &[String], option: &str) -> Option<PathBuf> {
    arguments
        .iter()
        .position(|argument| argument == option)
        .and_then(|index| arguments.get(index + 1))
        .map(PathBuf::from)
}

/// Reads a PSD without skipping image resources, layer data, or additional-info.
fn read_document(bytes: &[u8], path: &Path, preserve_channel_bytes: bool) -> Result<Psd, String> {
    ag_psd::read_psd(
        bytes,
        &ReadOptions {
            use_raw_data: preserve_channel_bytes.then_some(true),
            ..Default::default()
        },
    )
    .map_err(|error| format!("{}: {error}", path.display()))
}

/// Rejects any converter-owned round-trip records in this Photoshop baseline.
fn reject_private_metadata(document: &Psd) -> Result<(), String> {
    fn visit(layers: &[Layer]) -> bool {
        layers.iter().any(|layer| {
            layer
                .additional_info
                .additional_info_records
                .iter()
                .any(|record| record.key == *b"p2rt")
                || layer
                    .bounding_divider_additional_info
                    .as_ref()
                    .is_some_and(|info| {
                        info.additional_info_records
                            .iter()
                            .any(|record| record.key == *b"p2rt")
                    })
                || layer.children.as_deref().is_some_and(visit)
        })
    }
    if document.children.as_deref().is_some_and(visit) {
        Err("Photoshop baseline unexpectedly contains private p2rt metadata".to_string())
    } else {
        Ok(())
    }
}

/// Verifies byte-identical preserved metadata after writing and reading the candidate.
fn verify_preserved_metadata(source: &Psd, readback: &Psd) -> Result<(), String> {
    let source_resources = source
        .image_resources
        .as_ref()
        .map(|resources| &resources.resource_records[..])
        .unwrap_or_default();
    let readback_resources = readback
        .image_resources
        .as_ref()
        .map(|resources| &resources.resource_records[..])
        .unwrap_or_default();
    if source_resources != readback_resources {
        return Err("image-resource records changed during Step 8A round-trip".to_string());
    }
    if source.additional_info.additional_info_records
        != readback.additional_info.additional_info_records
    {
        return Err("document additional-info changed during Step 8A round-trip".to_string());
    }
    compare_layers(
        source.children.as_deref().unwrap_or_default(),
        readback.children.as_deref().unwrap_or_default(),
        "root",
    )
}

/// Compares ordered layer and bounding-divider metadata throughout the tree.
fn compare_layers(source: &[Layer], readback: &[Layer], path: &str) -> Result<(), String> {
    if source.len() != readback.len() {
        return Err(format!("layer count changed at {path}"));
    }
    for (index, (source, readback)) in source.iter().zip(readback).enumerate() {
        let child_path = format!("{path}/{index}");
        if source.additional_info.additional_info_records
            != readback.additional_info.additional_info_records
        {
            return Err(format!("layer additional-info changed at {child_path}"));
        }
        let source_divider = source
            .bounding_divider_additional_info
            .as_ref()
            .map(|info| &info.additional_info_records);
        let readback_divider = readback
            .bounding_divider_additional_info
            .as_ref()
            .map(|info| &info.additional_info_records);
        if source_divider != readback_divider {
            return Err(format!("bounding-divider metadata changed at {child_path}"));
        }
        compare_layers(
            source.children.as_deref().unwrap_or_default(),
            readback.children.as_deref().unwrap_or_default(),
            &child_path,
        )?;
    }
    Ok(())
}
