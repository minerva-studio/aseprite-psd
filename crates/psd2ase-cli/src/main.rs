use std::env;
use std::path::PathBuf;
use std::process::ExitCode;

use psd2ase_core::{
    AssociationDecisionStatus, ConvertOptions, LayerAssociationMode, LayerZOrderMode,
    StableOrderMode, VERSION, convert, inspect,
};

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

/// Executes the conversion command with an optional output path and overwrite flag.
fn run_convert(arguments: &[String]) -> Result<(), CliError> {
    let (input, output, overwrite, layer_association, z_order, stable_order, stable_order_explicit) =
        convert_arguments(arguments)?;
    if layer_association == LayerAssociationMode::Preserve && z_order == LayerZOrderMode::Auto {
        return Err(CliError::Usage(
            "--z-order auto requires --layer-association auto".to_string(),
        ));
    }
    if layer_association == LayerAssociationMode::Preserve && stable_order_explicit {
        return Err(CliError::Usage(
            "--stable-order requires --layer-association auto".to_string(),
        ));
    }
    let report = convert(
        &input,
        &output,
        &ConvertOptions {
            overwrite,
            layer_association,
            z_order,
            stable_order,
        },
    )
    .map_err(|error| CliError::Conversion(error.to_string()))?;
    println!("wrote {}", report.output.display());
    for warning in report.warnings {
        println!("warning: {warning}");
    }
    if let Some(association) = report.association {
        println!(
            "layer association: {} observations -> {} logical tracks",
            association.observation_count, association.track_count
        );
        println!("layer-association z-order: {:?}", association.z_order_mode);
        println!(
            "layer-association stable-order: {:?}",
            association.stable_order_mode
        );
        println!(
            "layer-association name catalog: v{}",
            association.name_catalog_version
        );
        if !association.omitted_source_layer_ids.is_empty() {
            println!(
                "layer-association omitted source pixel layers: {}",
                association.omitted_source_layer_ids.len()
            );
        }
        for warning in association.warnings {
            println!("layer-association warning: {warning}");
        }
        for diagnostic in association.z_order_diagnostics {
            println!("layer-association z-order diagnostic: {diagnostic}");
        }
        for diagnostic in association.stable_order_diagnostics {
            println!("layer-association stable-order diagnostic: {diagnostic}");
        }
        for diagnostic in association.exclusion_diagnostics {
            println!("layer-association exclusion diagnostic: {diagnostic}");
        }
        for diagnostic in association.family_diagnostics {
            println!("layer-association family diagnostic: {diagnostic}");
        }
        for diagnostic in association.name_diagnostics {
            println!("layer-association name diagnostic: {diagnostic}");
        }
        for decision in association.decisions {
            if matches!(
                decision.status,
                AssociationDecisionStatus::Ambiguous | AssociationDecisionStatus::NewTrack
            ) {
                println!(
                    "layer-association {:?}: frame {} source {} ({}) name {:?} base {:?} copy {:?} phase={:?} score={} margin={} same-frame={} tie={} -> track {}",
                    decision.status,
                    decision.frame_index,
                    decision.source_layer_id,
                    decision.source_path,
                    decision.original_name,
                    decision.normalized_base_name,
                    decision.copy_suffixes,
                    decision.association_phase,
                    decision.score,
                    decision.margin,
                    decision.same_frame_instance_count,
                    decision.matching_tie,
                    decision.track_id
                );
                if !decision.rejection_reasons.is_empty() {
                    println!(
                        "layer-association rejection reasons: {:?}",
                        decision.rejection_reasons
                    );
                }
            }
        }
    }
    Ok(())
}

/// Parses the conversion input, output, and overwrite options.
fn convert_arguments(
    arguments: &[String],
) -> Result<
    (
        PathBuf,
        PathBuf,
        bool,
        LayerAssociationMode,
        LayerZOrderMode,
        StableOrderMode,
        bool,
    ),
    CliError,
