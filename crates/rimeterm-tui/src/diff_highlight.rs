//! Tree-sitter diff highlighting registry.

use std::collections::HashMap;

use tree_sitter::Language;
use tree_sitter_highlight::{HighlightConfiguration, HighlightEvent, Highlighter};

/// Standard highlight capture names in a single, stable order. Callers that
/// map to theme tokens can index by variant.
pub const HIGHLIGHT_NAMES: &[&str] = &[
    "attribute",
    "comment",
    "constant",
    "function",
    "keyword",
    "number",
    "operator",
    "property",
    "punctuation",
    "string",
    "tag",
    "type",
    "variable",
];

struct Spec {
    ext: &'static [&'static str],
    name: &'static str,
    language: fn() -> Language,
    highlights: &'static str,
    injections: &'static str,
    locals: &'static str,
}

const SPECS: &[Spec] = &[
    Spec {
        ext: &["rs"],
        name: "rust",
        language: || tree_sitter_rust::LANGUAGE.into(),
        highlights: tree_sitter_rust::HIGHLIGHTS_QUERY,
        injections: tree_sitter_rust::INJECTIONS_QUERY,
        locals: "",
    },
    Spec {
        ext: &["c", "h"],
        name: "c",
        language: || tree_sitter_c::LANGUAGE.into(),
        highlights: tree_sitter_c::HIGHLIGHT_QUERY,
        injections: "",
        locals: "",
    },
    Spec {
        ext: &["cc", "cpp", "cxx", "hpp", "hh", "hxx"],
        name: "cpp",
        language: || tree_sitter_cpp::LANGUAGE.into(),
        highlights: tree_sitter_cpp::HIGHLIGHT_QUERY,
        injections: "",
        locals: "",
    },
    Spec {
        ext: &["py", "pyi"],
        name: "python",
        language: || tree_sitter_python::LANGUAGE.into(),
        highlights: tree_sitter_python::HIGHLIGHTS_QUERY,
        injections: "",
        locals: "",
    },
    Spec {
        ext: &["js", "mjs", "cjs", "jsx"],
        name: "javascript",
        language: || tree_sitter_javascript::LANGUAGE.into(),
        highlights: tree_sitter_javascript::HIGHLIGHT_QUERY,
        injections: tree_sitter_javascript::INJECTIONS_QUERY,
        locals: tree_sitter_javascript::LOCALS_QUERY,
    },
    Spec {
        ext: &["ts"],
        name: "typescript",
        language: || tree_sitter_typescript::LANGUAGE_TYPESCRIPT.into(),
        highlights: tree_sitter_typescript::HIGHLIGHTS_QUERY,
        injections: "",
        locals: tree_sitter_typescript::LOCALS_QUERY,
    },
    Spec {
        ext: &["tsx"],
        name: "tsx",
        language: || tree_sitter_typescript::LANGUAGE_TSX.into(),
        highlights: tree_sitter_typescript::HIGHLIGHTS_QUERY,
        injections: "",
        locals: tree_sitter_typescript::LOCALS_QUERY,
    },
    Spec {
        ext: &["json", "jsonc"],
        name: "json",
        language: || tree_sitter_json::LANGUAGE.into(),
        highlights: tree_sitter_json::HIGHLIGHTS_QUERY,
        injections: "",
        locals: "",
    },
    Spec {
        ext: &["toml"],
        name: "toml",
        language: || tree_sitter_toml_ng::LANGUAGE.into(),
        highlights: tree_sitter_toml_ng::HIGHLIGHTS_QUERY,
        injections: "",
        locals: "",
    },
    Spec {
        ext: &["yaml", "yml"],
        name: "yaml",
        language: || tree_sitter_yaml::LANGUAGE.into(),
        highlights: tree_sitter_yaml::HIGHLIGHTS_QUERY,
        injections: "",
        locals: "",
    },
    Spec {
        ext: &["sh", "bash", "zsh"],
        name: "bash",
        language: || tree_sitter_bash::LANGUAGE.into(),
        highlights: tree_sitter_bash::HIGHLIGHT_QUERY,
        injections: "",
        locals: "",
    },
];

/// Lazy cache: one `HighlightConfiguration` per grammar name.
pub struct DiffHighlighter {
    configs: HashMap<&'static str, HighlightConfiguration>,
    highlighter: Highlighter,
}

impl Default for DiffHighlighter {
    fn default() -> Self {
        Self::new()
    }
}

impl DiffHighlighter {
    pub fn new() -> Self {
        Self {
            configs: HashMap::new(),
            highlighter: Highlighter::new(),
        }
    }

    /// Highlight `source` for the language of `extension`; falls back to a
    /// single plain span for unknown languages or parser errors.
    pub fn highlight(&mut self, extension: &str, source: &str) -> Vec<HighlightSpan> {
        let ext = extension.trim_start_matches('.').to_ascii_lowercase();
        let Some(spec) = SPECS.iter().find(|spec| spec.ext.contains(&ext.as_str())) else {
            return plain(source);
        };
        let config = self
            .configs
            .entry(spec.name)
            .or_insert_with(|| build_config(spec));
        let Ok(iter) = self
            .highlighter
            .highlight(config, source.as_bytes(), None, |_| None)
        else {
            return plain(source);
        };

        let mut spans = Vec::new();
        let mut stack: Vec<usize> = Vec::new();
        for event in iter {
            let Ok(event) = event else {
                return plain(source);
            };
            match event {
                HighlightEvent::Source { start, end } => {
                    let label = stack
                        .last()
                        .copied()
                        .and_then(|idx| HIGHLIGHT_NAMES.get(idx).copied());
                    spans.push(HighlightSpan { start, end, label });
                }
                HighlightEvent::HighlightStart(h) => stack.push(h.0),
                HighlightEvent::HighlightEnd => {
                    stack.pop();
                }
            }
        }
        if spans.is_empty() {
            plain(source)
        } else {
            spans
        }
    }
}

fn build_config(spec: &Spec) -> HighlightConfiguration {
    let mut config = HighlightConfiguration::new(
        (spec.language)(),
        spec.name,
        spec.highlights,
        spec.injections,
        spec.locals,
    )
    .expect("compile tree-sitter query");
    config.configure(HIGHLIGHT_NAMES);
    config
}

fn plain(source: &str) -> Vec<HighlightSpan> {
    vec![HighlightSpan {
        start: 0,
        end: source.len(),
        label: None,
    }]
}

/// A contiguous byte range with an optional theme-token label.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct HighlightSpan {
    pub start: usize,
    pub end: usize,
    pub label: Option<&'static str>,
}
