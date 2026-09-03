use super::*;

use std::fs;
use std::io::Cursor;
use std::path::Path;
use std::time::{SystemTime, UNIX_EPOCH};

#[test]
fn missing_layer_ids_use_stable_name_independent_fallbacks() {
    let first = layer_id(None, "0/1").expect("fallback id");
    let repeated = layer_id(None, "0/1").expect("fallback id");
    let sibling = layer_id(None, "0/2").expect("fallback id");
    assert_eq!(first, repeated);
    assert_ne!(first, sibling);
}

#[test]
fn sixteen_and_thirty_two_bit_documents_are_accepted_by_normalization() {
    assert!(validate_normalization_bit_depth(Some(32.0)).is_ok());
    assert!(validate_normalization_bit_depth(Some(8.0)).is_ok());
    assert!(validate_normalization_bit_depth(Some(16.0)).is_ok());
}

#[test]
fn non_empty_layers_without_pixels_are_rejected() {
    let layer = ag_psd::psd::Layer {
        top: Some(0.0),
        left: Some(0.0),
        bottom: Some(2.0),
        right: Some(3.0),
        ..Default::default()
    };
    let error = build_layer(&layer, &["0".to_string()])
        .expect_err("non-empty layers must not become transparent placeholders");
    assert!(
        error
            .to_string()
            .contains("non-empty pixel layer has no RGBA8 data at 0")
    );
}

#[test]
fn zero_area_layers_without_pixels_remain_empty() {
    let layer = ag_psd::psd::Layer {
        top: Some(4.0),
        left: Some(3.0),
        bottom: Some(4.0),
        right: Some(3.0),
        ..Default::default()
    };
    let normalized = build_layer(&layer, &["0".to_string()]).expect("empty layer");
    let pixels = normalized.pixels.expect("empty pixel buffer");
    assert_eq!((pixels.width, pixels.height), (0, 0));
    assert!(pixels.data.is_empty());
}

/// Builds a small PSD fixture with a bitmap user mask and known alpha values.
fn bitmap_mask_fixture(clipping: bool, parameterized: bool) -> ag_psd::psd::Psd {
    let mut layer = ag_psd::psd::Layer::default();
    layer.additional_info.name = Some("Masked layer".to_string());
    layer.additional_info.id = Some(41.0);
    layer.top = Some(0.0);
    layer.left = Some(0.0);
    layer.bottom = Some(1.0);
    layer.right = Some(2.0);
    layer.clipping = Some(clipping);
    layer.image_data = Some(ag_psd::psd::PixelData {
        width: 2,
        height: 1,
        data: vec![10, 20, 30, 200, 40, 50, 60, 100],
    });
    layer.additional_info.mask = Some(ag_psd::psd::LayerMaskData {
        top: Some(0.0),
        left: Some(0.0),
        bottom: Some(1.0),
        right: Some(2.0),
        default_color: Some(0.0),
        image_data: Some(ag_psd::psd::PixelData {
            width: 2,
            height: 1,
            data: vec![64, 64, 64, 255, 128, 128, 128, 255],
        }),
        user_mask_density: parameterized.then_some(0.5),
        ..Default::default()
    });
    ag_psd::psd::Psd {
        width: 2.0,
        height: 1.0,
        channels: Some(4.0),
        bits_per_channel: Some(8.0),
        color_mode: Some(ag_psd::psd::ColorMode::Rgb),
        children: Some(vec![layer]),
        ..Default::default()
    }
}

/// Writes a PSD fixture through the real ag-psd serializer.
fn write_psd_fixture(path: &Path, psd: &ag_psd::psd::Psd) {
    fs::write(path, ag_psd::write_psd(psd, &Default::default())).expect("write PSD fixture");
}

/// Builds a minimal PSD directly from the PSD specification's binary sections.
///
/// This deliberately does not use `ag-psd` to write the fixture. It contains a
/// two-pixel RGBA layer, a raw `-2` user-mask channel, a layer ID block, and an
/// optional clipping flag so parser and conversion tests have an independent
/// PSD input source.
fn psd_spec_mask_fixture(clipping: bool) -> Vec<u8> {
    let mut bytes = Vec::new();
    bytes.extend_from_slice(b"8BPS");
    bytes.extend_from_slice(&1u16.to_be_bytes());
    bytes.extend_from_slice(&[0; 6]);
    bytes.extend_from_slice(&4u16.to_be_bytes());
    bytes.extend_from_slice(&1u32.to_be_bytes());
    bytes.extend_from_slice(&2u32.to_be_bytes());
    bytes.extend_from_slice(&8u16.to_be_bytes());
    bytes.extend_from_slice(&3u16.to_be_bytes());

    // Color mode data and image resources are empty in this valid RGB8 PSD.
    bytes.extend_from_slice(&0u32.to_be_bytes());
    bytes.extend_from_slice(&0u32.to_be_bytes());

    let mut layer_info = Vec::new();
    layer_info.extend_from_slice(&1i16.to_be_bytes());
    layer_info.extend_from_slice(&0i32.to_be_bytes());
    layer_info.extend_from_slice(&0i32.to_be_bytes());
    layer_info.extend_from_slice(&1i32.to_be_bytes());
    layer_info.extend_from_slice(&2i32.to_be_bytes());
    layer_info.extend_from_slice(&5u16.to_be_bytes());
    for id in [0i16, 1, 2, -1, -2] {
        layer_info.extend_from_slice(&id.to_be_bytes());
        layer_info.extend_from_slice(&4u32.to_be_bytes());
    }
    layer_info.extend_from_slice(b"8BIMnorm");
    layer_info.push(255);
    layer_info.push(u8::from(clipping));
    layer_info.push(0x08);
    layer_info.push(0);

    let mut extra = Vec::new();
    let mut mask_data = Vec::new();
    for value in [0i32, 0, 1, 2] {
        mask_data.extend_from_slice(&value.to_be_bytes());
    }
    mask_data.push(0);
    mask_data.push(0);
    mask_data.extend_from_slice(&[0, 0]);
    extra.extend_from_slice(&(mask_data.len() as u32).to_be_bytes());
    extra.extend_from_slice(&mask_data);
    extra.extend_from_slice(&8u32.to_be_bytes());
    extra.extend_from_slice(&[0; 8]);

    let name = b"PSD spec mask";
    extra.push(name.len() as u8);
    extra.extend_from_slice(name);
    extra.extend_from_slice(&[0, 0]);
    extra.extend_from_slice(b"8BIMlyid");
    extra.extend_from_slice(&4u32.to_be_bytes());
    extra.extend_from_slice(&41u32.to_be_bytes());

    layer_info.extend_from_slice(&(extra.len() as u32).to_be_bytes());
    layer_info.extend_from_slice(&extra);
    for channel in [[10u8, 40], [20, 50], [30, 60], [200, 100], [64, 128]] {
        layer_info.extend_from_slice(&0u16.to_be_bytes());
        layer_info.extend_from_slice(&channel);
    }

    let mut layer_and_mask = Vec::new();
    layer_and_mask.extend_from_slice(&(layer_info.len() as u32).to_be_bytes());
    layer_and_mask.extend_from_slice(&layer_info);
    layer_and_mask.extend_from_slice(&0u32.to_be_bytes());
    bytes.extend_from_slice(&(layer_and_mask.len() as u32).to_be_bytes());
    bytes.extend_from_slice(&layer_and_mask);

    bytes.extend_from_slice(&0u16.to_be_bytes());
    for channel in [[10u8, 40], [20, 50], [30, 60], [50, 50]] {
        bytes.extend_from_slice(&channel);
    }
    bytes
}

