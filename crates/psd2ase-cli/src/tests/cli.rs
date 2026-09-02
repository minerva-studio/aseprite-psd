use super::*;

use std::fs;
use std::time::{SystemTime, UNIX_EPOCH};

use aseprite::{AsepriteFile, ColorMode as AseColorMode, Pixels};

fn arguments(values: &[&str]) -> Vec<String> {
    values.iter().map(|value| (*value).to_string()).collect()
}

#[test]
fn extension_preserve_arguments_use_the_preserve_configuration() {
    let command = convert_arguments(&arguments(&["input.psd", "--output", "output.aseprite"]))
        .expect("extension preserve arguments should parse");

    assert_eq!(command.layer_association, LayerAssociation::Preserve);
    assert_eq!(command.linked_cels, LinkedCelMode::Off);
    assert_eq!(command.output, PathBuf::from("output.aseprite"));
    assert!(!command.preserve_photoshop_metadata);
}

#[test]
fn roundtrip_association_is_selected_without_auto_options() {
    let command = convert_arguments(&arguments(&[
        "input.psd",
        "--layer-association",
        "roundtrip",
    ]))
    .expect("roundtrip association should parse");

    assert_eq!(
        command.layer_association,
        LayerAssociation::AutoForRoundTrip
    );
}

#[test]
fn automatic_association_defaults_to_conservative() {
    let command = convert_arguments(&arguments(&["input.psd", "--layer-association", "auto"]))
        .expect("automatic association should parse");
    assert!(matches!(
        command.layer_association,
        LayerAssociation::Auto(AutoAssociationOptions {
            strategy: AssociationStrategy::Conservative {
                uncertain_layers: UncertainLayerMode::Group,
            },
            ..
        })
    ));
}

#[test]
fn frame_source_defaults_to_auto_and_accepts_top_level() {
    let default =
        convert_arguments(&arguments(&["input.psd"])).expect("default conversion should parse");
    assert_eq!(default.frame_source, FrameSource::Auto);

    let top_level = convert_arguments(&arguments(&["input.psd", "--frame-source", "top-level"]))
        .expect("top-level frame source should parse");
    assert_eq!(top_level.frame_source, FrameSource::TopLevel);
}

#[test]
fn unknown_frame_source_is_rejected() {
    let error = convert_arguments(&arguments(&["input.psd", "--frame-source", "procreate"]))
        .expect_err("unknown frame source must fail");
    assert!(error.to_string().contains("invalid --frame-source"));
}

#[test]
fn roundtrip_association_rejects_auto_only_options() {
    let error = convert_arguments(&arguments(&[
        "input.psd",
        "--layer-association",
        "roundtrip",
        "--z-order",
        "auto",
    ]))
    .expect_err("roundtrip must reject auto-only ordering options");
    assert_eq!(
        error.to_string(),
        "--z-order auto cannot be combined with --layer-association roundtrip"
    );
}

#[test]
fn photoshop_metadata_flag_is_opt_in() {
    let command = convert_arguments(&arguments(&["input.psd", "--preserve-photoshop-metadata"]))
        .expect("metadata flag should parse");
    assert!(command.preserve_photoshop_metadata);
}

#[test]
fn identical_linked_cels_option_requires_automatic_association() {
    let command = convert_arguments(&arguments(&[
        "input.psd",
        "--linked-cels",
        "identical",
        "--layer-association",
        "auto",
    ]))
    .expect("linked-cel option should parse with automatic association");

    assert_eq!(command.linked_cels, LinkedCelMode::Identical);
    assert!(matches!(
        command.layer_association,
        LayerAssociation::Auto(_)
    ));

    let error = convert_arguments(&arguments(&["input.psd", "--linked-cels", "identical"]))
        .expect_err("linked-cel option must reject preserve association");
    assert_eq!(
        error.to_string(),
        "--linked-cels identical requires --layer-association auto"
    );
    assert_eq!(error.exit_code(), 64);

    let error = convert_arguments(&arguments(&["input.psd", "--linked-cels", "unknown"]))
        .expect_err("unknown linked-cel mode must be rejected");
    assert_eq!(
        error.to_string(),
        "invalid --linked-cels value: \"unknown\""
    );
}

