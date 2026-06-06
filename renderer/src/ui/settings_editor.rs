// Settings editor — a centered modal (VSCode-style) for browsing and changing
// the settings that this editor actually honors. The schema below is bound 1:1 to
// the fields in `settings::Settings`, so every row does something real; there are
// no decorative placeholders.
//
// Division of labour (matches the rest of the codebase):
//   * This module owns state + schema + layout + hit-testing.
//   * `render.rs` pushes the pixels in a dedicated late pass.
//   * `main.rs` routes input and applies a returned `Action`.

use crate::theme;
use crate::widgets::Rect;

/// A row in the left accordion: a collapsible category header or one of its
/// settings (a leaf). `Category(0)` ("Commonly Used") is a leaf with no children.
#[derive(Clone, Copy)]
pub enum Nav {
    Category(usize), // index into `categories()`
    Setting(usize),  // index into SCHEMA
}

/// The control a setting is edited with.
pub enum Control {
    Bool,
    Number,
    Text,
    /// A fixed set of `(json-value, label)` choices shown as a dropdown.
    Enum(&'static [(&'static str, &'static str)]),
    /// Color theme — opens the existing theme quick-pick rather than a dropdown.
    Theme,
}

pub struct Def {
    pub key: &'static str,
    pub title: &'static str,
    pub desc: &'static str,
    pub category: &'static str,
    pub common: bool, // also shown under "Commonly Used"
    pub control: Control,
}

const WORD_WRAP: &[(&str, &str)] = &[("off", "off"), ("on", "on")];
const CURSOR_BLINK: &[(&str, &str)] = &[("blink", "blink"), ("solid", "solid")];
const AUTO_SAVE: &[(&str, &str)] = &[("off", "off"), ("afterDelay", "afterDelay")];
const EOL: &[(&str, &str)] = &[("\n", "LF (\\n)"), ("\r\n", "CRLF (\\r\\n)")];

pub const SCHEMA: &[Def] = &[
    Def { key: "editor.fontSize", title: "Editor: Font Size", desc: "Controls the font size in pixels.", category: "Text Editor", common: true, control: Control::Number },
    Def { key: "editor.lineHeight", title: "Editor: Line Height", desc: "Controls the line height in pixels. Use 0 to derive it from the font size.", category: "Text Editor", common: false, control: Control::Number },
    Def { key: "editor.fontFamily", title: "Editor: Font Family", desc: "Controls the font family of the editor.", category: "Text Editor", common: false, control: Control::Text },
    Def { key: "editor.tabSize", title: "Editor: Tab Size", desc: "The number of spaces a tab is equal to.", category: "Text Editor", common: true, control: Control::Number },
    Def { key: "editor.insertSpaces", title: "Editor: Insert Spaces", desc: "Insert spaces when pressing Tab.", category: "Text Editor", common: false, control: Control::Bool },
    Def { key: "editor.wordWrap", title: "Editor: Word Wrap", desc: "Controls how lines should wrap.", category: "Text Editor", common: true, control: Control::Enum(WORD_WRAP) },
    Def { key: "editor.cursorBlinking", title: "Editor: Cursor Blinking", desc: "Controls the cursor animation style.", category: "Text Editor", common: false, control: Control::Enum(CURSOR_BLINK) },
    Def { key: "editor.lineNumbers", title: "Editor: Line Numbers", desc: "Controls the display of line numbers.", category: "Text Editor", common: false, control: Control::Bool },
    Def { key: "editor.rulers", title: "Editor: Rulers", desc: "Render a vertical ruler after this column. Use 0 to disable.", category: "Text Editor", common: false, control: Control::Number },
    Def { key: "editor.renderLineHighlight", title: "Editor: Render Line Highlight", desc: "Highlight the line the cursor is on.", category: "Text Editor", common: false, control: Control::Bool },
    Def { key: "files.autoSave", title: "Files: Auto Save", desc: "Controls auto save of editors that have unsaved changes.", category: "Files", common: true, control: Control::Enum(AUTO_SAVE) },
    Def { key: "files.eol", title: "Files: Eol", desc: "The default end of line character for new files.", category: "Files", common: false, control: Control::Enum(EOL) },
    Def { key: "files.trimTrailingWhitespace", title: "Files: Trim Trailing Whitespace", desc: "Trim trailing whitespace when saving a file.", category: "Files", common: false, control: Control::Bool },
    Def { key: "workbench.colorTheme", title: "Workbench: Color Theme", desc: "Specifies the color theme used in the workbench.", category: "Workbench", common: true, control: Control::Theme },
    Def { key: "workbench.fontFamily", title: "Workbench: Font Family", desc: "Controls the font family used across the UI (menus, tabs, sidebar).", category: "Workbench", common: false, control: Control::Text },
    Def { key: "workbench.activityBar.visible", title: "Workbench: Activity Bar Visible", desc: "Controls the visibility of the activity bar.", category: "Workbench", common: false, control: Control::Bool },
    Def { key: "workbench.sideBar.visible", title: "Workbench: Side Bar Visible", desc: "Controls the visibility of the side bar.", category: "Workbench", common: false, control: Control::Bool },
];

/// What a click on the editor asks `main.rs` to do.
pub enum Action {
    Close,
    Navigate,                        // left-tree expand/collapse/select (no side-effect)
    Toggle(&'static str),            // bool: flip and persist
    OpenEnum(&'static str, Rect),    // enum: open the choice dropdown at this rect
    EditText(&'static str),          // number/text: focus the inline input
    OpenTheme(Rect),                 // color theme dropdown at this rect (stays in modal)
}

/// One visible row's hit geometry, recomputed by `render.rs` each frame so the
/// click handler (which runs after draw) can test variable-height rows.
#[derive(Clone, Copy)]
pub struct RowHit {
    pub idx: usize,    // index into SCHEMA
    pub row: Rect,     // full row band (already scroll-offset, may be clipped)
    pub control: Rect, // the interactive control's rect
}

pub struct SettingsEditor {
    pub open: bool,
    pub category: usize, // selected category (0 = Commonly Used)
    pub scroll: f32,
    pub query: String,
    pub edit_key: Option<&'static str>, // row whose inline input is focused
    pub expanded: Vec<bool>,            // per-category accordion expand state
    pub selected_key: Option<&'static str>, // highlighted leaf in the left tree
    pub scroll_to: Option<&'static str>, // one-shot: bring this setting into view
    pub content_h: f32,                 // total scrollable height (set by render)
    pub rows_cache: Vec<RowHit>,        // right-pane hit rects (set by render)
    pub nav_cache: Vec<(Rect, Nav)>,    // left-tree hit rects (set by render)
}

impl Default for SettingsEditor {
    fn default() -> Self {
        Self {
            open: false,
            category: 0,
            scroll: 0.0,
            query: String::new(),
            edit_key: None,
            expanded: vec![false; categories().len()], // collapsed by default
            selected_key: None,
            scroll_to: None,
            content_h: 0.0,
            rows_cache: Vec::new(),
            nav_cache: Vec::new(),
        }
    }
}

/// Ordered category names for the left list (always led by "Commonly Used").
pub fn categories() -> Vec<&'static str> {
    let mut out = vec!["Commonly Used"];
    for d in SCHEMA {
        if !out.contains(&d.category) {
            out.push(d.category);
        }
    }
    out
}

/// SCHEMA indices belonging to category `ci` (in `categories()` order). Category 0
/// ("Commonly Used") is a flat leaf with no children.
pub fn settings_in(ci: usize) -> Vec<usize> {
    if ci == 0 {
        return Vec::new();
    }
    let cats = categories();
    let Some(cat) = cats.get(ci).copied() else { return Vec::new() };
    SCHEMA.iter().enumerate().filter(|(_, d)| d.category == cat).map(|(i, _)| i).collect()
}

impl SettingsEditor {
    /// Indices into SCHEMA that should be listed, honoring the search query (which
    /// overrides the category) or the selected category otherwise.
    pub fn visible(&self) -> Vec<usize> {
        let q = self.query.trim().to_lowercase();
        if !q.is_empty() {
            return SCHEMA
                .iter()
                .enumerate()
                .filter(|(_, d)| {
                    d.title.to_lowercase().contains(&q)
                        || d.key.to_lowercase().contains(&q)
                        || d.desc.to_lowercase().contains(&q)
                })
                .map(|(i, _)| i)
                .collect();
        }
        let cats = categories();
        let cat = cats.get(self.category).copied().unwrap_or("Commonly Used");
        SCHEMA
            .iter()
            .enumerate()
            .filter(|(_, d)| if self.category == 0 { d.common } else { d.category == cat })
            .map(|(i, _)| i)
            .collect()
    }
}

/// Static geometry of the modal (everything except variable row heights).
pub struct Layout {
    pub card: Rect,
    pub header: Rect,
    pub close: Rect,
    pub search: Rect,
    pub left: Rect,  // category tree column
    pub right: Rect, // scrollable settings viewport
}

/// Lay the modal out within the window. Sized as a fraction of the screen,
/// clamped so it stays usable at any zoom/window size.
pub fn layout(screen: Rect) -> Layout {
    let z = theme::ui_zoom();
    let w = (screen.w * 0.72).clamp(0.0, 1100.0 * z).min(screen.w - 40.0 * z);
    let h = (screen.h * 0.82).min(screen.h - 60.0 * z);
    let card = Rect {
        x: screen.x + (screen.w - w) * 0.5,
        y: screen.y + (screen.h - h) * 0.5,
        w,
        h,
    };
    let pad = theme::zpx(16.0);
    let header_h = theme::zpx(40.0);
    let search_h = theme::zpx(34.0);
    let header = Rect { x: card.x, y: card.y, w: card.w, h: header_h };
    let close = Rect { x: card.x + card.w - theme::zpx(36.0), y: card.y, w: theme::zpx(36.0), h: header_h };
    let search = Rect { x: card.x + pad, y: header.y + header_h, w: card.w - pad * 2.0, h: search_h };
    let body_y = search.y + search_h + theme::zpx(10.0);
    let body_h = card.y + card.h - body_y - theme::zpx(8.0);
    let left_w = theme::zpx(210.0);
    let left = Rect { x: card.x + pad, y: body_y, w: left_w, h: body_h };
    let right = Rect {
        x: left.x + left_w + theme::zpx(12.0),
        y: body_y,
        w: card.x + card.w - (left.x + left_w + theme::zpx(12.0)) - pad,
        h: body_h,
    };
    Layout { card, header, close, search, left, right }
}

impl SettingsEditor {
    /// Category index of SCHEMA entry `idx` within `categories()`.
    fn category_of(&self, idx: usize) -> usize {
        let cats = categories();
        cats.iter().position(|c| *c == SCHEMA[idx].category).unwrap_or(0)
    }

    /// Resolve a click at `p`. Mutates lightweight nav state directly; anything with
    /// side-effects is returned as an `Action`.
    pub fn on_click(&mut self, lay: &Layout, p: (f32, f32)) -> Option<Action> {
        if lay.close.contains(p) {
            return Some(Action::Close);
        }
        // Left accordion tree: category header (expand/collapse + select) or a leaf
        // setting (select its category + scroll the right pane to it).
        if lay.left.contains(p) {
            for (r, nav) in self.nav_cache.clone() {
                if !r.contains(p) {
                    continue;
                }
                match nav {
                    Nav::Category(ci) => {
                        if ci != 0 {
                            if let Some(e) = self.expanded.get_mut(ci) {
                                *e = !*e;
                            }
                        }
                        self.category = ci;
                        self.scroll = 0.0;
                        self.scroll_to = None;
                        self.selected_key = None;
                    }
                    Nav::Setting(idx) => {
                        self.category = self.category_of(idx);
                        self.selected_key = Some(SCHEMA[idx].key);
                        self.scroll_to = Some(SCHEMA[idx].key);
                    }
                }
                return Some(Action::Navigate);
            }
            return None;
        }
        // Setting rows (right viewport, clipped).
        if lay.right.contains(p) {
            for hit in self.rows_cache.clone() {
                let d = &SCHEMA[hit.idx];
                if hit.control.contains(p) {
                    return Some(match &d.control {
                        Control::Bool => Action::Toggle(d.key),
                        Control::Theme => Action::OpenTheme(hit.control),
                        Control::Enum(_) => Action::OpenEnum(d.key, hit.control),
                        Control::Number | Control::Text => Action::EditText(d.key),
                    });
                }
            }
        }
        None
    }
}
