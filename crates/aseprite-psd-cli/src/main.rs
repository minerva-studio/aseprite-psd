use std::env;
use std::path::PathBuf;
use std::process::ExitCode;

use aseprite_psd_core::{
    AssociationDecisionStatus, AssociationStrategy, AutoAssociationOptions, ConversionError,
    ConvertOptions, ExportCompression, ExportOptions, FrameSource, JitterKind, JitterMode,
    JitterOptions, JitterProfile, LayerAssociation, LayerZOrderMode, LinkedCelMode,
    StableOrderMode, UncertainLayerMode, VERSION, convert, export, inspect,
    write_report_with_active_frame,
};

const CONVERT_USAGE: &str = "usage: aseprite-psd convert INPUT [-o OUTPUT] [--report PATH] [--overwrite] [--frame-source auto|static|top-level] [--preserve-photoshop-metadata] [--linked-cels off|identical] [--layer-association preserve|auto|roundtrip] [--association-strategy compact|conservative] [--z-order stable|auto] [--stable-order consensus|anchor|strict] [--uncertain-layers group|flat] [--jitter-mode off|report|assist|repair] [--jitter-kind alpha|color|all] [--jitter-profile conservative|balanced] [--jitter-alpha-threshold N] [--jitter-max-speck-area N] [--jitter-max-changed-ratio N] [--jitter-max-channel-delta N]";
const EXPORT_USAGE: &str = "usage: aseprite-psd export INPUT.aseprite -o OUTPUT.psd --composite COMPOSITE.aseprite [--active-frame-index N] [--compression raw|rle|zip|zip-prediction] [--empty-layers include|omit] [--report PATH] [--overwrite] [--roundtrip-metadata on|off]";

#[derive(Debug, PartialEq, Eq)]
struct ConvertCommand {
    input: PathBuf,
    output: PathBuf,
    overwrite: bool,
    preserve_photoshop_metadata: bool,
    frame_source: FrameSource,
    linked_cels: LinkedCelMode,
    layer_association: LayerAssociation,
    jitter: JitterOptions,
    report: Option<PathBuf>,
}

#[derive(Debug, PartialEq, Eq)]
struct ExportCommand {
    input: PathBuf,
    output: PathBuf,
    composite: PathBuf,
    report: Option<PathBuf>,
    overwrite: bool,
    active_frame_index: Option<u32>,
    compression: Option<ExportCompression>,
    embed_roundtrip_metadata: bool,
    include_empty_layers: bool,
}

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
            println!("aseprite-psd {VERSION}");
            Ok(())
        }
        Some("inspect") => run_inspect(&arguments[1..]),
        Some("convert") => run_convert(&arguments[1..]),
        Some("export") => run_export(&arguments[1..]),
        Some(command) => Err(CliError::Usage(format!("unknown command: {command}"))),
    }
}

/// Executes the independently validated Aseprite-to-PSD/PSB export command.
fn run_export(arguments: &[String]) -> Result<(), CliError> {
    let command = export_arguments(arguments)?;
    let report = export(
        &command.input,
        &command.composite,
        &command.output,
        &ExportOptions {
            overwrite: command.overwrite,
            active_frame_index: command.active_frame_index,
            compression: command.compression,
            embed_roundtrip_metadata: command.embed_roundtrip_metadata,
            include_empty_layers: command.include_empty_layers,
        },
    )
    .map_err(|error| CliError::Conversion(error.to_string()))?;
    println!("wrote {}", report.output.display());
    if let Some(path) = command.report {
        write_report_with_active_frame(
            &path,
            &report.input,
            &report.output,
            &report.information_loss,
            report.active_frame_index,
        )
        .map_err(|error| {
            CliError::Conversion(format!(
                "output generated, report write failed for {}: {error}",
                path.display()
            ))
        })?;
    }
    for loss in &report.information_loss.entries {
        println!(
            "information-loss {} {} count={} {}",
            loss.disposition.as_str(),
            loss.code.as_str(),
            loss.count,
            loss.detail
        );
    }
    Ok(())
}

/// Executes the metadata-only inspection command.
fn run_inspect(arguments: &[String]) -> Result<(), CliError> {
    let input = one_path_argument(arguments, "inspect")?;
    let document = inspect(&input).map_err(|error| CliError::Inspection(error.to_string()))?;
    println!("canvas: {}x{}", document.width, document.height);
    println!("bits per channel: {:?}", document.bits_per_channel);
    println!("color mode: {:?}", document.color_mode);
    println!("root layers: {}", document.root_layer_count);
    println!("roundtrip metadata: {}", document.roundtrip_marked);
    Ok(())
}