/// Encodes one PSD Unicode string as a UTF-16BE code-unit sequence.
fn push_unicode_string(bytes: &mut Vec<u8>, value: &str) {
    let units = value.encode_utf16().collect::<Vec<_>>();
    bytes.extend_from_slice(&(units.len() as u32).to_be_bytes());
    for unit in units {
        bytes.extend_from_slice(&unit.to_be_bytes());
    }
}

/// Encodes the null-terminated Unicode form used by descriptor class names.
fn push_padded_unicode_string(bytes: &mut Vec<u8>, value: &str) {
    let units = value.encode_utf16().collect::<Vec<_>>();
    bytes.extend_from_slice(&((units.len() + 1) as u32).to_be_bytes());
    for unit in units {
        bytes.extend_from_slice(&unit.to_be_bytes());
    }
    bytes.extend_from_slice(&0u16.to_be_bytes());
}

/// Builds a minimal PSD containing a hand-authored version-6 slices resource.
fn psd_spec_slices_fixture(version: u32, truncate: bool) -> Vec<u8> {
    let mut payload = Vec::new();
    payload.extend_from_slice(&version.to_be_bytes());
    if version == 6 {
        for value in [-8i32, -10, 40, 50] {
            payload.extend_from_slice(&value.to_be_bytes());
        }
        push_unicode_string(&mut payload, "组😀");
        payload.extend_from_slice(&2u32.to_be_bytes());
        for (id, group_id, origin, layer_id, name, bounds, url, alpha) in [
            (
                7u32,
                3u32,
                0u32,
                None,
                "区域😀",
                (-3i32, 4i32, 6i32, 4i32),
                "https://example.invalid",
                255u8,
            ),
            (
                8u32,
                3u32,
                1u32,
                Some(41u32),
                "",
                (2i32, -5i32, 5i32, 1i32),
                "",
                0u8,
            ),
        ] {
            payload.extend_from_slice(&id.to_be_bytes());
            payload.extend_from_slice(&group_id.to_be_bytes());
            payload.extend_from_slice(&origin.to_be_bytes());
            if let Some(layer_id) = layer_id {
                payload.extend_from_slice(&layer_id.to_be_bytes());
            }
            push_unicode_string(&mut payload, name);
            payload.extend_from_slice(&1u32.to_be_bytes());
            for value in [bounds.0, bounds.1, bounds.2, bounds.3] {
                payload.extend_from_slice(&value.to_be_bytes());
            }
            push_unicode_string(&mut payload, url);
            push_unicode_string(&mut payload, "");
            push_unicode_string(&mut payload, "message");
            push_unicode_string(&mut payload, "alt");
            payload.push(0);
            push_unicode_string(&mut payload, "");
            payload.extend_from_slice(&0u32.to_be_bytes());
            payload.extend_from_slice(&0u32.to_be_bytes());
            payload.extend_from_slice(&[alpha, 1, 2, 3]);
        }
        payload.extend_from_slice(&16u32.to_be_bytes());
        push_padded_unicode_string(&mut payload, "");
        payload.extend_from_slice(&0u32.to_be_bytes());
        payload.extend_from_slice(b"null");
        payload.extend_from_slice(&0u32.to_be_bytes());
    } else if version == 7 || version == 8 {
        push_descriptor_slices(&mut payload);
    }
    if truncate {
        payload.truncate(payload.len().saturating_sub(3));
    }

    let mut resources = Vec::new();
    resources.extend_from_slice(b"8BIM");
    resources.extend_from_slice(&1050u16.to_be_bytes());
    resources.extend_from_slice(&[0, 0]);
    resources.extend_from_slice(&(payload.len() as u32).to_be_bytes());
    resources.extend_from_slice(&payload);
    if payload.len() % 2 != 0 {
        resources.push(0);
    }

    let mut bytes = Vec::new();
    bytes.extend_from_slice(b"8BPS");
    bytes.extend_from_slice(&1u16.to_be_bytes());
    bytes.extend_from_slice(&[0; 6]);
    bytes.extend_from_slice(&3u16.to_be_bytes());
    bytes.extend_from_slice(&1u32.to_be_bytes());
    bytes.extend_from_slice(&1u32.to_be_bytes());
    bytes.extend_from_slice(&8u16.to_be_bytes());
    bytes.extend_from_slice(&3u16.to_be_bytes());
    bytes.extend_from_slice(&0u32.to_be_bytes());
    bytes.extend_from_slice(&(resources.len() as u32).to_be_bytes());
    bytes.extend_from_slice(&resources);
    bytes.extend_from_slice(&0u32.to_be_bytes());
    bytes.extend_from_slice(&0u16.to_be_bytes());
    bytes.extend_from_slice(&[0, 0, 0]);
    bytes
}

/// Builds a minimal PSB v2 carrying the same hand-authored slices resource.
fn psb_spec_slices_fixture(version: u32, malformed_layer_section: bool) -> Vec<u8> {
    let psd = psd_spec_slices_fixture(version, false);
    let resources_len = u32::from_be_bytes(psd[30..34].try_into().expect("resource length"));
    let resources_end = 34 + resources_len as usize;
    let resources = &psd[34..resources_end];

    let mut bytes = Vec::new();
    bytes.extend_from_slice(b"8BPS");
    bytes.extend_from_slice(&2u16.to_be_bytes());
    bytes.extend_from_slice(&[0; 6]);
    bytes.extend_from_slice(&3u16.to_be_bytes());
    bytes.extend_from_slice(&1u32.to_be_bytes());
    bytes.extend_from_slice(&1u32.to_be_bytes());
    bytes.extend_from_slice(&8u16.to_be_bytes());
    bytes.extend_from_slice(&3u16.to_be_bytes());
    bytes.extend_from_slice(&0u32.to_be_bytes());
    bytes.extend_from_slice(&(resources.len() as u32).to_be_bytes());
    bytes.extend_from_slice(resources);
    bytes.extend_from_slice(&if malformed_layer_section { 1u64 } else { 0u64 }.to_be_bytes());
    if malformed_layer_section {
        bytes.push(0);
    }
    bytes.extend_from_slice(&0u16.to_be_bytes());
    bytes.extend_from_slice(&[0, 0, 0]);
    bytes
}

/// Builds a one-pixel PSD with an explicit color mode, depth, and channel count.
fn psd_color_fixture(color_mode: u16, bits_per_channel: u16, channels: u16) -> Vec<u8> {
    let bytes_per_sample = usize::from(bits_per_channel.div_ceil(8));
    let sample_count = usize::from(channels) * bytes_per_sample;
    psd_color_fixture_with_data(
        color_mode,
        bits_per_channel,
        channels,
        &vec![0; sample_count],
    )
}

