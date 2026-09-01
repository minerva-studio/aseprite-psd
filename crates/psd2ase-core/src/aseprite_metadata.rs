//! Aseprite user-property encoding used for Photoshop round trips.

use aseprite::{PropertiesMap, PropertyValue, UserData};

use crate::{AnimationPoint, NormalizedLayer, photoshop_metadata};

pub(crate) const REFERENCE_POINTS_PROPERTY: &str = "psd2ase_reference_points";

/// Builds layer user data containing the meaningful Photoshop reference points.
pub(crate) fn reference_point_user_data(
    layer: &NormalizedLayer,
    frame_count: usize,
) -> Option<UserData> {
    let entries = (0..frame_count)
        .filter_map(|frame_index| {
            let state = layer.frame_states.get(frame_index)?;
            let point = state.reference_point?;
            if !point.x.is_finite()
                || !point.y.is_finite()
                || photoshop_metadata::is_default_reference_point(layer, frame_index, point)
            {
                return None;
            }
            Some(PropertyValue::Properties(vec![
                (
                    "frame_index".to_string(),
                    PropertyValue::UInt32(frame_index as u32),
                ),
                ("x".to_string(), PropertyValue::Double(point.x)),
                ("y".to_string(), PropertyValue::Double(point.y)),
            ]))
        })
        .collect::<Vec<_>>();
    if entries.is_empty() {
        return None;
    }
    Some(UserData {
        properties: vec![PropertiesMap {
            key: 0,
            entries: vec![(
                REFERENCE_POINTS_PROPERTY.to_string(),
                PropertyValue::Vector(entries),
            )],
        }],
        ..Default::default()
    })
}

/// Reads per-frame Photoshop reference points from Aseprite layer user data.
pub(crate) fn read_reference_point_user_data(
    user_data: Option<&UserData>,
    frame_count: usize,
) -> Vec<Option<AnimationPoint>> {
    let mut points = vec![None; frame_count];
    let Some(user_data) = user_data else {
        return points;
    };
    let Some(PropertyValue::Vector(entries)) = find_property(user_data, REFERENCE_POINTS_PROPERTY)
    else {
        return points;
    };
    for entry in entries {
        let PropertyValue::Properties(fields) = entry else {
            continue;
        };
        let Some(frame_index) = fields
            .iter()
            .find(|(name, _)| name == "frame_index")
            .and_then(|(_, value)| property_u32(value))
            .map(|value| value as usize)
        else {
            continue;
        };
        if frame_index >= frame_count {
            continue;
        }
        let Some(x) = fields
            .iter()
            .find(|(name, _)| name == "x")
            .and_then(|(_, value)| property_f64(value))
        else {
            continue;
        };
        let Some(y) = fields
            .iter()
            .find(|(name, _)| name == "y")
            .and_then(|(_, value)| property_f64(value))
        else {
            continue;
        };
        if x.is_finite() && y.is_finite() {
            points[frame_index] = Some(AnimationPoint { x, y });
        }
    }
    points
}

/// Finds a user-defined property in Aseprite's key-zero property map.
fn find_property<'a>(user_data: &'a UserData, name: &str) -> Option<&'a PropertyValue> {
    user_data
        .properties
        .iter()
        .find(|map| map.key == 0)
        .and_then(|map| map.entries.iter().find(|(key, _)| key == name))
        .map(|(_, value)| value)
}

/// Converts supported integer property values to a u32.
fn property_u32(value: &PropertyValue) -> Option<u32> {
    match value {
        PropertyValue::UInt8(value) => Some(u32::from(*value)),
        PropertyValue::UInt16(value) => Some(u32::from(*value)),
        PropertyValue::UInt32(value) => Some(*value),
        PropertyValue::Int8(value) if *value >= 0 => Some(*value as u32),
        PropertyValue::Int16(value) if *value >= 0 => Some(*value as u32),
        PropertyValue::Int32(value) if *value >= 0 => Some(*value as u32),
        _ => None,
    }
}

/// Converts supported numeric property values to a finite f64.
fn property_f64(value: &PropertyValue) -> Option<f64> {
    let value = match value {
        PropertyValue::Int8(value) => f64::from(*value),
        PropertyValue::UInt8(value) => f64::from(*value),
        PropertyValue::Int16(value) => f64::from(*value),
        PropertyValue::UInt16(value) => f64::from(*value),
        PropertyValue::Int32(value) => f64::from(*value),
        PropertyValue::UInt32(value) => f64::from(*value),
        PropertyValue::Int64(value) => *value as f64,
        PropertyValue::UInt64(value) => *value as f64,
        PropertyValue::Float(value) => f64::from(*value),
        PropertyValue::Double(value) => *value,
        _ => return None,
    };
    value.is_finite().then_some(value)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{NormalizedBounds, NormalizedLayerFrameState, NormalizedPixels};

    #[test]
    fn metadata_properties_round_trip_per_frame() {
        let mut layer = NormalizedLayer {
            id: 4,
            name: "layer".to_string(),
            kind: crate::NormalizedLayerKind::Pixel,
            bounds: NormalizedBounds {
                left: 0,
                top: 0,
                right: 2,
                bottom: 2,
            },
            opacity: None,
            blend_mode: None,
            hidden: Some(false),
            pixels: Some(NormalizedPixels {
                width: 2,
                height: 2,
                left: 0,
                top: 0,
                data: vec![1, 2, 3, 255].repeat(4),
            }),
            children: Vec::new(),
            frame_states: (0..3)
                .map(|frame_index| NormalizedLayerFrameState {
                    frame_index,
                    record_present: true,
                    enabled: true,
                    explicit_enable: true,
                    offset: None,
                    reference_point: None,
                    opacity: None,
                })
                .collect(),
        };
        layer.frame_states[1].reference_point = Some(AnimationPoint { x: 5.5, y: -2.0 });

        let user_data = reference_point_user_data(&layer, 3).expect("meaningful point");
        let points = read_reference_point_user_data(Some(&user_data), 3);
        assert_eq!(points[0], None);
        assert_eq!(points[1], Some(AnimationPoint { x: 5.5, y: -2.0 }));
        assert_eq!(points[2], None);
    }
}