#[test]
fn extension_compact_arguments_use_the_requested_ordering() {
    let command = convert_arguments(&arguments(&[
        "input.psd",
        "--output",
        "output.aseprite",
        "--overwrite",
        "--layer-association",
        "auto",
        "--association-strategy",
        "compact",
        "--z-order",
        "auto",
        "--stable-order",
        "anchor",
    ]))
    .expect("extension compact arguments should parse");

    assert!(command.overwrite);
    assert_eq!(
        command.layer_association,
        LayerAssociation::Auto(AutoAssociationOptions {
            strategy: AssociationStrategy::Compact,
            z_order: LayerZOrderMode::Auto,
            stable_order: StableOrderMode::Anchor,
        })
    );
}

#[test]
fn extension_conservative_arguments_encode_group_and_flat_policies() {
    for uncertain_layers in [UncertainLayerMode::Group, UncertainLayerMode::Flat] {
        let value = match uncertain_layers {
            UncertainLayerMode::Group => "group",
            UncertainLayerMode::Flat => "flat",
        };
        let command = convert_arguments(&arguments(&[
            "input.psd",
            "--output",
            "output.aseprite",
            "--layer-association",
            "auto",
            "--association-strategy",
            "conservative",
            "--z-order",
            "stable",
            "--stable-order",
            "strict",
            "--uncertain-layers",
            value,
        ]))
        .expect("extension conservative arguments should parse");

        assert_eq!(
            command.layer_association,
            LayerAssociation::Auto(AutoAssociationOptions {
                strategy: AssociationStrategy::Conservative { uncertain_layers },
                z_order: LayerZOrderMode::Stable,
                stable_order: StableOrderMode::Strict,
            })
        );
    }
}

#[test]
fn preserve_rejects_auto_only_options_with_stable_messages() {
    let cases = [
        (
            &["input.psd", "--z-order", "auto"][..],
            "--z-order auto requires --layer-association auto",
        ),
        (
            &["input.psd", "--stable-order", "anchor"][..],
            "--stable-order requires --layer-association auto",
        ),
        (
            &["input.psd", "--uncertain-layers", "flat"][..],
            "--uncertain-layers requires --layer-association auto",
        ),
        (
            &["input.psd", "--association-strategy", "conservative"][..],
            "--association-strategy requires --layer-association auto",
        ),
    ];

    for (values, expected) in cases {
        let error = convert_arguments(&arguments(values)).expect_err("options must be rejected");
        assert_eq!(error.to_string(), expected);
        assert_eq!(error.exit_code(), 64);
    }
}

#[test]
fn compact_rejects_uncertain_layer_policy() {
    let error = convert_arguments(&arguments(&[
        "input.psd",
        "--layer-association",
        "auto",
        "--association-strategy",
        "compact",
        "--uncertain-layers",
        "flat",
    ]))
    .expect_err("compact uncertain-layer policy must be rejected");

    assert_eq!(
        error.to_string(),
        "--uncertain-layers requires --association-strategy conservative"
    );
}

#[test]
fn jitter_repair_arguments_parse_and_preserve_defaults() {
    let command = convert_arguments(&arguments(&[
        "input.psd",
        "--layer-association",
        "auto",
        "--jitter-mode",
        "repair",
        "--jitter-kind",
        "all",
        "--jitter-profile",
        "balanced",
        "--jitter-alpha-threshold",
        "12",
        "--jitter-max-speck-area",
        "3",
        "--jitter-max-changed-ratio",
        "5",
        "--jitter-max-channel-delta",
        "9",
    ]))
    .expect("jitter arguments should parse");
    assert_eq!(command.jitter.mode, JitterMode::Repair);
    assert_eq!(command.jitter.profile, JitterProfile::Balanced);
    assert_eq!(command.jitter.alpha_threshold, Some(12));
    assert_eq!(command.jitter.max_speck_area, Some(3));
    assert_eq!(command.jitter.max_changed_ratio_percent, Some(5));
    assert_eq!(command.jitter.max_channel_delta, Some(9));
}

