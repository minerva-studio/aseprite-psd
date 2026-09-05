use std::env;
use std::fs;
use std::io::Read;
use std::path::PathBuf;
use std::process::ExitCode;

use ag_psd::descriptor::{Descriptor, DescriptorValue, ReferenceItem, read_version_and_descriptor};
use ag_psd::reader::PsdReader;
use flate2::bufread::ZlibDecoder;
use serde::Serialize;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};

const SCHEMA_VERSION: u32 = 1;

fn main() -> ExitCode {
    match run() {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("error: {error}");
            ExitCode::FAILURE
        }
    }
}

/// Audits one PSD/PSB without using the production document reader.
fn run() -> Result<(), String> {
    let args = env::args().skip(1).collect::<Vec<_>>();
    let input = value(&args, "--input").ok_or("missing --input")?;
    let output = value(&args, "--output").ok_or("missing --output")?;
    let bytes = fs::read(&input).map_err(|error| format!("{}: {error}", input.display()))?;
    let report = audit(&bytes)?;
    if let Some(parent) = output.parent() {
        fs::create_dir_all(parent).map_err(|error| error.to_string())?;
    }
    fs::write(
        &output,
        serde_json::to_vec_pretty(&report).map_err(|error| error.to_string())?,
    )
    .map_err(|error| error.to_string())?;
    println!("wrote {}", output.display());
    Ok(())
}

fn value(args: &[String], key: &str) -> Option<PathBuf> {
    args.iter()
        .position(|item| item == key)
        .and_then(|index| args.get(index + 1))
        .map(PathBuf::from)
}

#[derive(Debug, Serialize)]
struct AuditReport {
    schema_version: u32,
    file_length: usize,
    sha256: String,
    header: Header,
    sections: Vec<Block>,
    resources: Vec<Resource>,
    layers: Vec<LayerRecord>,
    layer_info_padding_hex: String,
    global_mask: Option<Block>,
    document_additional_info: Vec<AdditionalInfo>,
    composite: Composite,
    references: ReferenceGraph,
    issues: Vec<Issue>,
}

#[derive(Debug, Serialize)]
struct Header {
    signature: String,
    version: u16,
    reserved_hex: String,
    channels: u16,
    height: u32,
    width: u32,
    depth: u16,
    color_mode: u16,
}

#[derive(Debug, Clone, Serialize)]
struct Block {
    kind: String,
    start: usize,
    end: usize,
    declared_length: usize,
    consumed_length: usize,
    alignment: usize,
    padding_hex: String,
    sha256: String,
    preview_hex: String,
    status: String,
}

#[derive(Debug, Serialize)]
struct Resource {
    index: usize,
    block: Block,
    signature: String,
    id: u16,
    name_hex: String,
    name_padding_hex: String,
    payload_padding_hex: String,
    classification: String,
    subresources: Vec<AdditionalInfo>,
}

#[derive(Debug, Serialize)]
struct LayerRecord {
    index: usize,
    block: Block,
    bounds: [i32; 4],
    channels: Vec<Channel>,
    blend_signature: String,
    blend_mode: String,
    opacity: u8,
    clipping: u8,
    flags: u8,
    filler: u8,
    mask: Block,
    blending_ranges: Block,
    pascal_name_hex: String,
    additional_info: Vec<AdditionalInfo>,
    layer_id: Option<u32>,
    section_divider_type: Option<u32>,
    is_bounding_divider: bool,
}

