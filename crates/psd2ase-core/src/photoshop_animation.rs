//! Photoshop frame-animation metadata scanning and normalization.
//!
//! This module owns the PSD compatibility boundary. Its public structures do
//! not contain ag-psd types, so later Aseprite and reverse-PSD writers can
//! consume the same normalized animation model.

use std::collections::{HashMap, HashSet};
use std::fmt::{Display, Formatter};

use ag_psd::descriptor::{Descriptor, DescriptorValue, read_version_and_descriptor};
use ag_psd::psd::AnimationFrameFlags;
use ag_psd::reader::PsdReader;

/// A layer identity supplied by the base PSD probe.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AnimationLayerInput {
    /// Stable Photoshop layer identifier from the lyid additional-info key.
    pub id: u32,
    /// Normalized tree path, such as 0/2/1.
    pub path: String,
    /// Whether this node is a Photoshop group.
    pub is_group: bool,
    /// Whether this group only contains child groups and acts as an animation container.
    pub is_container_group: bool,
    /// Base hidden state before frame overrides are applied.
    pub hidden: bool,
    /// Ancestor group IDs in root-to-parent order.
    pub ancestor_ids: Vec<u32>,
}

/// A normalized Photoshop frame animation.
#[derive(Debug, Clone, PartialEq)]
pub struct PhotoshopAnimation {
    /// Frame records in Photoshop playback order.
    pub frames: Vec<PhotoshopFrame>,
    /// The selected playback loop policy, when Photoshop supplied one.
    pub loop_mode: Option<LoopMode>,
    /// The selected animation set's active frame index, when supplied.
    pub active_frame_index: Option<u32>,
    /// Per-layer state records, in the same order as the input layer tree.
    pub layer_states: Vec<LayerAnimationState>,
    /// Effective visible pixel-layer IDs for every frame, in tree order.
    pub visible_pixel_layers: Vec<VisibleFrameLayers>,
    /// Photoshop's optional per-layer animation flags.
    pub frame_flags: Option<AnimationFlags>,
    /// Image-resource IDs that carried the animation descriptor.
    pub resource_ids: Vec<u16>,
}

/// A normalized Photoshop frame record.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PhotoshopFrame {
    /// Photoshop frame identifier.
    pub id: u32,
    /// Frame duration in milliseconds.
    pub duration_ms: u32,
    /// Disposal policy as authored by Photoshop, if present.
    pub dispose: Option<String>,
}

/// A normalized loop policy.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LoopMode {
    /// Repeat forever (LCnt == 0).
    Infinite,
    /// Repeat the authored finite count.
    Finite(u32),
}

/// A normalized state for one layer over the complete frame sequence.
#[derive(Debug, Clone, PartialEq)]
pub struct LayerAnimationState {
    /// Layer identifier.
    pub layer_id: u32,
    /// Layer tree path.
    pub path: String,
    /// State for each global frame, in frame order.
    pub frames: Vec<LayerFrameState>,
}

/// A normalized layer state for one frame.
#[derive(Debug, Clone, PartialEq)]
pub struct LayerFrameState {
    /// Frame identifier.
    pub frame_id: u32,
    /// Whether the source supplied an mlst record for this frame.
    pub record_present: bool,
    /// Resolved visibility after enable inheritance.
    pub enabled: bool,
    /// Whether this record explicitly supplied enable.
    pub explicit_enable: bool,
    /// Optional authored layer offset.
    pub offset: Option<AnimationPoint>,
    /// Optional authored reference point.
    pub reference_point: Option<AnimationPoint>,
    /// Optional authored opacity override in the normalized 0.0..=1.0 range.
    pub opacity: Option<f64>,
}

/// A normalized frame-specific visible pixel-layer list.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VisibleFrameLayers {
    /// Frame identifier.
    pub frame_id: u32,
    /// Visible pixel-layer IDs after ancestor group visibility is applied.
    pub layer_ids: Vec<u32>,
}

/// A normalized point used by animation offsets and reference points.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct AnimationPoint {
    /// Horizontal coordinate.
    pub x: f64,
    /// Vertical coordinate.
    pub y: f64,
}

/// Normalized mdyn flags.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AnimationFlags {
    /// Whether frame one propagates to later frames.
    pub propagate_frame_one: bool,
    /// Whether layer positions are unified.
    pub unify_layer_position: bool,
    /// Whether layer styles are unified.
    pub unify_layer_style: bool,
    /// Whether layer visibility is unified.
    pub unify_layer_visibility: bool,
}

