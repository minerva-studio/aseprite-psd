//! Private PSD metadata used to recognize converter-owned cel materialization.

use std::collections::{BTreeMap, BTreeSet};

use crate::InspectionError;

const MARKER_KEY: &[u8; 4] = b"p2rt";
const MARKER_MAGIC: &[u8; 4] = b"P2RT";
const LEGACY_MARKER_VERSION: u16 = 1;
const FRAME_GROUP_MARKER_VERSION: u16 = 2;
const MARKER_SIZE: usize = 20;

/// The role of one layer in a converter-owned cel materialization.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum MarkerRole {
    Wrapper,
    Variant,
    FrameGroup,
    LayerCopy,
}

/// Identifies one converter-owned materialized cel layer.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct LayerMarker {
    pub(crate) version: u16,
    pub(crate) role: MarkerRole,
    pub(crate) logical_layer_id: u32,
    pub(crate) variant_index: u32,
    pub(crate) variant_count: u32,
}

/// Result of validating all round-trip markers found in one PSD/PSB.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct RoundTripStatus {
    /// Whether at least one marker block was present.
    pub marked: bool,
    /// Whether all marker blocks formed complete wrapper/variant sets.
    pub valid: bool,
}

/// Detailed converter-owned marker classification used by round-trip conversion.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub(crate) struct RoundTripLayout {
    /// Aggregate marker status retained for public inspection.
    pub(crate) status: RoundTripStatus,
    /// Marker protocol version when all markers use one known version.
    pub(crate) version: Option<u16>,
    /// Declared frame count for v2 frame-group markers.
    pub(crate) frame_count: Option<u32>,
}

/// Encodes one marker payload for a PSD additional-info block.
pub(crate) fn encode_marker(marker: LayerMarker) -> Vec<u8> {
    let role = match marker.role {
        MarkerRole::Wrapper => 1,
        MarkerRole::Variant => 2,
        MarkerRole::FrameGroup => 3,
        MarkerRole::LayerCopy => 4,
    };
    let mut payload = Vec::with_capacity(MARKER_SIZE);
    payload.extend_from_slice(MARKER_MAGIC);
    payload.extend_from_slice(&marker.version.to_be_bytes());
    payload.push(role);
    payload.push(0);
    payload.extend_from_slice(&marker.logical_layer_id.to_be_bytes());
    payload.extend_from_slice(&marker.variant_index.to_be_bytes());
    payload.extend_from_slice(&marker.variant_count.to_be_bytes());
    payload
}

/// Decodes one marker payload, rejecting unknown versions and malformed roles.
pub(crate) fn decode_marker(data: &[u8]) -> Option<LayerMarker> {
    if data.len() != MARKER_SIZE || &data[..4] != MARKER_MAGIC {
        return None;
    }
    let version = u16::from_be_bytes(data[4..6].try_into().ok()?);
    if data[7] != 0 {
        return None;
    }
    let role = match data[6] {
        1 if version == LEGACY_MARKER_VERSION => MarkerRole::Wrapper,
        2 if version == LEGACY_MARKER_VERSION => MarkerRole::Variant,
        3 if version == FRAME_GROUP_MARKER_VERSION => MarkerRole::FrameGroup,
        4 if version == FRAME_GROUP_MARKER_VERSION => MarkerRole::LayerCopy,
        _ => return None,
    };
    Some(LayerMarker {
        version,
        role,
        logical_layer_id: u32::from_be_bytes(data[8..12].try_into().ok()?),
        variant_index: u32::from_be_bytes(data[12..16].try_into().ok()?),
        variant_count: u32::from_be_bytes(data[16..20].try_into().ok()?),
    })
}

/// Scans PSD/PSB layer records and validates converter-owned marker sets.
pub fn inspect(bytes: &[u8]) -> Result<RoundTripStatus, InspectionError> {
    Ok(inspect_detailed(bytes)?.status)
}

