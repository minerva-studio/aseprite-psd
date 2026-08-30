use std::fmt::{Display, Formatter};
use std::io;
use std::path::PathBuf;

/// Errors raised while inspecting a PSD document.
#[derive(Debug)]
pub enum InspectionError {
    /// The input could not be read from disk.
    InputIo(io::Error),
    /// The PSD parser rejected the input.
    PsdRead(String),
    /// The PSD was readable but could not be represented safely in the model.
    Normalization(String),
}

impl Display for InspectionError {
    /// Formats an inspection error for a human-readable CLI message.
    fn fmt(&self, formatter: &mut Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::InputIo(error) => write!(formatter, "could not read input: {error}"),
            Self::PsdRead(error) => write!(formatter, "could not parse PSD: {error}"),
            Self::Normalization(error) => write!(formatter, "could not normalize PSD: {error}"),
        }
    }
}

impl std::error::Error for InspectionError {}

/// Errors raised before or during a conversion transaction.
#[derive(Debug)]
pub enum ConversionError {
    /// The input path is not a regular file.
    InputMissing(PathBuf),
    /// The output exists and overwrite was not authorized.
    OutputExists(PathBuf),
    /// A future conversion path is deliberately not enabled.
    ConversionNotReady,
    /// Reading and normalizing the input failed before writing started.
    InputInspection(String),
    /// Mapping the normalized document to Aseprite failed.
    Writer(String),
    /// The encoded file failed post-write structural validation.
    OutputValidation(String),
    /// An output transaction could not complete.
    OutputIo(io::Error),
}

impl Display for ConversionError {
    /// Formats a conversion error for a human-readable CLI message.
    fn fmt(&self, formatter: &mut Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::InputMissing(path) => {
                write!(formatter, "input is not a file: {}", path.display())
            }
            Self::OutputExists(path) => {
                write!(formatter, "output already exists: {}", path.display())
            }
            Self::ConversionNotReady => write!(
                formatter,
                "conversion is not enabled for this conversion path"
            ),
            Self::InputInspection(error) => write!(formatter, "could not inspect input: {error}"),
            Self::Writer(error) => write!(formatter, "could not write Aseprite output: {error}"),
            Self::OutputValidation(error) => {
                write!(formatter, "Aseprite output validation failed: {error}")
            }
            Self::OutputIo(error) => write!(formatter, "could not commit output: {error}"),
        }
    }
}

impl std::error::Error for ConversionError {}