/// Errors raised while scanning Photoshop animation metadata.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AnimationParseError {
    /// The PSD structure ended before a bounded field was complete.
    Truncated { section: String, offset: usize },
    /// A PSD section declared an invalid size.
    InvalidSection { section: String, offset: usize },
    /// A required PSD signature was not present.
    InvalidSignature { expected: String, offset: usize },
    /// A descriptor or layer record was malformed.
    InvalidData(String),
    /// A layer ID was absent where strict association requires it.
    MissingLayerId { path: String },
    /// A layer or frame identifier occurred more than once.
    DuplicateId { kind: String, id: u32 },
    /// The metadata references a layer not present in the normalized tree.
    UnknownLayerId(u32),
}

impl Display for AnimationParseError {
    /// Formats a bounded scanner error for human-readable probe output.
    fn fmt(&self, formatter: &mut Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Truncated { section, offset } => {
                write!(formatter, "truncated {section} at 0x{offset:x}")
            }
            Self::InvalidSection { section, offset } => {
                write!(formatter, "invalid {section} section at 0x{offset:x}")
            }
            Self::InvalidSignature { expected, offset } => {
                write!(formatter, "expected signature {expected} at 0x{offset:x}")
            }
            Self::InvalidData(message) => write!(formatter, "invalid animation data: {message}"),
            Self::MissingLayerId { path } => write!(formatter, "layer has no ID: {path}"),
            Self::DuplicateId { kind, id } => write!(formatter, "duplicate {kind} ID: {id}"),
            Self::UnknownLayerId(id) => {
                write!(formatter, "animation references unknown layer ID: {id}")
            }
        }
    }
}

impl std::error::Error for AnimationParseError {}

/// Parses Photoshop frame-animation metadata and resolves effective visibility.
pub fn parse_photoshop_animation(
    bytes: &[u8],
    layers: &[AnimationLayerInput],
) -> Result<Option<PhotoshopAnimation>, AnimationParseError> {
    let scanned = scan_psd_metadata(bytes)?;
    let has_layer_animation = scanned.layers.iter().any(|layer| layer.shmd.is_some());
    if scanned.animation_descriptors.is_empty() && !has_layer_animation {
        return Ok(None);
    }
    validate_input_layers(layers)?;
    if scanned.animation_descriptors.is_empty() {
        return Err(AnimationParseError::InvalidData(
            "layer animation metadata exists without a 4000/4003 frame catalog".to_string(),
        ));
    }

    let catalog = merge_animation_descriptors(&scanned.animation_descriptors)?;
    if catalog.frames.is_empty() {
        return Err(AnimationParseError::InvalidData(
            "animation catalog contains no frames".to_string(),
        ));
    }
    let raw_by_id = index_raw_layers(scanned.layers)?;
    let input_by_id = layers
        .iter()
        .enumerate()
        .map(|(index, layer)| (layer.id, index))
        .collect::<HashMap<_, _>>();
    for id in raw_by_id.keys() {
        if !input_by_id.contains_key(id) {
            return Err(AnimationParseError::UnknownLayerId(*id));
        }
    }

    let mut states = Vec::with_capacity(layers.len());
    let mut flags = None;
    for layer in layers {
        let raw = raw_by_id
            .get(&layer.id)
            .ok_or_else(|| AnimationParseError::MissingLayerId {
                path: layer.path.clone(),
            })?;
        if let Some(raw_flags) = raw.flags.as_ref() {
            let converted = normalize_flags(raw_flags);
            if let Some(existing) = flags.as_ref() {
                if existing != &converted {
                    return Err(AnimationParseError::InvalidData(
                        "conflicting mdyn flags across layers".to_string(),
                    ));
                }
            } else {
                flags = Some(converted);
            }
        }
        if let Some(raw_flags) = raw
            .shmd
            .as_ref()
            .and_then(|metadata| metadata.flags.as_ref())
        {
            let converted = normalize_flags(raw_flags);
            if let Some(existing) = flags.as_ref() {
                if existing != &converted {
                    return Err(AnimationParseError::InvalidData(
                        "conflicting mdyn flags across layers".to_string(),
                    ));
                }
            } else {
                flags = Some(converted);
            }
        }
        states.push(resolve_layer_states(layer, raw, &catalog.frames)?);
    }

    let visible_pixel_layers = resolve_visible_layers(layers, &states, &catalog.frames);
    Ok(Some(PhotoshopAnimation {
        frames: catalog.frames,
        loop_mode: catalog.loop_mode,
        active_frame_index: catalog.active_frame_index,
        layer_states: states,
        visible_pixel_layers,
        frame_flags: flags,
        resource_ids: scanned.resource_ids,
    }))
}