/// Scans markers and reports whether the document carries a coherent protocol layout.
pub(crate) fn inspect_detailed(bytes: &[u8]) -> Result<RoundTripLayout, InspectionError> {
    if bytes.len() < 34 || &bytes[..4] != b"8BPS" {
        return Err(InspectionError::PsdRead("invalid PSD header".to_string()));
    }
    let version = read_u16(bytes, 4)?;
    if version != 1 && version != 2 {
        return Err(InspectionError::PsdRead(format!(
            "unsupported Photoshop document version: {version}"
        )));
    }
    let psb = version == 2;
    let color_length = read_u32(bytes, 26)? as usize;
    let resources_start = 30usize
        .checked_add(4)
        .and_then(|value| value.checked_add(color_length))
        .ok_or_else(|| InspectionError::PsdRead("PSD color section overflow".to_string()))?;
    let resources_length = read_u32(bytes, resources_start - 4)? as usize;
    let layer_length_offset = resources_start
        .checked_add(resources_length)
        .ok_or_else(|| InspectionError::PsdRead("PSD resource section overflow".to_string()))?;
    let layer_info_start = layer_length_offset
        .checked_add(if psb { 8 } else { 4 })
        .ok_or_else(|| InspectionError::PsdRead("PSD layer section overflow".to_string()))?;
    let layer_mask_length = read_length(bytes, layer_length_offset, psb)?;
    let layer_mask_end = layer_info_start
        .checked_add(layer_mask_length)
        .ok_or_else(|| InspectionError::PsdRead("PSD layer section overflow".to_string()))?;
    let layer_info_length = read_length(bytes, layer_info_start, psb)?;
    let mut cursor = layer_info_start
        .checked_add(if psb { 8 } else { 4 })
        .ok_or_else(|| InspectionError::PsdRead("PSD layer records overflow".to_string()))?;
    let layer_info_end = cursor
        .checked_add(layer_info_length)
        .ok_or_else(|| InspectionError::PsdRead("PSD layer records overflow".to_string()))?;
    if layer_info_end > layer_mask_end || layer_info_end > bytes.len() {
        return Err(InspectionError::PsdRead(
            "PSD layer records are truncated".to_string(),
        ));
    }
    let count = read_i16(bytes, cursor)?.unsigned_abs() as usize;
    cursor += 2;
    let mut markers = Vec::new();
    for _ in 0..count {
        cursor = cursor
            .checked_add(16)
            .ok_or_else(|| InspectionError::PsdRead("PSD layer record overflow".to_string()))?;
        let channels = read_u16(bytes, cursor)? as usize;
        cursor += 2;
        let channel_width = if psb { 8 } else { 4 };
        cursor = cursor
            .checked_add(channels.checked_mul(2 + channel_width).ok_or_else(|| {
                InspectionError::PsdRead("PSD channel section overflow".to_string())
            })?)
            .and_then(|value| value.checked_add(12))
            .ok_or_else(|| InspectionError::PsdRead("PSD layer record overflow".to_string()))?;
        let extra_length = read_u32(bytes, cursor)? as usize;
        cursor += 4;
        let extra_end = cursor
            .checked_add(extra_length)
            .ok_or_else(|| InspectionError::PsdRead("PSD layer extra-data overflow".to_string()))?;
        if extra_end > layer_info_end {
            return Err(InspectionError::PsdRead(
                "PSD layer extra-data is truncated".to_string(),
            ));
        }
        markers.extend(scan_extra(&bytes[cursor..extra_end]));
        cursor = extra_end;
    }

    let marked = !markers.is_empty();
    if !marked {
        return Ok(RoundTripLayout {
            status: RoundTripStatus {
                marked: false,
                valid: true,
            },
            ..Default::default()
        });
    }
    let versions = markers
        .iter()
        .map(|marker| marker.version)
        .collect::<BTreeSet<_>>();
    if versions.len() != 1 {
        return Ok(RoundTripLayout {
            status: RoundTripStatus {
                marked: true,
                valid: false,
            },
            version: None,
            frame_count: None,
        });
    }
    let version = versions.iter().next().copied();
    if version == Some(FRAME_GROUP_MARKER_VERSION) {
        let mut frame_count = None;
        let mut frames = BTreeSet::new();
        let mut copies = BTreeMap::<u32, BTreeSet<u32>>::new();
        let mut valid = true;
        for marker in markers {
            if marker.variant_count == 0 || marker.logical_layer_id == 0 {
                valid = false;
                continue;
            }
            if marker.variant_index == 0 || marker.variant_index > marker.variant_count {
                valid = false;
            }
            if let Some(existing) = frame_count {
                valid &= existing == marker.variant_count;
            } else {
                frame_count = Some(marker.variant_count);
            }
            match marker.role {
                MarkerRole::FrameGroup => {
                    valid &= frames.insert(marker.variant_index);
                }
                MarkerRole::LayerCopy => {
                    valid &= copies
                        .entry(marker.logical_layer_id)
                        .or_default()
                        .insert(marker.variant_index);
                }
                _ => valid = false,
            }
        }
        if let Some(count) = frame_count {
            valid &=
                frames.len() == count as usize && (1..=count).all(|index| frames.contains(&index));
            valid &= !copies.is_empty();
            valid &= copies.values().all(|indices| {
                indices.len() == count as usize && (1..=count).all(|index| indices.contains(&index))
            });
        } else {
            valid = false;
        }
        return Ok(RoundTripLayout {
            status: RoundTripStatus {
                marked: true,
                valid,
            },
            version,
            frame_count,
        });
    }
    let mut wrappers = BTreeMap::<u32, u32>::new();
    let mut variants = BTreeMap::<u32, BTreeSet<u32>>::new();
    let mut valid = true;
    for marker in markers {
        if marker.variant_count == 0 || marker.logical_layer_id == 0 {
            valid = false;
            continue;
        }
        match marker.role {
            MarkerRole::Wrapper => {
                valid &= marker.variant_index == 0
                    && wrappers
                        .insert(marker.logical_layer_id, marker.variant_count)
                        .is_none();
            }
            MarkerRole::Variant => {
                valid &= marker.variant_index > 0
                    && marker.variant_index <= marker.variant_count
                    && variants
                        .entry(marker.logical_layer_id)
                        .or_default()
                        .insert(marker.variant_index);
            }
            _ => valid = false,
        }
    }
    valid &= wrappers.iter().all(|(id, count)| {
        variants.get(id).is_some_and(|indices| {
            indices.len() == *count as usize && (1..=*count).all(|index| indices.contains(&index))
        })
    });
    valid &= variants.keys().all(|id| wrappers.contains_key(id));
    Ok(RoundTripLayout {
        status: RoundTripStatus { marked, valid },
        version,
        frame_count: None,
    })
}