#[test]
fn color_repair_requires_automatic_association() {
    let error = convert_arguments(&arguments(&[
        "input.psd",
        "--jitter-mode",
        "repair",
        "--jitter-kind",
        "color",
    ]))
    .expect_err("color repair must require automatic association");
    assert_eq!(
        error.to_string(),
        "--jitter-kind color/all with --jitter-mode repair requires --layer-association auto"
    );
}

#[test]
fn export_requires_composite_and_preserves_all_paths() {
    let command = export_arguments(&arguments(&[
        "source.aseprite",
        "-o",
        "output.psb",
        "--composite",
        "flattened.aseprite",
        "--report",
        "loss.json",
        "--active-frame-index",
        "8",
        "--overwrite",
    ]))
    .expect("export arguments should parse");
    assert_eq!(command.input, PathBuf::from("source.aseprite"));
    assert_eq!(command.output, PathBuf::from("output.psb"));
    assert_eq!(command.composite, PathBuf::from("flattened.aseprite"));
    assert_eq!(command.report, Some(PathBuf::from("loss.json")));
    assert_eq!(command.active_frame_index, Some(8));
    assert_eq!(command.compression, None);
    assert!(command.overwrite);
    assert!(command.embed_roundtrip_metadata);
    assert!(!command.include_empty_layers);

    let command = export_arguments(&arguments(&[
        "source.aseprite",
        "-o",
        "output.psd",
        "--composite",
        "flattened.aseprite",
        "--roundtrip-metadata",
        "off",
    ]))
    .expect("round-trip metadata option should parse");
    assert!(!command.embed_roundtrip_metadata);

    let command = export_arguments(&arguments(&[
        "source.aseprite",
        "-o",
        "output.psd",
        "--composite",
        "flattened.aseprite",
        "--empty-layers",
        "omit",
    ]))
    .expect("empty-layer policy should parse");
    assert!(!command.include_empty_layers);

    let error = export_arguments(&arguments(&[
        "source.aseprite",
        "-o",
        "output.psd",
        "--composite",
        "flattened.aseprite",
        "--empty-layers",
        "invalid",
    ]))
    .expect_err("unknown empty-layer policy should be rejected");
    assert!(error.to_string().contains("invalid --empty-layers value"));

    for value in ["raw", "rle", "zip", "zip-prediction"] {
        let parsed = export_arguments(&arguments(&[
            "source.aseprite",
            "-o",
            "output.psd",
            "--composite",
            "flattened.aseprite",
            "--compression",
            value,
        ]))
        .expect("compression should parse");
        assert_eq!(parsed.compression.map(|mode| mode.as_str()), Some(value));
    }
    let error = export_arguments(&arguments(&[
        "source.aseprite",
        "-o",
        "output.psd",
        "--composite",
        "flattened.aseprite",
        "--compression",
        "invalid",
    ]))
    .expect_err("unknown compression should be rejected");
    assert!(error.to_string().contains("--compression expects"));

    let error = export_arguments(&arguments(&["source.aseprite", "-o", "output.psd"]))
        .expect_err("composite snapshot is mandatory");
    assert_eq!(error.to_string(), EXPORT_USAGE);
}

