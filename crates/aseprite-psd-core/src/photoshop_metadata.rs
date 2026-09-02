//! Shared Photoshop metadata classification for import and export.

use crate::{AnimationPoint, NormalizedBounds, NormalizedLayer, NormalizedLayerKind};

const POINT_EPSILON: f64 = 1.0e-6;

/// Returns whether a frame reference point is a finite, non-default value.
pub(crate) fn has_meaningful_reference_point(layer: &NormalizedLayer, frame_index: usize) -> bool {
    layer
        .frame_states
        .get(frame_index)
        .and_then(|state| state.reference_point)
        .is_some_and(|point| {
            point.x.is_finite()
                && point.y.is_finite()
                && !is_default_reference_point(layer, frame_index, point)
        })
}

/// Returns whether a layer itself contains at least one meaningful reference point.
pub(crate) fn layer_has_meaningful_reference_point(layer: &NormalizedLayer) -> bool {
    layer
        .frame_states
        .iter()
        .enumerate()
        .any(|(frame_index, _)| has_meaningful_reference_point(layer, frame_index))
}

/// Returns whether the stored point matches the effective frame bounds center.
pub(crate) fn is_default_reference_point(
    layer: &NormalizedLayer,
    frame_index: usize,
    point: AnimationPoint,
) -> bool {
    let Some(bounds) = effective_bounds(layer, frame_index) else {
        return false;
    };
    let center_x = (bounds.left + bounds.right) / 2.0;
    let center_y = (bounds.top + bounds.bottom) / 2.0;
    (point.x - center_x).abs() <= POINT_EPSILON && (point.y - center_y).abs() <= POINT_EPSILON
}

/// Computes the visible document-space bounds used for the default pivot.
fn effective_bounds(layer: &NormalizedLayer, frame_index: usize) -> Option<Bounds> {
    let state = layer.frame_states.get(frame_index)?;
    let offset = state.offset.unwrap_or(AnimationPoint { x: 0.0, y: 0.0 });
    let own_bounds = Bounds::from_normalized(layer.bounds).map(|bounds| bounds.translated(offset));

    if layer.kind == NormalizedLayerKind::Pixel {
        return own_bounds;
    }

    let child_bounds = layer
        .children
        .iter()
        .filter(|child| {
            child
                .frame_states
                .get(frame_index)
                .is_some_and(|child_state| child_state.enabled)
        })
        .filter_map(|child| effective_bounds(child, frame_index))
        .fold(None, |result, bounds| {
            result.map_or(Some(bounds), |value: Bounds| Some(value.union(bounds)))
        });

    child_bounds
        .map(|bounds| bounds.translated(offset))
        .or(own_bounds)
}

#[derive(Clone, Copy)]
struct Bounds {
    left: f64,
    top: f64,
    right: f64,
    bottom: f64,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{NormalizedLayerFrameState, NormalizedPixels};

    fn state(frame_index: u32) -> NormalizedLayerFrameState {
        NormalizedLayerFrameState {
            frame_index,
            record_present: true,
            enabled: true,
            explicit_enable: true,
            offset: None,
            reference_point: None,
            opacity: None,
        }
    }

    fn pixel(bounds: NormalizedBounds, states: Vec<NormalizedLayerFrameState>) -> NormalizedLayer {
        NormalizedLayer {
            id: 1,
            name: "pixel".to_string(),
            kind: NormalizedLayerKind::Pixel,
            bounds,
            opacity: None,
            blend_mode: None,
            hidden: Some(false),
            pixels: Some(NormalizedPixels {
                width: (bounds.right - bounds.left) as u32,
                height: (bounds.bottom - bounds.top) as u32,
                left: bounds.left,
                top: bounds.top,
                data: vec![1, 2, 3, 255],
            }),
            children: Vec::new(),
            frame_states: states,
        }
    }

