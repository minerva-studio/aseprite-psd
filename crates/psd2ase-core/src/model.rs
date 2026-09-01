/// Metadata extracted during read-only inspection.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DocumentInspection {
    /// Canvas width in pixels.
    pub width: u32,
    /// Canvas height in pixels.
    pub height: u32,
    /// Bits per channel, when declared by the source document.
    pub bits_per_channel: Option<u32>,
    /// Parser-reported color mode.
    pub color_mode: Option<String>,
    /// Number of top-level PSD layers.
    pub root_layer_count: usize,
}

/// Format-neutral document model used as the boundary between readers and writers.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct NormalizedDocument {
    /// Canvas dimensions in pixels.
    pub canvas: (u32, u32),
    /// Number of document channels, when declared by the source.
    pub channels: Option<u32>,
    /// Bits per channel, when declared by the source.
    pub bits_per_channel: Option<u32>,
    /// Stable lowercase color-mode name, when declared by the source.
    pub color_mode: Option<String>,
    /// Top-level layers in source order.
    pub root_layers: Vec<NormalizedLayer>,
    /// Animation frames in playback order.
    pub frames: Vec<NormalizedFrame>,
    /// Source animation loop policy, when one was authored.
    pub loop_mode: Option<NormalizedLoopMode>,
    /// Active source frame index, when one was authored.
    pub active_frame_index: Option<u32>,
    /// Image-resource IDs that carried the source animation descriptor.
    pub animation_resource_ids: Vec<u16>,
    /// Optional source animation flags.
    pub animation_frame_flags: Option<crate::AnimationFlags>,
}

impl NormalizedDocument {
    /// Finds a normalized layer by its stable source identifier.
    pub(crate) fn find_layer(&self, id: u32) -> Option<&NormalizedLayer> {
        find_layer_in(&self.root_layers, id)
    }
}

fn find_layer_in(layers: &[NormalizedLayer], id: u32) -> Option<&NormalizedLayer> {
    for layer in layers {
        if layer.id == id {
            return Some(layer);
        }
        if let Some(found) = find_layer_in(&layer.children, id) {
            return Some(found);
        }
    }
    None
}

/// A normalized PSD group or pixel layer.
#[derive(Debug, Clone, PartialEq)]
pub struct NormalizedLayer {
    /// Stable source identifier assigned by Photoshop.
    pub id: u32,
    /// User-authored layer name.
    pub name: String,
    /// Layer kind.
    pub kind: NormalizedLayerKind,
    /// Layer bounds in document coordinates.
    pub bounds: NormalizedBounds,
    /// Base layer opacity in the normalized 0.0..=1.0 range, when declared by the source.
    pub opacity: Option<f64>,
    /// Base layer blend mode, when declared by the source.
    pub blend_mode: Option<String>,
    /// Base layer hidden state, when declared by the source.
    pub hidden: Option<bool>,
    /// Owned RGBA8 pixels for a pixel layer.
    pub pixels: Option<NormalizedPixels>,
    /// Child layers in source order. Groups own this vector; pixel layers keep it empty.
    pub children: Vec<NormalizedLayer>,
    /// Per-frame state; effective visibility is derived with ancestor state at read time.
    pub frame_states: Vec<NormalizedLayerFrameState>,
}

/// The two layer kinds currently supported by the normalized reader.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NormalizedLayerKind {
    /// A recursive group layer.
    Group,
    /// A layer containing owned RGBA8 pixels.
    Pixel,
}

/// An integral layer rectangle in document coordinates.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct NormalizedBounds {
    /// Inclusive left coordinate.
    pub left: i32,
    /// Inclusive top coordinate.
    pub top: i32,
    /// Exclusive right coordinate.
    pub right: i32,
    /// Exclusive bottom coordinate.
    pub bottom: i32,
}

/// Owned RGBA8 layer data and its original document-space origin.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NormalizedPixels {
    /// Pixel width.
    pub width: u32,
    /// Pixel height.
    pub height: u32,
    /// Original document-space left coordinate.
    pub left: i32,
    /// Original document-space top coordinate.
    pub top: i32,
    /// RGBA8 bytes owned by the normalized model.
    pub data: Vec<u8>,
}

/// A normalized animation frame.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NormalizedFrame {
    /// Zero-based frame index.
    pub index: u32,
    /// Photoshop frame identifier, absent for a synthetic static frame.
    pub source_id: Option<u32>,
    /// Source-authored duration; static PSDs intentionally keep this unset.
    pub duration_ms: Option<u32>,
    /// Source-authored disposal policy, when present.
    pub dispose: Option<String>,
}

/// A normalized loop policy.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum NormalizedLoopMode {
    /// Repeat forever.
    Infinite,
    /// Repeat the authored finite count.
    Finite(u32),
}

/// A layer state for one normalized frame.
#[derive(Debug, Clone, PartialEq)]
pub struct NormalizedLayerFrameState {
    /// Zero-based normalized frame index.
    pub frame_index: u32,
    /// Whether the source supplied an mlst record for this frame.
    pub record_present: bool,
    /// Resolved layer enabled state after source enable inheritance.
    pub enabled: bool,
    /// Whether the source supplied an explicit enable value for this frame.
    pub explicit_enable: bool,
    /// Optional authored layer offset.
    pub offset: Option<crate::AnimationPoint>,
    /// Optional authored reference point.
    pub reference_point: Option<crate::AnimationPoint>,
    /// Optional authored opacity override in the normalized 0.0..=1.0 range.
    pub opacity: Option<f64>,
}

impl NormalizedLayer {
    /// Returns whether this layer is enabled after applying an ancestor result.
    pub fn is_effectively_visible(&self, frame_index: usize, ancestors_visible: bool) -> bool {
        ancestors_visible
            && self
                .frame_states
                .get(frame_index)
                .is_some_and(|state| state.enabled)
    }

    /// Appends visible pixel-layer IDs for one frame in tree order.
    pub fn collect_visible_pixel_layer_ids(
        &self,
        frame_index: usize,
        ancestors_visible: bool,
        output: &mut Vec<u32>,
    ) {
        let visible = self.is_effectively_visible(frame_index, ancestors_visible);
        match self.kind {
            NormalizedLayerKind::Group => {
                for child in &self.children {
                    child.collect_visible_pixel_layer_ids(frame_index, visible, output);
                }
            }
            NormalizedLayerKind::Pixel if visible => output.push(self.id),
            NormalizedLayerKind::Pixel => {}
        }
    }
}
