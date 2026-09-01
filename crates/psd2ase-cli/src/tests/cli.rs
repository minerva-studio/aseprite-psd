use super::*;

fn arguments(values: &[&str]) -> Vec<String> {
    values.iter().map(|value| (*value).to_string()).collect()
}

#[test]
fn extension_preserve_arguments_use_the_preserve_configuration() {
    let command = convert_arguments(&arguments(&["input.psd", "--output", "output.aseprite"]))
        .expect("extension preserve arguments should parse");

    assert_eq!(command.layer_association, LayerAssociation::Preserve);
    assert_eq!(command.output, PathBuf::from("output.aseprite"));
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
