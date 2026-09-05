use std::env;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use ag_psd::psd::{
    AdditionalInfoRecord, Compression, Layer, PluginResource, Psd, ReadOptions, WriteOptions,
};

/// Generates the controlled Step 11 Photoshop timeline candidates.
fn main() -> ExitCode {
    match run(env::args().skip(1).collect()) {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("error: {error}");
            ExitCode::FAILURE
        }
    }
}

/// Derives all candidates from one metadata-free failing RLE baseline.
fn run(arguments: Vec<String>) -> Result<(), String> {
    let base =
        argument_value(&arguments, "--base").ok_or_else(|| "missing --base PSD".to_string())?;
    let output = argument_value(&arguments, "--output")
        .ok_or_else(|| "missing --output directory".to_string())?;
    fs::create_dir_all(&output).map_err(|error| error.to_string())?;

    let baseline = read_document(&base)?;
    ensure_no_private_metadata(&baseline)?;

    let control = write_document(&baseline);
    let original = fs::read(&base).map_err(|error| format!("{}: {error}", base.display()))?;
    if control != original {
        return Err(format!(
            "lossless control differs from the baseline ({} versus {} bytes)",
            control.len(),
            original.len()
        ));
    }

    let mut all_layer_mlst = baseline.clone();
    add_all_layer_mlst(&mut all_layer_mlst)?;
    write_candidate(&output.join("11a-all-layer-mlst-rle.psd"), &all_layer_mlst)?;

    let mut boundary = baseline.clone();
    extend_ands_tail(&mut boundary, 2)?;
    write_candidate(&output.join("11b-ands-boundary-rle.psd"), &boundary)?;

    let mut no_tail = baseline.clone();
    remove_ands_tail(&mut no_tail, 2)?;
    write_candidate(&output.join("11d-ands-no-tail-rle.psd"), &no_tail)?;

    if arguments
        .iter()
        .any(|argument| argument == "--include-combined")
    {
        extend_ands_tail(&mut all_layer_mlst, 2)?;
        write_candidate(
            &output.join("11c-all-layer-mlst-and-boundary-rle.psd"),
            &all_layer_mlst,
        )?;
    }
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

/// Reads a PSD while retaining its ordered resources and layer additional-info records.
fn read_document(path: &Path) -> Result<Psd, String> {
    let bytes = fs::read(path).map_err(|error| format!("{}: {error}", path.display()))?;
    ag_psd::read_psd(&bytes, &ReadOptions::default())
        .map_err(|error| format!("{}: {error}", path.display()))
}

/// Serializes the diagnostic document using the baseline's RLE contract.
fn write_document(document: &Psd) -> Vec<u8> {
    ag_psd::write_psd(
        document,
        &WriteOptions {
            compression: Some(Compression::RleCompressed),
            ..Default::default()
        },
    )
}

/// Writes one diagnostic candidate without changing its source document.
fn write_candidate(path: &Path, document: &Psd) -> Result<(), String> {
    let bytes = write_document(document);
    fs::write(path, bytes).map_err(|error| format!("{}: {error}", path.display()))?;
    println!("wrote {}", path.display());
    Ok(())
}

/// Rejects a baseline that would mix private converter metadata into the experiment.
fn ensure_no_private_metadata(document: &Psd) -> Result<(), String> {
    let mut pending = document
        .children
        .as_deref()
        .unwrap_or_default()
        .iter()
        .collect::<Vec<_>>();
    while let Some(layer) = pending.pop() {
        if layer
            .additional_info
            .additional_info_records
            .iter()
            .any(|record| record.key == *b"p2rt")
        {
            return Err("baseline contains private p2rt metadata".to_string());
        }
        pending.extend(layer.children.as_deref().unwrap_or_default());
    }
    Ok(())
}

/// Copies each frame group's complete state directory to every real descendant layer.
fn add_all_layer_mlst(document: &mut Psd) -> Result<(), String> {
    let roots = document
        .children
        .as_mut()
        .ok_or_else(|| "baseline has no frame groups".to_string())?;
    for root in roots {
        let root_id = layer_id(root)?;
        let template = shmd_record(root)?.clone();
        let children = root
            .children
            .as_mut()
            .ok_or_else(|| format!("frame group {root_id} has no children"))?;
        let mut pending = children.iter_mut().collect::<Vec<_>>();
        while let Some(layer) = pending.pop() {
            let id = layer_id(layer)?;
            if shmd_record(layer).is_err() {
                let payload = replace_mlst_layer_id(&template.payload, root_id, id)?;
                layer
                    .additional_info
                    .additional_info_records
                    .push(AdditionalInfoRecord::new(*b"8BIM", *b"shmd", payload));
            }
            pending.extend(layer.children.as_mut().into_iter().flatten());
        }
    }
    Ok(())
}

/// Returns one layer's integral PSD identifier.
fn layer_id(layer: &Layer) -> Result<u32, String> {
    let value = layer
        .additional_info
        .id
        .ok_or_else(|| "layer has no lyid".to_string())?;
    if value < 0.0 || value > f64::from(u32::MAX) || value.fract() != 0.0 {
        return Err(format!("invalid layer ID {value}"));
    }
    Ok(value as u32)
}

/// Returns a layer's preserved shmd record.
fn shmd_record(layer: &Layer) -> Result<&AdditionalInfoRecord, String> {
    layer
        .additional_info
        .additional_info_records
        .iter()
        .find(|record| record.key == *b"shmd")
        .ok_or_else(|| format!("layer {} has no shmd", layer_id(layer).unwrap_or_default()))
}

/// Rewrites the single mlst LaID field while preserving every other byte.
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

/// Adds bytes inside the declared AnDs payload and updates the enclosing mani length.
fn extend_ands_tail(document: &mut Psd, extra: usize) -> Result<(), String> {
    let animation = document
        .image_resources
        .as_mut()
        .ok_or_else(|| "baseline has no image resources".to_string())?
        .resource_records
        .iter_mut()
        .find(|record| matches!(record.plugin, Some(PluginResource::ManiIrfr { .. })))
        .ok_or_else(|| "baseline has no maniIRFR resource".to_string())?;
    animation.payload = append_ands_bytes(&animation.payload, extra)?;
    Ok(())
}

/// Appends zero bytes to the AnDs child without re-encoding its descriptor tree.
fn append_ands_bytes(payload: &[u8], extra: usize) -> Result<Vec<u8>, String> {
    if payload.get(0..8) != Some(&b"maniIRFR"[..]) || payload.len() < 12 {
        return Err("animation resource is not maniIRFR".to_string());
    }
    let outer_length = read_u32(payload, 8)? as usize;
    let outer_end = 12usize
        .checked_add(outer_length)
        .ok_or_else(|| "maniIRFR length overflow".to_string())?;
    if outer_end > payload.len() {
        return Err("truncated maniIRFR section".to_string());
    }
    let mut cursor = 12usize;
    while cursor < outer_end {
        let key = payload
            .get(cursor + 4..cursor + 8)
            .ok_or_else(|| "truncated animation child key".to_string())?;
        let length = read_u32(payload, cursor + 8)? as usize;
        let data_start = cursor
            .checked_add(12)
            .ok_or_else(|| "animation child offset overflow".to_string())?;
        let data_end = data_start
            .checked_add(length)
            .ok_or_else(|| "animation child length overflow".to_string())?;
        if data_end > outer_end {
            return Err("truncated animation child".to_string());
        }
        if key == b"AnDs" {
            let child_length = length
                .checked_add(extra)
                .ok_or_else(|| "AnDs length overflow".to_string())?;
            let new_outer_length = outer_length
                .checked_add(extra)
                .ok_or_else(|| "maniIRFR length overflow".to_string())?;
            let mut result = Vec::with_capacity(payload.len() + extra);
            result.extend_from_slice(&payload[..8]);
            result.extend_from_slice(
                &u32::try_from(new_outer_length)
                    .map_err(|_| "maniIRFR exceeds u32")?
                    .to_be_bytes(),
            );
            result.extend_from_slice(&payload[12..cursor + 8]);
            result.extend_from_slice(
                &u32::try_from(child_length)
                    .map_err(|_| "AnDs exceeds u32")?
                    .to_be_bytes(),
            );
            result.extend_from_slice(&payload[data_start..data_end]);
            result.resize(result.len() + extra, 0);
            result.extend_from_slice(&payload[data_end..]);
            return Ok(result);
        }
        cursor = data_end
            .checked_add(length & 1)
            .ok_or_else(|| "animation child padding overflow".to_string())?;
    }
    Err("maniIRFR has no AnDs child".to_string())
}

/// Removes only the existing AnDs tail while preserving its descriptor bytes.
fn remove_ands_tail(document: &mut Psd, remove: usize) -> Result<(), String> {
    let animation = document
        .image_resources
        .as_mut()
        .ok_or_else(|| "baseline has no image resources".to_string())?
        .resource_records
        .iter_mut()
        .find(|record| matches!(record.plugin, Some(PluginResource::ManiIrfr { .. })))
        .ok_or_else(|| "baseline has no maniIRFR resource".to_string())?;
    animation.payload = truncate_ands_payload(&animation.payload, remove)?;
    Ok(())
}

/// Truncates the final bytes of AnDs and updates both bounded lengths.
fn truncate_ands_payload(payload: &[u8], remove: usize) -> Result<Vec<u8>, String> {
    if payload.get(0..8) != Some(&b"maniIRFR"[..]) || payload.len() < 12 {
        return Err("animation resource is not maniIRFR".to_string());
    }
    let outer_length = read_u32(payload, 8)? as usize;
    let outer_end = 12usize
        .checked_add(outer_length)
        .ok_or_else(|| "maniIRFR length overflow".to_string())?;
    if outer_end > payload.len() {
        return Err("truncated maniIRFR section".to_string());
    }
    let mut cursor = 12usize;
    while cursor < outer_end {
        let key = payload
            .get(cursor + 4..cursor + 8)
            .ok_or_else(|| "truncated animation child key".to_string())?;
        let length = read_u32(payload, cursor + 8)? as usize;
        let data_start = cursor
            .checked_add(12)
            .ok_or_else(|| "animation child offset overflow".to_string())?;
        let data_end = data_start
            .checked_add(length)
            .ok_or_else(|| "animation child length overflow".to_string())?;
        if data_end > outer_end {
            return Err("truncated animation child".to_string());
        }
        if key == b"AnDs" {
            if remove > length {
                return Err("cannot remove more bytes than AnDs payload".to_string());
            }
            let child_length = length - remove;
            let new_outer_length = outer_length - remove;
            let mut result = Vec::with_capacity(payload.len() - remove);
            result.extend_from_slice(&payload[..8]);
            result.extend_from_slice(
                &u32::try_from(new_outer_length)
                    .map_err(|_| "maniIRFR exceeds u32")?
                    .to_be_bytes(),
            );
            result.extend_from_slice(&payload[12..cursor + 8]);
            result.extend_from_slice(
                &u32::try_from(child_length)
                    .map_err(|_| "AnDs exceeds u32")?
                    .to_be_bytes(),
            );
            result.extend_from_slice(&payload[data_start..data_end - remove]);
            result.extend_from_slice(&payload[data_end..]);
            return Ok(result);
        }
        cursor = data_end
            .checked_add(length & 1)
            .ok_or_else(|| "animation child padding overflow".to_string())?;
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
