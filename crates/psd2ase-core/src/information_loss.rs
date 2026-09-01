//! Structured compatibility losses collected during PSD conversion.

use std::fmt;

/// Stable identifiers for source information that cannot be represented exactly.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum InformationLossCode {
    UnsupportedColor,
    PixelMask,
    VectorMask,
    Clipping,
    TextLayer,
    AdjustmentLayer,
    LayerEffects,
    SmartObject,
    Slices,
    LayerComps,
    Artboards,
    EmbeddedColorProfile,
    UnknownBlendMode,
    OpacityQuantization,
    ReferencePoint,
    GroupFrameOpacity,
    ActiveFrame,
    PixelLayerChildren,
}

impl InformationLossCode {
    /// Returns the stable wire identifier used by CLI reports.
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::UnsupportedColor => "unsupported_color",
            Self::PixelMask => "pixel_mask",
            Self::VectorMask => "vector_mask",
            Self::Clipping => "clipping",
            Self::TextLayer => "text_layer",
            Self::AdjustmentLayer => "adjustment_layer",
            Self::LayerEffects => "layer_effects",
            Self::SmartObject => "smart_object",
            Self::Slices => "slices",
            Self::LayerComps => "layer_comps",
            Self::Artboards => "artboards",
            Self::EmbeddedColorProfile => "embedded_color_profile",
            Self::UnknownBlendMode => "unknown_blend_mode",
            Self::OpacityQuantization => "opacity_quantization",
            Self::ReferencePoint => "reference_point",
            Self::GroupFrameOpacity => "group_frame_opacity",
            Self::ActiveFrame => "active_frame",
            Self::PixelLayerChildren => "pixel_layer_children",
        }
    }
}

/// Describes how a source value was handled by the output mapping.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum LossDisposition {
    Rasterized,
    Degraded,
    Dropped,
    Unknown,
}

impl LossDisposition {
    /// Returns the stable wire identifier used by CLI reports.
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Rasterized => "rasterized",
            Self::Degraded => "degraded",
            Self::Dropped => "dropped",
            Self::Unknown => "unknown",
        }
    }
}

/// A source location associated with a loss.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InformationLocation {
    pub layer_id: Option<u32>,
    pub path: String,
}

/// One aggregated compatibility loss.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InformationLoss {
    pub code: InformationLossCode,
    pub disposition: LossDisposition,
    pub count: usize,
    pub locations: Vec<InformationLocation>,
    pub detail: String,
    pub visual_impact: bool,
    pub editability_impact: bool,
}

/// The single compatibility-loss accumulator shared by reader and writer.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct InformationLossReport {
    pub entries: Vec<InformationLoss>,
}

impl InformationLossReport {
    /// Adds one occurrence, aggregating by stable code and disposition.
    pub fn add(
        &mut self,
        code: InformationLossCode,
        disposition: LossDisposition,
        location: InformationLocation,
        detail: impl Into<String>,
        visual_impact: bool,
        editability_impact: bool,
    ) {
        let detail = detail.into();
        if let Some(entry) = self
            .entries
            .iter_mut()
            .find(|entry| entry.code == code && entry.disposition == disposition)
        {
            entry.count += 1;
            if entry.locations.len() < 8 {
                entry.locations.push(location);
            }
            return;
        }
        self.entries.push(InformationLoss {
            code,
            disposition,
            count: 1,
            locations: vec![location],
            detail,
            visual_impact,
            editability_impact,
        });
    }
    /// Returns whether at least one loss was recorded.
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }
}

impl fmt::Display for InformationLossCode {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn aggregates_loss_occurrences_and_bounds_locations() {
        let mut report = InformationLossReport::default();
        for index in 0..10 {
            report.add(
                InformationLossCode::Clipping,
                LossDisposition::Dropped,
                InformationLocation {
                    layer_id: Some(index),
                    path: format!("layer/{index}"),
                },
                "clipping is not represented",
                true,
                true,
            );
        }
        assert_eq!(report.entries.len(), 1);
        assert_eq!(report.entries[0].count, 10);
        assert_eq!(report.entries[0].locations.len(), 8);
    }
}