/// Executes the conversion command with an optional output path and overwrite flag.
fn run_convert(arguments: &[String]) -> Result<(), CliError> {
    let command = convert_arguments(arguments)?;
    let report = convert(
        &command.input,
        &command.output,
        &ConvertOptions {
            overwrite: command.overwrite,
            preserve_photoshop_metadata: command.preserve_photoshop_metadata,
            frame_source: command.frame_source,
            linked_cels: command.linked_cels,
            layer_association: command.layer_association,
            jitter: command.jitter,
        },
    )
    .map_err(|error| match error {
        ConversionError::RoundTripRecoveryRequired(message) => CliError::Recovery(message),
        error => CliError::Conversion(error.to_string()),
    })?;
    println!("wrote {}", report.output.display());
    if let Some(path) = command.report {
        write_report_with_active_frame(
            &path,
            &report.input,
            &report.output,
            &report.information_loss,
            report.active_frame_index,
        )
        .map_err(|error| {
            CliError::Conversion(format!(
                "output generated, report write failed for {}: {error}",
                path.display()
            ))
        })?;
    }
    println!(
        "cel reuse: {} pixel cels, {} linked cels",
        report.cel_reuse.pixel_cel_count, report.cel_reuse.linked_cel_count
    );
    for warning in report.warnings {
        println!("warning: {warning}");
    }
    for loss in &report.information_loss.entries {
        println!(
            "information-loss {} {} count={} {}",
            loss.disposition.as_str(),
            loss.code.as_str(),
            loss.count,
            loss.detail
        );
    }
    if let Some(association) = report.association {
        let exclusion_diagnostics = association.exclusion_diagnostics();
        let family_diagnostics = association.family_diagnostics();
        let name_diagnostics = association.name_diagnostics();
        println!(
            "layer association: {} observations -> {} logical tracks",
            association.observation_count, association.track_count
        );
        println!("layer-association z-order: {:?}", association.z_order_mode);
        println!(
            "layer-association strategy: {}",
            association.strategy.name()
        );
        println!(
            "layer-association stable-order: {:?}",
            association.stable_order_mode
        );
        println!(
            "layer-association uncertain-layers: {:?}",
            association.uncertain_layer_mode
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
        for diagnostic in exclusion_diagnostics {
            println!("layer-association exclusion diagnostic: {diagnostic}");
        }
        for diagnostic in family_diagnostics {
            println!("layer-association family diagnostic: {diagnostic}");
        }
        for diagnostic in name_diagnostics {
            println!("layer-association name diagnostic: {diagnostic}");
        }
        for candidate_group in association.candidate_groups {
            let status = if candidate_group.emitted {
                "emitted"
            } else {
                "not-emitted"
            };
            println!(
                "layer-association candidate group: {:?} anchor={} members={:?} status={}",
                candidate_group.name,
                candidate_group.anchor_track_id,
                candidate_group.member_track_ids,
                status
            );
            if !candidate_group.evidence.is_empty() {
                println!(
                    "layer-association candidate evidence: {:?}",
                    candidate_group.evidence
                );
            }
            println!(
                "layer-association candidate complete interval: {}",
                candidate_group.complete_interval
            );
            for relation in candidate_group.relations {
                println!(
                    "layer-association candidate relation: {} <-> {} {:?} co-visible-frames={:?}",
                    relation.left_track_id,
                    relation.right_track_id,
                    relation.relation,
                    relation.co_visible_frames
                );
            }
            if let Some(reason) = candidate_group.rejection_reason {
                println!("layer-association candidate rejection: {reason}");
            }
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
    if let Some(jitter) = report.jitter {
        println!(
            "jitter: inspected={} alpha-candidates={} alpha-repairs={} color-candidates={} color-repairs={}",
            jitter.inspected_cels,
            jitter.alpha_candidates,
            jitter.alpha_repairs,
            jitter.color_candidates,
            jitter.color_repairs
        );
        for diagnostic in jitter.diagnostics {
            println!("jitter diagnostic: {diagnostic}");
        }
    }
    Ok(())
}

/// Parses the conversion input, output, and overwrite options.
fn convert_arguments(arguments: &[String]) -> Result<ConvertCommand, CliError> {
    if arguments.is_empty() {
        return Err(CliError::Usage(CONVERT_USAGE.to_string()));
    }
    let mut input = None;
    let mut output = None;
    let mut report = None;
    let mut overwrite = false;
    let mut preserve_photoshop_metadata = false;
    let mut frame_source = FrameSource::Auto;
    let mut linked_cels = LinkedCelMode::Off;
    let mut automatic = false;
    let mut roundtrip = false;
    let mut conservative = true;
    let mut association_strategy_explicit = false;
    let mut z_order = LayerZOrderMode::Stable;
    let mut stable_order = StableOrderMode::Consensus;
    let mut stable_order_explicit = false;
    let mut uncertain_layers = UncertainLayerMode::Group;
    let mut uncertain_layers_explicit = false;
    let mut jitter = JitterOptions::default();
    let mut jitter_kind_explicit = false;
    let mut jitter_profile_explicit = false;
    let mut jitter_numeric_explicit = false;
    let mut index = 0;
    while index < arguments.len() {
        match arguments[index].as_str() {
            "--overwrite" => overwrite = true,
            "--preserve-photoshop-metadata" => preserve_photoshop_metadata = true,
            "--frame-source" => {
                index += 1;
                let value = arguments
                    .get(index)
                    .ok_or_else(|| CliError::Usage(CONVERT_USAGE.to_string()))?;
                frame_source = match value.as_str() {
                    "auto" => FrameSource::Auto,
                    "static" => FrameSource::Static,
                    "top-level" => FrameSource::TopLevel,
                    _ => {
                        return Err(CliError::Usage(format!(
                            "invalid --frame-source value: {value:?}"
                        )));
                    }
                };
            }
            "--linked-cels" => {
                index += 1;
                let value = arguments
                    .get(index)
                    .ok_or_else(|| CliError::Usage(CONVERT_USAGE.to_string()))?;
                linked_cels = match value.as_str() {
                    "off" => LinkedCelMode::Off,
                    "identical" => LinkedCelMode::Identical,
                    _ => {
                        return Err(CliError::Usage(format!(
                            "invalid --linked-cels value: {value:?}"
                        )));
                    }
                };
            }
            "--layer-association" => {
                index += 1;
                let value = arguments
                    .get(index)
                    .ok_or_else(|| CliError::Usage(CONVERT_USAGE.to_string()))?;
                automatic = match value.as_str() {
                    "preserve" => {
                        roundtrip = false;
                        false
                    }
                    "auto" => {
                        roundtrip = false;
                        true
                    }
                    "roundtrip" => {
                        roundtrip = true;
                        false
                    }
                    _ => {
                        return Err(CliError::Usage(format!(
                            "invalid --layer-association value: {value:?}"
                        )));
                    }
                };
            }
            "--z-order" => {
                index += 1;
                let value = arguments
                    .get(index)
                    .ok_or_else(|| CliError::Usage(CONVERT_USAGE.to_string()))?;
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
            "--association-strategy" => {
                index += 1;
                let value = arguments
                    .get(index)
                    .ok_or_else(|| CliError::Usage(CONVERT_USAGE.to_string()))?;
                conservative = match value.as_str() {
                    "compact" => false,
                    "conservative" => true,
                    _ => {
                        return Err(CliError::Usage(format!(
                            "invalid --association-strategy value: {value:?}"
                        )));
                    }
                };
                association_strategy_explicit = true;
            }
            "--stable-order" => {
                index += 1;
                let value = arguments
                    .get(index)
                    .ok_or_else(|| CliError::Usage(CONVERT_USAGE.to_string()))?;
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
            "--uncertain-layers" => {
                index += 1;
                let value = arguments
                    .get(index)
                    .ok_or_else(|| CliError::Usage(CONVERT_USAGE.to_string()))?;
                uncertain_layers = match value.as_str() {
                    "group" => UncertainLayerMode::Group,
                    "flat" => UncertainLayerMode::Flat,
                    _ => {
                        return Err(CliError::Usage(format!(
                            "invalid --uncertain-layers value: {value:?}"
                        )));
                    }
                };
                uncertain_layers_explicit = true;
            }
            "--jitter-mode" => {
                index += 1;
                let value = arguments
                    .get(index)
                    .ok_or_else(|| CliError::Usage(CONVERT_USAGE.to_string()))?;
                jitter.mode = match value.as_str() {
                    "off" => JitterMode::Off,
                    "report" => JitterMode::Report,
                    "assist" => JitterMode::Assist,
                    "repair" => JitterMode::Repair,
                    _ => {
                        return Err(CliError::Usage(format!(
                            "invalid --jitter-mode value: {value:?}"
                        )));
                    }
                };
            }
            "--jitter-kind" => {
                index += 1;
                let value = arguments
                    .get(index)
                    .ok_or_else(|| CliError::Usage(CONVERT_USAGE.to_string()))?;
                jitter.kind = match value.as_str() {
                    "alpha" => JitterKind::Alpha,
                    "color" => JitterKind::Color,
                    "all" => JitterKind::All,
                    _ => {
                        return Err(CliError::Usage(format!(
                            "invalid --jitter-kind value: {value:?}"
                        )));
                    }
                };
                jitter_kind_explicit = true;
            }
            "--jitter-profile" => {
                index += 1;
                let value = arguments
                    .get(index)
                    .ok_or_else(|| CliError::Usage(CONVERT_USAGE.to_string()))?;
                jitter.profile = match value.as_str() {
                    "conservative" => JitterProfile::Conservative,
                    "balanced" => JitterProfile::Balanced,
                    _ => {
                        return Err(CliError::Usage(format!(
                            "invalid --jitter-profile value: {value:?}"
                        )));
                    }
                };
                jitter_profile_explicit = true;
            }
            "--jitter-alpha-threshold" => {
                jitter_numeric_explicit = true;
                jitter.alpha_threshold = Some(parse_jitter_u8(
                    arguments,
                    &mut index,
                    "--jitter-alpha-threshold",
                )?)
            }
            "--jitter-max-speck-area" => {
                jitter_numeric_explicit = true;
                jitter.max_speck_area = Some(parse_jitter_usize(
                    arguments,
                    &mut index,
                    "--jitter-max-speck-area",
                )?)
            }
            "--jitter-max-changed-ratio" => {
                jitter_numeric_explicit = true;
                jitter.max_changed_ratio_percent = Some(parse_jitter_u8(
                    arguments,
                    &mut index,
                    "--jitter-max-changed-ratio",
                )?)
            }
            "--jitter-max-channel-delta" => {
                jitter_numeric_explicit = true;
                jitter.max_channel_delta = Some(parse_jitter_u8(
                    arguments,
                    &mut index,
                    "--jitter-max-channel-delta",
                )?)
            }
            "-o" | "--output" => {
                index += 1;
                let value = arguments
                    .get(index)
                    .ok_or_else(|| CliError::Usage(CONVERT_USAGE.to_string()))?;
                output = Some(PathBuf::from(value));
            }
            "--report" => {
                index += 1;
                let value = arguments
                    .get(index)
                    .ok_or_else(|| CliError::Usage(CONVERT_USAGE.to_string()))?;
                report = Some(PathBuf::from(value));
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
    let input = input.ok_or_else(|| CliError::Usage(CONVERT_USAGE.to_string()))?;
    let output = output.unwrap_or_else(|| input.with_extension("aseprite"));
    if roundtrip && z_order == LayerZOrderMode::Auto {
        return Err(CliError::Usage(
            "--z-order auto cannot be combined with --layer-association roundtrip".to_string(),
        ));
    }
    if !automatic && !roundtrip && z_order == LayerZOrderMode::Auto {
        return Err(CliError::Usage(
            "--z-order auto requires --layer-association auto".to_string(),
        ));
    }
    if roundtrip && stable_order_explicit {
        return Err(CliError::Usage(
            "--stable-order cannot be combined with --layer-association roundtrip".to_string(),
        ));
    }
    if !automatic && !roundtrip && stable_order_explicit {
        return Err(CliError::Usage(
            "--stable-order requires --layer-association auto".to_string(),
        ));
    }
    if roundtrip && uncertain_layers_explicit {
        return Err(CliError::Usage(
            "--uncertain-layers cannot be combined with --layer-association roundtrip".to_string(),
        ));
    }
    if !automatic && !roundtrip && uncertain_layers_explicit {
        return Err(CliError::Usage(
            "--uncertain-layers requires --layer-association auto".to_string(),
        ));
    }
    if roundtrip && association_strategy_explicit {
        return Err(CliError::Usage(
            "--association-strategy cannot be combined with --layer-association roundtrip"
                .to_string(),
        ));
    }
    if !automatic && !roundtrip && association_strategy_explicit {
        return Err(CliError::Usage(
            "--association-strategy requires --layer-association auto".to_string(),
        ));
    }
    if roundtrip && linked_cels == LinkedCelMode::Identical {
        return Err(CliError::Usage(
            "--linked-cels identical cannot be combined with --layer-association roundtrip"
                .to_string(),
        ));
    }
    if !automatic && !roundtrip && linked_cels == LinkedCelMode::Identical {
        return Err(CliError::Usage(
            "--linked-cels identical requires --layer-association auto".to_string(),
        ));
    }
    if !conservative && uncertain_layers_explicit {
        return Err(CliError::Usage(
            "--uncertain-layers requires --association-strategy conservative".to_string(),
        ));
    }
    if roundtrip && jitter.mode == JitterMode::Assist {
        return Err(CliError::Usage(
            "--jitter-mode assist cannot be combined with --layer-association roundtrip"
                .to_string(),
        ));
    }
    if !automatic && !roundtrip && jitter.mode == JitterMode::Assist {
        return Err(CliError::Usage(
            "--jitter-mode assist requires --layer-association auto".to_string(),
        ));
    }
    if roundtrip
        && matches!(jitter.kind, JitterKind::Color | JitterKind::All)
        && jitter.mode == JitterMode::Repair
    {
        return Err(CliError::Usage(
            "--jitter-kind color/all with --jitter-mode repair cannot be combined with --layer-association roundtrip".to_string(),
        ));
    }
    if !automatic
        && !roundtrip
        && matches!(jitter.kind, JitterKind::Color | JitterKind::All)
        && jitter.mode == JitterMode::Repair
    {
        return Err(CliError::Usage(
            "--jitter-kind color/all with --jitter-mode repair requires --layer-association auto"
                .to_string(),
        ));
    }
    if jitter.mode == JitterMode::Off
        && (jitter_kind_explicit || jitter_profile_explicit || jitter_numeric_explicit)
    {
        return Err(CliError::Usage(
            "jitter options require --jitter-mode report|assist|repair".to_string(),
        ));
    }
    jitter.thresholds().map_err(CliError::Usage)?;
    let layer_association = if roundtrip {
        LayerAssociation::AutoForRoundTrip
    } else if automatic {
        let strategy = if conservative {
            AssociationStrategy::Conservative { uncertain_layers }
        } else {
            AssociationStrategy::Compact
        };
        LayerAssociation::Auto(AutoAssociationOptions {
            strategy,
            z_order,
            stable_order,
        })
    } else {
        LayerAssociation::Preserve
    };
    Ok(ConvertCommand {
        input,
        output,
        overwrite,
        preserve_photoshop_metadata,
        frame_source,
        linked_cels,
        layer_association,
        jitter,
        report,
    })
}

/// Parses the deliberately narrow export command without adding another protocol layer.
fn export_arguments(arguments: &[String]) -> Result<ExportCommand, CliError> {
    if arguments.is_empty() {
        return Err(CliError::Usage(EXPORT_USAGE.to_string()));
    }
    let mut input = None;
    let mut output = None;
    let mut composite = None;
    let mut report = None;
    let mut overwrite = false;
    let mut active_frame_index = None;
    let mut compression = None;
    let mut embed_roundtrip_metadata = true;
    let mut include_empty_layers = false;
    let mut index = 0;
    while index < arguments.len() {
        match arguments[index].as_str() {
            "-o" | "--output" => {
                index += 1;
                output = Some(PathBuf::from(
                    arguments
                        .get(index)
                        .ok_or_else(|| CliError::Usage(EXPORT_USAGE.to_string()))?,
                ));
            }
            "--composite" => {
                index += 1;
                composite = Some(PathBuf::from(
                    arguments
                        .get(index)
                        .ok_or_else(|| CliError::Usage(EXPORT_USAGE.to_string()))?,
                ));
            }
            "--report" => {
                index += 1;
                report = Some(PathBuf::from(
                    arguments
                        .get(index)
                        .ok_or_else(|| CliError::Usage(EXPORT_USAGE.to_string()))?,
                ));
            }
            "--active-frame-index" => {
                index += 1;
                active_frame_index = Some(
                    arguments
                        .get(index)
                        .ok_or_else(|| CliError::Usage(EXPORT_USAGE.to_string()))?
                        .parse::<u32>()
                        .map_err(|_| {
                            CliError::Usage(
                                "--active-frame-index expects a non-negative integer".to_string(),
                            )
                        })?,
                );
            }
            "--compression" => {
                index += 1;
                let value = arguments
                    .get(index)
                    .ok_or_else(|| CliError::Usage(EXPORT_USAGE.to_string()))?;
                compression = Some(ExportCompression::parse(value).ok_or_else(|| {
                    CliError::Usage(
                        "--compression expects raw, rle, zip, or zip-prediction".to_string(),
                    )
                })?);
            }
            "--overwrite" => overwrite = true,
            "--roundtrip-metadata" => {
                index += 1;
                let value = arguments
                    .get(index)
                    .ok_or_else(|| CliError::Usage(EXPORT_USAGE.to_string()))?;
                embed_roundtrip_metadata = match value.as_str() {
                    "on" => true,
                    "off" => false,
                    _ => {
                        return Err(CliError::Usage(format!(
                            "invalid --roundtrip-metadata value: {value:?}"
                        )));
                    }
                };
            }
            "--empty-layers" => {
                index += 1;
                let value = arguments
                    .get(index)
                    .ok_or_else(|| CliError::Usage(EXPORT_USAGE.to_string()))?;
                include_empty_layers = match value.as_str() {
                    "include" => true,
                    "omit" => false,
                    _ => {
                        return Err(CliError::Usage(format!(
                            "invalid --empty-layers value: {value:?}"
                        )));
                    }
                };
            }
            value if value.starts_with('-') => {
                return Err(CliError::Usage(format!("unknown export option: {value}")));
            }
            value => {
                if input.replace(PathBuf::from(value)).is_some() {
                    return Err(CliError::Usage(EXPORT_USAGE.to_string()));
                }
            }
        }
        index += 1;
    }
    Ok(ExportCommand {
        input: input.ok_or_else(|| CliError::Usage(EXPORT_USAGE.to_string()))?,
        output: output.ok_or_else(|| CliError::Usage(EXPORT_USAGE.to_string()))?,
        composite: composite.ok_or_else(|| CliError::Usage(EXPORT_USAGE.to_string()))?,
        report,
        overwrite,
        active_frame_index,
        compression,
        embed_roundtrip_metadata,
        include_empty_layers,
    })
}

/// Parses a bounded unsigned jitter parameter.
fn parse_jitter_u8(arguments: &[String], index: &mut usize, flag: &str) -> Result<u8, CliError> {
    *index += 1;
    let value = arguments
        .get(*index)
        .ok_or_else(|| CliError::Usage(CONVERT_USAGE.to_string()))?;
    value
        .parse::<u8>()
        .map_err(|_| CliError::Usage(format!("{flag} expects an integer between 0 and 255")))
}

/// Parses a positive jitter area parameter.
fn parse_jitter_usize(
    arguments: &[String],
    index: &mut usize,
    flag: &str,
) -> Result<usize, CliError> {
    *index += 1;
    let value = arguments
        .get(*index)
        .ok_or_else(|| CliError::Usage(CONVERT_USAGE.to_string()))?;
    value
        .parse::<usize>()
        .map_err(|_| CliError::Usage(format!("{flag} expects a positive integer")))
}

/// Extracts the single positional path accepted by a phase-one command.
fn one_path_argument(arguments: &[String], command: &str) -> Result<PathBuf, CliError> {
    if arguments.len() != 1 || arguments[0].starts_with('-') {
        return Err(CliError::Usage(format!(
            "usage: aseprite-psd {command} INPUT"
        )));
    }
    Ok(PathBuf::from(&arguments[0]))
}

/// Prints the supported command-line syntax.
fn print_help() {
    println!(
        "aseprite-psd {VERSION}\n\n\
         Usage:\n  aseprite-psd inspect INPUT\n  {CONVERT_USAGE}\n  {EXPORT_USAGE}\n  aseprite-psd --version"
    );
}

/// Errors surfaced by the command-line adapter.
#[derive(Debug)]
enum CliError {
    Usage(String),
    Inspection(String),
    Conversion(String),
    Recovery(String),
}

impl CliError {
    /// Returns the stable exit code assigned to this CLI error category.
    fn exit_code(&self) -> u8 {
        match self {
            Self::Usage(_) => 64,
            Self::Inspection(_) => 3,
            Self::Conversion(_) => 2,
            Self::Recovery(_) => 4,
        }
    }
}

impl std::fmt::Display for CliError {
    /// Formats a CLI error without exposing internal Rust types.
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Usage(message)
            | Self::Inspection(message)
            | Self::Conversion(message)
            | Self::Recovery(message) => formatter.write_str(message),
        }
    }
}

impl std::error::Error for CliError {}

#[cfg(test)]
#[path = "tests/cli.rs"]
mod tests;
