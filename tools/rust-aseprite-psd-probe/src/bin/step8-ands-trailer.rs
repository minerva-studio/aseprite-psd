use std::env;
use std::fs;
use std::path::PathBuf;
use std::process::ExitCode;

use ag_psd::psd::{PluginResource, ReadOptions, WriteOptions};

/// Produces Step 8G by adding two zero bytes to the `AnDs` payload.
fn main() -> ExitCode {
    match run(env::args().skip(1).collect()) {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("error: {error}");
            ExitCode::FAILURE
        }
    }
}

/// Changes only the declared `AnDs` child payload and enclosing section length.
fn run(arguments: Vec<String>) -> Result<(), String> {
    let input =
        argument_value(&arguments, "--input").ok_or_else(|| "missing --input".to_string())?;
    let output =
        argument_value(&arguments, "--output").ok_or_else(|| "missing --output".to_string())?;
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
    animation.payload = add_ands_trailer(&animation.payload)?;

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

/// Appends two zero bytes inside the declared `AnDs` child payload.
fn add_ands_trailer(payload: &[u8]) -> Result<Vec<u8>, String> {
    if payload.get(0..8) != Some(&b"maniIRFR"[..]) || payload.len() < 12 {
        return Err("animation resource is not maniIRFR".to_string());
    }
    let outer_length = read_u32(payload, 8)? as usize;
    let outer_end = 12usize
        .checked_add(outer_length)
        .ok_or_else(|| "maniIRFR section length overflow".to_string())?;
    if outer_end > payload.len() {
        return Err("truncated maniIRFR section".to_string());
    }
    let mut cursor = 12;
    while cursor < outer_end {
        let key = payload
            .get(cursor + 4..cursor + 8)
            .ok_or_else(|| "truncated child key".to_string())?;
        let length = read_u32(payload, cursor + 8)? as usize;
        let data_start = cursor + 12;
        let data_end = data_start
            .checked_add(length)
            .ok_or_else(|| "child length overflow".to_string())?;
        if data_end > outer_end {
            return Err("truncated child payload".to_string());
        }
        if key == b"AnDs" {
            let new_child_length = length
                .checked_add(2)
                .ok_or_else(|| "AnDs length overflow".to_string())?;
            let new_outer_length = outer_length
                .checked_add(2)
                .ok_or_else(|| "maniIRFR length overflow".to_string())?;
            let mut result = Vec::with_capacity(payload.len() + 2);
            result.extend_from_slice(&payload[..8]);
            result.extend_from_slice(
                &u32::try_from(new_outer_length)
                    .map_err(|_| "maniIRFR exceeds 4 GiB".to_string())?
                    .to_be_bytes(),
            );
            result.extend_from_slice(&payload[12..cursor + 8]);
            result.extend_from_slice(
                &u32::try_from(new_child_length)
                    .map_err(|_| "AnDs exceeds 4 GiB".to_string())?
                    .to_be_bytes(),
            );
            result.extend_from_slice(&payload[data_start..data_end]);
            result.extend_from_slice(&[0, 0]);
            result.extend_from_slice(&payload[data_end..]);
            return Ok(result);
        }
        cursor = data_end + (length & 1);
    }
    Err("maniIRFR has no AnDs child".to_string())
}

/// Reads one bounded big-endian integer.
fn read_u32(bytes: &[u8], offset: usize) -> Result<u32, String> {
    bytes
        .get(offset..offset + 4)
        .and_then(|value| value.try_into().ok())
        .map(u32::from_be_bytes)
        .ok_or_else(|| "truncated u32".to_string())
}