#[derive(Debug, Serialize)]
struct Channel {
    id: i16,
    declared_length: u64,
    data_start: Option<usize>,
    data_end: Option<usize>,
    compression: Option<u16>,
    sha256: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
struct AdditionalInfo {
    index: usize,
    block: Block,
    signature: String,
    key: String,
    copy_on_sheet_duplication: Option<bool>,
    layer_id: Option<u32>,
    section_divider_type: Option<u32>,
    descriptor: Option<Value>,
    metadata_records: Vec<AdditionalInfo>,
}

#[derive(Debug, Serialize)]
struct Composite {
    block: Block,
    compression: Option<u16>,
    expected_decoded_bytes: Option<u64>,
    decoded_bytes: Option<usize>,
    compressed_bytes_consumed: Option<usize>,
    validation: String,
}

#[derive(Debug, Serialize)]
struct Issue {
    offset: usize,
    severity: String,
    message: String,
}

#[derive(Debug, Serialize)]
struct ReferenceGraph {
    layer_ids: Vec<u32>,
    bounding_divider_layer_ids: Vec<u32>,
    frame_ids: Vec<i64>,
    mlst_layer_ids: Vec<i64>,
    mlst_frame_ids: Vec<i64>,
    animation_set_ids: Vec<i64>,
    active_animation_sets: Vec<i64>,
    document_id_seed: Option<u32>,
    findings: Vec<String>,
}

fn audit(bytes: &[u8]) -> Result<AuditReport, String> {
    let mut cursor = Cursor::new(bytes);
    let signature = cursor.take(4, "header signature")?;
    let version = cursor.u16("version")?;
    let reserved = cursor.take(6, "reserved")?;
    let channels = cursor.u16("channels")?;
    let height = cursor.u32("height")?;
    let width = cursor.u32("width")?;
    let depth = cursor.u16("depth")?;
    let color_mode = cursor.u16("color mode")?;
    let header = Header {
        signature: ascii(signature),
        version,
        reserved_hex: hex(reserved),
        channels,
        height,
        width,
        depth,
        color_mode,
    };
    let mut issues = Vec::new();
    if signature != b"8BPS" {
        issues.push(issue(0, "error", "header signature is not 8BPS"));
    }
    if version != 1 && version != 2 {
        issues.push(issue(4, "error", "unsupported PSD version"));
    }
    if reserved.iter().any(|byte| *byte != 0) {
        issues.push(issue(6, "warning", "reserved header bytes are not zero"));
    }

    let mut sections = Vec::new();
    let color = length_block_u32(&mut cursor, "color_mode_data", 1)?;
    sections.push(color.clone());
    let resources_block = length_block_u32(&mut cursor, "image_resources", 1)?;
    let resources_bytes = &bytes[resources_block.start + 4..resources_block.end];
    let resources_base = resources_block.start + 4;
    sections.push(resources_block.clone());
    let resources = parse_resources(resources_bytes, resources_base, &mut issues)?;

    let layer_mask_start = cursor.pos;
    let layer_mask_length = if version == 2 {
        cursor.u64("layer and mask length")? as usize
    } else {
        cursor.u32("layer and mask length")? as usize
    };
    let layer_mask_prefix = if version == 2 { 8 } else { 4 };
    let layer_mask_data_start = cursor.pos;
    let layer_mask_data = cursor.take(layer_mask_length, "layer and mask data")?;
    let layer_mask = block(
        "layer_and_mask",
        layer_mask_start,
        cursor.pos,
        layer_mask_length,
        layer_mask_length,
        1,
        &[],
        layer_mask_data,
        "parsed",
    );
    sections.push(layer_mask);
    let (layers, layer_info_padding_hex, global_mask, document_additional_info) = parse_layer_mask(
        layer_mask_data,
        layer_mask_data_start,
        version == 2,
        &mut issues,
    )?;

    let composite_start = cursor.pos;
    let composite_bytes = cursor.take(cursor.remaining(), "composite image data")?;
    let compression = composite_bytes
        .get(..2)
        .map(|data| u16::from_be_bytes([data[0], data[1]]));
    let expected = u64::from(channels)
        .checked_mul(u64::from(width))
        .and_then(|value| value.checked_mul(u64::from(height)))
        .and_then(|value| value.checked_mul(u64::from(depth).div_ceil(8)));
    let (decoded_bytes, compressed_bytes_consumed, validation) = validate_composite(
        composite_bytes,
        compression,
        expected,
        version == 2,
        channels,
        height,
    );
    let composite = Composite {
        block: block(
            "composite_image_data",
            composite_start,
            bytes.len(),
            composite_bytes.len(),
            composite_bytes.len(),
            1,
            &[],
            composite_bytes,
            if compression.is_some() {
                "opaque"
            } else {
                "truncated"
            },
        ),
        compression,
        expected_decoded_bytes: expected,
        decoded_bytes,
        compressed_bytes_consumed,
        validation,
    };
    if layer_mask_start + layer_mask_prefix + layer_mask_length != composite_start {
        issues.push(issue(
            composite_start,
            "error",
            "top-level section coverage has a gap",
        ));
    }

    let references = build_reference_graph(&resources, &layers);
    Ok(AuditReport {
        schema_version: SCHEMA_VERSION,
        file_length: bytes.len(),
        sha256: digest(bytes),
        header,
        sections,
        resources,
        layers,
        layer_info_padding_hex,
        global_mask,
        document_additional_info,
        composite,
        references,
        issues,
    })
}

fn build_reference_graph(resources: &[Resource], layers: &[LayerRecord]) -> ReferenceGraph {
    let layer_ids = layers
        .iter()
        .filter_map(|layer| layer.layer_id)
        .collect::<Vec<_>>();
    let bounding_divider_layer_ids = layers
        .iter()
        .filter(|layer| layer.is_bounding_divider)
        .filter_map(|layer| layer.layer_id)
        .collect::<Vec<_>>();
    let descriptors = resources
        .iter()
        .flat_map(|resource| resource.subresources.iter())
        .filter_map(|record| record.descriptor.as_ref())
        .collect::<Vec<_>>();
    let layer_descriptors = layers
        .iter()
        .flat_map(|layer| layer.additional_info.iter())
        .flat_map(|record| record.metadata_records.iter())
        .filter_map(|record| record.descriptor.as_ref())
        .collect::<Vec<_>>();
    let frame_ids = collect_numbers(&descriptors, "FrID");
    let mlst_layer_ids = collect_numbers(&layer_descriptors, "LaID");
    let animation_set_ids = collect_numbers(&descriptors, "FsID");
    let active_animation_sets = collect_numbers(&descriptors, "AFSt");
    let mlst_frame_ids = collect_numbers(&layer_descriptors, "FrLs");
    let document_id_seed = resources
        .iter()
        .find(|resource| resource.id == 1044)
        .and_then(|resource| {
            let preview = &resource.block.preview_hex;
            (preview.len() >= 8)
                .then(|| u32::from_str_radix(&preview[..8], 16).ok())
                .flatten()
        });
    let mut findings = Vec::new();
    push_duplicates(
        "layer",
        layer_ids.iter().map(|value| i64::from(*value)),
        &mut findings,
    );
    push_duplicates("frame", frame_ids.iter().copied(), &mut findings);
    let frame_set = frame_ids
        .iter()
        .copied()
        .collect::<std::collections::HashSet<_>>();
    for id in mlst_frame_ids
        .iter()
        .copied()
        .collect::<std::collections::HashSet<_>>()
    {
        if !frame_set.contains(&id) {
            findings.push(format!("mlst references missing frame ID {id}"));
        }
    }
    let layer_set = layer_ids
        .iter()
        .copied()
        .collect::<std::collections::HashSet<_>>();
    for id in &frame_ids {
        if u32::try_from(*id)
            .ok()
            .is_some_and(|id| layer_set.contains(&id))
        {
            findings.push(format!("frame ID {id} collides with a layer ID"));
        }
    }
    ReferenceGraph {
        layer_ids,
        bounding_divider_layer_ids,
        frame_ids,
        mlst_layer_ids,
        mlst_frame_ids,
        animation_set_ids,
        active_animation_sets,
        document_id_seed,
        findings,
    }
}

fn collect_numbers(descriptors: &[&Value], wanted: &str) -> Vec<i64> {
    let mut values = Vec::new();
    for descriptor in descriptors {
        collect_numbers_in_value(descriptor, wanted, &mut values);
    }
    values
}

fn collect_numbers_in_value(value: &Value, wanted: &str, output: &mut Vec<i64>) {
    match value {
        Value::Array(values) => values
            .iter()
            .for_each(|value| collect_numbers_in_value(value, wanted, output)),
        Value::Object(map) => {
            if map.get("key").and_then(Value::as_str) == Some(wanted)
                && let Some(value) = map.get("value")
            {
                collect_plain_numbers(value, output);
            }
            map.values()
                .for_each(|value| collect_numbers_in_value(value, wanted, output));
        }
        _ => {}
    }
}

fn collect_plain_numbers(value: &Value, output: &mut Vec<i64>) {
    match value {
        Value::Number(value) => {
            if let Some(value) = value.as_i64() {
                output.push(value);
            }
        }
        Value::Array(values) => values
            .iter()
            .for_each(|value| collect_plain_numbers(value, output)),
        Value::Object(map) => map
            .values()
            .for_each(|value| collect_plain_numbers(value, output)),
        _ => {}
    }
}

fn push_duplicates(kind: &str, values: impl Iterator<Item = i64>, findings: &mut Vec<String>) {
    let mut seen = std::collections::HashSet::new();
    for value in values {
        if !seen.insert(value) {
            findings.push(format!("duplicate {kind} ID {value}"));
        }
    }
}

fn validate_composite(
    data: &[u8],
    compression: Option<u16>,
    expected: Option<u64>,
    is_psb: bool,
    channels: u16,
    height: u32,
) -> (Option<usize>, Option<usize>, String) {
    let Some(compression) = compression else {
        return (None, None, "truncated compression field".to_string());
    };
    let payload = &data[2..];
    match compression {
        0 => {
            let valid = expected == Some(payload.len() as u64);
            (
                Some(payload.len()),
                Some(payload.len()),
                if valid {
                    "valid raw"
                } else {
                    "raw length mismatch"
                }
                .to_string(),
            )
        }
        1 => {
            let rows = usize::from(channels).saturating_mul(height as usize);
            let width = if is_psb { 4 } else { 2 };
            let table = rows.saturating_mul(width);
            if payload.len() < table {
                return (None, None, "truncated RLE row table".to_string());
            }
            let encoded = (0..rows).try_fold(0usize, |sum, row| {
                let offset = row * width;
                let length = if is_psb {
                    u32::from_be_bytes(payload[offset..offset + 4].try_into().ok()?) as usize
                } else {
                    usize::from(u16::from_be_bytes(
                        payload[offset..offset + 2].try_into().ok()?,
                    ))
                };
                sum.checked_add(length)
            });
            let consumed = encoded.and_then(|value| table.checked_add(value));
            let valid = consumed == Some(payload.len());
            (
                expected.and_then(|value| usize::try_from(value).ok()),
                consumed,
                if valid {
                    "valid RLE framing"
                } else {
                    "RLE framing mismatch"
                }
                .to_string(),
            )
        }
        2 | 3 => {
            let mut decoder = ZlibDecoder::new(payload);
            let mut decoded = Vec::new();
            match decoder.read_to_end(&mut decoded) {
                Ok(_) => {
                    let consumed = decoder.total_in() as usize;
                    let valid = expected == Some(decoded.len() as u64) && consumed == payload.len();
                    (
                        Some(decoded.len()),
                        Some(consumed),
                        if valid {
                            "valid single zlib stream"
                        } else {
                            "zlib length or trailing-data mismatch"
                        }
                        .to_string(),
                    )
                }
                Err(error) => (
                    None,
                    Some(decoder.total_in() as usize),
                    format!("zlib error: {error}"),
                ),
            }
        }
        other => (None, None, format!("unknown compression {other}")),
    }
}

fn parse_resources(
    data: &[u8],
    base: usize,
    issues: &mut Vec<Issue>,
) -> Result<Vec<Resource>, String> {
    let mut cursor = Cursor::with_base(data, base);
    let mut records = Vec::new();
    while cursor.remaining() > 0 {
        let start = cursor.absolute();
        let signature = cursor.take(4, "resource signature")?;
        let id = cursor.u16("resource id")?;
        let name_length = usize::from(cursor.u8("resource name length")?);
        let name = cursor.take(name_length, "resource name")?;
        let name_padding = if (name_length + 1) % 2 != 0 {
            cursor.take(1, "resource name padding")?
        } else {
            &[]
        };
        let payload_length = cursor.u32("resource payload length")? as usize;
        let payload_start = cursor.absolute();
        let payload = cursor.take(payload_length, "resource payload")?;
        let payload_padding = if payload_length % 2 != 0 {
            cursor.take(1, "resource payload padding")?
        } else {
            &[]
        };
        let classification = classify_resource(id, payload);
        let subresources = if payload.starts_with(b"maniIRFR") {
            parse_mani(payload, payload_start, issues)?
        } else {
            Vec::new()
        };
        records.push(Resource {
            index: records.len(),
            block: block(
                "image_resource",
                start,
                cursor.absolute(),
                payload_length,
                payload.len(),
                2,
                payload_padding,
                payload,
                if signature == b"8BIM" || signature == b"MeSa" {
                    "parsed"
                } else {
                    issues.push(issue(start, "warning", "unknown image resource signature"));
                    "opaque"
                },
            ),
            signature: ascii(signature),
            id,
            name_hex: hex(name),
            name_padding_hex: hex(name_padding),
            payload_padding_hex: hex(payload_padding),
            classification,
            subresources,
        });
    }
    Ok(records)
}

fn classify_resource(id: u16, payload: &[u8]) -> String {
    if payload.starts_with(b"maniIRFR") {
        "maniIRFR".to_string()
    } else if payload.starts_with(b"mopt") {
        "mopt".to_string()
    } else if payload.starts_with(b"mset") {
        "mset".to_string()
    } else if payload.starts_with(b"ms4w") {
        "ms4w".to_string()
    } else if payload.starts_with(b"mfri") {
        "mfri".to_string()
    } else if (4000..=4999).contains(&id) {
        "opaque_plugin".to_string()
    } else {
        "standard_or_opaque".to_string()
    }
}

fn parse_mani(
    payload: &[u8],
    base: usize,
    issues: &mut Vec<Issue>,
) -> Result<Vec<AdditionalInfo>, String> {
    let mut outer = Cursor::with_base(payload, base);
    outer.take(8, "mani signature")?;
    let declared = outer.u32("mani length")? as usize;
    let body = outer.take(declared, "mani body")?;
    let mut records =
        parse_additional_stream(body, base + 12, false, "animation_subresource", issues)?;
    if outer.remaining() != 0 {
        let start = outer.absolute();
        let tail = outer.take(outer.remaining(), "mani opaque tail")?;
        records.push(opaque_tail(records.len(), "mani_opaque_tail", start, tail));
    }
    Ok(records)
}

fn parse_layer_mask(
    data: &[u8],
    base: usize,
    is_psb: bool,
    issues: &mut Vec<Issue>,
) -> Result<(Vec<LayerRecord>, String, Option<Block>, Vec<AdditionalInfo>), String> {
    if data.is_empty() {
        return Ok((Vec::new(), String::new(), None, Vec::new()));
    }
    let mut cursor = Cursor::with_base(data, base);
    let layer_info_length = if is_psb {
        cursor.u64("layer info length")? as usize
    } else {
        cursor.u32("layer info length")? as usize
    };
    let layer_info_start = cursor.absolute();
    let layer_info = cursor.take(layer_info_length, "layer info")?;
    let mut layers = parse_layer_info(layer_info, layer_info_start, is_psb, issues)?;
    let global_mask = if cursor.remaining() >= 4 {
        Some(length_block_u32(&mut cursor, "global_layer_mask", 1)?)
    } else {
        None
    };
    let document = if cursor.remaining() > 0 {
        let absolute = cursor.absolute();
        let remaining = cursor.take(cursor.remaining(), "document additional info")?;
        parse_additional_stream(
            remaining,
            absolute,
            is_psb,
            "document_additional_info",
            issues,
        )?
    } else {
        Vec::new()
    };
    let channel_data_start = layer_info_start
        + if is_psb { 8 } else { 4 }
        + layer_info_length.saturating_sub(layer_info_length);
    let _ = channel_data_start;
    let layer_info_padding_hex =
        assign_channel_offsets(layer_info, layer_info_start, &mut layers, issues)?;
    Ok((layers, layer_info_padding_hex, global_mask, document))
}

fn parse_layer_info(
    data: &[u8],
    base: usize,
    is_psb: bool,
    issues: &mut Vec<Issue>,
) -> Result<Vec<LayerRecord>, String> {
    if data.is_empty() {
        return Ok(Vec::new());
    }
    let mut cursor = Cursor::with_base(data, base);
    let count = usize::from(cursor.i16("layer count")?.unsigned_abs());
    let mut layers = Vec::with_capacity(count);
    for index in 0..count {
        let start = cursor.absolute();
        let bounds = [
            cursor.i32("layer top")?,
            cursor.i32("layer left")?,
            cursor.i32("layer bottom")?,
            cursor.i32("layer right")?,
        ];
        let channel_count = usize::from(cursor.u16("channel count")?);
        let mut channels = Vec::with_capacity(channel_count);
        for _ in 0..channel_count {
            channels.push(Channel {
                id: cursor.i16("channel id")?,
                declared_length: if is_psb {
                    cursor.u64("channel length")?
                } else {
                    u64::from(cursor.u32("channel length")?)
                },
                data_start: None,
                data_end: None,
                compression: None,
                sha256: None,
            });
        }
        let blend_signature = ascii(cursor.take(4, "blend signature")?);
        let blend_mode = ascii(cursor.take(4, "blend mode")?);
        let opacity = cursor.u8("opacity")?;
        let clipping = cursor.u8("clipping")?;
        let flags = cursor.u8("flags")?;
        let filler = cursor.u8("filler")?;
        let extra_length = cursor.u32("layer extra length")? as usize;
        let extra_start = cursor.absolute();
        let extra = cursor.take(extra_length, "layer extra")?;
        let mut extra_cursor = Cursor::with_base(extra, extra_start);
        let mask = length_block_u32(&mut extra_cursor, "layer_mask", 1)?;
        let blending_ranges = length_block_u32(&mut extra_cursor, "blending_ranges", 1)?;
        let name_length = usize::from(extra_cursor.u8("layer name length")?);
        let name = extra_cursor.take(name_length, "layer name")?;
        let name_padding_length = (4 - ((name_length + 1) % 4)) % 4;
        extra_cursor.take(name_padding_length, "layer name padding")?;
        let info_start = extra_cursor.absolute();
        let info_data = extra_cursor.take(extra_cursor.remaining(), "layer additional info")?;
        let additional_info = parse_additional_stream(
            info_data,
            info_start,
            is_psb,
            "layer_additional_info",
            issues,
        )?;
        let layer_id = additional_info.iter().find_map(|item| item.layer_id);
        let section_divider_type = additional_info
            .iter()
            .find_map(|item| item.section_divider_type);
        layers.push(LayerRecord {
            index,
            block: block(
                "layer_record",
                start,
                cursor.absolute(),
                extra_length,
                extra.len(),
                1,
                &[],
                &data[start - base..cursor.absolute() - base],
                "parsed",
            ),
            bounds,
            channels,
            blend_signature,
            blend_mode,
            opacity,
            clipping,
            flags,
            filler,
            mask,
            blending_ranges,
            pascal_name_hex: hex(name),
            additional_info,
            layer_id,
            section_divider_type,
            is_bounding_divider: section_divider_type == Some(3),
        });
    }
    Ok(layers)
}

fn assign_channel_offsets(
    layer_info: &[u8],
    base: usize,
    layers: &mut [LayerRecord],
    issues: &mut Vec<Issue>,
) -> Result<String, String> {
    if layer_info.is_empty() {
        return Ok(String::new());
    }
    let records_end = layers.last().map_or(base + 2, |layer| layer.block.end);
    let mut offset = records_end;
    let limit = base + layer_info.len();
    for layer in layers {
        for channel in &mut layer.channels {
            let length = usize::try_from(channel.declared_length)
                .map_err(|_| "channel length exceeds platform".to_string())?;
            let end = offset.saturating_add(length);
            if end > limit {
                issues.push(issue(offset, "error", "layer channel exceeds layer info"));
                return Ok(String::new());
            }
            let relative = offset - base;
            let bytes = &layer_info[relative..relative + length];
            channel.data_start = Some(offset);
            channel.data_end = Some(end);
            channel.compression = bytes
                .get(..2)
                .map(|data| u16::from_be_bytes([data[0], data[1]]));
            channel.sha256 = Some(digest(bytes));
            offset = end;
        }
    }
    let trailing = &layer_info[offset.saturating_sub(base)..];
    if trailing.len() > 3 || trailing.iter().any(|byte| *byte != 0) {
        issues.push(issue(
            offset,
            "warning",
            "non-padding bytes remain in layer info",
        ));
    }
    Ok(hex(trailing))
}

fn parse_additional_stream(
    data: &[u8],
    base: usize,
    is_psb: bool,
    kind: &str,
    issues: &mut Vec<Issue>,
) -> Result<Vec<AdditionalInfo>, String> {
    let mut cursor = Cursor::with_base(data, base);
    let mut records = Vec::new();
    while cursor.remaining() > 0 {
        if cursor.remaining() < 12 {
            let start = cursor.absolute();
            let tail = cursor.take(cursor.remaining(), "additional-info opaque tail")?;
            records.push(opaque_tail(
                records.len(),
                "additional_info_opaque_tail",
                start,
                tail,
            ));
            break;
        }
        let start = cursor.absolute();
        let signature_bytes = cursor.take(4, "additional signature")?;
        let signature = ascii(signature_bytes);
        let key_bytes = cursor.take(4, "additional key")?;
        let key = ascii(key_bytes);
        let length = if signature_bytes == b"8B64" || (is_psb && is_large_key(&key)) {
            cursor.u64("additional length")? as usize
        } else {
            cursor.u32("additional length")? as usize
        };
        let payload_start = cursor.absolute();
        let payload = cursor.take(length, "additional payload")?;
        let alignment = if kind == "animation_subresource" {
            2
        } else {
            4
        };
        let padding_length = cursor.padding_before_signature(alignment - 1);
        let padding = if padding_length != 0 {
            cursor.take(padding_length, "additional padding")?
        } else {
            &[]
        };
        let layer_id = (key == "lyid" && payload.len() == 4)
            .then(|| u32::from_be_bytes(payload.try_into().expect("length checked")));
        let section_divider_type = ((key == "lsct" || key == "lsdk") && payload.len() >= 4)
            .then(|| u32::from_be_bytes(payload[..4].try_into().expect("length checked")));
        let metadata_records = if key == "shmd" {
            parse_shmd(payload, payload_start, issues)?
        } else {
            Vec::new()
        };
        let descriptor = (key == "AnDs")
            .then(|| parse_descriptor(payload, payload_start, issues))
            .flatten();
        records.push(AdditionalInfo {
            index: records.len(),
            block: block(
                kind,
                start,
                cursor.absolute(),
                length,
                payload.len(),
                alignment,
                padding,
                payload,
                if matches!(
                    key.as_str(),
                    "lyid" | "lsct" | "lsdk" | "shmd" | "mlst" | "mdyn" | "AnDs" | "Roll"
                ) {
                    "parsed"
                } else {
                    "opaque"
                },
            ),
            signature,
            key,
            copy_on_sheet_duplication: None,
            layer_id,
            section_divider_type,
            descriptor,
            metadata_records,
        });
    }
    Ok(records)
}

fn opaque_tail(index: usize, kind: &str, start: usize, payload: &[u8]) -> AdditionalInfo {
    AdditionalInfo {
        index,
        block: block(
            kind,
            start,
            start + payload.len(),
            payload.len(),
            payload.len(),
            1,
            &[],
            payload,
            "opaque",
        ),
        signature: String::new(),
        key: "<opaque-tail>".to_string(),
        copy_on_sheet_duplication: None,
        layer_id: None,
        section_divider_type: None,
        descriptor: None,
        metadata_records: Vec::new(),
    }
}

fn parse_shmd(
    data: &[u8],
    base: usize,
    issues: &mut Vec<Issue>,
) -> Result<Vec<AdditionalInfo>, String> {
    let mut cursor = Cursor::with_base(data, base);
    let count = cursor.u32("shmd count")? as usize;
    let mut records = Vec::with_capacity(count);
    for index in 0..count {
        let start = cursor.absolute();
        let signature = ascii(cursor.take(4, "shmd signature")?);
        let key = ascii(cursor.take(4, "shmd key")?);
        let copy_bytes = cursor.take(4, "shmd copy flags")?;
        let copy = copy_bytes.iter().any(|byte| *byte != 0);
        let length = cursor.u32("shmd payload length")? as usize;
        let payload = cursor.take(length, "shmd payload")?;
        let padding_length = length % 2;
        let padding = if padding_length != 0 {
            cursor.take(padding_length, "shmd padding")?
        } else {
            &[]
        };
        let descriptor = (key == "mlst")
            .then(|| parse_descriptor(payload, start, issues))
            .flatten();
        records.push(AdditionalInfo {
            index,
            block: block(
                "shmd_record",
                start,
                cursor.absolute(),
                length,
                payload.len(),
                2,
                padding,
                payload,
                if key == "mlst" || key == "mdyn" {
                    "parsed"
                } else {
                    "opaque"
                },
            ),
            signature,
            key,
            copy_on_sheet_duplication: Some(copy),
            layer_id: None,
            section_divider_type: None,
            descriptor,
            metadata_records: Vec::new(),
        });
    }
    if cursor.remaining() > 0 {
        issues.push(issue(
            cursor.absolute(),
            "warning",
            "shmd has trailing bytes",
        ));
    }
    Ok(records)
}

fn parse_descriptor(payload: &[u8], offset: usize, issues: &mut Vec<Issue>) -> Option<Value> {
    let mut reader = PsdReader::new(payload, None, None);
    match read_version_and_descriptor(&mut reader) {
        Ok(descriptor) => {
            if reader.offset != payload.len()
                && payload[reader.offset..].iter().any(|byte| *byte != 0)
            {
                issues.push(issue(
                    offset + reader.offset,
                    "warning",
                    "descriptor has trailing bytes",
                ));
            }
            Some(descriptor_json(&descriptor))
        }
        Err(error) => {
            issues.push(issue(
                offset,
                "warning",
                &format!("descriptor parse failed: {error}"),
            ));
            None
        }
    }
}

fn descriptor_json(descriptor: &Descriptor) -> Value {
    json!({
        "name": descriptor.name,
        "class_id": descriptor.class_id,
        "items": descriptor.items.iter().map(|(key, value)| json!({
            "key": key,
            "type": value.os_type(),
            "value": descriptor_value_json(value),
        })).collect::<Vec<_>>(),
    })
}

fn descriptor_value_json(value: &DescriptorValue) -> Value {
    match value {
        DescriptorValue::Reference(items) => {
            Value::Array(items.iter().map(reference_json).collect())
        }
        DescriptorValue::Descriptor(value) => descriptor_json(value),
        DescriptorValue::List(items) => {
            Value::Array(items.iter().map(descriptor_value_json).collect())
        }
        DescriptorValue::Double(value) => json!(value),
        DescriptorValue::UnitDouble(value) => json!({ "units": value.units, "value": value.value }),
        DescriptorValue::Text(value)
        | DescriptorValue::Enum(value)
        | DescriptorValue::Alias(value) => json!(value),
        DescriptorValue::Integer(value) => json!(value),
        DescriptorValue::LargeInteger(value) => json!({ "low": value.low, "high": value.high }),
        DescriptorValue::Boolean(value) => json!(value),
        DescriptorValue::Class(value) => json!({ "name": value.name, "class_id": value.class_id }),
        DescriptorValue::RawData(value) => {
            json!({ "length": value.len(), "sha256": digest(value), "preview_hex": hex(&value[..value.len().min(32)]) })
        }
        DescriptorValue::ObjectArray(items) => Value::Array(
            items
                .iter()
                .map(|item| json!({ "type": item.type_, "values": item.values }))
                .collect(),
        ),
        DescriptorValue::Path(value) => json!({ "signature": value.sig, "path": value.path }),
    }
}

fn reference_json(value: &ReferenceItem) -> Value {
    match value {
        ReferenceItem::Property(value) => json!({ "type": "property", "value": value }),
        ReferenceItem::Class(value) => {
            json!({ "type": "class", "name": value.name, "class_id": value.class_id })
        }
        ReferenceItem::Enumerated(value) => json!({ "type": "enumerated", "value": value }),
        ReferenceItem::Offset(value) => json!({ "type": "offset", "value": value }),
        ReferenceItem::Identifier(value) => json!({ "type": "identifier", "value": value }),
        ReferenceItem::Index(value) => json!({ "type": "index", "value": value }),
        ReferenceItem::Name(value) => json!({ "type": "name", "value": value }),
    }
}

fn is_large_key(key: &str) -> bool {
    matches!(
        key,
        "LMsk"
            | "Lr16"
            | "Lr32"
            | "Layr"
            | "Mt16"
            | "Mt32"
            | "Mtrn"
            | "Alph"
            | "FMsk"
            | "lnk2"
            | "FEid"
            | "FXid"
            | "PxSD"
    )
}

fn length_block_u32(
    cursor: &mut Cursor<'_>,
    kind: &str,
    alignment: usize,
) -> Result<Block, String> {
    let start = cursor.absolute();
    let length = cursor.u32(&format!("{kind} length"))? as usize;
    let payload = cursor.take(length, kind)?;
    Ok(block(
        kind,
        start,
        cursor.absolute(),
        length,
        payload.len(),
        alignment,
        &[],
        payload,
        "parsed",
    ))
}

fn block(
    kind: &str,
    start: usize,
    end: usize,
    declared_length: usize,
    consumed_length: usize,
    alignment: usize,
    padding: &[u8],
    payload: &[u8],
    status: &str,
) -> Block {
    Block {
        kind: kind.to_string(),
        start,
        end,
        declared_length,
        consumed_length,
        alignment,
        padding_hex: hex(padding),
        sha256: digest(payload),
        preview_hex: hex(&payload[..payload.len().min(32)]),
        status: status.to_string(),
    }
}

fn issue(offset: usize, severity: &str, message: &str) -> Issue {
    Issue {
        offset,
        severity: severity.to_string(),
        message: message.to_string(),
    }
}

fn digest(bytes: &[u8]) -> String {
    Sha256::digest(bytes)
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}

fn ascii(bytes: &[u8]) -> String {
    bytes
        .iter()
        .map(|byte| {
            if byte.is_ascii_graphic() || *byte == b' ' {
                char::from(*byte)
            } else {
                '.'
            }
        })
        .collect()
}

struct Cursor<'a> {
    data: &'a [u8],
    pos: usize,
    base: usize,
}

