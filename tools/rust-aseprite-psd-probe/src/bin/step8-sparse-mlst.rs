use std::env;
use std::fs;
use std::path::PathBuf;
use std::process::ExitCode;

use ag_psd::descriptor::{
    Descriptor, DescriptorValue, read_version_and_descriptor, write_version_and_descriptor,
};
use ag_psd::psd::{AdditionalInfoRecord, Layer, Psd, ReadOptions, WriteOptions};
use ag_psd::reader::PsdReader;
use ag_psd::writer::{
    create_writer_default, get_writer_buffer, write_bytes, write_section, write_uint8, write_uint32,
};

/// Produces Step 8C by sparsifying only `mlst.enab` state transitions.
fn main() -> ExitCode {
    match run(env::args().skip(1).collect()) {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("error: {error}");
            ExitCode::FAILURE
        }
    }
}

/// Reads the generated-topology baseline and writes its sparse-state counterpart.
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
    let changed = sparsify_document(&mut document)?;
    if changed == 0 {
        return Err("no mlst records were changed".to_string());
    }
    let encoded = ag_psd::write_psd(&document, &WriteOptions::default());
    if let Some(parent) = output.parent() {
        fs::create_dir_all(parent).map_err(|error| error.to_string())?;
    }
    fs::write(&output, encoded).map_err(|error| error.to_string())?;
    println!("wrote {} ({changed} sparse mlst records)", output.display());
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

/// Rewrites every ordinary-layer and divider `shmd/mlst` record.
fn sparsify_document(document: &mut Psd) -> Result<usize, String> {
    fn visit(layers: &mut [Layer], changed: &mut usize) -> Result<(), String> {
        for layer in layers {
            *changed += sparsify_records(&mut layer.additional_info.additional_info_records)?;
            if let Some(divider) = layer.bounding_divider_additional_info.as_mut() {
                *changed += sparsify_records(&mut divider.additional_info_records)?;
            }
            if let Some(children) = layer.children.as_mut() {
                visit(children, changed)?;
            }
        }
        Ok(())
    }
    let mut changed = 0;
    if let Some(children) = document.children.as_mut() {
        visit(children, &mut changed)?;
    }
    Ok(changed)
}

/// Rewrites `shmd` payloads in one ordered additional-info directory.
fn sparsify_records(records: &mut [AdditionalInfoRecord]) -> Result<usize, String> {
    let mut changed = 0;
    for record in records.iter_mut().filter(|record| record.key == *b"shmd") {
        let (payload, did_change) = sparsify_shmd(&record.payload)?;
        if did_change {
            record.payload = payload;
            record.payload_padding = vec![0; record.payload.len() & 1];
            changed += 1;
        }
    }
    Ok(changed)
}

/// Preserves the `shmd` record directory while rewriting its `mlst` descriptor.
fn sparsify_shmd(payload: &[u8]) -> Result<(Vec<u8>, bool), String> {
    let count = read_u32(payload, 0)? as usize;
    let mut cursor = 4;
    let mut output = create_writer_default();
    write_uint32(&mut output, count as u32);
    let mut changed = false;
    for _ in 0..count {
        let signature = take(payload, &mut cursor, 4)?;
        let key = take(payload, &mut cursor, 4)?;
        let copy = *take(payload, &mut cursor, 1)?.first().unwrap();
        let reserved = take(payload, &mut cursor, 3)?;
        let length = read_u32(payload, cursor)? as usize;
        cursor += 4;
        let data = take(payload, &mut cursor, length)?;
        if length & 1 != 0 {
            cursor += 1;
        }
        write_bytes(&mut output, Some(signature));
        write_bytes(&mut output, Some(key));
        write_uint8(&mut output, copy);
        write_bytes(&mut output, Some(reserved));
        if key == b"mlst" {
            let mut reader = PsdReader::new(data, None, None);
            let mut descriptor =
                read_version_and_descriptor(&mut reader).map_err(|error| error.to_string())?;
            changed |= sparsify_descriptor(&mut descriptor)?;
            write_section(
                &mut output,
                2,
                |writer| write_version_and_descriptor(writer, &descriptor),
                false,
                false,
            );
        } else {
            write_section(
                &mut output,
                2,
                |writer| write_bytes(writer, Some(data)),
                false,
                false,
            );
        }
    }
    if cursor != payload.len() {
        return Err("shmd payload has unparsed trailing bytes".to_string());
    }
    Ok((get_writer_buffer(&output), changed))
}

/// Omits unchanged `enab` values after the first `LaSt` state.
fn sparsify_descriptor(descriptor: &mut Descriptor) -> Result<bool, String> {
    let states = descriptor
        .items
        .iter_mut()
        .find_map(|(key, value)| (key == "LaSt").then_some(value))
        .ok_or_else(|| "mlst descriptor has no LaSt".to_string())?;
    let DescriptorValue::List(states) = states else {
        return Err("mlst LaSt is not a list".to_string());
    };
    let mut previous = None;
    let mut changed = false;
    for (index, state) in states.iter_mut().enumerate() {
        let DescriptorValue::Descriptor(state) = state else {
            return Err("mlst LaSt contains a non-descriptor".to_string());
        };
        let current = state.items.iter().find_map(|(key, value)| {
            if key == "enab" {
                match value {
                    DescriptorValue::Boolean(value) => Some(*value),
                    _ => None,
                }
            } else {
                None
            }
        });
        if index == 0 && current.is_none() {
            return Err("first mlst state has no explicit enab".to_string());
        }
        let effective = current.or(previous).unwrap_or(false);
        if index > 0 && previous == Some(effective) {
            let before = state.items.len();
            state.items.retain(|(key, _)| key != "enab");
            changed |= state.items.len() != before;
        }
        previous = Some(effective);
    }
    Ok(changed)
}

/// Reads one bounded big-endian integer.
fn read_u32(bytes: &[u8], offset: usize) -> Result<u32, String> {
    bytes
        .get(offset..offset + 4)
        .and_then(|value| value.try_into().ok())
        .map(u32::from_be_bytes)
        .ok_or_else(|| "truncated u32".to_string())
}

/// Takes one bounded byte range and advances its cursor.
fn take<'a>(bytes: &'a [u8], cursor: &mut usize, length: usize) -> Result<&'a [u8], String> {
    let result = bytes
        .get(*cursor..*cursor + length)
        .ok_or_else(|| "truncated shmd record".to_string())?;
    *cursor += length;
    Ok(result)
}
