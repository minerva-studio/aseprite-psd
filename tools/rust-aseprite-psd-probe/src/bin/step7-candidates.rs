use std::env;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use ag_psd::descriptor::{
    DescriptorValue, read_version_and_descriptor, write_version_and_descriptor,
};
use ag_psd::psd::{
    AdditionalInfoRecord, Compression, Layer, LayerAdditionalInfo, PluginResource, Psd,
    ReadOptions, WriteOptions,
};
use ag_psd::reader::PsdReader;
use ag_psd::writer::{
    create_writer_default, get_writer_buffer, write_bytes, write_section, write_signature,
};

/// Generates the cumulative Step 7 Photoshop timeline candidates.
fn main() -> ExitCode {
    match run(env::args().skip(1).collect()) {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("error: {error}");
            ExitCode::FAILURE
        }
    }
}

/// Reads the rejected Step 5 candidate and emits controlled RLE variants.
fn run(arguments: Vec<String>) -> Result<(), String> {
    let base = argument_value(&arguments, "--base")
        .ok_or_else(|| "missing --base Step 5 PSD".to_string())?;
    let original = argument_value(&arguments, "--original")
        .ok_or_else(|| "missing --original Photoshop PSD".to_string())?;
    let output = argument_value(&arguments, "--output")
        .ok_or_else(|| "missing --output directory".to_string())?;
    fs::create_dir_all(&output).map_err(|error| error.to_string())?;

    let mut candidate = read_document(&base)?;
    let photoshop = read_document(&original)?;
    add_divider_timeline_metadata(&mut candidate)?;
    write_candidate(
        &output.join("后撤步-7a-divider-metadata-rle.psd"),
        &candidate,
    )?;

    add_frame_global_angle(&mut candidate, 90.0)?;
    write_candidate(&output.join("后撤步-7b-frame-angle-rle.psd"), &candidate)?;

    copy_document_records(&photoshop, &mut candidate, [*b"Patt", *b"FMsk"])?;
    write_candidate(&output.join("后撤步-7c-document-info-rle.psd"), &candidate)?;

    remove_private_roundtrip_metadata(&mut candidate);
    write_candidate(
        &output.join("后撤步-7d-no-private-metadata-rle.psd"),
        &candidate,
    )?;
    Ok(())
}

/// Returns the path following a two-token command-line option.
fn argument_value(arguments: &[String], option: &str) -> Option<PathBuf> {
    arguments
        .iter()
        .position(|argument| argument == option)
        .and_then(|index| arguments.get(index + 1))
        .map(PathBuf::from)
}

/// Reads a PSD while retaining all layers and raw metadata records.
fn read_document(path: &Path) -> Result<Psd, String> {
    let bytes = fs::read(path).map_err(|error| format!("{}: {error}", path.display()))?;
    ag_psd::read_psd(&bytes, &ReadOptions::default())
        .map_err(|error| format!("{}: {error}", path.display()))
}

/// Writes one diagnostic candidate with Photoshop-targeted RLE compression.
fn write_candidate(path: &Path, document: &Psd) -> Result<(), String> {
    let bytes = ag_psd::write_psd(
        document,
        &WriteOptions {
            compression: Some(Compression::RleCompressed),
            ..Default::default()
        },
    );
    fs::write(path, bytes).map_err(|error| format!("{}: {error}", path.display()))?;
    println!("wrote {}", path.display());
    Ok(())
}

/// Adds independent IDs and timeline state to every physical group divider.
fn add_divider_timeline_metadata(document: &mut Psd) -> Result<(), String> {
    let mut next_id = maximum_used_id(document).saturating_add(1);
    let children = document
        .children
        .as_mut()
        .ok_or_else(|| "base PSD has no layers".to_string())?;
    add_divider_metadata_to_layers(children, &mut next_id)?;
    if let Some(resources) = document.image_resources.as_mut() {
        resources.ids_seed_number = Some(f64::from(next_id.saturating_sub(1)));
    }
    Ok(())
}

