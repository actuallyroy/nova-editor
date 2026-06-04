// Code completion: a popup of candidate completions for the identifier being typed.
//
// Source 1 (here): word-based — every identifier already in the open document, like
// VSCode's built-in `editor.wordBasedSuggestions`. Works in any file with no
// language server. Source 2 (LSP `textDocument/completion`) merges into the same
// popup later; `Item` already carries the fields an LSP item needs.

/// Broad category, used to pick the popup's leading icon/color.
#[derive(Clone, Copy, PartialEq)]
pub enum Kind {
    Word,
    Keyword,
    Function,
    Variable,
    Type,
    Field,
    Module,
    Snippet,
}

/// One candidate. `label` is shown; `insert` is what replaces the typed prefix
/// (usually equal to `label` for word-based; LSP may differ). `detail` is a short
/// right-aligned hint.
#[derive(Clone)]
pub struct Item {
    pub label: String,
    pub insert: String,
    pub detail: String,
    pub kind: Kind,
}

pub struct Completion {
    pub active: bool,
    pub items: Vec<Item>,
    pub selected: usize,
    /// Byte offset where the replaced prefix begins (caret − prefix length).
    pub prefix_start: usize,
    /// Scroll offset (first visible row) for long lists.
    pub scroll: usize,
}

/// Max candidates kept after filtering (popup stays snappy on large files).
const MAX_ITEMS: usize = 200;
/// Visible rows in the popup before it scrolls.
pub const VISIBLE_ROWS: usize = 9;

impl Default for Completion {
    fn default() -> Self {
        Self { active: false, items: Vec::new(), selected: 0, prefix_start: 0, scroll: 0 }
    }
}

impl Completion {
    pub fn close(&mut self) {
        self.active = false;
        self.items.clear();
        self.selected = 0;
        self.scroll = 0;
    }

    /// Recompute word-based candidates for the identifier ending at `caret`. Closes
    /// if there's no prefix or nothing matches. Returns whether the popup is active.
    pub fn update_words(&mut self, text: &str, caret: usize) -> bool {
        let prefix_start = word_start(text, caret);
        let prefix = &text[prefix_start..caret];
        if prefix.is_empty() {
            self.close();
            return false;
        }
        let items = word_candidates(text, prefix, prefix_start, caret);
        self.set_items(items, prefix_start);
        self.active
    }

    /// Install a fresh candidate list (sorted by the source). Preserves nothing —
    /// selection resets to the top. Closes if empty.
    pub fn set_items(&mut self, items: Vec<Item>, prefix_start: usize) {
        if items.is_empty() {
            self.close();
            return;
        }
        self.items = items;
        self.prefix_start = prefix_start;
        self.selected = 0;
        self.scroll = 0;
        self.active = true;
    }

    /// Move the selection by `delta`, wrapping, and keep it within the scroll window.
    pub fn move_sel(&mut self, delta: i32) {
        if self.items.is_empty() {
            return;
        }
        let n = self.items.len() as i32;
        self.selected = (((self.selected as i32 + delta) % n + n) % n) as usize;
        if self.selected < self.scroll {
            self.scroll = self.selected;
        } else if self.selected >= self.scroll + VISIBLE_ROWS {
            self.scroll = self.selected + 1 - VISIBLE_ROWS;
        }
    }

    pub fn selected_item(&self) -> Option<&Item> {
        self.items.get(self.selected)
    }
}

/// Icon + color for a popup item's kind (delegates to the shared symbol scheme).
pub fn kind_icon(kind: Kind) -> (char, glyphon::Color) {
    crate::theme::symbol_icon(match kind {
        Kind::Function => "fn",
        Kind::Variable => "var",
        Kind::Field => "field",
        Kind::Type => "class",
        Kind::Module => "mod",
        Kind::Keyword => "keyword",
        Kind::Snippet => "snippet",
        Kind::Word => "",
    })
}

/// Map LSP completion items (generic across servers) into popup items, translating
/// the LSP `CompletionItemKind` number into our `Kind`.
pub fn from_lsp(items: Vec<crate::lsp::CompletionItem>) -> Vec<Item> {
    items
        .into_iter()
        .map(|it| {
            let kind = match it.kind {
                2 | 3 => Kind::Function,           // Method, Function
                5 | 10 => Kind::Field,             // Field, Property
                6 | 12 | 21 => Kind::Variable,     // Variable, Value, Constant
                7 | 8 | 13 | 22 | 25 => Kind::Type, // Class, Interface, Enum, Struct, TypeParam
                9 => Kind::Module,                 // Module
                14 => Kind::Keyword,               // Keyword
                15 => Kind::Snippet,               // Snippet
                _ => Kind::Word,
            };
            Item { label: it.label, insert: it.insert, detail: it.detail, kind }
        })
        .collect()
}