/// Validates the normalized layer identity set before strict association.
fn validate_input_layers(layers: &[AnimationLayerInput]) -> Result<(), AnimationParseError> {
    let mut ids = HashSet::new();
    for layer in layers {
        if layer.id == 0 {
            return Err(AnimationParseError::MissingLayerId {
                path: layer.path.clone(),
            });
        }
        if !ids.insert(layer.id) {
            return Err(AnimationParseError::DuplicateId {
                kind: "input layer".to_string(),
                id: layer.id,
            });
        }
    }
    Ok(())
}

#[derive(Debug, Default)]
struct ScanResult {
    animation_descriptors: Vec<(u16, Descriptor)>,
    resource_ids: Vec<u16>,
    layers: Vec<RawLayer>,
}

#[derive(Debug, Default)]
struct RawLayer {
    id: Option<u32>,
    shmd: Option<LayerMetadata>,
    flags: Option<AnimationFrameFlags>,
    is_bounding_divider: bool,
}

#[derive(Debug, Default)]
struct LayerMetadata {
    frames: Vec<RawFrameState>,
    flags: Option<AnimationFrameFlags>,
}

#[derive(Debug, Clone)]
struct RawFrameState {
    frame_id: u32,
    enable: Option<bool>,
    offset: Option<AnimationPoint>,
    reference_point: Option<AnimationPoint>,
    opacity: Option<f64>,
}

#[derive(Debug)]
struct AnimationCatalog {
    frames: Vec<PhotoshopFrame>,
    loop_mode: Option<LoopMode>,
    active_frame_index: Option<u32>,
}

/// Scans only the PSD sections needed by the animation compatibility layer.
fn scan_psd_metadata(bytes: &[u8]) -> Result<ScanResult, AnimationParseError> {
    let mut cursor = Cursor::new(bytes);
    cursor.expect_bytes(b"8BPS", "PSD header")?;
    cursor.skip(22, "PSD header")?;
    let color_length = cursor.u32("color mode data length")? as usize;
    cursor.skip(color_length, "color mode data")?;
    let resources_length = cursor.u32("image resources length")? as usize;
    let resources = cursor.take(resources_length, "image resources")?;
    let mut result = ScanResult::default();
    scan_resources(resources, &mut result)?;
    let layer_mask_length = cursor.u32("layer and mask info length")? as usize;
    let layer_mask = cursor.take(layer_mask_length, "layer and mask info")?;
    result.layers = scan_layer_records(layer_mask)?;
    Ok(result)
}

/// Reads image resources and extracts 4000/4003 animation descriptors.
fn scan_resources(resources: &[u8], result: &mut ScanResult) -> Result<(), AnimationParseError> {
    let mut cursor = Cursor::new(resources);
    while cursor.remaining() > 0 {
        cursor.expect_bytes(b"8BIM", "image resource signature")?;
        let id = cursor.u16("image resource ID")?;
        let name_length = cursor.u8("image resource name length")? as usize;
        cursor.skip(name_length, "image resource name")?;
        if !(1 + name_length).is_multiple_of(2) {
            cursor.skip(1, "image resource name padding")?;
        }
        let data_length = cursor.u32("image resource length")? as usize;
        let data = cursor.take(data_length, "image resource data")?;
        if !data_length.is_multiple_of(2) {
            cursor.skip(1, "image resource data padding")?;
        }
        if (id == 4000 || id == 4003)
            && let Some(descriptor) = parse_animation_resource(data)?
        {
            result.animation_descriptors.push((id, descriptor));
            result.resource_ids.push(id);
        }
    }
    Ok(())
}

/// Parses one Photoshop animation resource body.
fn parse_animation_resource(data: &[u8]) -> Result<Option<Descriptor>, AnimationParseError> {
    let mut cursor = Cursor::new(data);
    if cursor.remaining() < 4 || cursor.peek_bytes(4)? != b"mani" {
        return Ok(None);
    }
    cursor.expect_bytes(b"mani", "animation resource")?;
    cursor.expect_bytes(b"IRFR", "animation resource")?;
    let section_length = cursor.u32("animation resource section length")? as usize;
    let section = cursor.take(section_length, "animation resource section")?;
    let mut section_cursor = Cursor::new(section);
    let mut descriptor = None;
    while section_cursor.remaining() > 0 {
        section_cursor.expect_bytes(b"8BIM", "animation subresource")?;
        let key = section_cursor.take(4, "animation subresource key")?;
        let payload_length = section_cursor.u32("animation descriptor length")? as usize;
        let payload = section_cursor.take(payload_length, "animation descriptor")?;
        if !payload_length.is_multiple_of(2) {
            section_cursor.skip(1, "animation descriptor padding")?;
        }
        if key == b"AnDs" {
            if descriptor.is_some() {
                return Err(AnimationParseError::InvalidData(
                    "duplicate AnDs animation descriptor".to_string(),
                ));
            }
            descriptor = Some(read_descriptor(payload, "animation descriptor")?);
        }
    }
    Ok(descriptor)
}