/// Recursively mirrors each group's own `shmd` state onto its divider record.
fn add_divider_metadata_to_layers(layers: &mut [Layer], next_id: &mut u32) -> Result<(), String> {
    for layer in layers {
        if let Some(children) = layer.children.as_mut() {
            add_divider_metadata_to_layers(children, next_id)?;
            let group_id = layer
                .additional_info
                .id
                .map(|value| value as u32)
                .ok_or_else(|| "group layer has no lyid".to_string())?;
            let shmd = layer
                .additional_info
                .additional_info_records
                .iter()
                .find(|record| record.key == *b"shmd")
                .ok_or_else(|| format!("group layer {group_id} has no shmd"))?;
            let divider_id = *next_id;
            *next_id = next_id.saturating_add(1);
            let divider_shmd = replace_mlst_layer_id(&shmd.payload, group_id, divider_id)?;
            layer.bounding_divider_additional_info = Some(LayerAdditionalInfo {
                additional_info_records: vec![
                    AdditionalInfoRecord::new(
                        *b"8BIM",
                        *b"lyid",
                        divider_id.to_be_bytes().to_vec(),
                    ),
                    AdditionalInfoRecord::new(*b"8BIM", *b"lsct", 3_u32.to_be_bytes().to_vec()),
                    AdditionalInfoRecord::new(*b"8BIM", *b"shmd", divider_shmd),
                ],
                id: Some(f64::from(divider_id)),
                ..Default::default()
            });
        }
    }
    Ok(())
}

/// Rewrites the one `LaID/long` value in an `mlst` metadata payload.
fn replace_mlst_layer_id(payload: &[u8], old_id: u32, new_id: u32) -> Result<Vec<u8>, String> {
    let mut result = payload.to_vec();
    let mut needle = Vec::from(&b"LaIDlong"[..]);
    needle.extend_from_slice(&old_id.to_be_bytes());
    let matches = result
        .windows(needle.len())
        .enumerate()
        .filter_map(|(index, bytes)| (bytes == needle).then_some(index))
        .collect::<Vec<_>>();
    if matches.len() != 1 {
        return Err(format!(
            "expected one mlst LaID for layer {old_id}, found {}",
            matches.len()
        ));
    }
    let value_start = matches[0] + 8;
    result[value_start..value_start + 4].copy_from_slice(&new_id.to_be_bytes());
    Ok(result)
}

/// Returns the greatest layer, divider, or animation frame ID currently used.
fn maximum_used_id(document: &Psd) -> u32 {
    fn visit(layers: &[Layer], maximum: &mut u32) {
        for layer in layers {
            if let Some(id) = layer.additional_info.id {
                *maximum = (*maximum).max(id as u32);
            }
            if let Some(children) = layer.children.as_deref() {
                visit(children, maximum);
            }
        }
    }
    let mut maximum = 0;
    if let Some(children) = document.children.as_deref() {
        visit(children, &mut maximum);
    }
    if let Some(animations) = document
        .image_resources
        .as_ref()
        .and_then(|resources| resources.animations.as_ref())
    {
        for frame in &animations.frames {
            maximum = maximum.max(frame.id as u32);
        }
    }
    maximum
}

/// Adds Photoshop's observed per-frame `FrGA` field to the retained `AnDs` descriptor.
fn add_frame_global_angle(document: &mut Psd, angle: f64) -> Result<(), String> {
    let records = &mut document
        .image_resources
        .as_mut()
        .ok_or_else(|| "base PSD has no image resources".to_string())?
        .resource_records;
    let animation = records
        .iter_mut()
        .find(|record| matches!(record.plugin, Some(PluginResource::ManiIrfr { .. })))
        .ok_or_else(|| "base PSD has no maniIRFR resource".to_string())?;
    animation.payload = rewrite_ands(&animation.payload, |descriptor| {
        let frames = descriptor
            .items
            .iter_mut()
            .find_map(|(key, value)| (key == "FrIn").then_some(value))
            .ok_or_else(|| "AnDs has no FrIn".to_string())?;
        let DescriptorValue::List(frames) = frames else {
            return Err("AnDs FrIn is not a list".to_string());
        };
        for frame in frames {
            let DescriptorValue::Descriptor(frame) = frame else {
                return Err("AnDs FrIn contains a non-descriptor".to_string());
            };
            if frame.get("FrGA").is_none() {
                frame.set("FrGA", DescriptorValue::Double(angle));
            }
        }
        Ok(())
    })?;
    Ok(())
}

