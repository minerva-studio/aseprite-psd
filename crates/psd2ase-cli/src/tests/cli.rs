use super::*;

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