/// Builds a one-pixel PSD with caller-provided big-endian channel samples.
fn psd_color_fixture_with_data(
    color_mode: u16,
    bits_per_channel: u16,
    channels: u16,
    samples: &[u8],
) -> Vec<u8> {
    let source = psd_spec_slices_fixture(6, false);
    let resources_len = u32::from_be_bytes(source[30..34].try_into().expect("resource length"));
    let resources_end = 34 + resources_len as usize;
    let resources = &source[34..resources_end];
    let color_data = if color_mode == 2 {
        (0..768).map(|value| value as u8).collect::<Vec<_>>()
    } else {
        Vec::new()
    };
    let bytes_per_sample = usize::from(bits_per_channel.div_ceil(8));
    let sample_count = usize::from(channels) * bytes_per_sample;
    assert_eq!(samples.len(), sample_count, "sample payload length");

    let mut bytes = Vec::new();
    bytes.extend_from_slice(b"8BPS");
    bytes.extend_from_slice(&1u16.to_be_bytes());
    bytes.extend_from_slice(&[0; 6]);
    bytes.extend_from_slice(&channels.to_be_bytes());
    bytes.extend_from_slice(&1u32.to_be_bytes());
    bytes.extend_from_slice(&1u32.to_be_bytes());
    bytes.extend_from_slice(&bits_per_channel.to_be_bytes());
    bytes.extend_from_slice(&color_mode.to_be_bytes());
    bytes.extend_from_slice(&(color_data.len() as u32).to_be_bytes());
    bytes.extend_from_slice(&color_data);
    bytes.extend_from_slice(&(resources.len() as u32).to_be_bytes());
    bytes.extend_from_slice(resources);
    bytes.extend_from_slice(&0u32.to_be_bytes());
    if color_mode == 2 {
        // ag-psd follows the upstream reader and accepts indexed composite data
        // only through PackBits/RLE, not the raw indexed path.
        bytes.extend_from_slice(&1u16.to_be_bytes());
        bytes.extend_from_slice(&2u16.to_be_bytes());
        bytes.extend_from_slice(&[0, 0]);
    } else {
        bytes.extend_from_slice(&0u16.to_be_bytes());
        bytes.extend_from_slice(samples);
    }
    bytes
}

/// Encodes the ASCII-or-class-ID form used by PSD descriptors.
fn push_descriptor_ascii_or_class_id(bytes: &mut Vec<u8>, value: &str) {
    if value.len() == 4 && value.is_ascii() {
        bytes.extend_from_slice(&0u32.to_be_bytes());
        bytes.extend_from_slice(value.as_bytes());
    } else {
        bytes.extend_from_slice(&(value.len() as u32).to_be_bytes());
        bytes.extend_from_slice(value.as_bytes());
    }
}

/// Encodes one descriptor class structure (Unicode name followed by class ID).
fn push_descriptor_class(bytes: &mut Vec<u8>, name: &str, class_id: &str) {
    push_padded_unicode_string(bytes, name);
    push_descriptor_ascii_or_class_id(bytes, class_id);
}

/// Encodes a descriptor key and its OSType payload.
fn push_descriptor_key(bytes: &mut Vec<u8>, key: &str, type_: &str, value: &[u8]) {
    push_descriptor_ascii_or_class_id(bytes, key);
    bytes.extend_from_slice(type_.as_bytes());
    bytes.extend_from_slice(value);
}

/// Builds the version-16 descriptor payload used by slice resource versions 7 and 8.
fn push_descriptor_slices(bytes: &mut Vec<u8>) {
    bytes.extend_from_slice(&16u32.to_be_bytes());
    push_descriptor_class(bytes, "", "null");
    bytes.extend_from_slice(&3u32.to_be_bytes());

    let mut base_name = Vec::new();
    push_padded_unicode_string(&mut base_name, "组😀");
    push_descriptor_key(bytes, "baseName", "TEXT", &base_name);

    let mut group_bounds = Vec::new();
    push_descriptor_class(&mut group_bounds, "", "Rct1");
    group_bounds.extend_from_slice(&4u32.to_be_bytes());
    for (key, value) in [("Top ", -8i32), ("Left", -10), ("Btom", 40), ("Rght", 50)] {
        let mut integer = Vec::new();
        integer.extend_from_slice(&value.to_be_bytes());
        push_descriptor_key(&mut group_bounds, key, "long", &integer);
    }
    push_descriptor_key(bytes, "bounds", "Objc", &group_bounds);

    let mut slices = Vec::new();
    for (id, name, bounds, origin, with_metadata) in [
        (7, "区域😀", (-3, 4, 6, 4), "userGenerated", true),
        (8, "", (2, -5, 5, 1), "layer", false),
    ] {
        let mut slice = Vec::new();
        push_descriptor_class(&mut slice, "", "slcD");
        let item_count = if with_metadata { 12 } else { 6 };
        slice.extend_from_slice(&(item_count as u32).to_be_bytes());

        for (key, value) in [("sliceID", id as i32), ("groupID", 3i32)] {
            let mut integer = Vec::new();
            integer.extend_from_slice(&value.to_be_bytes());
            push_descriptor_key(&mut slice, key, "long", &integer);
        }
        for (key, value) in [
            ("origin", format!("ESliceOrigin.{origin}")),
            ("Type", "ESliceType.Img ".to_string()),
        ] {
            let mut enumeration = Vec::new();
            let (enum_type, enum_value) = value.split_once('.').expect("descriptor enum");
            push_descriptor_ascii_or_class_id(&mut enumeration, enum_type);
            push_descriptor_ascii_or_class_id(&mut enumeration, enum_value);
            push_descriptor_key(&mut slice, key, "enum", &enumeration);
        }

        let mut bounds_desc = Vec::new();
        push_descriptor_class(&mut bounds_desc, "", "Rct1");
        bounds_desc.extend_from_slice(&4u32.to_be_bytes());
        for (key, value) in [
            ("Top ", bounds.1 as i32),
            ("Left", bounds.0 as i32),
            ("Btom", bounds.3 as i32),
            ("Rght", bounds.2 as i32),
        ] {
            let mut integer = Vec::new();
            integer.extend_from_slice(&value.to_be_bytes());
            push_descriptor_key(&mut bounds_desc, key, "long", &integer);
        }
        push_descriptor_key(&mut slice, "bounds", "Objc", &bounds_desc);

        let mut text = Vec::new();
        push_padded_unicode_string(&mut text, name);
        push_descriptor_key(&mut slice, "Nm  ", "TEXT", &text);

        if with_metadata {
            let mut url = Vec::new();
            push_padded_unicode_string(&mut url, "https://example.invalid");
            push_descriptor_key(&mut slice, "url", "TEXT", &url);
            let mut bg_type = Vec::new();
            push_descriptor_ascii_or_class_id(&mut bg_type, "ESliceBGColorType");
            push_descriptor_ascii_or_class_id(&mut bg_type, "Clr ");
            push_descriptor_key(&mut slice, "bgColorType", "enum", &bg_type);
            let mut outset = Vec::new();
            outset.extend_from_slice(&2i32.to_be_bytes());
            push_descriptor_key(&mut slice, "topOutset", "long", &outset);
            push_descriptor_key(&mut slice, "leftOutset", "long", &outset);
            push_descriptor_key(&mut slice, "bottomOutset", "long", &outset);
            push_descriptor_key(&mut slice, "rightOutset", "long", &outset);
        }
        slices.extend_from_slice(b"Objc");
        slices.extend_from_slice(&slice);
    }
    let mut list = Vec::new();
    list.extend_from_slice(&2u32.to_be_bytes());
    list.extend_from_slice(&slices);
    push_descriptor_key(bytes, "slices", "VlLs", &list);
}

