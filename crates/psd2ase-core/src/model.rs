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

/// Format-neutral document model reserved for the conversion phase.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct NormalizedDocument {
    /// Canvas dimensions in pixels.
    pub canvas: (u32, u32),
    /// Top-level layers in source order.
    pub root_layers: Vec<NormalizedLayer>,
    /// Animation frames in playback order.
    pub frames: Vec<NormalizedFrame>,
}

/// A normalized PSD group or pixel layer.
#[derive(Debug, Clone, PartialEq)]
pub struct NormalizedLayer {
    /// Stable source identifier assigned during normalization.
    pub id: u32,
    /// User-authored layer name.
    pub name: String,
    /// Whether this layer represents a group.
    pub is_group: bool,
}

/// A normalized animation frame.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NormalizedFrame {
    /// Zero-based frame index.
    pub index: u32,
    /// Frame duration in milliseconds.
    pub duration_ms: u32,
}
