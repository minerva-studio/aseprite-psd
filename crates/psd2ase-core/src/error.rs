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
}

impl Display for InspectionError {
    /// Formats an inspection error for a human-readable CLI message.
    fn fmt(&self, formatter: &mut Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::InputIo(error) => write!(formatter, "could not read input: {error}"),
            Self::PsdRead(error) => write!(formatter, "could not parse PSD: {error}"),
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
    /// The conversion writer has not passed its compatibility gate yet.
    ConversionNotReady,
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
                "conversion is not enabled until the PSD compatibility probe passes"
            ),
        }
    }
}

impl std::error::Error for ConversionError {}