/// Adds the hand-authored slices resource to the independent pixel-layer fixture.
fn psd_spec_layer_and_slices_fixture() -> Vec<u8> {
    let slices = psd_spec_slices_fixture(6, false);
    let resource_len = u32::from_be_bytes(slices[30..34].try_into().expect("resource length"));
    let resources = &slices[34..34 + resource_len as usize];
    let mask = psd_spec_mask_fixture(false);
    let mut combined = Vec::with_capacity(mask.len() + resources.len());
    combined.extend_from_slice(&mask[..30]);
    combined.extend_from_slice(&resource_len.to_be_bytes());
    combined.extend_from_slice(resources);
    combined.extend_from_slice(&mask[34..]);
    combined
}

#[test]
fn normalizes_hand_built_version_6_slices_in_source_order() {
    let (document, report) = normalize_bytes(&psd_spec_slices_fixture(6, false))
        .expect("normalize hand-built slices fixture");
    assert!(report.is_empty());
    assert_eq!(document.slices.len(), 2);
    assert_eq!(
        (
            document.slices[0].source_id,
            document.slices[0].name.as_str()
        ),
        (7, "区域😀")
    );
    assert_eq!(
        (
            document.slices[1].source_id,
            document.slices[1].name.as_str()
        ),
        (8, "")
    );
    assert_eq!(
        document.slices[0].keys,
        vec![NormalizedSliceKey {
            frame: 0,
            x: -3,
            y: 4,
            width: 9,
            height: 0,
            pivot: None
        }]
    );
    assert_eq!(
        document.slices[1].keys,
        vec![NormalizedSliceKey {
            frame: 0,
            x: 2,
            y: -5,
            width: 3,
            height: 6,
            pivot: None
        }]
    );
    assert!(
        document.slices[0]
            .unrepresentable_fields
            .contains(&"url".to_string())
    );
    assert!(
        document.slices[1]
            .unrepresentable_fields
            .contains(&"associated_layer_id".to_string())
    );
}

#[test]
fn normalizes_hand_built_descriptor_slices_versions_7_and_8() {
    for version in [7u32, 8u32] {
        let (document, report) = normalize_bytes(&psd_spec_slices_fixture(version, false))
            .unwrap_or_else(|error| {
                panic!("normalize hand-built version-{version} slices: {error}")
            });
        assert!(
            report.is_empty(),
            "version-{version} should not add parser losses"
        );
        assert_eq!(document.slices.len(), 2);
        assert_eq!(document.slices[0].source_id, 7);
        assert_eq!(document.slices[0].name, "区域😀");
        assert_eq!(document.slices[1].source_id, 8);
        assert_eq!(document.slices[1].name, "");
        assert_eq!(
            document.slices[0].keys,
            vec![NormalizedSliceKey {
                frame: 0,
                x: -3,
                y: 4,
                width: 9,
                height: 0,
                pivot: None
            }]
        );
        assert_eq!(
            document.slices[1].keys,
            vec![NormalizedSliceKey {
                frame: 0,
                x: 2,
                y: -5,
                width: 3,
                height: 6,
                pivot: None
            }]
        );
        assert!(
            document.slices[0]
                .unrepresentable_fields
                .contains(&"url".to_string())
        );
        assert!(
            document.slices[0]
                .unrepresentable_fields
                .contains(&"background".to_string())
        );
        assert!(
            document.slices[0]
                .unrepresentable_fields
                .contains(&"outsets".to_string())
        );
    }
}

#[test]
fn normalizes_psb_v2_descriptor_slices_and_preserves_container_shape() {
    for version in [7u32, 8u32] {
        let (document, report) = normalize_bytes(&psb_spec_slices_fixture(version, false))
            .unwrap_or_else(|error| panic!("normalize PSB version-{version} fixture: {error}"));
        assert!(report.is_empty());
        assert_eq!(document.canvas, (1, 1));
        assert_eq!(document.frames.len(), 1);
        assert!(document.root_layers.is_empty());
        assert_eq!(document.slices.len(), 2);
        assert_eq!(document.slices[0].name, "区域😀");
        assert_eq!(document.slices[1].name, "");
        assert_eq!(document.slices[0].keys[0].frame, 0);
        assert_eq!(document.slices[0].keys[0].x, -3);
        assert_eq!(document.slices[0].keys[0].width, 9);
    }
}

#[test]
fn malformed_psb_layer_section_does_not_create_output() {
    let directory = fixture_directory("aseprite-psd-psb-malformed");
    let input = directory.join("malformed.psb");
    let output = directory.join("malformed.aseprite");
    fs::write(&input, psb_spec_slices_fixture(8, true)).expect("write malformed PSB fixture");
    let error = convert(&input, &output, &ConvertOptions::default())
        .expect_err("malformed PSB layer section must fail conversion");
    assert!(error.to_string().contains("could not parse PSD"));
    assert!(!output.exists());
    fs::remove_dir_all(directory).expect("remove malformed PSB fixture");
}

#[test]
fn upstream_psb_slices_fixture_normalizes_and_converts() {
    let input = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../tests/fixtures/psb/psd-tools-slices/input.psb");
    let bytes = fs::read(&input).expect("read upstream PSB");
    assert_eq!(&bytes[0..4], b"8BPS");
    assert_eq!(u16::from_be_bytes([bytes[4], bytes[5]]), 2);
    let color_len = u32::from_be_bytes(bytes[26..30].try_into().expect("color length")) as usize;
    let resources_len_offset = 30 + color_len;
    let resources_len = u32::from_be_bytes(
        bytes[resources_len_offset..resources_len_offset + 4]
            .try_into()
            .expect("resource length"),
    ) as usize;
    let layer_len_offset = resources_len_offset + 4 + resources_len;
    assert!(layer_len_offset + 8 <= bytes.len());
    let layer_len = u64::from_be_bytes(
        bytes[layer_len_offset..layer_len_offset + 8]
            .try_into()
            .expect("PSB layer length"),
    );
    assert_eq!(layer_len, 40);

    let (document, _report) = normalize_bytes(&bytes).expect("normalize upstream PSB fixture");
    assert_eq!(document.canvas, (240, 180));
    assert_eq!(document.frames.len(), 1);
    assert_eq!(document.frames[0].index, 0);
    assert!(document.root_layers.is_empty());
    assert_eq!(document.slices.len(), 10);
    assert_eq!(
        document
            .slices
            .iter()
            .map(|slice| slice.name.as_str())
            .collect::<Vec<_>>(),
        vec![
            "slices_06",
            "Slice 1",
            "slices_04",
            "",
            "",
            "",
            "",
            "",
            "",
            ""
        ]
    );
    assert_eq!(
        document.slices[0].keys[0],
        NormalizedSliceKey {
            frame: 0,
            x: 133,
            y: 70,
            width: 68,
            height: 68,
            pivot: None,
        }
    );
    assert_eq!(
        document.slices[9].keys[0],
        NormalizedSliceKey {
            frame: 0,
            x: 0,
            y: 0,
            width: 240,
            height: 180,
            pivot: None,
        }
    );
    let directory = fixture_directory("aseprite-psd-real-psb");
    let output = directory.join("output.aseprite");
    fs::copy(&input, directory.join("input.psb")).expect("stage upstream PSB");
    let conversion = convert(
        &directory.join("input.psb"),
        &output,
        &ConvertOptions::default(),
    )
    .expect("convert upstream PSB fixture");
    assert!(output.exists());
    assert!(
        conversion
            .information_loss
            .entries
            .iter()
            .any(|entry| entry.code == InformationLossCode::Slices)
    );
    let output_file = aseprite::AsepriteFile::from_reader(Cursor::new(
        fs::read(&output).expect("read converted upstream PSB"),
    ))
    .expect("parse converted upstream PSB");
    assert_eq!(output_file.slices().len(), document.slices.len());
    assert_eq!(
        output_file
            .slices()
            .iter()
            .map(|slice| slice.name.as_str())
            .collect::<Vec<_>>(),
        vec![
            "slices_06",
            "Slice 1",
            "slices_04",
            "",
            "",
            "",
            "",
            "",
            "",
            ""
        ]
    );
    assert_eq!(output_file.slices()[0].keys[0].frame, 0);
    assert_eq!(
        (
            output_file.slices()[0].keys[0].x,
            output_file.slices()[0].keys[0].y,
            output_file.slices()[0].keys[0].width,
            output_file.slices()[0].keys[0].height,
        ),
        (133, 70, 68, 68)
    );

    let auto_output = directory.join("auto-output.aseprite");
    let auto_conversion = convert(
        &directory.join("input.psb"),
        &auto_output,
        &ConvertOptions {
            layer_association: LayerAssociation::Auto(AutoAssociationOptions::default()),
            ..ConvertOptions::default()
        },
    )
    .expect("automatic association should allow slice-only PSB");
    assert!(auto_output.exists());
    let association = auto_conversion
        .association
        .expect("automatic association report");
    assert_eq!(association.observation_count, 0);
    assert_eq!(association.track_count, 0);
    assert!(association.warnings.iter().any(|warning| {
        warning.contains("no source layers") && warning.contains("empty layer plan")
    }));

    fs::remove_dir_all(directory).expect("remove real PSB fixture");
}