/// Walks PSD layer records and excludes bounding section dividers.
fn scan_layer_records(data: &[u8]) -> Result<Vec<RawLayer>, AnimationParseError> {
    if data.is_empty() {
        return Ok(Vec::new());
    }
    if data.len() < 4 {
        return Err(AnimationParseError::Truncated {
            section: "layer info length".to_string(),
            offset: 0,
        });
    }
    let mut outer = Cursor::new(data);
    let layer_info_length = outer.u32("layer info length")? as usize;
    if layer_info_length == 0 {
        return Ok(Vec::new());
    }
    let layer_info = outer.take(layer_info_length, "layer info")?;
    let mut cursor = Cursor::new(layer_info);
    let count = cursor.i16("layer count")?;
    let count = usize::from(count.unsigned_abs());
    let mut layers = Vec::with_capacity(count);
    for _ in 0..count {
        cursor.skip(16, "layer bounds")?;
        let channel_count = cursor.u16("layer channel count")? as usize;
        cursor.skip(
            channel_count.checked_mul(6).ok_or_else(|| {
                AnimationParseError::InvalidData("layer channel count overflow".to_string())
            })?,
            "layer channel records",
        )?;
        cursor.expect_bytes(b"8BIM", "layer blend signature")?;
        cursor.skip(4, "layer blend mode")?;
        cursor.skip(4, "layer opacity and flags")?;
        let extra_length = cursor.u32("layer extra data length")? as usize;
        let extra = cursor.take(extra_length, "layer extra data")?;
        let layer = scan_layer_extra(extra)?;
        if !layer.is_bounding_divider {
            layers.push(layer);
        }
    }
    Ok(layers)
}

/// Reads layer additional-info keys relevant to animation metadata.
fn scan_layer_extra(data: &[u8]) -> Result<RawLayer, AnimationParseError> {
    let mut cursor = Cursor::new(data);
    let mask_length = cursor.u32("layer mask length")? as usize;
    cursor.skip(mask_length, "layer mask")?;
    let blending_length = cursor.u32("layer blending ranges length")? as usize;
    cursor.skip(blending_length, "layer blending ranges")?;
    let name_length = cursor.u8("layer name length")? as usize;
    let name_bytes = (1 + name_length).div_ceil(4) * 4;
    cursor.skip(name_bytes.saturating_sub(1), "layer name")?;

    let mut layer = RawLayer::default();
    while cursor.remaining() > 0 {
        let signature = cursor.take(4, "layer additional-info signature")?;
        if signature != b"8BIM" && signature != b"8B64" {
            return Err(AnimationParseError::InvalidSignature {
                expected: "8BIM or 8B64".to_string(),
                offset: cursor.offset().saturating_sub(4),
            });
        }
        let key = cursor.take(4, "layer additional-info key")?;
        let length = cursor.u32("layer additional-info length")? as usize;
        let value = cursor.take(length, "layer additional-info value")?;
        if !length.is_multiple_of(2) {
            cursor.skip(1, "layer additional-info padding")?;
        }
        match key {
            b"lyid" => {
                if length != 4 {
                    return Err(AnimationParseError::InvalidData(
                        "lyid must contain four bytes".to_string(),
                    ));
                }
                let id = u32::from_be_bytes(value.try_into().expect("length checked"));
                if layer.id.replace(id).is_some() {
                    return Err(AnimationParseError::DuplicateId {
                        kind: "layer".to_string(),
                        id,
                    });
                }
            }
            b"shmd" => {
                if layer.shmd.is_some() {
                    return Err(AnimationParseError::InvalidData(
                        "duplicate shmd layer metadata".to_string(),
                    ));
                }
                layer.shmd = Some(parse_shmd(value)?);
            }
            b"mdyn" => {
                if layer.flags.is_some() {
                    return Err(AnimationParseError::InvalidData(
                        "duplicate mdyn layer metadata".to_string(),
                    ));
                }
                layer.flags = Some(parse_mdyn(value)?);
            }
            b"lsct" | b"lsdk" if length >= 4 => {
                let divider_type =
                    u32::from_be_bytes(value[..4].try_into().expect("length checked"));
                layer.is_bounding_divider = divider_type == 3;
            }
            _ => {}
        }
    }
    Ok(layer)
}

