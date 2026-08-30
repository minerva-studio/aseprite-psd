//! Format-independent conversion boundaries for PSD to Aseprite.
//!
//! The first implementation slice only establishes the public API and a
//! metadata inspection path. Conversion writing remains deliberately gated on
//! the PSD parser compatibility probe and is not represented as successful
//! output until that gate passes.

mod error;
mod model;

pub use error::{ConversionError, InspectionError};
pub use model::{DocumentInspection, NormalizedDocument, NormalizedFrame, NormalizedLayer};

use std::fs;
use std::path::{Path, PathBuf};

/// The package version exposed to the CLI and reports.
pub const VERSION: &str = env!("CARGO_PKG_VERSION");

/// Options controlling a conversion transaction.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ConvertOptions {
    /// Allow replacing an existing output path after successful validation.
    pub overwrite: bool,
}

/// Summary produced after a conversion has committed its output.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConversionReport {
    /// The source PSD path supplied by the caller.
    pub input: PathBuf,
    /// The output path supplied by the caller.
    pub output: PathBuf,
    /// Warnings produced while mapping or validating the document.
    pub warnings: Vec<String>,
}

/// Reads PSD structure metadata without creating an output file.
pub fn inspect(input: &Path) -> Result<DocumentInspection, InspectionError> {
    let bytes = fs::read(input).map_err(InspectionError::InputIo)?;
    let options = ag_psd::psd::ReadOptions {
        skip_layer_image_data: Some(true),
        skip_composite_image_data: Some(true),
        skip_thumbnail: Some(true),
        ..Default::default()
    };
    let psd = ag_psd::read_psd(&bytes, &options)
        .map_err(|error| InspectionError::PsdRead(error.to_string()))?;

    Ok(DocumentInspection {
        width: psd.width as u32,
        height: psd.height as u32,
        bits_per_channel: psd.bits_per_channel.map(|value| value as u32),
        color_mode: psd.color_mode.map(|value| format!("{value:?}")),
        root_layer_count: psd.children.as_ref().map_or(0, Vec::len),
    })
}

/// Converts a PSD into an Aseprite file after validation and mapping.
pub fn convert(
    input: &Path,
    output: &Path,
    _options: &ConvertOptions,
) -> Result<ConversionReport, ConversionError> {
    if !input.is_file() {
        return Err(ConversionError::InputMissing(input.to_path_buf()));
    }

    if output.exists() && !_options.overwrite {
        return Err(ConversionError::OutputExists(output.to_path_buf()));
    }

    Err(ConversionError::ConversionNotReady)
}