#[test]
fn malformed_upstream_psb_variants_fail_without_output() {
    let input = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../tests/fixtures/psb/psd-tools-slices/input.psb");
    let original = fs::read(&input).expect("read upstream PSB");
    let color_len = u32::from_be_bytes(original[26..30].try_into().expect("color length")) as usize;
    let resources_len_offset = 30 + color_len;
    let resources_len = u32::from_be_bytes(
        original[resources_len_offset..resources_len_offset + 4]
            .try_into()
            .expect("resource length"),
    ) as usize;
    let layer_len_offset = resources_len_offset + 4 + resources_len;
    let layer_len = u64::from_be_bytes(
        original[layer_len_offset..layer_len_offset + 8]
            .try_into()
            .expect("PSB layer length"),
    );

    let mut variants = Vec::new();
    let layer_end = layer_len_offset
        .checked_add(8)
        .and_then(|offset| offset.checked_add(usize::try_from(layer_len).expect("layer size")))
        .expect("layer section end");
    variants.push(("truncated", original[..layer_end - 1].to_vec()));
    let mut invalid_version = original.clone();
    invalid_version[4..6].copy_from_slice(&3u16.to_be_bytes());
    variants.push(("invalid version", invalid_version));
    let mut oversized_layer = original.clone();
    oversized_layer[layer_len_offset..layer_len_offset + 8]
        .copy_from_slice(&u64::MAX.to_be_bytes());
    variants.push(("oversized layer section", oversized_layer));

    for (name, bytes) in variants {
        let directory = fixture_directory(&format!("aseprite-psd-psb-{name}"));
        let staged_input = directory.join("input.psb");
        let output = directory.join("output.aseprite");
        fs::write(&staged_input, bytes).expect("write malformed PSB variant");
        let error = convert(&staged_input, &output, &ConvertOptions::default())
            .expect_err("malformed PSB must fail conversion");
        assert!(
            error.to_string().contains("could not parse PSD"),
            "unexpected {name} error: {error}"
        );
        assert!(!output.exists(), "malformed {name} created output");
        fs::remove_dir_all(directory).expect("remove malformed PSB fixture");
    }
}

#[test]
fn converts_16_bit_input_with_explicit_rgba8_degradation() {
    let directory = fixture_directory("aseprite-psd-16-bit");
    let input = directory.join("input.psd");
    let output = directory.join("output.aseprite");
    let fixture = psd_color_fixture_with_data(3, 16, 3, &[0x12, 0x34, 0x80, 0x00, 0xff, 0x00]);
    let parsed = ag_psd::read_psd(
        &fixture,
        &ag_psd::psd::ReadOptions {
            use_image_data: Some(true),
            ..Default::default()
        },
    )
    .expect("read 16-bit composite");
    assert_eq!(
        parsed.image_data.expect("16-bit composite pixels").data,
        vec![0x12, 0x80, 0xff, 0xff]
    );
    fs::write(&input, fixture).expect("write 16-bit fixture");
    let report = convert(&input, &output, &ConvertOptions::default())
        .expect("16-bit input should normalize to RGBA8");
    let loss = report
        .information_loss
        .entries
        .iter()
        .find(|entry| entry.code == InformationLossCode::UnsupportedColor)
        .expect("16-bit degradation report");
    assert_eq!(loss.disposition, LossDisposition::Degraded);
    assert!(loss.detail.contains("normalized to RGBA8"));
    assert!(output.exists());
    fs::remove_dir_all(directory).expect("remove 16-bit fixture");
}

#[test]
fn converts_grayscale_and_indexed_input_with_color_loss_report() {
    for (color_mode, channels) in [(1u16, 1u16), (2u16, 1u16), (1u16, 2u16)] {
        let directory = fixture_directory("aseprite-psd-color-mode");
        let input = directory.join("input.psd");
        let output = directory.join("output.aseprite");
        fs::write(&input, psd_color_fixture(color_mode, 8, channels))
            .expect("write color-mode fixture");
        let report = convert(&input, &output, &ConvertOptions::default())
            .expect("non-RGB input should normalize to RGBA8");
        let loss = report
            .information_loss
            .entries
            .iter()
            .find(|entry| entry.code == InformationLossCode::UnsupportedColor)
            .expect("color-mode degradation report");
        assert_eq!(loss.disposition, LossDisposition::Degraded);
        assert!(loss.detail.contains("normalized to RGBA8"));
        assert!(output.exists());
        fs::remove_dir_all(directory).expect("remove color-mode fixture");
    }
}

#[test]
fn converts_32_bit_float_input_with_explicit_rgba8_degradation() {
    let directory = fixture_directory("aseprite-psd-32-bit");
    let input = directory.join("input.psd");
    let output = directory.join("output.aseprite");
    let mut fixture = Vec::new();
    for value in [1.0f32, 0.5, 0.0] {
        fixture.extend_from_slice(&value.to_be_bytes());
    }
    let fixture = psd_color_fixture_with_data(3, 32, 3, &fixture);
    let parsed = ag_psd::read_psd(
        &fixture,
        &ag_psd::psd::ReadOptions {
            use_image_data: Some(true),
            ..Default::default()
        },
    )
    .expect("read 32-bit composite");
    assert_eq!(
        parsed.image_data.expect("32-bit composite pixels").data,
        vec![255, 128, 0, 255]
    );
    fs::write(&input, fixture).expect("write 32-bit fixture");
    let report = convert(&input, &output, &ConvertOptions::default())
        .expect("32-bit input should normalize to RGBA8");
    let loss = report
        .information_loss
        .entries
        .iter()
        .find(|entry| entry.code == InformationLossCode::UnsupportedColor)
        .expect("32-bit degradation report");
    assert_eq!(loss.disposition, LossDisposition::Degraded);
    assert!(loss.detail.contains("normalized to RGBA8"));
    let output_file = aseprite::AsepriteFile::from_reader(Cursor::new(
        fs::read(&output).expect("read 32-bit conversion output"),
    ))
    .expect("parse 32-bit conversion output");
    assert_eq!(output_file.color_mode(), aseprite::ColorMode::Rgba);
    fs::remove_dir_all(directory).expect("remove 32-bit fixture");
}

