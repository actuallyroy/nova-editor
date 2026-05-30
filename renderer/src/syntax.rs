// Tree-sitter syntax highlighting. Produces (text, Attrs) spans for a document's
// glyphon buffer, coloured per VSCode Dark+. Configurations are built once and
// cached; unsupported languages return None (caller falls back to plain text).

use std::sync::OnceLock;

use glyphon::{Attrs, Color, Family};
use tree_sitter_highlight::{Highlight, HighlightConfiguration, HighlightEvent, Highlighter};

use crate::theme;

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Lang {
    Rust,
    Json,
    Markdown,
    PlainText,
}

impl Lang {
    pub fn from_ext(ext: &str) -> Lang {
        match ext.to_ascii_lowercase().as_str() {
            "rs" => Lang::Rust,
            "json" | "mcp" => Lang::Json,
            "md" | "markdown" => Lang::Markdown,
            _ => Lang::PlainText,
        }
    }
}

/// Capture names we recognise. The `Highlight(usize)` index returned by the
/// highlighter indexes into this list.
const HIGHLIGHT_NAMES: &[&str] = &[
    "attribute",
    "comment",
    "constant",
    "constant.builtin",
    "constructor",
    "escape",
    "function",
    "function.builtin",
    "function.macro",
    "function.method",
    "keyword",
    "keyword.control",
    "label",
    "number",
    "operator",
    "property",
    "punctuation",
    "punctuation.bracket",
    "punctuation.delimiter",
    "string",
    "string.escape",
    "type",
    "type.builtin",
    "variable",
    "variable.builtin",
    "variable.parameter",
];

fn color_for(idx: usize) -> Color {
    match HIGHLIGHT_NAMES[idx] {
        "comment" => theme::SYN_COMMENT(),
        "string" | "string.escape" | "escape" => theme::SYN_STRING(),
        "keyword.control" => theme::SYN_KEYWORD_CTRL(),
        "keyword" | "operator" => theme::SYN_KEYWORD(),
        "function" | "function.builtin" | "function.macro" | "function.method" | "attribute" => {
            theme::SYN_FUNCTION()
        }
        "type" | "type.builtin" | "constructor" => theme::SYN_TYPE(),
        "number" => theme::SYN_NUMBER(),
        "constant" | "constant.builtin" | "variable.builtin" => theme::SYN_CONSTANT(),
        "property" | "variable.parameter" => theme::SYN_VARIABLE(),
        "label" => theme::SYN_LABEL(),
        _ => theme::FG_TEXT(),
    }
}

fn config_for(lang: Lang) -> Option<&'static HighlightConfiguration> {
    match lang {
        Lang::Rust => {
            static C: OnceLock<HighlightConfiguration> = OnceLock::new();
            Some(C.get_or_init(|| {
                let mut c = HighlightConfiguration::new(
                    tree_sitter_rust::LANGUAGE.into(),
                    "rust",
                    tree_sitter_rust::HIGHLIGHTS_QUERY,
                    "",
                    "",
                )
                .expect("rust highlight config");
                c.configure(HIGHLIGHT_NAMES);
                c
            }))
        }
        Lang::Json => {
            static C: OnceLock<HighlightConfiguration> = OnceLock::new();
            Some(C.get_or_init(|| {
                let mut c = HighlightConfiguration::new(
                    tree_sitter_json::LANGUAGE.into(),
                    "json",
                    tree_sitter_json::HIGHLIGHTS_QUERY,
                    "",
                    "",
                )
                .expect("json highlight config");
                c.configure(HIGHLIGHT_NAMES);
                c
            }))
        }
        _ => None,
    }
}

/// Highlight `text` for `lang`, returning per-span (text, attrs). Returns None
/// for languages without a tree-sitter config (caller uses plain text).
pub fn highlight_spans(lang: Lang, text: &str) -> Option<Vec<(String, Attrs<'static>)>> {
    let config = config_for(lang)?;
    let mono = |c: Color| Attrs::new().family(Family::Name(theme::MONO_FAMILY())).color(c);
    let mut hl = Highlighter::new();
    let events = hl.highlight(config, text.as_bytes(), None, |_| None).ok()?;
    let mut spans: Vec<(String, Attrs<'static>)> = Vec::new();
    let mut stack: Vec<usize> = Vec::new();
    for ev in events {
        match ev.ok()? {
            HighlightEvent::HighlightStart(Highlight(i)) => stack.push(i),
            HighlightEvent::HighlightEnd => {
                stack.pop();
            }
            HighlightEvent::Source { start, end } => {
                if start >= end {
                    continue;
                }
                let color = stack.last().map(|&i| color_for(i)).unwrap_or(theme::FG_TEXT());
                spans.push((text[start..end].to_string(), mono(color)));
            }
        }
    }
    Some(spans)
}
