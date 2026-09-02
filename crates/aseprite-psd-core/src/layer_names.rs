//! Language-aware layer-name normalization used by logical association.

/// Version of the built-in copy-suffix catalog.
pub const COPY_SUFFIX_CATALOG_VERSION: u16 = 1;

/// Maximum number of chained copy suffixes parsed from one layer name.
pub const MAX_COPY_SUFFIX_DEPTH: usize = 8;

/// Describes the broad meaning of a recognized copy suffix.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CopySuffixKind {
    /// A suffix meaning a copied layer.
    Copy,
    /// A suffix meaning a duplicated layer.
    Duplicate,
    /// A suffix meaning a cloned layer.
    Clone,
}

/// One entry in the built-in copy-suffix catalog.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CopySuffixRule {
    /// The normalized suffix token.
    pub token: &'static str,
    /// Short language identifier used for diagnostics.
    pub language: &'static str,
    /// Semantic category of the suffix.
    pub kind: CopySuffixKind,
}

/// A copy suffix recognized at the end of a layer name.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CopySuffixMatch {
    /// The token as it appears in the catalog.
    pub token: String,
    /// Short language identifier used for diagnostics.
    pub language: String,
    /// Semantic category of the suffix.
    pub kind: CopySuffixKind,
    /// Optional copy number following the suffix.
    pub ordinal: Option<u32>,
}

/// Normalized information extracted from one source layer name.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParsedLayerName {
    /// Whitespace- and case-normalized original name.
    pub normalized_name: String,
    /// Name used as the candidate family key.
    pub base_name: String,
    /// Whether the base name is a generic numbered layer name.
    pub generic: bool,
    /// Copy suffixes removed from the end of the name.
    pub copy_suffixes: Vec<CopySuffixMatch>,
    /// Whether another suffix remained after the safety limit was reached.
    pub suffix_limit_reached: bool,
}

/// The built-in, versioned catalog of common copy suffixes.
#[derive(Debug, Clone, Copy, Default)]
pub struct CopySuffixCatalog;

impl CopySuffixCatalog {
    /// Returns the catalog version used in association diagnostics.
    pub const fn version(self) -> u16 {
        COPY_SUFFIX_CATALOG_VERSION
    }

    /// Returns the immutable suffix rules used by this catalog.
    pub const fn rules(self) -> &'static [CopySuffixRule] {
        COPY_SUFFIXES
    }

    /// Parses a layer name without changing its original spelling.
    pub fn parse(self, name: &str) -> ParsedLayerName {
        let normalized_name = normalize_name(name);
        let mut base_name = normalized_name.clone();
        let mut copy_suffixes = Vec::new();

        for _ in 0..MAX_COPY_SUFFIX_DEPTH {
            let Some((next_base, suffix)) = strip_copy_suffix(&base_name) else {
                break;
            };
            base_name = next_base;
            copy_suffixes.push(suffix);
        }

        let suffix_limit_reached =
            copy_suffixes.len() == MAX_COPY_SUFFIX_DEPTH && strip_copy_suffix(&base_name).is_some();
        let generic = is_generic_name(&base_name);
        ParsedLayerName {
            normalized_name,
            base_name,
            generic,
            copy_suffixes,
            suffix_limit_reached,
        }
    }
}