    #[test]
    fn filters_even_odd_and_fractional_default_centers() {
        let mut even = pixel(
            NormalizedBounds {
                left: 0,
                top: 0,
                right: 4,
                bottom: 2,
            },
            vec![state(0)],
        );
        even.frame_states[0].reference_point = Some(AnimationPoint { x: 2.0, y: 1.0 });
        assert!(!has_meaningful_reference_point(&even, 0));

        let mut odd = pixel(
            NormalizedBounds {
                left: -1,
                top: -1,
                right: 2,
                bottom: 2,
            },
            vec![state(0)],
        );
        odd.frame_states[0].reference_point = Some(AnimationPoint { x: 0.5, y: 0.5 });
        assert!(!has_meaningful_reference_point(&odd, 0));

        odd.frame_states[0].reference_point = Some(AnimationPoint { x: 0.6, y: 0.5 });
        assert!(has_meaningful_reference_point(&odd, 0));
    }

    #[test]
    fn preserves_external_and_negative_reference_points() {
        let mut layer = pixel(
            NormalizedBounds {
                left: 10,
                top: 20,
                right: 14,
                bottom: 24,
            },
            vec![state(0)],
        );
        layer.frame_states[0].reference_point = Some(AnimationPoint { x: -3.0, y: 40.0 });
        assert!(has_meaningful_reference_point(&layer, 0));
    }

    #[test]
    fn uses_visible_child_union_for_group_center() {
        let mut child = pixel(
            NormalizedBounds {
                left: 0,
                top: 0,
                right: 4,
                bottom: 2,
            },
            vec![state(0)],
        );
        child.frame_states[0].offset = Some(AnimationPoint { x: 2.0, y: 4.0 });
        let mut group_state = state(0);
        group_state.offset = Some(AnimationPoint { x: 10.0, y: 20.0 });
        let mut group = NormalizedLayer {
            id: 2,
            name: "group".to_string(),
            kind: NormalizedLayerKind::Group,
            bounds: NormalizedBounds {
                left: 0,
                top: 0,
                right: 0,
                bottom: 0,
            },
            opacity: None,
            blend_mode: None,
            hidden: Some(false),
            pixels: None,
            children: vec![child],
            frame_states: vec![group_state],
        };
        group.frame_states[0].reference_point = Some(AnimationPoint { x: 14.0, y: 25.0 });
        assert!(!has_meaningful_reference_point(&group, 0));
        group.frame_states[0].reference_point = Some(AnimationPoint { x: 15.0, y: 25.0 });
        assert!(has_meaningful_reference_point(&group, 0));
    }

    #[test]
    fn missing_bounds_are_kept_conservatively() {
        let mut layer = pixel(
            NormalizedBounds {
                left: 0,
                top: 0,
                right: 0,
                bottom: 0,
            },
            vec![state(0)],
        );
        layer.frame_states[0].reference_point = Some(AnimationPoint { x: 0.0, y: 0.0 });
        assert!(has_meaningful_reference_point(&layer, 0));
    }
}

impl Bounds {
    /// Converts a normalized integral rectangle into floating-point bounds.
    fn from_normalized(value: NormalizedBounds) -> Option<Self> {
        (value.right > value.left && value.bottom > value.top).then_some(Self {
            left: f64::from(value.left),
            top: f64::from(value.top),
            right: f64::from(value.right),
            bottom: f64::from(value.bottom),
        })
    }

    /// Applies a frame-local translation to these bounds.
    fn translated(self, offset: AnimationPoint) -> Self {
        Self {
            left: self.left + offset.x,
            top: self.top + offset.y,
            right: self.right + offset.x,
            bottom: self.bottom + offset.y,
        }
    }

    /// Returns the union of two document-space rectangles.
    fn union(self, other: Self) -> Self {
        Self {
            left: self.left.min(other.left),
            top: self.top.min(other.top),
            right: self.right.max(other.right),
            bottom: self.bottom.max(other.bottom),
        }
    }
}