/// Parses and rewrites only the `AnDs` child of a `maniIRFR` payload.
fn rewrite_ands(
    payload: &[u8],
    mut mutate: impl FnMut(&mut ag_psd::descriptor::Descriptor) -> Result<(), String>,
) -> Result<Vec<u8>, String> {
    if payload.get(0..8) != Some(&b"maniIRFR"[..]) || payload.len() < 12 {
        return Err("animation resource is not maniIRFR".to_string());
    }
    let outer_length = read_u32(payload, 8)? as usize;
    let outer = payload
        .get(12..12 + outer_length)
        .ok_or_else(|| "truncated mani section".to_string())?;
    let mut cursor = 0;
    let mut output = create_writer_default();
    let mut rewritten = false;
    while cursor < outer.len() {
        let signature = outer
            .get(cursor..cursor + 4)
            .ok_or_else(|| "truncated mani child signature".to_string())?;
        let key = outer
            .get(cursor + 4..cursor + 8)
            .ok_or_else(|| "truncated mani child key".to_string())?;
        let length = read_u32(outer, cursor + 8)? as usize;
        let data_start = cursor + 12;
        let data = outer
            .get(data_start..data_start + length)
            .ok_or_else(|| "truncated mani child".to_string())?;
        write_bytes(&mut output, Some(signature));
        write_bytes(&mut output, Some(key));
        if key == b"AnDs" {
            if rewritten {
                return Err("maniIRFR has multiple AnDs children".to_string());
            }
            let mut reader = PsdReader::new(data, None, None);
            let mut descriptor =
                read_version_and_descriptor(&mut reader).map_err(|error| error.to_string())?;
            mutate(&mut descriptor)?;
            rewritten = true;
            write_section(
                &mut output,
                1,
                |writer| write_version_and_descriptor(writer, &descriptor),
                false,
                false,
            );
        } else {
            write_section(
                &mut output,
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
    let children = get_writer_buffer(&output);
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
    Ok(get_writer_buffer(&result))
}

/// Copies selected document-level records from the Photoshop source in source order.
fn copy_document_records(source: &Psd, target: &mut Psd, keys: [[u8; 4]; 2]) -> Result<(), String> {
    for key in keys {
        let record = source
            .additional_info
            .additional_info_records
            .iter()
            .find(|record| record.key == key)
            .ok_or_else(|| {
                format!(
                    "Photoshop source has no {} record",
                    String::from_utf8_lossy(&key)
                )
            })?;
        target
            .additional_info
            .additional_info_records
            .retain(|item| item.key != key);
        target
            .additional_info
            .additional_info_records
            .push(record.clone());
    }
    Ok(())
}

/// Removes converter-owned `p2rt` records while retaining Photoshop metadata.
fn remove_private_roundtrip_metadata(document: &mut Psd) {
    fn visit(layers: &mut [Layer]) {
        for layer in layers {
            layer
                .additional_info
                .additional_info_records
                .retain(|record| record.key != *b"p2rt");
            if let Some(divider) = layer.bounding_divider_additional_info.as_mut() {
                divider
                    .additional_info_records
                    .retain(|record| record.key != *b"p2rt");
            }
            if let Some(children) = layer.children.as_mut() {
                visit(children);
            }
        }
    }
    if let Some(children) = document.children.as_mut() {
        visit(children);
    }
}

/// Reads one big-endian 32-bit value from a bounded byte slice.
fn read_u32(bytes: &[u8], offset: usize) -> Result<u32, String> {
    bytes
        .get(offset..offset + 4)
        .and_then(|value| value.try_into().ok())
        .map(u32::from_be_bytes)
        .ok_or_else(|| "truncated u32".to_string())
}