#[test]
fn export_command_writes_psd_and_psb_reports_for_unicode_paths() {
    let directory = std::env::temp_dir().join(format!(
        "psd2ase-cli-export-{}-导出",
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("clock")
            .as_nanos()
    ));
    fs::create_dir_all(&directory).expect("create export test directory");
    let input = directory.join("源文件.aseprite");
    let composite = directory.join("合成.aseprite");
    write_export_snapshot(&input, &[255, 0, 0, 255]);
    write_export_snapshot(&composite, &[255, 0, 0, 255]);

    let psd = directory.join("结果.psd");
    let psd_report = directory.join("报告.json");
    run(vec![
        "export".to_string(),
        input.to_string_lossy().into_owned(),
        "-o".to_string(),
        psd.to_string_lossy().into_owned(),
        "--composite".to_string(),
        composite.to_string_lossy().into_owned(),
        "--report".to_string(),
        psd_report.to_string_lossy().into_owned(),
    ])
    .expect("CLI PSD export should succeed");
    let psd_bytes = fs::read(&psd).expect("read PSD");
    assert_eq!(&psd_bytes[..6], b"8BPS\0\x01");
    let report: serde_json::Value =
        serde_json::from_slice(&fs::read(&psd_report).expect("read PSD report"))
            .expect("PSD report should be JSON");
    assert_eq!(report["schema_version"], 3);
    assert_eq!(report["summary"]["total"], 0);
    assert!(report["losses"].as_array().expect("loss array").is_empty());

    let psb = directory.join("结果.psb");
    run(vec![
        "export".to_string(),
        input.to_string_lossy().into_owned(),
        "-o".to_string(),
        psb.to_string_lossy().into_owned(),
        "--composite".to_string(),
        composite.to_string_lossy().into_owned(),
    ])
    .expect("CLI PSB export should succeed");
    let psb_bytes = fs::read(&psb).expect("read PSB");
    assert_eq!(&psb_bytes[..6], b"8BPS\0\x02");

    let conflict = run(vec![
        "export".to_string(),
        input.to_string_lossy().into_owned(),
        "-o".to_string(),
        psd.to_string_lossy().into_owned(),
        "--composite".to_string(),
        composite.to_string_lossy().into_owned(),
    ])
    .expect_err("existing output should be rejected");
    assert!(conflict.to_string().contains("already exists"));

    fs::remove_dir_all(directory).expect("remove export test directory");
}

#[test]
fn export_report_failure_keeps_verified_output() {
    let directory = std::env::temp_dir().join(format!(
        "psd2ase-cli-report-failure-{}",
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("clock")
            .as_nanos()
    ));
    fs::create_dir_all(&directory).expect("create report failure directory");
    let input = directory.join("source.aseprite");
    let composite = directory.join("composite.aseprite");
    let output = directory.join("output.psd");
    let report_directory = directory.join("report-directory");
    write_export_snapshot(&input, &[0, 255, 0, 255]);
    write_export_snapshot(&composite, &[0, 255, 0, 255]);
    fs::create_dir_all(&report_directory).expect("create report target directory");

    let error = run(vec![
        "export".to_string(),
        input.to_string_lossy().into_owned(),
        "-o".to_string(),
        output.to_string_lossy().into_owned(),
        "--composite".to_string(),
        composite.to_string_lossy().into_owned(),
        "--report".to_string(),
        report_directory.to_string_lossy().into_owned(),
    ])
    .expect_err("report write should fail for a directory target");
    assert!(
        error
            .to_string()
            .contains("output generated, report write failed")
    );
    assert!(output.is_file(), "validated output must remain available");

    fs::remove_dir_all(directory).expect("remove report failure directory");
}

/// Writes a minimal authentic Aseprite snapshot for CLI export integration tests.
fn write_export_snapshot(path: &std::path::Path, rgba: &[u8; 4]) {
    let mut file = AsepriteFile::new(1, 1, AseColorMode::Rgba);
    let layer = file.add_layer("测试层");
    let frame = file.add_frame(100);
    file.set_cel(
        layer,
        frame,
        Pixels::new(rgba.to_vec(), 1, 1, AseColorMode::Rgba).expect("snapshot pixels"),
        0,
        0,
    )
    .expect("snapshot cel");
    let mut bytes = Vec::new();
    file.write_to(&mut bytes).expect("serialize snapshot");
    fs::write(path, bytes).expect("write snapshot");
}
