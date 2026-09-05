use std::env;
use std::fs;
use std::path::PathBuf;
use std::process::ExitCode;

use ag_psd::psd::{PluginResource, ReadOptions, WriteOptions};

/// Produces controlled variants of the bytes following the `maniIRFR` section.
fn main() -> ExitCode {
    match run(env::args().skip(1).collect()) {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("error: {error}");
            ExitCode::FAILURE
        }
    }
}

/// Changes only the retained outer trailing bytes of one animation resource.
fn run(arguments: Vec<String>) -> Result<(), String> {
    let input =
        argument_value(&arguments, "--input").ok_or_else(|| "missing --input".to_string())?;
    let output =
        argument_value(&arguments, "--output").ok_or_else(|| "missing --output".to_string())?;
    let mode = argument_value(&arguments, "--mode")
        .and_then(|value| value.to_str().map(str::to_owned))
        .ok_or_else(|| "missing --mode remove|zero".to_string())?;
    let bytes = fs::read(&input).map_err(|error| error.to_string())?;
    let mut document = ag_psd::read_psd(
        &bytes,
        &ReadOptions {
            use_raw_data: Some(true),
            ..Default::default()
        },
    )
    .map_err(|error| error.to_string())?;
    let animation = document
        .image_resources
        .as_mut()
        .ok_or_else(|| "PSD has no image resources".to_string())?
        .resource_records
        .iter_mut()
        .find(|record| matches!(record.plugin, Some(PluginResource::ManiIrfr { .. })))
        .ok_or_else(|| "PSD has no maniIRFR resource".to_string())?;
    let outer_length = read_u32(&animation.payload, 8)? as usize;
    let outer_end = 12usize
        .checked_add(outer_length)
        .ok_or_else(|| "maniIRFR section length overflow".to_string())?;
    let trailing = animation
        .payload
        .get_mut(outer_end..)
        .ok_or_else(|| "truncated maniIRFR section".to_string())?;
    if trailing.is_empty() {
        return Err("maniIRFR has no outer trailing bytes".to_string());
    }
    match mode.as_str() {
        "remove" => animation.payload.truncate(outer_end),
        "zero" => trailing.fill(0),
        _ => return Err("--mode must be remove or zero".to_string()),
    }

    let encoded = ag_psd::write_psd(&document, &WriteOptions::default());
    if let Some(parent) = output.parent() {
        fs::create_dir_all(parent).map_err(|error| error.to_string())?;
    }
    fs::write(&output, encoded).map_err(|error| error.to_string())?;
    println!("wrote {}", output.display());
    Ok(())
}

/// Returns the path following one command-line option.
fn argument_value(arguments: &[String], option: &str) -> Option<PathBuf> {
    arguments
        .iter()
        .position(|argument| argument == option)
        .and_then(|index| arguments.get(index + 1))
        .map(PathBuf::from)
}

/// Reads one bounded big-endian integer.
fn read_u32(bytes: &[u8], offset: usize) -> Result<u32, String> {
    bytes
        .get(offset..offset + 4)
        .and_then(|value| value.try_into().ok())
        .map(u32::from_be_bytes)
        .ok_or_else(|| "truncated u32".to_string())
}