> {
    if arguments.is_empty() {
        return Err(CliError::Usage(
            "usage: psd2ase convert INPUT [-o OUTPUT] [--overwrite] [--layer-association preserve|auto] [--z-order stable|auto] [--stable-order consensus|anchor|strict]".to_string(),
        ));
    }
    let mut input = None;
    let mut output = None;
    let mut overwrite = false;
    let mut layer_association = LayerAssociationMode::Preserve;
    let mut z_order = LayerZOrderMode::Stable;
    let mut stable_order = StableOrderMode::Consensus;
    let mut stable_order_explicit = false;
    let mut index = 0;
    while index < arguments.len() {
        match arguments[index].as_str() {
            "--overwrite" => overwrite = true,
            "--layer-association" => {
                index += 1;
                let value = arguments.get(index).ok_or_else(|| {
                    CliError::Usage(
                        "usage: psd2ase convert INPUT [-o OUTPUT] [--overwrite] [--layer-association preserve|auto] [--z-order stable|auto] [--stable-order consensus|anchor|strict]".to_string(),
                    )
                })?;
                layer_association = match value.as_str() {
                    "preserve" => LayerAssociationMode::Preserve,
                    "auto" => LayerAssociationMode::Auto,
                    _ => {
                        return Err(CliError::Usage(format!(
                            "invalid --layer-association value: {value:?}"
                        )));
                    }
                };
            }
            "--z-order" => {
                index += 1;
                let value = arguments.get(index).ok_or_else(|| {
                    CliError::Usage(
                        "usage: psd2ase convert INPUT [-o OUTPUT] [--overwrite] [--layer-association preserve|auto] [--z-order stable|auto] [--stable-order consensus|anchor|strict]".to_string(),
                    )
                })?;
                z_order = match value.as_str() {
                    "stable" => LayerZOrderMode::Stable,
                    "auto" => LayerZOrderMode::Auto,
                    _ => {
                        return Err(CliError::Usage(format!(
                            "invalid --z-order value: {value:?}"
                        )));
                    }
                };
            }
            "--stable-order" => {
                index += 1;
                let value = arguments.get(index).ok_or_else(|| {
                    CliError::Usage(
                        "usage: psd2ase convert INPUT [-o OUTPUT] [--overwrite] [--layer-association preserve|auto] [--z-order stable|auto] [--stable-order consensus|anchor|strict]".to_string(),
                    )
                })?;
                stable_order = match value.as_str() {
                    "consensus" => StableOrderMode::Consensus,
                    "anchor" => StableOrderMode::Anchor,
                    "strict" => StableOrderMode::Strict,
                    _ => {
                        return Err(CliError::Usage(format!(
                            "invalid --stable-order value: {value:?}"
                        )));
                    }
                };
                stable_order_explicit = true;
            }
            "-o" | "--output" => {
                index += 1;
                let value = arguments.get(index).ok_or_else(|| {
                    CliError::Usage(
                        "usage: psd2ase convert INPUT [-o OUTPUT] [--overwrite] [--layer-association preserve|auto] [--z-order stable|auto] [--stable-order consensus|anchor|strict]".to_string(),
                    )
                })?;
                output = Some(PathBuf::from(value));
            }
            value if value.starts_with('-') => {
                return Err(CliError::Usage(format!("unknown convert option: {value}")));
            }
            value if input.is_none() => input = Some(PathBuf::from(value)),
            value => {
                return Err(CliError::Usage(format!(
                    "unexpected convert argument: {value}"
                )));
            }
        }
        index += 1;
    }
    let input = input.ok_or_else(|| {
        CliError::Usage("usage: psd2ase convert INPUT [-o OUTPUT] [--overwrite] [--layer-association preserve|auto] [--z-order stable|auto] [--stable-order consensus|anchor|strict]".to_string())
    })?;
    let output = output.unwrap_or_else(|| input.with_extension("aseprite"));
    Ok((
        input,
        output,
        overwrite,
        layer_association,
        z_order,
        stable_order,
        stable_order_explicit,
    ))
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
         Usage:\n  psd2ase inspect INPUT\n  psd2ase convert INPUT [-o OUTPUT] [--overwrite] [--layer-association preserve|auto] [--z-order stable|auto] [--stable-order consensus|anchor|strict]\n  psd2ase --version"
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