#[test]
fn malformed_32_bit_input_does_not_create_output() {
    let directory = fixture_directory("aseprite-psd-32-bit-malformed");
    let input = directory.join("input.psd");
    let output = directory.join("output.aseprite");
    let mut fixture = psd_color_fixture(3, 32, 3);
    fixture.truncate(fixture.len().saturating_sub(14));
    fs::write(&input, fixture).expect("write malformed 32-bit fixture");
    let error = convert(&input, &output, &ConvertOptions::default())
        .expect_err("malformed 32-bit input must fail conversion");
    assert!(error.to_string().contains("could not parse PSD"));
    assert!(!output.exists());
    fs::remove_dir_all(directory).expect("remove malformed 32-bit fixture");
}

#[test]
fn malformed_and_unsupported_slice_resources_fail_before_output() {
    let truncated = normalize_bytes(&psd_spec_slices_fixture(6, true))
        .expect_err("truncated slices resource must fail");
    assert!(truncated.to_string().contains("could not parse PSD"));
    let unsupported = normalize_bytes(&psd_spec_slices_fixture(9, false))
        .expect_err("unsupported slices resource must fail");
    assert!(
        unsupported
            .to_string()
            .contains("Invalid slices version (9)")
    );
    for version in [7u32, 8u32] {
        let truncated = normalize_bytes(&psd_spec_slices_fixture(version, true))
            .expect_err("truncated descriptor slices resource must fail");
        assert!(
            truncated.to_string().contains("could not parse PSD"),
            "version-{version} error should identify PSD parsing: {truncated}"
        );
    }
}

#[test]
fn rejects_non_integral_and_reversed_normalized_slice_bounds() {
    let group = ag_psd::psd::SliceGroup {
        bounds: ag_psd::psd::LtrbBounds {
            left: 0.0,
            top: 0.0,
            right: 1.0,
            bottom: 1.0,
        },
        group_name: String::new(),
        slices: vec![ag_psd::psd::Slice {
            id: 1.0,
            bounds: ag_psd::psd::LtrbBounds {
                left: 1.5,
                top: 0.0,
                right: 1.0,
                bottom: 1.0,
            },
            ..Default::default()
        }],
    };
    let resources = ag_psd::psd::ImageResources {
        slices: Some(vec![group]),
        ..Default::default()
    };
    let non_integral = normalize_slices(Some(&resources)).expect_err("fractional bounds must fail");
    assert!(
        non_integral
            .to_string()
            .contains("must be a finite integer")
    );

    let mut resources = resources;
    resources.slices.as_mut().unwrap()[0].slices[0].bounds.left = 2.0;
    let reversed = normalize_slices(Some(&resources)).expect_err("reversed bounds must fail");
    assert!(reversed.to_string().contains("reversed bounds"));
}

#[test]
fn converts_slices_and_preserves_existing_layer_frame_and_pixels() {
    let directory = fixture_directory("aseprite-psd-slices-convert");
    let input = directory.join("slices.psd");
    let output = directory.join("slices.aseprite");
    fs::write(&input, psd_spec_layer_and_slices_fixture()).expect("write slices fixture");

    let report = convert(
        &input,
        &output,
        &ConvertOptions {
            layer_association: LayerAssociation::Preserve,
            ..Default::default()
        },
    )
    .expect("convert slices fixture");
    let loss = report
        .information_loss
        .entries
        .iter()
        .find(|entry| entry.code == InformationLossCode::Slices)
        .expect("degraded slice metadata report");
    assert_eq!(loss.disposition, LossDisposition::Degraded);
    assert_eq!(loss.count, 2);
    assert!(loss.detail.contains("Photoshop fields"));

    let file = aseprite::AsepriteFile::from_reader(Cursor::new(
        fs::read(&output).expect("read converted Aseprite"),
    ))
    .expect("parse converted Aseprite");
    assert_eq!(file.frames().len(), 1);
    assert_eq!(file.layers().len(), 1);
    assert_eq!(file.layers()[0].name, "PSD spec mask");
    assert_eq!(file.slices().len(), 2);
    assert_eq!(file.slices()[0].name, "区域😀");
    assert_eq!(file.slices()[1].name, "");
    assert_eq!(file.slices()[0].keys[0].frame, 0);
    assert_eq!(
        (
            file.slices()[0].keys[0].x,
            file.slices()[0].keys[0].y,
            file.slices()[0].keys[0].width,
            file.slices()[0].keys[0].height,
        ),
        (-3, 4, 9, 0)
    );
    let cel = file
        .cel(file.layer_ref(0).expect("pixel layer"), 0)
        .expect("pixel cel");
    let pixels = match &cel.kind {
        aseprite::CelKind::Raw { pixels, .. } | aseprite::CelKind::Compressed { pixels, .. } => {
            pixels
        }
        kind => panic!("unexpected converted cel kind: {kind:?}"),
    };
    assert_eq!(pixels.data, vec![10, 20, 30, 50, 40, 50, 60, 50]);

    fs::remove_dir_all(directory).expect("remove slices conversion fixture");
}

#[test]
fn malformed_slice_resource_does_not_create_output() {
    let directory = fixture_directory("aseprite-psd-slices-malformed");
    let input = directory.join("malformed.psd");
    let output = directory.join("malformed.aseprite");
    fs::write(&input, psd_spec_slices_fixture(6, true)).expect("write malformed fixture");
    let error = convert(&input, &output, &ConvertOptions::default())
        .expect_err("malformed slice resource must fail conversion");
    assert!(error.to_string().contains("invalid slices resource"));
    assert!(!output.exists());
    fs::remove_dir_all(directory).expect("remove malformed slices fixture");
}

/// Creates a unique temporary directory for a core conversion fixture.
fn fixture_directory(prefix: &str) -> std::path::PathBuf {
    let directory = std::env::temp_dir().join(format!(
        "{prefix}-{}",
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("clock")
            .as_nanos()
    ));
    fs::create_dir_all(&directory).expect("create fixture directory");
    directory
}

#[test]
fn bitmap_user_mask_rasterizes_alpha_and_reports_editability_loss() {
    let directory = fixture_directory("aseprite-psd-bitmap-mask");
    let input = directory.join("masked.psd");
    write_psd_fixture(&input, &bitmap_mask_fixture(false, false));

    let (normalized, report) = normalize_bytes(&fs::read(&input).expect("read masked PSD"))
        .expect("normalize bitmap mask fixture");
    let layer = &normalized.root_layers[0];
    assert_eq!(layer.name, "Masked layer");
    assert_eq!(
        layer.bounds,
        NormalizedBounds {
            left: 0,
            top: 0,
            right: 2,
            bottom: 1
        }
    );
    assert_eq!(
        layer.pixels.as_ref().expect("masked layer pixels").data,
        vec![10, 20, 30, 50, 40, 50, 60, 50]
    );
    let loss = report
        .entries
        .iter()
        .find(|entry| entry.code == InformationLossCode::PixelMask)
        .expect("bitmap mask loss report");
    assert_eq!(loss.disposition, LossDisposition::Rasterized);
    assert_eq!(loss.count, 1);
    assert_eq!(loss.locations[0].path, "0");
    assert_eq!(loss.locations[0].layer_id, Some(41));
    assert!(loss.editability_impact);

    fs::remove_dir_all(directory).expect("remove bitmap mask fixture");
}

