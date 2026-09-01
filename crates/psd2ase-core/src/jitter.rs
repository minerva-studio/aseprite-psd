//! Conservative pixel stabilization for imported animation cels.

use std::collections::{HashMap, HashSet, VecDeque};

use crate::{LayerWritePlan, NormalizedDocument, NormalizedLayerKind, NormalizedPixels};

/// Selects whether jitter detection changes output pixels or only supplies evidence.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum JitterMode {
    /// Do not inspect or alter pixels.
    #[default]
    Off,
    /// Inspect pixels and report candidates without changing output.
    Report,
    /// Supply stabilized comparison evidence without changing output.
    Assist,
    /// Apply accepted stabilizations to the converted output.
    Repair,
}

/// Selects which stabilization passes are enabled.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum JitterKind {
    /// Detect and repair low-alpha specks only.
    Alpha,
    /// Detect and repair small color differences only.
    Color,
    /// Run both stabilization passes.
    #[default]
    All,
}

/// Conservative presets for jitter thresholds.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum JitterProfile {
    /// Favor avoiding accidental removal of authored pixels.
    #[default]
    Conservative,
    /// Accept a larger but still bounded amount of noise.
    Balanced,
}

/// Numeric thresholds used by the jitter passes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct JitterThresholds {
    /// Maximum alpha treated as a speck candidate.
    pub alpha_threshold: u8,
    /// Maximum four-connected candidate speck area.
    pub max_speck_area: usize,
    /// Maximum fraction of changed pixels for color canonicalization.
    pub max_changed_ratio_percent: u8,
    /// Maximum absolute channel difference for a changed pixel.
    pub max_channel_delta: u8,
}

impl JitterProfile {
    /// Returns stable defaults for the selected profile.
    pub const fn thresholds(self) -> JitterThresholds {
        match self {
            Self::Conservative => JitterThresholds {
                alpha_threshold: 8,
                max_speck_area: 2,
                max_changed_ratio_percent: 1,
                max_channel_delta: 6,
            },
            Self::Balanced => JitterThresholds {
                alpha_threshold: 16,
                max_speck_area: 4,
                max_changed_ratio_percent: 3,
                max_channel_delta: 12,
            },
        }
    }
}

/// Public conversion configuration for pixel stabilization.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct JitterOptions {
    /// Detection, evidence, and mutation mode.
    pub mode: JitterMode,
    /// Enabled stabilization passes.
    pub kind: JitterKind,
    /// Preset threshold family.
    pub profile: JitterProfile,
    /// Optional override for the alpha threshold.
    pub alpha_threshold: Option<u8>,
    /// Optional override for maximum speck area.
    pub max_speck_area: Option<usize>,
    /// Optional override for changed-pixel ratio in percent.
    pub max_changed_ratio_percent: Option<u8>,
    /// Optional override for per-channel color delta.
    pub max_channel_delta: Option<u8>,
}

impl Default for JitterOptions {
    fn default() -> Self {
        Self {
            mode: JitterMode::Off,
            kind: JitterKind::All,
            profile: JitterProfile::Conservative,
            alpha_threshold: None,
            max_speck_area: None,
            max_changed_ratio_percent: None,
            max_channel_delta: None,
        }
    }
}

impl JitterOptions {
    /// Resolves preset values and validates user overrides.
    pub fn thresholds(self) -> Result<JitterThresholds, String> {
        let mut value = self.profile.thresholds();
        if let Some(v) = self.alpha_threshold {
            value.alpha_threshold = v;
        }
        if let Some(v) = self.max_speck_area {
            if v == 0 {
                return Err("--jitter-max-speck-area must be greater than zero".to_string());
            }
            value.max_speck_area = v;
        }
        if let Some(v) = self.max_changed_ratio_percent {
            if v > 100 {
                return Err("--jitter-max-changed-ratio must be between 0 and 100".to_string());
            }
            value.max_changed_ratio_percent = v;
        }
        if let Some(v) = self.max_channel_delta {
            value.max_channel_delta = v;
        }
        Ok(value)
    }
}

/// A deterministic summary of stabilization work.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct JitterReport {
    /// Number of visible source cels inspected.
    pub inspected_cels: usize,
    /// Number of alpha speck candidates found.
    pub alpha_candidates: usize,
    /// Number of alpha specks actually cleared.
    pub alpha_repairs: usize,
    /// Number of color comparison candidates found.
    pub color_candidates: usize,
    /// Number of color representative mappings applied.
    pub color_repairs: usize,
    /// Human-readable accepted and rejected decisions.
    pub diagnostics: Vec<String>,
}

/// Resolved pixel data and diagnostics used by all downstream consumers.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct JitterPlan {
    /// Per-source-layer repaired RGBA buffers.
    pub repaired_pixels: HashMap<u32, Vec<u8>>,
    /// Per-source-layer representative pixel source for color repair.
    pub representative_layers: HashMap<u32, u32>,
    /// Stabilization diagnostics.
    pub report: JitterReport,
}