impl<'a> Cursor<'a> {
    fn new(data: &'a [u8]) -> Self {
        Self {
            data,
            pos: 0,
            base: 0,
        }
    }
    fn with_base(data: &'a [u8], base: usize) -> Self {
        Self { data, pos: 0, base }
    }
    fn absolute(&self) -> usize {
        self.base + self.pos
    }
    fn remaining(&self) -> usize {
        self.data.len().saturating_sub(self.pos)
    }
    fn padding_before_signature(&self, maximum: usize) -> usize {
        if self.remaining() == 0 {
            return 0;
        }
        (0..=maximum.min(self.remaining()))
            .find(|padding| {
                let rest = &self.data[self.pos + padding..];
                rest.len() >= 4 && (rest.starts_with(b"8BIM") || rest.starts_with(b"8B64"))
            })
            .unwrap_or(0)
    }
    fn take(&mut self, length: usize, label: &str) -> Result<&'a [u8], String> {
        let end = self
            .pos
            .checked_add(length)
            .ok_or_else(|| format!("{label} length overflow at 0x{:x}", self.absolute()))?;
        if end > self.data.len() {
            return Err(format!(
                "truncated {label} at 0x{:x}: need {length}, have {}",
                self.absolute(),
                self.remaining()
            ));
        }
        let value = &self.data[self.pos..end];
        self.pos = end;
        Ok(value)
    }
    fn u8(&mut self, label: &str) -> Result<u8, String> {
        Ok(self.take(1, label)?[0])
    }
    fn u16(&mut self, label: &str) -> Result<u16, String> {
        Ok(u16::from_be_bytes(
            self.take(2, label)?.try_into().expect("length"),
        ))
    }
    fn i16(&mut self, label: &str) -> Result<i16, String> {
        Ok(i16::from_be_bytes(
            self.take(2, label)?.try_into().expect("length"),
        ))
    }
    fn u32(&mut self, label: &str) -> Result<u32, String> {
        Ok(u32::from_be_bytes(
            self.take(4, label)?.try_into().expect("length"),
        ))
    }
    fn i32(&mut self, label: &str) -> Result<i32, String> {
        Ok(i32::from_be_bytes(
            self.take(4, label)?.try_into().expect("length"),
        ))
    }
    fn u64(&mut self, label: &str) -> Result<u64, String> {
        Ok(u64::from_be_bytes(
            self.take(8, label)?.try_into().expect("length"),
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cursor_reports_absolute_truncation() {
        let mut cursor = Cursor::with_base(&[1, 2], 100);
        let error = cursor
            .take(3, "fixture")
            .expect_err("must reject truncation");
        assert!(error.contains("0x64"));
    }

    #[test]
    fn opaque_block_keeps_wire_identity() {
        let bytes = [1, 2, 3, 4, 5];
        let record = block("opaque", 9, 14, 5, 5, 2, &[0], &bytes, "opaque");
        assert_eq!(record.sha256, digest(&bytes));
        assert_eq!(record.preview_hex, "0102030405");
        assert_eq!(record.padding_hex, "00");
    }

    #[test]
    fn minimal_psd_covers_all_top_level_sections() {
        let mut bytes = Vec::new();
        bytes.extend_from_slice(b"8BPS");
        bytes.extend_from_slice(&1u16.to_be_bytes());
        bytes.extend_from_slice(&[0; 6]);
        bytes.extend_from_slice(&4u16.to_be_bytes());
        bytes.extend_from_slice(&1u32.to_be_bytes());
        bytes.extend_from_slice(&1u32.to_be_bytes());
        bytes.extend_from_slice(&8u16.to_be_bytes());
        bytes.extend_from_slice(&3u16.to_be_bytes());
        bytes.extend_from_slice(&0u32.to_be_bytes());
        bytes.extend_from_slice(&0u32.to_be_bytes());
        bytes.extend_from_slice(&0u32.to_be_bytes());
        bytes.extend_from_slice(&0u16.to_be_bytes());
        bytes.extend_from_slice(&[1, 2, 3, 4]);

        let report = audit(&bytes).expect("minimal PSD should audit");
        assert_eq!(report.composite.validation, "valid raw");
        assert_eq!(report.composite.block.end, bytes.len());
        assert!(report.issues.is_empty());
    }

    #[test]
    fn additional_info_preserves_three_padding_bytes() {
        let mut bytes = Vec::new();
        bytes.extend_from_slice(b"8BIMCAI ");
        bytes.extend_from_slice(&1u32.to_be_bytes());
        bytes.push(7);
        bytes.extend_from_slice(&[0, 0, 0]);
        bytes.extend_from_slice(b"8BIMPatt");
        bytes.extend_from_slice(&0u32.to_be_bytes());
        let mut issues = Vec::new();
        let records =
            parse_additional_stream(&bytes, 50, false, "document_additional_info", &mut issues)
                .expect("additional info should parse");
        assert_eq!(records.len(), 2);
        assert_eq!(records[0].block.padding_hex, "000000");
        assert!(issues.is_empty());
    }
}