const COPY_SUFFIXES: &[CopySuffixRule] = &[
    CopySuffixRule {
        token: "copy",
        language: "en",
        kind: CopySuffixKind::Copy,
    },
    CopySuffixRule {
        token: "duplicate",
        language: "en",
        kind: CopySuffixKind::Duplicate,
    },
    CopySuffixRule {
        token: "clone",
        language: "en/fr/it/pt",
        kind: CopySuffixKind::Clone,
    },
    CopySuffixRule {
        token: "拷贝",
        language: "zh",
        kind: CopySuffixKind::Copy,
    },
    CopySuffixRule {
        token: "副本",
        language: "zh",
        kind: CopySuffixKind::Copy,
    },
    CopySuffixRule {
        token: "复制",
        language: "zh",
        kind: CopySuffixKind::Duplicate,
    },
    CopySuffixRule {
        token: "克隆",
        language: "zh",
        kind: CopySuffixKind::Clone,
    },
    CopySuffixRule {
        token: "コピー",
        language: "ja",
        kind: CopySuffixKind::Copy,
    },
    CopySuffixRule {
        token: "複製",
        language: "ja",
        kind: CopySuffixKind::Duplicate,
    },
    CopySuffixRule {
        token: "クローン",
        language: "ja",
        kind: CopySuffixKind::Clone,
    },
    CopySuffixRule {
        token: "복사",
        language: "ko",
        kind: CopySuffixKind::Copy,
    },
    CopySuffixRule {
        token: "사본",
        language: "ko",
        kind: CopySuffixKind::Copy,
    },
    CopySuffixRule {
        token: "복제",
        language: "ko",
        kind: CopySuffixKind::Duplicate,
    },
    CopySuffixRule {
        token: "클론",
        language: "ko",
        kind: CopySuffixKind::Clone,
    },
    CopySuffixRule {
        token: "копия",
        language: "ru",
        kind: CopySuffixKind::Copy,
    },
    CopySuffixRule {
        token: "копія",
        language: "uk",
        kind: CopySuffixKind::Copy,
    },
    CopySuffixRule {
        token: "дубликат",
        language: "ru",
        kind: CopySuffixKind::Duplicate,
    },
    CopySuffixRule {
        token: "клон",
        language: "ru",
        kind: CopySuffixKind::Clone,
    },
    CopySuffixRule {
        token: "kopie",
        language: "de/nl/cs",
        kind: CopySuffixKind::Copy,
    },
    CopySuffixRule {
        token: "duplikat",
        language: "de/pl",
        kind: CopySuffixKind::Duplicate,
    },
    CopySuffixRule {
        token: "klon",
        language: "de/pl",
        kind: CopySuffixKind::Clone,
    },
    CopySuffixRule {
        token: "duplicaat",
        language: "nl",
        kind: CopySuffixKind::Duplicate,
    },
    CopySuffixRule {
        token: "kloon",
        language: "nl",
        kind: CopySuffixKind::Clone,
    },
    CopySuffixRule {
        token: "copie",
        language: "fr",
        kind: CopySuffixKind::Copy,
    },
    CopySuffixRule {
        token: "dupliqué",
        language: "fr",
        kind: CopySuffixKind::Duplicate,
    },
    CopySuffixRule {
        token: "copia",
        language: "es/it",
        kind: CopySuffixKind::Copy,
    },
    CopySuffixRule {
        token: "duplicado",
        language: "es/pt",
        kind: CopySuffixKind::Duplicate,
    },
    CopySuffixRule {
        token: "duplicato",
        language: "it",
        kind: CopySuffixKind::Duplicate,
    },
    CopySuffixRule {
        token: "clon",
        language: "es",
        kind: CopySuffixKind::Clone,
    },
    CopySuffixRule {
        token: "cópia",
        language: "pt",
        kind: CopySuffixKind::Copy,
    },
    CopySuffixRule {
        token: "kopia",
        language: "pl/sv",
        kind: CopySuffixKind::Copy,
    },
    CopySuffixRule {
        token: "kopi",
        language: "da/no",
        kind: CopySuffixKind::Copy,
    },
    CopySuffixRule {
        token: "kopio",
        language: "fi",
        kind: CopySuffixKind::Copy,
    },
    CopySuffixRule {
        token: "másolat",
        language: "hu",
        kind: CopySuffixKind::Copy,
    },
    CopySuffixRule {
        token: "αντίγραφο",
        language: "el",
        kind: CopySuffixKind::Copy,
    },
];

fn normalize_name(name: &str) -> String {
    name.split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .to_lowercase()
}

fn strip_copy_suffix(value: &str) -> Option<(String, CopySuffixMatch)> {
    for rule in COPY_SUFFIXES {
        let Some(index) = value.rfind(rule.token) else {
            continue;
        };
        let prefix = &value[..index];
        if !prefix.is_empty() && !prefix.chars().next_back().is_some_and(is_suffix_boundary) {
            continue;
        }
        let tail = &value[index + rule.token.len()..];
        let Some(ordinal) = parse_suffix_tail(tail) else {
            continue;
        };
        let base = prefix.trim_end_matches(is_base_separator).trim_end();
        if base.is_empty() {
            continue;
        }
        return Some((
            base.to_string(),
            CopySuffixMatch {
                token: rule.token.to_string(),
                language: rule.language.to_string(),
                kind: rule.kind,
                ordinal,
            },
        ));
    }
    None
}

fn parse_suffix_tail(tail: &str) -> Option<Option<u32>> {
    let mut tail = tail.trim();
    while tail.chars().next_back().is_some_and(is_closing_wrapper) {
        tail = tail[..tail.len() - tail.chars().next_back()?.len_utf8()].trim_end();
    }
    if tail.is_empty() {
        return Some(None);
    }
    let digits = tail.trim_start_matches(['-', '_', '.']);
    let digits = digits.strip_prefix('#').unwrap_or(digits).trim();
    if digits.is_empty() || !digits.chars().all(|character| character.is_ascii_digit()) {
        return None;
    }
    digits.parse().ok().map(Some)
}

fn is_suffix_boundary(character: char) -> bool {
    character.is_whitespace() || matches!(character, '-' | '_' | '.' | '(' | '[' | '{')
}

fn is_base_separator(character: char) -> bool {
    matches!(character, '-' | '_' | '.' | '(' | '[' | '{')
}

fn is_closing_wrapper(character: char) -> bool {
    matches!(character, ')' | ']' | '}')
}

fn is_generic_name(name: &str) -> bool {
    name.strip_prefix("图层 ")
        .or_else(|| name.strip_prefix("layer "))
        .is_some_and(|value| {
            !value.is_empty() && value.chars().all(|character| character.is_ascii_digit())
        })
}

#[cfg(test)]
#[path = "tests/layer_names.rs"]
mod tests;