/// Parses the nested shmd metadata record collection.
fn parse_shmd(data: &[u8]) -> Result<LayerMetadata, AnimationParseError> {
    let mut cursor = Cursor::new(data);
    let count = cursor.u32("shmd record count")? as usize;
    let mut metadata = LayerMetadata::default();
    for _ in 0..count {
        cursor.expect_bytes(b"8BIM", "shmd record signature")?;
        let key = cursor.take(4, "shmd record key")?;
        cursor.skip(4, "shmd record copy flags")?;
        let length = cursor.u32("shmd record length")? as usize;
        let payload = cursor.take(length, "shmd record payload")?;
        if !length.is_multiple_of(2) {
            cursor.skip(1, "shmd record padding")?;
        }
        match key {
            b"mlst" => parse_mlst(payload, &mut metadata)?,
            b"mdyn" => {
                if metadata.flags.is_some() {
                    return Err(AnimationParseError::InvalidData(
                        "duplicate mdyn layer metadata".to_string(),
                    ));
                }
                metadata.flags = Some(parse_mdyn(payload)?);
            }
            _ => {}
        }
    }
    Ok(metadata)
}

/// Converts one mlst descriptor into strict per-frame records.
fn parse_mlst(data: &[u8], metadata: &mut LayerMetadata) -> Result<(), AnimationParseError> {
    let descriptor = read_descriptor(data, "mlst descriptor")?;
    let list = descriptor_list(&descriptor, "LaSt")?;
    for item in list {
        let frame_descriptor = item_descriptor(item, "LaSt frame")?;
        let frame_ids = descriptor_integer_list(frame_descriptor, "FrLs")?;
        if frame_ids.is_empty() {
            return Err(AnimationParseError::InvalidData(
                "LaSt frame has no FrLs IDs".to_string(),
            ));
        }
        let enable = descriptor_bool(frame_descriptor, "enab")?;
        let offset = descriptor_point(frame_descriptor, "Ofst")?;
        let reference_point = descriptor_point(frame_descriptor, "FXRf")?;
        let opacity = descriptor_opacity(frame_descriptor)?;
        for frame_id in frame_ids {
            if metadata
                .frames
                .iter()
                .any(|frame| frame.frame_id == frame_id)
            {
                return Err(AnimationParseError::DuplicateId {
                    kind: "layer frame".to_string(),
                    id: frame_id,
                });
            }
            metadata.frames.push(RawFrameState {
                frame_id,
                enable,
                offset,
                reference_point,
                opacity,
            });
        }
    }
    Ok(())
}

/// Parses mdyn propagation and unification flags.
fn parse_mdyn(data: &[u8]) -> Result<AnimationFrameFlags, AnimationParseError> {
    let mut cursor = Cursor::new(data);
    cursor.skip(2, "mdyn version")?;
    let propagate = cursor.u8("mdyn propagate")? != 0;
    let flags = cursor.u8("mdyn flags")?;
    Ok(AnimationFrameFlags {
        propagate_frame_one: Some(!propagate),
        unify_layer_position: Some(flags & 1 != 0),
        unify_layer_style: Some(flags & 2 != 0),
        unify_layer_visibility: Some(flags & 4 != 0),
    })
}

/// Parses one descriptor inside a bounded byte section.
fn read_descriptor(data: &[u8], section: &str) -> Result<Descriptor, AnimationParseError> {
    let mut reader = PsdReader::new(data, None, None);
    let descriptor = read_version_and_descriptor(&mut reader)
        .map_err(|error| AnimationParseError::InvalidData(format!("{section}: {error}")))?;
    if reader.offset > data.len() {
        return Err(AnimationParseError::Truncated {
            section: section.to_string(),
            offset: reader.offset,
        });
    }
    Ok(descriptor)
}

/// Validates and normalizes the global frame catalog and playback set.
fn merge_animation_descriptors(
    descriptors: &[(u16, Descriptor)],
) -> Result<AnimationCatalog, AnimationParseError> {
    if descriptors.len() > 1 {
        return Err(AnimationParseError::InvalidData(
            "multiple animation resources are ambiguous".to_string(),
        ));
    }
    let (_, descriptor) = &descriptors[0];
    let frames = descriptor_list(descriptor, "FrIn")?
        .iter()
        .map(|item| parse_catalog_frame(item_descriptor(item, "FrIn frame")?))
        .collect::<Result<Vec<_>, _>>()?;
    let set_values = descriptor_list(descriptor, "FSts")?;
    if set_values.len() > 1 {
        return Err(AnimationParseError::InvalidData(
            "multiple animation sets are ambiguous".to_string(),
        ));
    }
    let (loop_mode, active_frame_index) = if let Some(item) = set_values.first() {
        let set = item_descriptor(item, "FSts set")?;
        let set_frame_ids = descriptor_integer_list(set, "FsFr")?;
        let catalog_ids = frames.iter().map(|frame| frame.id).collect::<Vec<_>>();
        if set_frame_ids != catalog_ids {
            return Err(AnimationParseError::InvalidData(
                "animation set frame order differs from FrIn".to_string(),
            ));
        }
        let loop_count = descriptor_number(set, "LCnt")?.ok_or_else(|| {
            AnimationParseError::InvalidData("animation set has no LCnt loop value".to_string())
        })?;
        let loop_mode = Some(if loop_count == 0.0 {
            LoopMode::Infinite
        } else {
            LoopMode::Finite(number_to_u32(loop_count, "LCnt")?)
        });
        let active = descriptor_number(set, "AFrm")?
            .map(|value| number_to_u32(value, "AFrm"))
            .transpose()?;
        (loop_mode, active)
    } else {
        (None, None)
    };
    Ok(AnimationCatalog {
        frames,
        loop_mode,
        active_frame_index,
    })
}