/// Byte offset where the identifier prefix ending at `caret` begins, or None if the
/// caret isn't right after a word character (so nothing to complete).
pub fn word_prefix(text: &str, caret: usize) -> Option<usize> {
    let start = word_start(text, caret);
    (start < caret).then_some(start)
}

/// First byte of the identifier ending at `caret` (caret if not on a word char).
fn word_start(text: &str, caret: usize) -> usize {
    let bytes = text.as_bytes();
    let mut i = caret;
    while i > 0 && is_word_byte(bytes[i - 1]) {
        i -= 1;
    }
    i
}

fn is_word_byte(b: u8) -> bool {
    b == b'_' || b.is_ascii_alphanumeric() || b >= 0x80 // keep multibyte identifier chars
}

/// Unique identifiers in `text` that start with `prefix` (case-insensitive), minus
/// the word currently being typed. Sorted: case-sensitive prefix match first, then
/// shorter, then alphabetical. Capped to `MAX_ITEMS`.
fn word_candidates(text: &str, prefix: &str, prefix_start: usize, caret: usize) -> Vec<Item> {
    let lower_prefix = prefix.to_ascii_lowercase();
    let mut seen = std::collections::HashSet::new();
    let bytes = text.as_bytes();
    let mut i = 0;
    let n = bytes.len();
    let _ = caret;
    while i < n {
        if is_word_start_byte(bytes[i]) {
            let start = i;
            i += 1;
            while i < n && is_word_byte(bytes[i]) {
                i += 1;
            }
            // Skip the word the cursor is inside (it's what you're typing). Any OTHER
            // word that starts with the prefix is a match — including ones equal to it,
            // so the popup stays open as long as something matches (it only closes when
            // nothing in the document starts with the prefix).
            if start == prefix_start {
                continue;
            }
            let word = &text[start..i];
            if word.len() >= prefix.len() && word.to_ascii_lowercase().starts_with(&lower_prefix) {
                seen.insert(word.to_string());
            }
        } else {
            i += 1;
        }
    }
    let mut items: Vec<Item> = seen
        .into_iter()
        .map(|w| Item { label: w.clone(), insert: w, detail: String::new(), kind: Kind::Word })
        .collect();
    items.sort_by(|a, b| {
        let ap = a.label.starts_with(prefix);
        let bp = b.label.starts_with(prefix);
        bp.cmp(&ap) // exact-case prefix match first
            .then(a.label.len().cmp(&b.label.len()))
            .then_with(|| a.label.cmp(&b.label))
    });
    items.truncate(MAX_ITEMS);
    items
}

fn is_word_start_byte(b: u8) -> bool {
    b == b'_' || b.is_ascii_alphabetic() || b >= 0x80
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn suggests_matching_words_excluding_the_typed_one() {
        let text = "fn compute_total() { let computed = 1; let count = computed; comp }";
        // Caret sits right after the trailing "comp".
        let caret = text.rfind("comp").unwrap() + 4;
        let mut c = Completion::default();
        assert!(c.update_words(text, caret));
        let labels: Vec<&str> = c.items.iter().map(|i| i.label.as_str()).collect();
        assert!(labels.contains(&"compute_total"), "got {labels:?}");
        assert!(labels.contains(&"computed"), "got {labels:?}");
        // "comp" itself (the prefix being typed) is not offered.
        assert!(!labels.contains(&"comp"));
        // "count" doesn't start with "comp".
        assert!(!labels.contains(&"count"));
    }

    #[test]
    fn no_prefix_closes() {
        let text = "let x = 1; ";
        let mut c = Completion::default();
        assert!(!c.update_words(text, text.len())); // caret after a space → no prefix
        assert!(!c.active);
    }

    #[test]
    fn selection_wraps_and_tracks_scroll() {
        let mut c = Completion::default();
        c.set_items(
            (0..20).map(|i| Item { label: format!("item{i:02}"), insert: String::new(), detail: String::new(), kind: Kind::Word }).collect(),
            0,
        );
        c.move_sel(-1); // wrap to last
        assert_eq!(c.selected, 19);
        assert!(c.scroll + VISIBLE_ROWS > c.selected); // last row visible
        c.move_sel(1); // wrap back to first
        assert_eq!(c.selected, 0);
        assert_eq!(c.scroll, 0);
    }
}