fn scan_extra(extra: &[u8]) -> Vec<LayerMarker> {
    let mut markers = Vec::new();
    if extra.len() < 9 {
        return markers;
    }
    let name_length = extra[8] as usize;
    let mut cursor = (9usize.saturating_add(name_length) + 3) & !3;
    while cursor + 12 <= extra.len() {
        let signature = &extra[cursor..cursor + 4];
        let key = &extra[cursor + 4..cursor + 8];
        cursor += 8;
        let length = if signature == b"8B64" {
            let Some(value) = extra.get(cursor..cursor + 8) else {
                break;
            };
            cursor += 8;
            u64::from_be_bytes(value.try_into().expect("length checked")) as usize
        } else {
            let Some(value) = extra.get(cursor..cursor + 4) else {
                break;
            };
            cursor += 4;
            u32::from_be_bytes(value.try_into().expect("length checked")) as usize
        };
        let Some(end) = cursor.checked_add(length) else {
            break;
        };
        if end > extra.len() {
            break;
        }
        if key == MARKER_KEY {
            if let Some(marker) = decode_marker(&extra[cursor..end]) {
                markers.push(marker);
            } else {
                markers.push(LayerMarker {
                    version: 0,
                    role: MarkerRole::Wrapper,
                    logical_layer_id: 0,
                    variant_index: 0,
                    variant_count: 0,
                });
            }
        }
        cursor = (end + 1) & !1;
    }
    markers
}

fn read_u16(bytes: &[u8], offset: usize) -> Result<u16, InspectionError> {
    bytes
        .get(offset..offset + 2)
        .and_then(|value| value.try_into().ok())
        .map(u16::from_be_bytes)
        .ok_or_else(|| InspectionError::PsdRead("PSD field is truncated".to_string()))
}

fn read_i16(bytes: &[u8], offset: usize) -> Result<i16, InspectionError> {
    read_u16(bytes, offset).map(|value| i16::from_be_bytes(value.to_be_bytes()))
}

fn read_u32(bytes: &[u8], offset: usize) -> Result<u32, InspectionError> {
    bytes
        .get(offset..offset + 4)
        .and_then(|value| value.try_into().ok())
        .map(u32::from_be_bytes)
        .ok_or_else(|| InspectionError::PsdRead("PSD field is truncated".to_string()))
}

fn read_length(bytes: &[u8], offset: usize, psb: bool) -> Result<usize, InspectionError> {
    if psb {
        let value = bytes
            .get(offset..offset + 8)
            .and_then(|value| value.try_into().ok())
            .map(u64::from_be_bytes)
            .ok_or_else(|| InspectionError::PsdRead("PSD length is truncated".to_string()))?;
        usize::try_from(value)
            .map_err(|_| InspectionError::PsdRead("PSD length exceeds platform".to_string()))
    } else {
        Ok(read_u32(bytes, offset)? as usize)
    }
}