/// Normalizes one FrIn descriptor into a frame record.
fn parse_catalog_frame(descriptor: &Descriptor) -> Result<PhotoshopFrame, AnimationParseError> {
    let id = number_to_u32(
        descriptor_number(descriptor, "FrID")?
            .ok_or_else(|| AnimationParseError::InvalidData("frame has no FrID".to_string()))?,
        "FrID",
    )?;
    let delay = descriptor_number(descriptor, "FrDl")?.ok_or_else(|| {
        AnimationParseError::InvalidData(format!("frame {id} has no FrDl duration"))
    })?;
    let duration_ms = number_to_u32(delay * 10.0, "FrDl")?;
    let dispose = descriptor_enum(descriptor, "FrDs")?;
    Ok(PhotoshopFrame {
        id,
        duration_ms,
        dispose,
    })
}

/// Indexes raw layer records by strict Photoshop IDs.
fn index_raw_layers(layers: Vec<RawLayer>) -> Result<HashMap<u32, RawLayer>, AnimationParseError> {
    let mut indexed = HashMap::with_capacity(layers.len());
    for layer in layers {
        let id = layer
            .id
            .ok_or_else(|| AnimationParseError::MissingLayerId {
                path: "<raw layer>".to_string(),
            })?;
        if indexed.insert(id, layer).is_some() {
            return Err(AnimationParseError::DuplicateId {
                kind: "layer".to_string(),
                id,
            });
        }
    }
    Ok(indexed)
}

/// Resolves enable inheritance and optional per-frame properties.
fn resolve_layer_states(
    layer: &AnimationLayerInput,
    raw: &RawLayer,
    frames: &[PhotoshopFrame],
) -> Result<LayerAnimationState, AnimationParseError> {
    let has_animation_records = raw
        .shmd
        .as_ref()
        .is_some_and(|metadata| !metadata.frames.is_empty());
    let mut previous_enabled = !layer.hidden;
    let states = frames
        .iter()
        .map(|frame| {
            let record = raw.shmd.as_ref().and_then(|metadata| {
                metadata
                    .frames
                    .iter()
                    .find(|item| item.frame_id == frame.id)
            });
            let record_present = record.is_some();
            let explicit_enable = record.and_then(|item| item.enable).is_some();
            if layer.is_group && !layer.is_container_group && has_animation_records {
                previous_enabled = record.and_then(|item| item.enable).unwrap_or(false);
            } else if record_present {
                if let Some(enable) = record.and_then(|item| item.enable) {
                    previous_enabled = enable;
                }
            } else if has_animation_records {
                previous_enabled = false;
            }
            LayerFrameState {
                frame_id: frame.id,
                record_present,
                enabled: previous_enabled,
                explicit_enable,
                offset: record.and_then(|item| item.offset),
                reference_point: record.and_then(|item| item.reference_point),
                opacity: record.and_then(|item| item.opacity),
            }
        })
        .collect();
    Ok(LayerAnimationState {
        layer_id: layer.id,
        path: layer.path.clone(),
        frames: states,
    })
}

/// Applies layer and ancestor-group visibility to pixel leaves.
fn resolve_visible_layers(
    layers: &[AnimationLayerInput],
    states: &[LayerAnimationState],
    frames: &[PhotoshopFrame],
) -> Vec<VisibleFrameLayers> {
    frames
        .iter()
        .map(|frame| {
            let layer_ids = layers
                .iter()
                .zip(states)
                .filter(|(layer, state)| {
                    !layer.is_group
                        && state_for_frame(state, frame.id).is_some_and(|state| {
                            state.enabled
                                && layer.ancestor_ids.iter().all(|ancestor_id| {
                                    states
                                        .iter()
                                        .find(|candidate| candidate.layer_id == *ancestor_id)
                                        .and_then(|candidate| state_for_frame(candidate, frame.id))
                                        .is_some_and(|ancestor| ancestor.enabled)
                                })
                        })
                })
                .map(|(layer, _)| layer.id)
                .collect();
            VisibleFrameLayers {
                frame_id: frame.id,
                layer_ids,
            }
        })
        .collect()
}

