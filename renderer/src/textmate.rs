// A minimal TextMate-grammar interpreter: parses a VSCode `*.tmLanguage.json`
// and tokenizes text line-by-line via its `match` patterns + capture scopes,
// mapping scopes to Nova's theme token colors. This is the "parse the extension's
// grammar and run it in Rust" path — no JS engine needed.
//
// Scope: top-level `match` rules with optional `captures` (covers rainbow-csv and
// many simple grammars). `begin`/`end` regions, `include`, and `repository` are
// not handled yet.

use std::collections::{BTreeMap, HashMap};
use std::path::Path;
use std::sync::{OnceLock, RwLock};

use fancy_regex::Regex;
use glyphon::{Attrs, Color, Family};
use serde_json::Value;

use crate::theme;

struct Rule {
    regex: Regex,
    name: Option<String>,
    captures: BTreeMap<usize, String>, // group index -> scope name
}

pub struct Grammar {
    file_types: Vec<String>,
    rules: Vec<Rule>,
}

impl Grammar {
    /// Load a `*.tmLanguage.json` grammar. Returns its file extensions + grammar.
    pub fn load(path: &Path) -> Option<Grammar> {
        let txt = std::fs::read_to_string(path).ok()?;
        let v: Value = serde_json::from_str(&txt).ok()?;
        let file_types = v["fileTypes"]
            .as_array()
            .map(|a| a.iter().filter_map(|x| x.as_str().map(|s| s.to_lowercase())).collect())
            .unwrap_or_default();
        let mut rules = Vec::new();
        if let Some(patterns) = v["patterns"].as_array() {
            for p in patterns {
                if let Some(m) = p.get("match").and_then(|x| x.as_str()) {
                    let Ok(regex) = Regex::new(m) else { continue };
                    let name = p.get("name").and_then(|x| x.as_str()).map(String::from);
                    let mut captures = BTreeMap::new();
                    if let Some(caps) = p.get("captures").and_then(|c| c.as_object()) {
                        for (k, val) in caps {
                            if let (Ok(idx), Some(scope)) =
                                (k.parse::<usize>(), val.get("name").and_then(|n| n.as_str()))
                            {
                                captures.insert(idx, scope.to_string());
                            }
                        }
                    }
                    rules.push(Rule { regex, name, captures });
                }
            }
        }
        if rules.is_empty() {
            return None;
        }
        Some(Grammar { file_types, rules })
    }
}

/// Map a TextMate scope to a Nova theme color (by leading standard segment).
fn scope_color(scope: &str) -> Color {
    let s = scope;
    if s.starts_with("comment") {
        theme::SYN_COMMENT()
    } else if s.starts_with("string") {
        theme::SYN_STRING()
    } else if s.starts_with("keyword.control") {
        theme::SYN_KEYWORD_CTRL()
    } else if s.starts_with("keyword") || s.starts_with("storage") {
        theme::SYN_KEYWORD()
    } else if s.contains("entity.name.function") || s.contains("support.function") {
        theme::SYN_FUNCTION()
    } else if s.contains("entity.name.type") || s.contains("support.type") || s.contains("entity.name.class") {
        theme::SYN_TYPE()
    } else if s.starts_with("constant.numeric") {
        theme::SYN_NUMBER()
    } else if s.starts_with("constant") {
        theme::SYN_CONSTANT()
    } else if s.starts_with("variable") {
        theme::SYN_VARIABLE()
    } else if s.starts_with("markup.bold") {
        theme::FG_ACTIVE()
    } else if s.starts_with("invalid") {
        Color::rgb(0xF4, 0x47, 0x47)
    } else if s.starts_with("rainbow1") {
        theme::SYN_KEYWORD_CTRL()
    } else {
        theme::FG_TEXT()
    }
}

