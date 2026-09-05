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
    #[serde(skip_serializing_if = "Option::is_none")]
    pub frame_index: Option<u32>,
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
    #[serde(skip_serializing_if = "Option::is_none")]
    active_frame_index: Option<u32>,
    summary: JsonSummary,
    losses: &'a [InformationLoss],
    #[serde(skip_serializing_if = "Option::is_none")]
    content_reuse: Option<JsonContentReuse>,
}

#[derive(Serialize)]
struct JsonContentReuse {
    requested: &'static str,
    actual: &'static str,
    baseline_physical_layer_count: usize,
    physical_layer_count: usize,
    explicit_link_reuse_count: usize,
    exact_match_reuse_count: usize,
    fallback_reasons: Vec<String>,
    output_bytes: usize,
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
    report_json_with_active_frame(input, output, report, None)
}

/// Serializes a compatibility report with an optional source active frame.
pub fn report_json_with_active_frame(
    input: &Path,
    output: &Path,
    report: &InformationLossReport,
    active_frame_index: Option<u32>,
) -> Result<Vec<u8>, serde_json::Error> {
    serde_json::to_vec_pretty(&JsonReport {
        schema_version: 3,
        tool_version: crate::VERSION,
        input: input.display().to_string(),
        output: output.display().to_string(),
        active_frame_index,
        summary: JsonSummary {
            total: report.entries.len(),
        },
        losses: &report.entries,
        content_reuse: None,
    })
}

/// Serializes a compatibility report with the export layout and byte statistics.
#[allow(clippy::too_many_arguments)]
pub fn report_json_with_export(
    input: &Path,
    output: &Path,
    report: &InformationLossReport,
    active_frame_index: Option<u32>,
    requested: &'static str,
    actual: &'static str,
    baseline_physical_layer_count: usize,
    physical_layer_count: usize,
    explicit_link_reuse_count: usize,
    exact_match_reuse_count: usize,
    fallback_reasons: Vec<String>,
    output_bytes: usize,
) -> Result<Vec<u8>, serde_json::Error> {
    serde_json::to_vec_pretty(&JsonReport {
        schema_version: 3,
        tool_version: crate::VERSION,
        input: input.display().to_string(),
        output: output.display().to_string(),
        active_frame_index,
        summary: JsonSummary {
            total: report.entries.len(),
        },
        losses: &report.entries,
        content_reuse: Some(JsonContentReuse {
            requested,
            actual,
            baseline_physical_layer_count,
            physical_layer_count,
            explicit_link_reuse_count,
            exact_match_reuse_count,
            fallback_reasons,
            output_bytes,
        }),
    })
}

/// Atomically writes one compatibility report, replacing a prior report.
pub fn write_report(
    path: &Path,
    input: &Path,
    output: &Path,
    report: &InformationLossReport,
) -> io::Result<()> {
    write_report_with_active_frame(path, input, output, report, None)
}

/// Atomically writes a compatibility report with an optional active frame.
pub fn write_report_with_active_frame(
    path: &Path,
    input: &Path,
    output: &Path,
    report: &InformationLossReport,
    active_frame_index: Option<u32>,
) -> io::Result<()> {
    let payload = report_json_with_active_frame(input, output, report, active_frame_index)
        .map_err(io::Error::other)?;
    commit_bytes(path, &payload, true)
}

/// Atomically writes a compatibility report with export layout statistics.
#[allow(clippy::too_many_arguments)]
pub fn write_export_report_with_active_frame(
    path: &Path,
    input: &Path,
    output: &Path,
    report: &InformationLossReport,
    active_frame_index: Option<u32>,
    requested: &'static str,
    actual: &'static str,
    baseline_physical_layer_count: usize,
    physical_layer_count: usize,
    explicit_link_reuse_count: usize,
    exact_match_reuse_count: usize,
    fallback_reasons: Vec<String>,
    output_bytes: usize,
) -> io::Result<()> {
    let bytes = report_json_with_export(
        input,
        output,
        report,
        active_frame_index,
        requested,
        actual,
        baseline_physical_layer_count,
        physical_layer_count,
        explicit_link_reuse_count,
        exact_match_reuse_count,
        fallback_reasons,
        output_bytes,
    )
    .map_err(|error| io::Error::other(error.to_string()))?;
    commit_bytes(path, &bytes, true).map_err(io::Error::other)
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
                    frame_index: None,
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

    #[test]
    fn report_json_uses_v3_and_omits_unset_frame_indices() {
        let mut report = InformationLossReport::default();
        report.add(
            InformationLossCode::ReferencePoint,
            LossDisposition::Dropped,
            InformationLocation {
                layer_id: Some(42),
                path: "角色/手臂".to_string(),
                frame_index: Some(2),
            },
            "reference point is not serialized",
            false,
            true,
        );
        report.add(
            InformationLossCode::ActiveFrame,
            LossDisposition::Dropped,
            InformationLocation {
                layer_id: None,
                path: String::new(),
                frame_index: None,
            },
            "active frame is not serialized",
            false,
            true,
        );

        let value: serde_json::Value = serde_json::from_slice(
            &report_json(
                Path::new("input.psd"),
                Path::new("output.aseprite"),
                &report,
            )
            .expect("report JSON should serialize"),
        )
        .expect("report JSON should decode");
        assert_eq!(value["schema_version"], 3);
        assert!(value.get("active_frame_index").is_none());
        assert_eq!(value["losses"][0]["locations"][0]["frame_index"], 2);
        assert!(
            value["losses"][1]["locations"][0]
                .get("frame_index")
                .is_none()
        );
    }

    #[test]
    fn report_json_includes_optional_active_frame_index() {
        let report = InformationLossReport::default();
        let value: serde_json::Value = serde_json::from_slice(
            &report_json_with_active_frame(
                Path::new("input.psd"),
                Path::new("output.aseprite"),
                &report,
                Some(8),
            )
            .expect("report JSON should serialize"),
        )
        .expect("report JSON should decode");
        assert_eq!(value["schema_version"], 3);
        assert_eq!(value["active_frame_index"], 8);
    }

    #[test]
    fn export_report_json_includes_content_reuse_statistics() {
        let report = InformationLossReport::default();
        let value: serde_json::Value = serde_json::from_slice(
            &report_json_with_export(
                Path::new("input.aseprite"),
                Path::new("output.psd"),
                &report,
                Some(1),
                "aggressive",
                "aggressive",
                12,
                8,
                0,
                2,
                vec!["tilemap input fell back to frame folders".to_string()],
                4096,
            )
            .expect("export report JSON should serialize"),
        )
        .expect("export report JSON should decode");
        assert_eq!(value["content_reuse"]["requested"], "aggressive");
        assert_eq!(value["content_reuse"]["actual"], "aggressive");
        assert_eq!(value["content_reuse"]["baseline_physical_layer_count"], 12);
        assert_eq!(value["content_reuse"]["physical_layer_count"], 8);
        assert_eq!(value["content_reuse"]["exact_match_reuse_count"], 2);
        assert_eq!(value["content_reuse"]["output_bytes"], 4096);
        assert_eq!(
            value["content_reuse"]["fallback_reasons"][0],
            "tilemap input fell back to frame folders"
        );
    }
}
