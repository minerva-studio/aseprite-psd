use super::*;

#[test]
fn catalog_parses_common_multilingual_suffixes() {
    for (name, base, token, language, ordinal) in [
        ("图层1 拷贝", "图层1", "拷贝", "zh", None),
        ("图层1 Copy #2", "图层1", "copy", "en", Some(2)),
        ("Layer 1 (copy)", "layer 1", "copy", "en", None),
        ("Layer 1 Kopie 3", "layer 1", "kopie", "de/nl/cs", Some(3)),
        ("Layer 1 copia", "layer 1", "copia", "es/it", None),
        ("Layer 1 duplicate-3", "layer 1", "duplicate", "en", Some(3)),
        ("Layer 1 dupliqué", "layer 1", "dupliqué", "fr", None),
        ("レイヤー1 コピー 2", "レイヤー1", "コピー", "ja", Some(2)),
        ("レイヤー1 複製", "レイヤー1", "複製", "ja", None),
        ("레이어1 복사", "레이어1", "복사", "ko", None),
        ("слой1 копия", "слой1", "копия", "ru", None),
    ] {
        let parsed = CopySuffixCatalog.parse(name);
        assert_eq!(parsed.base_name, base, "{name}");
        assert_eq!(parsed.copy_suffixes.len(), 1, "{name}");
        assert_eq!(parsed.copy_suffixes[0].token, token, "{name}");
        assert_eq!(parsed.copy_suffixes[0].language, language, "{name}");
        assert_eq!(parsed.copy_suffixes[0].ordinal, ordinal, "{name}");
    }
}

#[test]
fn catalog_peels_a_bounded_chain_of_copy_suffixes() {
    let parsed = CopySuffixCatalog.parse("Layer 1 Copy Copy 2");
    assert_eq!(parsed.base_name, "layer 1");
    assert_eq!(parsed.copy_suffixes.len(), 2);
    assert_eq!(parsed.copy_suffixes[0].ordinal, Some(2));
    assert_eq!(parsed.copy_suffixes[1].ordinal, None);
}

#[test]
fn catalog_parses_long_copy_suffix_chains_with_a_safety_limit() {
    let parsed = CopySuffixCatalog.parse("Layer 1 Copy Copy Copy Copy Copy Copy Copy Copy 3");
    assert_eq!(parsed.base_name, "layer 1");
    assert_eq!(parsed.copy_suffixes.len(), MAX_COPY_SUFFIX_DEPTH);
    assert!(!parsed.suffix_limit_reached);

    let parsed =
        CopySuffixCatalog.parse("Layer 1 Copy Copy Copy Copy Copy Copy Copy Copy Copy Copy 3");
    assert_eq!(parsed.copy_suffixes.len(), MAX_COPY_SUFFIX_DEPTH);
    assert!(parsed.suffix_limit_reached);
}

#[test]
fn catalog_does_not_strip_embedded_or_standalone_words() {
    for name in ["Copybook", "Layer Copybook", "Copy"] {
        let parsed = CopySuffixCatalog.parse(name);
        assert!(parsed.copy_suffixes.is_empty(), "{name}");
        assert_eq!(parsed.base_name, parsed.normalized_name);
    }
}