/// Tokenize a single line into non-overlapping (start, end, color) spans.
fn tokenize_line(g: &Grammar, line: &str) -> Vec<(usize, usize, Color)> {
    let mut spans: Vec<(usize, usize, Color)> = Vec::new();
    for rule in &g.rules {
        if let Ok(Some(caps)) = rule.regex.captures(line) {
            if !rule.captures.is_empty() {
                for (idx, scope) in &rule.captures {
                    if let Some(m) = caps.get(*idx) {
                        if m.end() > m.start() {
                            spans.push((m.start(), m.end(), scope_color(scope)));
                        }
                    }
                }
            } else if let Some(name) = &rule.name {
                if let Some(m) = caps.get(0) {
                    spans.push((m.start(), m.end(), scope_color(name)));
                }
            }
            break; // simplistic: first matching top-level rule wins
        }
    }
    spans.sort_by_key(|s| s.0);
    spans
}

fn registry() -> &'static RwLock<HashMap<String, std::sync::Arc<Grammar>>> {
    static R: OnceLock<RwLock<HashMap<String, std::sync::Arc<Grammar>>>> = OnceLock::new();
    R.get_or_init(|| RwLock::new(HashMap::new()))
}

/// Register a grammar under its file extensions (plus any extra `exts`).
pub fn register(grammar: Grammar, extra_exts: &[String]) {
    let exts: Vec<String> = grammar
        .file_types
        .iter()
        .cloned()
        .chain(extra_exts.iter().cloned())
        .collect();
    let g = std::sync::Arc::new(grammar);
    let mut reg = registry().write().unwrap();
    for e in exts {
        reg.insert(e, g.clone());
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn rainbow_csv_colors_columns() {
        let home = std::env::var("USERPROFILE").or_else(|_| std::env::var("HOME")).unwrap();
        let path = std::path::Path::new(&home)
            .join(".vscode/extensions/mechatroner.rainbow-csv-3.24.1/syntaxes/csv.tmLanguage.json");
        if !path.exists() {
            eprintln!("rainbow-csv grammar not installed; skipping");
            return;
        }
        let g = Grammar::load(&path).expect("load grammar");
        let toks = tokenize_line(&g, "alpha,beta,gamma,delta");
        // Expect 4 column spans with at least 3 distinct colors.
        assert!(toks.len() >= 4, "got {} spans", toks.len());
        let mut colors: Vec<[f32; 4]> = toks.iter().map(|(_, _, c)| [c.r() as f32, c.g() as f32, c.b() as f32, 0.0]).collect();
        colors.dedup();
        assert!(colors.len() >= 3, "expected varied column colors, got {:?}", colors);
    }
}

pub fn has(ext: &str) -> bool {
    registry().read().unwrap().contains_key(&ext.to_lowercase())
}

/// Produce rich-text spans for `text` if a grammar is registered for `ext`.
pub fn spans_for(ext: &str, text: &str) -> Option<Vec<(String, Attrs<'static>)>> {
    let g = registry().read().unwrap().get(&ext.to_lowercase())?.clone();
    let mono = |c: Color| Attrs::new().family(Family::Name(theme::MONO_FAMILY())).color(c);
    let mut out: Vec<(String, Attrs<'static>)> = Vec::new();
    for line in text.split_inclusive('\n') {
        let has_nl = line.ends_with('\n');
        let content = line.strip_suffix('\n').unwrap_or(line);
        let toks = tokenize_line(&g, content);
        let mut pos = 0usize;
        for (s, e, col) in toks {
            let s = s.max(pos);
            if e <= s {
                continue;
            }
            if s > pos {
                out.push((content[pos..s].to_string(), mono(theme::FG_TEXT())));
            }
            out.push((content[s..e].to_string(), mono(col)));
            pos = e;
        }
        if pos < content.len() {
            out.push((content[pos..].to_string(), mono(theme::FG_TEXT())));
        }
        if has_nl {
            out.push(("\n".to_string(), mono(theme::FG_TEXT())));
        }
    }
    Some(out)
}
