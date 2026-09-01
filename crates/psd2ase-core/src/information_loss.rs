//! Structured compatibility losses collected during PSD conversion.

use serde::Serialize;
use std::fmt;
use std::io;
use std::path::Path;

use crate::atomic_output::commit_bytes;

/// Stable identifiers for source information that cannot be represented exactly.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize)]
#[serde(rename_all = "snake_case")]
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
    AnimationTagName,
    Tilemap,
    CelZIndex,
    EmptyPixelLayer,
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
            Self::AnimationTagName => "animation_tag_name",
            Self::Tilemap => "tilemap",
            Self::CelZIndex => "cel_z_index",
            Self::EmptyPixelLayer => "empty_pixel_layer",
        }
    }
}

/// Describes how a source value was handled by the output mapping.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize)]
#[serde(rename_all = "snake_case")]
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
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct InformationLocation {
    pub layer_id: Option<u32>,
    pub path: String,
}

/// One aggregated compatibility loss.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
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
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct InformationLossReport {
    pub entries: Vec<InformationLoss>,
}

#[derive(Serialize)]
struct JsonReport<'a> {
    schema_version: u32,
    tool_version: &'static str,
    input: String,
    output: String,
    summary: JsonSummary,
    losses: &'a [InformationLoss],
}

#[derive(Serialize)]
struct JsonSummary {
    total: usize,
}

/// Serializes one compatibility report using the shared stable JSON schema.
pub fn report_json(
    input: &Path,
    output: &Path,
    report: &InformationLossReport,
) -> Result<Vec<u8>, serde_json::Error> {
    serde_json::to_vec_pretty(&JsonReport {
        schema_version: 1,
        tool_version: crate::VERSION,
        input: input.display().to_string(),
        output: output.display().to_string(),
        summary: JsonSummary {
            total: report.entries.len(),
        },
        losses: &report.entries,
    })
}

/// Atomically writes one compatibility report, replacing a prior report.
pub fn write_report(
    path: &Path,
    input: &Path,
    output: &Path,
    report: &InformationLossReport,
) -> io::Result<()> {
    let payload = report_json(input, output, report).map_err(io::Error::other)?;
    commit_bytes(path, &payload, true)
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