/// Builds a stabilization plan from normalized pixels and an optional logical plan.
pub fn build_jitter_plan(
    document: &NormalizedDocument,
    logical_plan: Option<&LayerWritePlan>,
    options: JitterOptions,
) -> Result<JitterPlan, String> {
    let thresholds = options.thresholds()?;
    let mut plan = JitterPlan::default();
    let mut layers = Vec::new();
    collect_pixel_layers(&document.root_layers, &mut layers);
    let visible = visible_layer_ids(document);
    plan.report.inspected_cels = visible.len();

    if options.mode == JitterMode::Off {
        return Ok(plan);
    }
    if matches!(options.kind, JitterKind::Alpha | JitterKind::All) {
        for layer in layers.iter().filter(|layer| visible.contains(&layer.id)) {
            let pixels = layer
                .pixels
                .as_ref()
                .ok_or_else(|| format!("pixel layer {} has no normalized pixels", layer.id))?;
            let (data, candidates, repairs) = clear_alpha_specks(pixels, thresholds);
            plan.report.alpha_candidates += candidates;
            plan.report.alpha_repairs += repairs;
            if repairs > 0 && matches!(options.mode, JitterMode::Assist | JitterMode::Repair) {
                plan.repaired_pixels.insert(layer.id, data);
            }
        }
    }
    if matches!(options.kind, JitterKind::Color | JitterKind::All) {
        let Some(logical_plan) = logical_plan else {
            if options.mode == JitterMode::Assist {
                return Err("--jitter-mode assist with color stabilization requires --layer-association auto".to_string());
            }
            if options.mode == JitterMode::Repair {
                return Err("--jitter-kind color/all with --jitter-mode repair requires --layer-association auto".to_string());
            }
            return Ok(plan);
        };
        for track in &logical_plan.tracks {
            let ids = track
                .cels
                .iter()
                .flatten()
                .map(|cel| cel.source_layer_id)
                .collect::<Vec<_>>();
            let mut unique = ids;
            unique.sort_unstable();
            unique.dedup();
            for left in 0..unique.len() {
                for right in (left + 1)..unique.len() {
                    let left_layer = document
                        .find_layer(unique[left])
                        .ok_or_else(|| "planned source layer was not found".to_string())?;
                    let right_layer = document
                        .find_layer(unique[right])
                        .ok_or_else(|| "planned source layer was not found".to_string())?;
                    let (Some(left_pixels), Some(right_pixels)) =
                        (left_layer.pixels.as_ref(), right_layer.pixels.as_ref())
                    else {
                        continue;
                    };
                    if left_pixels.width != right_pixels.width
                        || left_pixels.height != right_pixels.height
                        || left_pixels.left != right_pixels.left
                        || left_pixels.top != right_pixels.top
                    {
                        continue;
                    }
                    let left_data = plan
                        .repaired_pixels
                        .get(&left_layer.id)
                        .unwrap_or(&left_pixels.data);
                    let right_data = plan
                        .repaired_pixels
                        .get(&right_layer.id)
                        .unwrap_or(&right_pixels.data);
                    if near_equal(left_data, right_data, thresholds) {
                        plan.report.color_candidates += 1;
                        let representative = if left_layer.id <= right_layer.id {
                            left_layer.id
                        } else {
                            right_layer.id
                        };
                        let replaced = if representative == left_layer.id {
                            right_layer.id
                        } else {
                            left_layer.id
                        };
                        if matches!(options.mode, JitterMode::Assist | JitterMode::Repair) {
                            plan.representative_layers.insert(replaced, representative);
                        }
                        plan.report.color_repairs +=
                            usize::from(options.mode == JitterMode::Repair);
                        plan.report.diagnostics.push(format!("color jitter: layer {replaced} uses representative layer {representative}"));
                    }
                }
            }
        }
    }
    Ok(plan)
}

/// Resolves one source layer's final pixels through alpha and color mappings.
pub fn resolved_pixels<'a>(
    document: &'a NormalizedDocument,
    plan: &'a JitterPlan,
    source_layer_id: u32,
) -> Option<NormalizedPixels> {
    let source_id = plan
        .representative_layers
        .get(&source_layer_id)
        .copied()
        .unwrap_or(source_layer_id);
    let source = document.find_layer(source_id)?;
    let pixels = source.pixels.as_ref()?.clone();
    let data = plan
        .repaired_pixels
        .get(&source_id)
        .cloned()
        .unwrap_or(pixels.data);
    Some(NormalizedPixels { data, ..pixels })
}

/// Returns a cloned normalized document with the plan applied for evidence passes.
pub fn stabilized_document(document: &NormalizedDocument, plan: &JitterPlan) -> NormalizedDocument {
    fn visit(
        layer: &crate::NormalizedLayer,
        document: &NormalizedDocument,
        plan: &JitterPlan,
    ) -> crate::NormalizedLayer {
        let mut clone = layer.clone();
        if layer.kind == NormalizedLayerKind::Pixel
            && let Some(pixels) = resolved_pixels(document, plan, layer.id)
        {
            clone.pixels = Some(pixels);
        }
        clone.children = layer
            .children
            .iter()
            .map(|child| visit(child, document, plan))
            .collect();
        clone
    }
    let mut clone = document.clone();
    clone.root_layers = document
        .root_layers
        .iter()
        .map(|layer| visit(layer, document, plan))
        .collect();
    clone
}