/// Finds a normalized frame state by frame ID.
fn state_for_frame(state: &LayerAnimationState, frame_id: u32) -> Option<&LayerFrameState> {
    state.frames.iter().find(|frame| frame.frame_id == frame_id)
}

/// Converts the ag-psd flag container into the format-neutral model.
fn normalize_flags(flags: &AnimationFrameFlags) -> AnimationFlags {
    AnimationFlags {
        propagate_frame_one: flags.propagate_frame_one.unwrap_or(false),
        unify_layer_position: flags.unify_layer_position.unwrap_or(false),
        unify_layer_style: flags.unify_layer_style.unwrap_or(false),
        unify_layer_visibility: flags.unify_layer_visibility.unwrap_or(false),
    }
}

/// Gets a descriptor list while rejecting missing or wrongly typed fields.
fn descriptor_list<'a>(
    descriptor: &'a Descriptor,
    key: &str,
) -> Result<&'a Vec<DescriptorValue>, AnimationParseError> {
    match descriptor.get(key) {
        Some(DescriptorValue::List(items)) => Ok(items),
        Some(_) => Err(AnimationParseError::InvalidData(format!(
            "descriptor field {key} is not a list"
        ))),
        None => Err(AnimationParseError::InvalidData(format!(
            "descriptor field {key} is missing"
        ))),
    }
}

/// Gets a nested descriptor while rejecting wrongly typed fields.
fn item_descriptor<'a>(
    value: &'a DescriptorValue,
    field: &str,
) -> Result<&'a Descriptor, AnimationParseError> {
    match value {
        DescriptorValue::Descriptor(descriptor) => Ok(descriptor),
        _ => Err(AnimationParseError::InvalidData(format!(
            "{field} entry is not a descriptor"
        ))),
    }
}

/// Converts a descriptor list of integral values into normalized IDs.
fn descriptor_integer_list(
    descriptor: &Descriptor,
    key: &str,
) -> Result<Vec<u32>, AnimationParseError> {
    descriptor_list(descriptor, key)?
        .iter()
        .map(|value| match value {
            DescriptorValue::Integer(value) => number_to_u32(*value as f64, key),
            DescriptorValue::Double(value) => number_to_u32(*value, key),
            _ => Err(AnimationParseError::InvalidData(format!(
                "descriptor field {key} contains a non-number"
            ))),
        })
        .collect()
}

/// Reads an optional numeric descriptor field.
fn descriptor_number(
    descriptor: &Descriptor,
    key: &str,
) -> Result<Option<f64>, AnimationParseError> {
    match descriptor.get(key) {
        None => Ok(None),
        Some(DescriptorValue::Integer(value)) => Ok(Some(*value as f64)),
        Some(DescriptorValue::Double(value)) => Ok(Some(*value)),
        Some(DescriptorValue::UnitDouble(value)) => Ok(Some(value.value)),
        Some(_) => Err(AnimationParseError::InvalidData(format!(
            "descriptor field {key} is not numeric"
        ))),
    }
}

/// Reads an optional boolean descriptor field.
fn descriptor_bool(
    descriptor: &Descriptor,
    key: &str,
) -> Result<Option<bool>, AnimationParseError> {
    match descriptor.get(key) {
        None => Ok(None),
        Some(DescriptorValue::Boolean(value)) => Ok(Some(*value)),
        Some(_) => Err(AnimationParseError::InvalidData(format!(
            "descriptor field {key} is not boolean"
        ))),
    }
}

/// Reads an optional enum descriptor field.
fn descriptor_enum(
    descriptor: &Descriptor,
    key: &str,
) -> Result<Option<String>, AnimationParseError> {
    match descriptor.get(key) {
        None => Ok(None),
        Some(DescriptorValue::Enum(value)) => Ok(Some(value.clone())),
        Some(_) => Err(AnimationParseError::InvalidData(format!(
            "descriptor field {key} is not enum"
        ))),
    }
}

/// Reads the normalized opacity stored in a layer-frame blendOptions object.
fn descriptor_opacity(descriptor: &Descriptor) -> Result<Option<f64>, AnimationParseError> {
    let Some(value) = descriptor.get("blendOptions") else {
        return Ok(None);
    };
    let blend_options = item_descriptor(value, "blendOptions")?;
    Ok(descriptor_number(blend_options, "Opct")?.map(|value| value / 100.0))
}

