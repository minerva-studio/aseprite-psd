use std::env;
use std::path::PathBuf;
use std::process::ExitCode;

use psd2ase_core::{ConvertOptions, VERSION, convert, inspect};

/// Runs the command-line entry point and returns its stable process result.
fn main() -> ExitCode {
    match run(env::args().skip(1).collect()) {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("error: {error}");
            ExitCode::from(error.exit_code())
        }
    }
}

/// Dispatches the intentionally small phase-one CLI surface.
fn run(arguments: Vec<String>) -> Result<(), CliError> {
    match arguments.first().map(String::as_str) {
        None | Some("--help") | Some("-h") => {
            print_help();
            Ok(())
        }
        Some("--version") | Some("-V") => {
            println!("psd2ase {VERSION}");
            Ok(())
        }
        Some("inspect") => run_inspect(&arguments[1..]),
        Some("convert") => run_convert(&arguments[1..]),
        Some(command) => Err(CliError::Usage(format!("unknown command: {command}"))),
    }
}

/// Executes the metadata-only inspection command.
fn run_inspect(arguments: &[String]) -> Result<(), CliError> {
    let input = one_path_argument(arguments, "inspect")?;
    let document = inspect(&input).map_err(|error| CliError::Inspection(error.to_string()))?;
    println!("canvas: {}x{}", document.width, document.height);
    println!("bits per channel: {:?}", document.bits_per_channel);
    println!("color mode: {:?}", document.color_mode);
    println!("root layers: {}", document.root_layer_count);
    Ok(())
}

/// Executes the conversion command while preserving the phase-one safety gate.
fn run_convert(arguments: &[String]) -> Result<(), CliError> {
    let input = one_path_argument(arguments, "convert")?;
    let output = input.with_extension("aseprite");
    convert(&input, &output, &ConvertOptions::default())
        .map(|_| ())
        .map_err(|error| CliError::Conversion(error.to_string()))
}

/// Extracts the single positional path accepted by a phase-one command.
fn one_path_argument(arguments: &[String], command: &str) -> Result<PathBuf, CliError> {
    if arguments.len() != 1 || arguments[0].starts_with('-') {
        return Err(CliError::Usage(format!("usage: psd2ase {command} INPUT")));
    }
    Ok(PathBuf::from(&arguments[0]))
}

/// Prints the supported command-line syntax.
fn print_help() {
    println!(
        "psd2ase {VERSION}\n\n\
         Usage:\n  psd2ase inspect INPUT\n  psd2ase convert INPUT\n  psd2ase --version"
    );
}

/// Errors surfaced by the command-line adapter.
#[derive(Debug)]
enum CliError {
    Usage(String),
    Inspection(String),
    Conversion(String),
}

impl CliError {
    /// Returns the stable exit code assigned to this CLI error category.
    fn exit_code(&self) -> u8 {
        match self {
            Self::Usage(_) => 64,
            Self::Inspection(_) => 3,
            Self::Conversion(_) => 2,
        }
    }
}

impl std::fmt::Display for CliError {
    /// Formats a CLI error without exposing internal Rust types.
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Usage(message) | Self::Inspection(message) | Self::Conversion(message) => {
                formatter.write_str(message)
            }
        }
    }
}

impl std::error::Error for CliError {}