fn collect_pixel_layers<'a>(
    layers: &'a [crate::NormalizedLayer],
    output: &mut Vec<&'a crate::NormalizedLayer>,
) {
    for layer in layers {
        if layer.kind == NormalizedLayerKind::Pixel {
            output.push(layer);
        }
        collect_pixel_layers(&layer.children, output);
    }
}

fn visible_layer_ids(document: &NormalizedDocument) -> HashSet<u32> {
    let mut ids = HashSet::new();
    for frame in 0..document.frames.len() {
        for layer in &document.root_layers {
            let mut visible = Vec::new();
            layer.collect_visible_pixel_layer_ids(frame, true, &mut visible);
            ids.extend(visible);
        }
    }
    ids
}

fn clear_alpha_specks(
    pixels: &NormalizedPixels,
    thresholds: JitterThresholds,
) -> (Vec<u8>, usize, usize) {
    let width = pixels.width as usize;
    let height = pixels.height as usize;
    let mut data = pixels.data.clone();
    let mut visited = vec![false; width.saturating_mul(height)];
    let mut candidates = 0;
    let mut repairs = 0;
    for index in 0..visited.len() {
        if visited[index]
            || data[index * 4 + 3] == 0
            || data[index * 4 + 3] > thresholds.alpha_threshold
        {
            continue;
        }
        candidates += 1;
        let mut queue = VecDeque::from([index]);
        let mut component = Vec::new();
        let mut touches_opaque = false;
        visited[index] = true;
        while let Some(current) = queue.pop_front() {
            component.push(current);
            let x = current % width;
            let y = current / width;
            for (nx, ny) in [
                (x.wrapping_sub(1), y),
                (x + 1, y),
                (x, y.wrapping_sub(1)),
                (x, y + 1),
            ] {
                if nx >= width || ny >= height {
                    continue;
                }
                let next = ny * width + nx;
                let alpha = data[next * 4 + 3];
                if alpha == 0 {
                    continue;
                }
                if alpha <= thresholds.alpha_threshold {
                    if !visited[next] {
                        visited[next] = true;
                        queue.push_back(next);
                    }
                } else {
                    touches_opaque = true;
                }
            }
        }
        if component.len() <= thresholds.max_speck_area && !touches_opaque {
            for pixel in component {
                data[pixel * 4..pixel * 4 + 4].copy_from_slice(&[0, 0, 0, 0]);
            }
            repairs += 1;
        }
    }
    (data, candidates, repairs)
}

fn near_equal(left: &[u8], right: &[u8], thresholds: JitterThresholds) -> bool {
    if left.len() != right.len() {
        return false;
    }
    let mut changed = 0usize;
    for (a, b) in left
        .as_chunks::<4>()
        .0
        .iter()
        .zip(right.as_chunks::<4>().0.iter())
    {
        if a == b {
            continue;
        }
        let delta = a
            .iter()
            .zip(b)
            .map(|(x, y)| x.abs_diff(*y))
            .max()
            .unwrap_or(0);
        if delta > thresholds.max_channel_delta {
            return false;
        }
        changed += 1;
    }
    changed > 0
        && changed.saturating_mul(100)
            <= left.len() / 4 * usize::from(thresholds.max_changed_ratio_percent)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pixels(data: Vec<u8>, width: u32, height: u32) -> NormalizedPixels {
        NormalizedPixels {
            width,
            height,
            left: 0,
            top: 0,
            data,
        }
    }

    #[test]
    fn clears_isolated_low_alpha_pixel_but_preserves_attached_pixel() {
        let thresholds = JitterProfile::Conservative.thresholds();
        let isolated = pixels(vec![1, 2, 3, 4, 0, 0, 0, 0, 10, 20, 30, 255], 3, 1);
        let (data, candidates, repairs) = clear_alpha_specks(&isolated, thresholds);
        assert_eq!(candidates, 1);
        assert_eq!(repairs, 1);
        assert_eq!(&data[..4], &[0, 0, 0, 0]);

        let attached = pixels(vec![1, 2, 3, 4, 10, 20, 30, 255], 2, 1);
        let (_, _, repairs) = clear_alpha_specks(&attached, thresholds);
        assert_eq!(repairs, 0);
    }

    #[test]
    fn near_equal_obeys_changed_ratio_and_channel_delta() {
        let thresholds = JitterProfile::Conservative.thresholds();
        let left = vec![10, 10, 10, 255, 10, 10, 10, 255];
        let right = vec![15, 10, 10, 255, 10, 10, 10, 255];
        assert!(!near_equal(&left, &right, thresholds));
        let mut relaxed = thresholds;
        relaxed.max_changed_ratio_percent = 50;
        assert!(near_equal(&left, &right, relaxed));
    }
}