/// Reads a layer-frame offset or reference point descriptor.
fn descriptor_point(
    descriptor: &Descriptor,
    key: &str,
) -> Result<Option<AnimationPoint>, AnimationParseError> {
    let Some(value) = descriptor.get(key) else {
        return Ok(None);
    };
    let point = item_descriptor(value, key)?;
    let x = descriptor_number(point, "Hrzn")?.ok_or_else(|| {
        AnimationParseError::InvalidData(format!("{key} point has no Hrzn value"))
    })?;
    let y = descriptor_number(point, "Vrtc")?.ok_or_else(|| {
        AnimationParseError::InvalidData(format!("{key} point has no Vrtc value"))
    })?;
    Ok(Some(AnimationPoint { x, y }))
}

/// Converts a PSD number into a checked normalized u32.
fn number_to_u32(value: f64, field: &str) -> Result<u32, AnimationParseError> {
    if !value.is_finite() || value < 0.0 || value.fract() != 0.0 || value > u32::MAX as f64 {
        return Err(AnimationParseError::InvalidData(format!(
            "{field} is not a non-negative integer: {value}"
        )));
    }
    Ok(value as u32)
}

#[derive(Debug, Clone, Copy)]
struct Cursor<'a> {
    bytes: &'a [u8],
    position: usize,
}

impl<'a> Cursor<'a> {
    /// Creates a cursor bounded to one PSD section.
    fn new(bytes: &'a [u8]) -> Self {
        Self { bytes, position: 0 }
    }

    /// Returns the unread byte count.
    fn remaining(&self) -> usize {
        self.bytes.len().saturating_sub(self.position)
    }

    /// Returns the current section-relative offset.
    fn offset(&self) -> usize {
        self.position
    }

    /// Takes a checked byte range from this section.
    fn take(&mut self, length: usize, section: &str) -> Result<&'a [u8], AnimationParseError> {
        let end = self.position.checked_add(length).ok_or_else(|| {
            AnimationParseError::InvalidSection {
                section: section.to_string(),
                offset: self.position,
            }
        })?;
        if end > self.bytes.len() {
            return Err(AnimationParseError::Truncated {
                section: section.to_string(),
                offset: self.position,
            });
        }
        let value = &self.bytes[self.position..end];
        self.position = end;
        Ok(value)
    }

    /// Advances over a checked byte range.
    fn skip(&mut self, length: usize, section: &str) -> Result<(), AnimationParseError> {
        self.take(length, section).map(|_| ())
    }

    /// Peeks at a checked byte range without advancing.
    fn peek_bytes(&self, length: usize) -> Result<&'a [u8], AnimationParseError> {
        let end = self.position.checked_add(length).ok_or_else(|| {
            AnimationParseError::InvalidSection {
                section: "peek".to_string(),
                offset: self.position,
            }
        })?;
        self.bytes
            .get(self.position..end)
            .ok_or_else(|| AnimationParseError::Truncated {
                section: "peek".to_string(),
                offset: self.position,
            })
    }

    /// Checks and consumes an exact signature.
    fn expect_bytes(&mut self, expected: &[u8], section: &str) -> Result<(), AnimationParseError> {
        let offset = self.position;
        if self.take(expected.len(), section)? != expected {
            return Err(AnimationParseError::InvalidSignature {
                expected: String::from_utf8_lossy(expected).into_owned(),
                offset,
            });
        }
        Ok(())
    }

    /// Reads a bounded big-endian byte.
    fn u8(&mut self, section: &str) -> Result<u8, AnimationParseError> {
        Ok(self.take(1, section)?[0])
    }

    /// Reads a bounded big-endian unsigned 16-bit value.
    fn u16(&mut self, section: &str) -> Result<u16, AnimationParseError> {
        Ok(u16::from_be_bytes(
            self.take(2, section)?.try_into().expect("length checked"),
        ))
    }

    /// Reads a bounded big-endian signed 16-bit value.
    fn i16(&mut self, section: &str) -> Result<i16, AnimationParseError> {
        Ok(i16::from_be_bytes(
            self.take(2, section)?.try_into().expect("length checked"),
        ))
    }

    /// Reads a bounded big-endian unsigned 32-bit value.
    fn u32(&mut self, section: &str) -> Result<u32, AnimationParseError> {
        Ok(u32::from_be_bytes(
            self.take(4, section)?.try_into().expect("length checked"),
        ))
    }
}

#[cfg(test)]
#[cfg(test)]
mod tests;