#[test]
fn bitmap_user_mask_accepts_hand_built_psd_spec_fixture() {
    let (normalized, report) = normalize_bytes(&psd_spec_mask_fixture(false))
        .expect("normalize hand-built PSD specification fixture");
    let layer = &normalized.root_layers[0];
    assert_eq!(layer.name, "PSD spec mask");
    assert_eq!(layer.id, 41);
    // The TS ag-psd oracle exposes source layer pixels; normalization applies
    // the PSD user mask to alpha as the destination contract requires.
    assert_eq!(
        layer.pixels.as_ref().expect("spec fixture pixels").data,
        vec![10, 20, 30, 50, 40, 50, 60, 50]
    );
    let loss = report
        .entries
        .iter()
        .find(|entry| entry.code == InformationLossCode::PixelMask)
        .expect("spec fixture mask loss report");
    assert_eq!(loss.disposition, LossDisposition::Rasterized);
    assert_eq!(loss.locations[0].path, "0");
    assert_eq!(loss.locations[0].layer_id, Some(41));
}

#[test]
fn bitmap_user_mask_maps_pixels_in_document_coordinates() {
    let directory = fixture_directory("aseprite-psd-bitmap-mask-offset");
    let input = directory.join("masked.psd");
    let mut psd = bitmap_mask_fixture(false, false);
    let layer = &mut psd.children.as_mut().expect("fixture layer")[0];
    let mask = layer.additional_info.mask.as_mut().expect("fixture mask");
    mask.left = Some(1.0);
    mask.right = Some(2.0);
    mask.default_color = Some(255.0);
    mask.image_data = Some(ag_psd::psd::PixelData {
        width: 1,
        height: 1,
        data: vec![64, 64, 64, 255],
    });
    write_psd_fixture(&input, &psd);

    let (normalized, _) = normalize_bytes(&fs::read(&input).expect("read offset mask PSD"))
        .expect("normalize offset bitmap mask fixture");
    assert_eq!(
        normalized.root_layers[0]
            .pixels
            .as_ref()
            .expect("offset masked pixels")
            .data,
        vec![10, 20, 30, 200, 40, 50, 60, 25]
    );

    fs::remove_dir_all(directory).expect("remove offset bitmap mask fixture");
}

#[test]
fn bitmap_user_mask_survives_convert_as_masked_aseprite_pixels() {
    let directory = fixture_directory("aseprite-psd-bitmap-mask-convert");
    let input = directory.join("masked.psd");
    let output = directory.join("masked.aseprite");
    fs::write(&input, psd_spec_mask_fixture(false)).expect("write PSD specification fixture");

    let report = convert(
        &input,
        &output,
        &ConvertOptions {
            layer_association: LayerAssociation::Preserve,
            ..Default::default()
        },
    )
    .expect("convert bitmap mask fixture");
    let loss = report
        .information_loss
        .entries
        .iter()
        .find(|entry| entry.code == InformationLossCode::PixelMask)
        .expect("bitmap mask conversion report");
    assert_eq!(loss.disposition, LossDisposition::Rasterized);

    let bytes = fs::read(&output).expect("read converted Aseprite");
    let file =
        aseprite::AsepriteFile::from_reader(Cursor::new(bytes)).expect("parse converted Aseprite");
    assert_eq!(file.frames().len(), 1);
    assert_eq!(file.layers().len(), 1);
    assert_eq!(file.layers()[0].name, "PSD spec mask");
    let cel = file
        .cel(file.layer_ref(0).expect("converted pixel layer"), 0)
        .expect("converted masked cel");
    match &cel.kind {
        aseprite::CelKind::Raw { pixels, x, y }
        | aseprite::CelKind::Compressed { pixels, x, y, .. } => {
            assert_eq!((*x, *y), (0, 0));
            assert_eq!(pixels.data, vec![10, 20, 30, 50, 40, 50, 60, 50]);
        }
        kind => panic!("unexpected converted cel kind: {kind:?}"),
    }

    fs::remove_dir_all(directory).expect("remove bitmap mask conversion fixture");
}

#[test]
fn clipping_is_rejected_before_output_commit() {
    let directory = fixture_directory("aseprite-psd-clipping");
    let input = directory.join("clipping.psd");
    let output = directory.join("clipping.aseprite");
    fs::write(&input, psd_spec_mask_fixture(true)).expect("write PSD specification fixture");

    let error = convert(
        &input,
        &output,
        &ConvertOptions {
            overwrite: true,
            layer_association: LayerAssociation::Preserve,
            ..Default::default()
        },
    )
    .expect_err("clipping must be rejected");
    let message = error.to_string();
    assert!(message.contains("clipping is unsupported at layer path 0"));
    assert!(message.contains("layer id Some(41)"));
    assert!(!output.exists(), "rejected clipping must not commit output");

    fs::remove_dir_all(directory).expect("remove clipping fixture");
}

#[test]
fn parameterized_bitmap_mask_remains_explicitly_unsupported() {
    let directory = fixture_directory("aseprite-psd-parameterized-mask");
    let input = directory.join("parameterized.psd");
    write_psd_fixture(&input, &bitmap_mask_fixture(false, true));

    let (normalized, report) = normalize_bytes(&fs::read(&input).expect("read parameterized PSD"))
        .expect("normalize parameterized mask fixture");
    assert_eq!(
        normalized.root_layers[0]
            .pixels
            .as_ref()
            .expect("parameterized mask pixels")
            .data,
        vec![10, 20, 30, 200, 40, 50, 60, 100]
    );
    let loss = report
        .entries
        .iter()
        .find(|entry| entry.code == InformationLossCode::PixelMask)
        .expect("parameterized mask loss report");
    assert_eq!(loss.disposition, LossDisposition::Dropped);
    assert!(loss.detail.contains("not represented"));

    fs::remove_dir_all(directory).expect("remove parameterized mask fixture");
}

#[test]
fn vector_derived_mask_remains_explicitly_unsupported() {
    let directory = fixture_directory("aseprite-psd-vector-mask");
    let input = directory.join("vector-mask.psd");
    let mut psd = bitmap_mask_fixture(false, false);
    psd.children.as_mut().expect("fixture layer")[0]
        .additional_info
        .mask
        .as_mut()
        .expect("fixture mask")
        .from_vector_data = Some(true);
    write_psd_fixture(&input, &psd);

    let (normalized, report) = normalize_bytes(&fs::read(&input).expect("read vector mask PSD"))
        .expect("normalize vector mask fixture");
    let loss = report
        .entries
        .iter()
        .find(|entry| entry.code == InformationLossCode::PixelMask)
        .expect("vector mask loss report");
    assert_eq!(loss.disposition, LossDisposition::Dropped);
    assert_eq!(
        normalized.root_layers[0]
            .pixels
            .as_ref()
            .expect("vector mask pixels")
            .data,
        vec![10, 20, 30, 200, 40, 50, 60, 100]
    );

    fs::remove_dir_all(directory).expect("remove vector mask fixture");
}

fn state(frame_index: u32, enabled: bool) -> NormalizedLayerFrameState {
    NormalizedLayerFrameState {
        frame_index,
        record_present: false,
        enabled,
        explicit_enable: false,
        offset: None,
        reference_point: None,
        opacity: None,
    }
}

