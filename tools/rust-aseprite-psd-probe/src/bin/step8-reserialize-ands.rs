use std::env;
use std::fs;
use std::path::PathBuf;
use std::process::ExitCode;

use ag_psd::descriptor::{read_version_and_descriptor, write_version_and_descriptor};
use ag_psd::psd::{PluginResource, ReadOptions, WriteOptions};
use ag_psd::reader::PsdReader;
use ag_psd::writer::{
    create_writer_default, get_writer_buffer, write_bytes, write_section, write_signature,
};

/// Produces Step 8D by reserializing only the retained `AnDs` descriptor.
fn main() -> ExitCode {
    match run(env::args().skip(1).collect()) {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("error: {error}");
            ExitCode::FAILURE
        }
    }
}

/// Preserves the input PSD while replacing the wire encoding of `AnDs`.
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
    let records = &mut document
        .image_resources
        .as_mut()
        .ok_or_else(|| "PSD has no image resources".to_string())?
        .resource_records;
    let animation = records
        .iter_mut()
        .find(|record| matches!(record.plugin, Some(PluginResource::ManiIrfr { .. })))
        .ok_or_else(|| "PSD has no maniIRFR resource".to_string())?;
    let original_payload = animation.payload.clone();
    animation.payload = reserialize_ands(&original_payload)?;
    if animation.payload == original_payload {
        return Err("AnDs reserialization produced byte-identical payload".to_string());
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

/// Re-encodes `AnDs` and retains every other `maniIRFR` child verbatim.
fn reserialize_ands(payload: &[u8]) -> Result<Vec<u8>, String> {
    if payload.get(0..8) != Some(&b"maniIRFR"[..]) || payload.len() < 12 {
        return Err("animation resource is not maniIRFR".to_string());
    }
    let outer_length = read_u32(payload, 8)? as usize;
    let outer_end = 12usize
        .checked_add(outer_length)
        .ok_or_else(|| "maniIRFR section length overflow".to_string())?;
    let outer = payload
        .get(12..outer_end)
        .ok_or_else(|| "truncated maniIRFR section".to_string())?;
    let trailing = payload
        .get(outer_end..)
        .ok_or_else(|| "truncated maniIRFR trailing data".to_string())?;
    let mut cursor = 0;
    let mut children = create_writer_default();
    let mut rewritten = false;
    while cursor < outer.len() {
        let signature = outer
            .get(cursor..cursor + 4)
            .ok_or_else(|| "truncated child signature".to_string())?;
        let key = outer
            .get(cursor + 4..cursor + 8)
            .ok_or_else(|| "truncated child key".to_string())?;
        let length = read_u32(outer, cursor + 8)? as usize;
        let data_start = cursor + 12;
        let data = outer
            .get(data_start..data_start + length)
            .ok_or_else(|| "truncated child payload".to_string())?;
        write_bytes(&mut children, Some(signature));
        write_bytes(&mut children, Some(key));
        if key == b"AnDs" {
            if rewritten {
                return Err("maniIRFR has multiple AnDs children".to_string());
            }
            let mut reader = PsdReader::new(data, None, None);
            let descriptor =
                read_version_and_descriptor(&mut reader).map_err(|error| error.to_string())?;
            write_section(
                &mut children,
                1,
                |writer| write_version_and_descriptor(writer, &descriptor),
                false,
                false,
            );
            rewritten = true;
        } else {
            write_section(
                &mut children,
                1,
                |writer| write_bytes(writer, Some(data)),
                false,
                false,
            );
        }
        cursor = data_start + length + (length & 1);
    }
    if !rewritten {
        return Err("maniIRFR has no AnDs child".to_string());
    }
    let children = get_writer_buffer(&children);
    let mut result = create_writer_default();
    write_signature(&mut result, "mani");
    write_signature(&mut result, "IRFR");
    write_section(
        &mut result,
        1,
        |writer| write_bytes(writer, Some(&children)),
        false,
        false,
    );
    write_bytes(&mut result, Some(trailing));
    Ok(get_writer_buffer(&result))
}

/// Reads one bounded big-endian integer.
fn read_u32(bytes: &[u8], offset: usize) -> Result<u32, String> {
    bytes
        .get(offset..offset + 4)
        .and_then(|value| value.try_into().ok())
        .map(u32::from_be_bytes)
        .ok_or_else(|| "truncated u32".to_string())
}