fn layer(
    id: u32,
    kind: NormalizedLayerKind,
    hidden: Option<bool>,
    children: Vec<NormalizedLayer>,
    frame_states: Vec<NormalizedLayerFrameState>,
) -> NormalizedLayer {
    NormalizedLayer {
        id,
        name: String::new(),
        kind,
        bounds: NormalizedBounds {
            left: 0,
            top: 0,
            right: 1,
            bottom: 1,
        },
        opacity: None,
        blend_mode: None,
        hidden,
        pixels: None,
        children,
        frame_states,
    }
}

#[test]
fn recursive_visibility_applies_ancestor_state_without_storing_a_list() {
    let child = layer(
        2,
        NormalizedLayerKind::Pixel,
        None,
        Vec::new(),
        vec![state(0, true)],
    );
    let group = layer(
        1,
        NormalizedLayerKind::Group,
        Some(true),
        vec![child],
        vec![state(0, false)],
    );
    let mut visible = Vec::new();
    group.collect_visible_pixel_layer_ids(0, true, &mut visible);
    assert!(visible.is_empty());
    assert!(!group.is_effectively_visible(0, true));
}

#[test]
fn static_frame_has_no_serialization_duration() {
    let frame = NormalizedFrame {
        index: 0,
        source_id: None,
        duration_ms: None,
        dispose: None,
    };
    assert_eq!(frame.source_id, None);
    assert_eq!(frame.duration_ms, None);
}

#[test]
fn top_level_frame_source_keeps_background_shared() {
    let mut background = layer(
        1,
        NormalizedLayerKind::Pixel,
        Some(true),
        Vec::new(),
        vec![state(0, true)],
    );
    background.name = "Background".to_string();
    let mut first = layer(
        2,
        NormalizedLayerKind::Pixel,
        Some(true),
        Vec::new(),
        vec![state(0, true)],
    );
    first.name = "First".to_string();
    let mut hidden_child = layer(
        4,
        NormalizedLayerKind::Pixel,
        Some(true),
        Vec::new(),
        vec![state(0, true)],
    );
    hidden_child.name = "Hidden child".to_string();
    let mut second = layer(
        3,
        NormalizedLayerKind::Group,
        Some(true),
        vec![hidden_child],
        vec![state(0, true)],
    );
    second.name = "Second".to_string();
    let mut document = NormalizedDocument {
        root_layers: vec![background, first, second],
        frames: vec![NormalizedFrame {
            index: 0,
            source_id: None,
            duration_ms: None,
            dispose: None,
        }],
        ..Default::default()
    };

    let warnings = apply_frame_source(&mut document, FrameSource::TopLevel)
        .expect("top-level frames should be constructed");

    assert_eq!(document.frames.len(), 2);
    assert_eq!(
        document.root_layers[0]
            .frame_states
            .iter()
            .map(|state| state.enabled)
            .collect::<Vec<_>>(),
        vec![false, false]
    );
    assert_eq!(
        document.root_layers[1]
            .frame_states
            .iter()
            .map(|state| state.enabled)
            .collect::<Vec<_>>(),
        vec![true, false]
    );
    assert_eq!(
        document.root_layers[2]
            .frame_states
            .iter()
            .map(|state| state.enabled)
            .collect::<Vec<_>>(),
        vec![false, true]
    );
    assert_eq!(
        document.root_layers[2].children[0]
            .frame_states
            .iter()
            .map(|state| state.enabled)
            .collect::<Vec<_>>(),
        vec![false, false]
    );
    assert!(warnings[0].contains("2 top-level frames"));
    assert!(warnings[0].contains("Background"));
}

#[test]
fn automatic_frame_source_does_not_infer_top_level_animation() {
    let mut document = NormalizedDocument {
        root_layers: vec![
            layer(
                1,
                NormalizedLayerKind::Pixel,
                None,
                Vec::new(),
                vec![state(0, true)],
            ),
            layer(
                2,
                NormalizedLayerKind::Group,
                None,
                Vec::new(),
                vec![state(0, true)],
            ),
        ],
        frames: vec![NormalizedFrame {
            index: 0,
            source_id: None,
            duration_ms: None,
            dispose: None,
        }],
        ..Default::default()
    };

    apply_frame_source(&mut document, FrameSource::Auto)
        .expect("auto should preserve static input");

    assert_eq!(document.frames.len(), 1);
    assert!(
        document
            .root_layers
            .iter()
            .all(|layer| layer.frame_states[0].enabled)
    );
}

#[test]
fn top_level_frame_source_rejects_photoshop_timeline() {
    let mut document = NormalizedDocument {
        animation_resource_ids: vec![4000],
        ..Default::default()
    };
    let error = apply_frame_source(&mut document, FrameSource::TopLevel)
        .expect_err("timeline input must not be reinterpreted");
    assert!(error.contains("cannot replace a Photoshop timeline"));
}

#[test]
fn photoshop_frame_duration_validation_uses_ten_millisecond_quantization() {
    assert_eq!(canonical_frame_duration_ms(83), 80);
    assert_eq!(canonical_frame_duration_ms(65_535), 65_530);
    assert_eq!(canonical_frame_duration_ms(84), 80);
    assert_ne!(
        canonical_frame_duration_ms(83),
        canonical_frame_duration_ms(90)
    );
}

#[test]
fn roundtrip_preset_falls_back_to_auto_for_unmarked_documents() {
    let (exact, association) = resolve_roundtrip_association(roundtrip::RoundTripLayout {
        status: roundtrip::RoundTripStatus {
            marked: false,
            valid: true,
        },
        version: None,
        frame_count: None,
    })
    .expect("unmarked documents should use automatic association");
    assert!(!exact);
    assert!(matches!(association, LayerAssociation::Auto(_)));
}

#[test]
fn roundtrip_preset_keeps_invalid_markers_on_recovery_path() {
    let error = resolve_roundtrip_association(roundtrip::RoundTripLayout {
        status: roundtrip::RoundTripStatus {
            marked: true,
            valid: false,
        },
        version: Some(2),
        frame_count: Some(2),
    })
    .expect_err("invalid markers must require recovery");
    assert!(matches!(
        error,
        ConversionError::RoundTripRecoveryRequired(_)
    ));
}

#[test]
fn pixel_data_is_owned_and_keeps_origin() {
    let source = ag_psd::psd::PixelData {
        width: 1,
        height: 1,
        data: vec![1, 2, 3, 4],
    };
    let normalized = copy_rgba8_pixels(
        &source,
        NormalizedBounds {
            left: -4,
            top: 7,
            right: -3,
            bottom: 8,
        },
        "test",
    )
    .expect("valid RGBA8 data");
    assert_eq!(normalized.data, vec![1, 2, 3, 4]);
    assert_eq!((normalized.left, normalized.top), (-4, 7));
}

#[test]
fn malformed_pixel_length_is_rejected() {
    let source = ag_psd::psd::PixelData {
        width: 1,
        height: 1,
        data: vec![1, 2, 3],
    };
    let error = copy_rgba8_pixels(
        &source,
        NormalizedBounds {
            left: 0,
            top: 0,
            right: 1,
            bottom: 1,
        },
        "test",
    )
    .expect_err("short pixel data must fail");
    assert!(error.to_string().contains("pixel buffer length mismatch"));
}

#[test]
fn non_integral_and_out_of_range_bounds_are_rejected() {
    assert!(integral_i32(Some(1.5), "left").is_err());
    assert!(integral_i32(Some(i32::MAX as f64 + 1.0), "right").is_err());
    assert!(integral_i32(Some(i32::MIN as f64 - 1.0), "top").is_err());
}
