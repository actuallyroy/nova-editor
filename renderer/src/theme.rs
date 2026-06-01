// Theme + layout constants.
//
// COLORS are runtime-configurable: they live in a global `Theme` (so a loaded
// VSCode color theme can replace them) and are read through same-named accessor
// functions, e.g. `theme::FG_TEXT()`. DIMENSIONS, fonts, and icon glyphs stay
// compile-time `const` — a theme never changes those.
#![allow(non_snake_case)]

use std::sync::{OnceLock, RwLock};

use glyphon::Color;

/// All themeable colors. `wgpu::Color` for the clear value, `glyphon::Color` for
/// text, and `[f32; 4]` for quad fills.
#[derive(Clone)]
pub struct Theme {
    pub bg_editor: wgpu::Color,
    pub fg_text: Color,
    pub syn_comment: Color,
    pub syn_string: Color,
    pub syn_keyword: Color,
    pub syn_keyword_ctrl: Color,
    pub syn_function: Color,
    pub syn_type: Color,
    pub syn_number: Color,
    pub syn_constant: Color,
    pub syn_variable: Color,
    pub syn_label: Color,
    pub md_heading: Color,
    pub md_quote: Color,
    pub md_code: Color,
    pub md_rule: Color,
    pub md_list: Color,
    pub fg_dim: Color,
    pub fg_active: Color,
    pub fg_gutter: Color,
    pub fg_gutter_active: Color,
    pub cursor: [f32; 4],
    pub scrollbar_thumb: [f32; 4],
    pub scrollbar_thumb_hover: [f32; 4],
    pub selection: [f32; 4],
    pub line_highlight: [f32; 4],
    pub find_match: [f32; 4],
    pub activity_bar_bg: [f32; 4],
    pub activity_bar_active: [f32; 4],
    pub sidebar_bg: [f32; 4],
    pub panel_bg: [f32; 4],     // integrated terminal / bottom panel background
    pub panel_border: [f32; 4], // low-contrast divider between editor and panel
    pub tab_bar_bg: [f32; 4],
    pub tab_inactive: [f32; 4],
    pub tab_active: [f32; 4],
    pub tab_hover: [f32; 4],
    pub tab_fg_active: Color,
    pub tab_fg_inactive: Color,
    pub close_fg: Color,
    pub close_fg_hover: Color,
    pub status_bar_bg: [f32; 4],
    pub status_bar_fg: Color,
    pub border: [f32; 4],
    pub tree_hover: [f32; 4],
    pub tree_active_file: [f32; 4],
    pub tree_selected: [f32; 4],
    pub palette_bg: [f32; 4],
    pub palette_border: [f32; 4],
    pub palette_input_bg: [f32; 4],
    pub palette_selected: [f32; 4],
    pub dialog_btn: [f32; 4],
    pub dialog_btn_hover: [f32; 4],
    pub dialog_overlay: [f32; 4],
    pub activity_icon_fg: Color,
    pub activity_icon_active: Color,
    pub icon_folder_color: Color,
    pub icon_file_color: Color,
    pub title_bar_bg: [f32; 4],
    pub title_fg: Color,
    pub search_bg: [f32; 4],
    pub search_bg_hover: [f32; 4],
    pub search_border: [f32; 4],
    pub title_close_hover: [f32; 4],
    pub title_btn_hover: [f32; 4],
    pub menu_hover: [f32; 4],
    pub context_bg: [f32; 4],
    pub context_border: [f32; 4],
    pub context_sel: [f32; 4],
}

impl Theme {
    /// The built-in VSCode Dark Modern / Dark+ palette (default).
    pub fn dark() -> Self {
        Self {
            bg_editor: wgpu::Color { r: 0.076, g: 0.078, b: 0.078, a: 1.0 },
            fg_text: Color::rgb(0xD4, 0xD4, 0xD4),
            syn_comment: Color::rgb(0x6A, 0x99, 0x55),
            syn_string: Color::rgb(0xCE, 0x91, 0x78),
            syn_keyword: Color::rgb(0x56, 0x9C, 0xD6),
            syn_keyword_ctrl: Color::rgb(0xC5, 0x86, 0xC0),
            syn_function: Color::rgb(0xDC, 0xDC, 0xAA),
            syn_type: Color::rgb(0x4E, 0xC9, 0xB0),
            syn_number: Color::rgb(0xB5, 0xCE, 0xA8),
            syn_constant: Color::rgb(0x56, 0x9C, 0xD6),
            syn_variable: Color::rgb(0x9C, 0xDC, 0xFE),
            syn_label: Color::rgb(0xC8, 0xC8, 0xC8),
            md_heading: Color::rgb(0x56, 0x9C, 0xD6),
            md_quote: Color::rgb(0x6A, 0x99, 0x55),
            md_code: Color::rgb(0xCE, 0x91, 0x78),
            md_rule: Color::rgb(0x80, 0x80, 0x80),
            md_list: Color::rgb(0x64, 0x9B, 0xD6),
            fg_dim: Color::rgb(0x85, 0x85, 0x85),
            fg_active: Color::rgb(0xFF, 0xFF, 0xFF),
            fg_gutter: Color::rgb(0x6E, 0x73, 0x81),
            fg_gutter_active: Color::rgb(0xC6, 0xC6, 0xC6),
            cursor: [0.82, 0.82, 0.82, 1.0],
            scrollbar_thumb: [1.0, 1.0, 1.0, 0.16],
            scrollbar_thumb_hover: [1.0, 1.0, 1.0, 0.34],
            selection: [0.16, 0.32, 0.55, 0.45],
            line_highlight: [1.0, 1.0, 1.0, 0.04],
            find_match: [0.6, 0.5, 0.0, 0.45],
            activity_bar_bg: [0.129, 0.137, 0.141, 1.0],
            activity_bar_active: [1.0, 1.0, 1.0, 0.08],
            sidebar_bg: [0.102, 0.110, 0.114, 1.0],
            // Slightly raised vs the editor (0.076) so the terminal reads as its
            // own surface, and a soft translucent-white divider on top.
            panel_bg: [0.094, 0.098, 0.102, 1.0],
            panel_border: [1.0, 1.0, 1.0, 0.10],
            tab_bar_bg: [0.098, 0.102, 0.106, 1.0],
            tab_inactive: [0.098, 0.102, 0.106, 1.0],
            tab_active: [0.076, 0.078, 0.078, 1.0],
            tab_hover: [0.21, 0.21, 0.21, 1.0],
            tab_fg_active: Color::rgb(0xFF, 0xFF, 0xFF),
            tab_fg_inactive: Color::rgb(0xB0, 0xB0, 0xB0),
            close_fg: Color::rgb(0x9A, 0x9A, 0x9A),
            close_fg_hover: Color::rgb(0xFF, 0xFF, 0xFF),
            status_bar_bg: [0.129, 0.129, 0.125, 1.0],
            status_bar_fg: Color::rgb(0xFF, 0xFF, 0xFF),
            border: [0.0, 0.0, 0.0, 1.0],
            tree_hover: [1.0, 1.0, 1.0, 0.04],
            tree_active_file: [1.0, 1.0, 1.0, 0.09],
            tree_selected: [0.15, 0.30, 0.45, 0.6],
            palette_bg: [0.176, 0.176, 0.176, 1.0],
            palette_border: [0.30, 0.30, 0.30, 1.0],
            palette_input_bg: [0.20, 0.20, 0.20, 1.0],
            palette_selected: [0.07, 0.36, 0.61, 0.85],
            dialog_btn: [0.24, 0.24, 0.26, 1.0],
            dialog_btn_hover: [0.20, 0.42, 0.66, 1.0],
            dialog_overlay: [0.0, 0.0, 0.0, 0.5],
            activity_icon_fg: Color::rgb(0xC8, 0xC8, 0xC8),
            activity_icon_active: Color::rgb(0xFF, 0xFF, 0xFF),
            icon_folder_color: Color::rgb(0x8A, 0xB4, 0xE8),
            icon_file_color: Color::rgb(0xC5, 0xC5, 0xC5),
            title_bar_bg: [0.145, 0.149, 0.152, 1.0],
            title_fg: Color::rgb(0xCC, 0xCC, 0xCC),
            search_bg: [0.118, 0.118, 0.125, 1.0],
            search_bg_hover: [0.22, 0.22, 0.23, 1.0],
            search_border: [0.27, 0.27, 0.28, 1.0],
            title_close_hover: [0.78, 0.16, 0.16, 1.0],
            title_btn_hover: [1.0, 1.0, 1.0, 0.08],
            menu_hover: [1.0, 1.0, 1.0, 0.08],
            context_bg: [0.18, 0.18, 0.19, 1.0],
            context_border: [0.30, 0.30, 0.30, 1.0],
            context_sel: [0.07, 0.36, 0.61, 0.85],
        }
    }
}

fn current() -> &'static RwLock<Theme> {
    static T: OnceLock<RwLock<Theme>> = OnceLock::new();
    T.get_or_init(|| RwLock::new(Theme::dark()))
}

/// Replace the active theme (e.g. after loading a VSCode color theme).
pub fn set(theme: Theme) {
    *current().write().unwrap() = theme;
}

fn parse_hex(s: &str) -> Option<(u8, u8, u8, f32)> {
    let h = s.strip_prefix('#')?;
    let n = |i: usize| u8::from_str_radix(&h[i..i + 2], 16).ok();
    match h.len() {
        6 => Some((n(0)?, n(2)?, n(4)?, 1.0)),
        8 => Some((n(0)?, n(2)?, n(4)?, n(6)? as f32 / 255.0)),
        _ => None,
    }
}

/// Parse a VSCode `*-color-theme.json` into a `Theme`, starting from `dark()` and
/// overriding the keys it specifies (`colors` map + `tokenColors` scopes).
pub fn load_vscode(path: &std::path::Path) -> Option<Theme> {
    let txt = std::fs::read_to_string(path).ok()?;
    // Themes are JSON-with-comments sometimes; serde_json is strict, so strip // and /* */.
    let v: serde_json::Value = serde_json::from_str(&strip_jsonc(&txt)).ok()?;
    let mut t = Theme::dark();

    if let Some(colors) = v.get("colors").and_then(|c| c.as_object()) {
        let rgb = |key: &str| colors.get(key).and_then(|x| x.as_str()).and_then(parse_hex);
        let mut col = |key: &str, dst: &mut Color| {
            if let Some((r, g, b, _)) = rgb(key) {
                *dst = Color::rgb(r, g, b);
            }
        };
        col("editor.foreground", &mut t.fg_text);
        col("statusBar.foreground", &mut t.status_bar_fg);
        col("titleBar.activeForeground", &mut t.title_fg);
        col("tab.activeForeground", &mut t.tab_fg_active);
        col("tab.inactiveForeground", &mut t.tab_fg_inactive);
        col("editorLineNumber.foreground", &mut t.fg_gutter);
        col("icon.foreground", &mut t.activity_icon_fg);

        let mut quad = |key: &str, dst: &mut [f32; 4]| {
            if let Some((r, g, b, a)) = rgb(key) {
                *dst = [r as f32 / 255.0, g as f32 / 255.0, b as f32 / 255.0, a];
            }
        };
        if let Some((r, g, b, _)) = rgb("editor.background") {
            t.bg_editor = wgpu::Color {
                r: r as f64 / 255.0,
                g: g as f64 / 255.0,
                b: b as f64 / 255.0,
                a: 1.0,
            };
            t.tab_active = [r as f32 / 255.0, g as f32 / 255.0, b as f32 / 255.0, 1.0];
            // Default the panel to a faintly-raised editor bg so it stays distinct
            // on light themes too; panel.background overrides below if present.
            t.panel_bg = [
                (r as f32 / 255.0 + 0.02).min(1.0),
                (g as f32 / 255.0 + 0.02).min(1.0),
                (b as f32 / 255.0 + 0.02).min(1.0),
                1.0,
            ];
        }
        quad("panel.background", &mut t.panel_bg);
        quad("panel.border", &mut t.panel_border);
        quad("activityBar.background", &mut t.activity_bar_bg);
        quad("sideBar.background", &mut t.sidebar_bg);
        quad("editorGroupHeader.tabsBackground", &mut t.tab_bar_bg);
        quad("tab.inactiveBackground", &mut t.tab_inactive);
        quad("tab.activeBackground", &mut t.tab_active);
        quad("statusBar.background", &mut t.status_bar_bg);
        quad("titleBar.activeBackground", &mut t.title_bar_bg);
        quad("editor.selectionBackground", &mut t.selection);
        quad("list.hoverBackground", &mut t.tree_hover);
        quad("list.inactiveSelectionBackground", &mut t.tree_active_file);
        quad("list.activeSelectionBackground", &mut t.tree_selected);
        quad("quickInput.background", &mut t.palette_bg);
        quad("menu.background", &mut t.context_bg);
    }

    if let Some(tokens) = v.get("tokenColors").and_then(|t| t.as_array()) {
        let mut syn = |scope_match: &dyn Fn(&str) -> bool, dst: &mut Color| {
            for tok in tokens {
                let fg = tok
                    .get("settings")
                    .and_then(|s| s.get("foreground"))
                    .and_then(|f| f.as_str());
                let Some(fg) = fg else { continue };
                let scopes = match tok.get("scope") {
                    Some(serde_json::Value::String(s)) => s.split(',').map(|x| x.trim().to_string()).collect::<Vec<_>>(),
                    Some(serde_json::Value::Array(a)) => a.iter().filter_map(|x| x.as_str().map(String::from)).collect(),
                    _ => continue,
                };
                if scopes.iter().any(|s| scope_match(s)) {
                    if let Some((r, g, b, _)) = parse_hex(fg) {
                        *dst = Color::rgb(r, g, b);
                        return;
                    }
                }
            }
        };
        syn(&|s| s.starts_with("comment"), &mut t.syn_comment);
        syn(&|s| s.starts_with("string"), &mut t.syn_string);
        syn(&|s| s == "keyword.control" || s.starts_with("keyword.control"), &mut t.syn_keyword_ctrl);
        syn(&|s| s == "keyword" || s.starts_with("keyword") || s.starts_with("storage"), &mut t.syn_keyword);
        syn(&|s| s.contains("function") || s.contains("entity.name.function"), &mut t.syn_function);
        syn(&|s| s.contains("entity.name.type") || s.contains("support.type") || s.contains("entity.name.class"), &mut t.syn_type);
        syn(&|s| s.starts_with("constant.numeric"), &mut t.syn_number);
        syn(&|s| s.starts_with("constant"), &mut t.syn_constant);
        syn(&|s| s.starts_with("variable"), &mut t.syn_variable);
    }
    Some(t)
}

/// Strip `//` line and `/* */` block comments (VSCode themes are JSONC).
fn strip_jsonc(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut chars = s.chars().peekable();
    let mut in_str = false;
    while let Some(c) = chars.next() {
        if in_str {
            out.push(c);
            if c == '\\' {
                if let Some(n) = chars.next() {
                    out.push(n);
                }
            } else if c == '"' {
                in_str = false;
            }
            continue;
        }
        match c {
            '"' => {
                in_str = true;
                out.push(c);
            }
            '/' if chars.peek() == Some(&'/') => {
                for n in chars.by_ref() {
                    if n == '\n' {
                        out.push('\n');
                        break;
                    }
                }
            }
            '/' if chars.peek() == Some(&'*') => {
                chars.next();
                let mut prev = ' ';
                for n in chars.by_ref() {
                    if prev == '*' && n == '/' {
                        break;
                    }
                    prev = n;
                }
            }
            _ => out.push(c),
        }
    }
    out
}

// ---- Color accessors (read the active theme). Named to match call sites. ----
pub fn BG_EDITOR() -> wgpu::Color { current().read().unwrap().bg_editor }
pub fn FG_TEXT() -> Color { current().read().unwrap().fg_text }
pub fn SYN_COMMENT() -> Color { current().read().unwrap().syn_comment }
pub fn SYN_STRING() -> Color { current().read().unwrap().syn_string }
pub fn SYN_KEYWORD() -> Color { current().read().unwrap().syn_keyword }
pub fn SYN_KEYWORD_CTRL() -> Color { current().read().unwrap().syn_keyword_ctrl }
pub fn SYN_FUNCTION() -> Color { current().read().unwrap().syn_function }
pub fn SYN_TYPE() -> Color { current().read().unwrap().syn_type }
pub fn SYN_NUMBER() -> Color { current().read().unwrap().syn_number }
pub fn SYN_CONSTANT() -> Color { current().read().unwrap().syn_constant }
pub fn SYN_VARIABLE() -> Color { current().read().unwrap().syn_variable }
pub fn SYN_LABEL() -> Color { current().read().unwrap().syn_label }
pub fn MD_HEADING() -> Color { current().read().unwrap().md_heading }
pub fn MD_QUOTE() -> Color { current().read().unwrap().md_quote }
pub fn MD_CODE() -> Color { current().read().unwrap().md_code }
pub fn MD_RULE() -> Color { current().read().unwrap().md_rule }
pub fn MD_LIST() -> Color { current().read().unwrap().md_list }
pub fn FG_DIM() -> Color { current().read().unwrap().fg_dim }
pub fn FG_ACTIVE() -> Color { current().read().unwrap().fg_active }
pub fn FG_GUTTER() -> Color { current().read().unwrap().fg_gutter }
pub fn FG_GUTTER_ACTIVE() -> Color { current().read().unwrap().fg_gutter_active }
pub fn CURSOR() -> [f32; 4] { current().read().unwrap().cursor }
pub fn SCROLLBAR_THUMB() -> [f32; 4] { current().read().unwrap().scrollbar_thumb }
pub fn SCROLLBAR_THUMB_HOVER() -> [f32; 4] { current().read().unwrap().scrollbar_thumb_hover }
pub fn SELECTION() -> [f32; 4] { current().read().unwrap().selection }
pub fn LINE_HIGHLIGHT() -> [f32; 4] { current().read().unwrap().line_highlight }
pub fn FIND_MATCH() -> [f32; 4] { current().read().unwrap().find_match }
pub fn ACTIVITY_BAR_BG() -> [f32; 4] { current().read().unwrap().activity_bar_bg }
pub fn ACTIVITY_BAR_ACTIVE() -> [f32; 4] { current().read().unwrap().activity_bar_active }
pub fn SIDEBAR_BG() -> [f32; 4] { current().read().unwrap().sidebar_bg }
pub fn PANEL_BG() -> [f32; 4] { current().read().unwrap().panel_bg }
pub fn PANEL_BORDER() -> [f32; 4] { current().read().unwrap().panel_border }
pub fn TAB_BAR_BG() -> [f32; 4] { current().read().unwrap().tab_bar_bg }
pub fn TAB_INACTIVE() -> [f32; 4] { current().read().unwrap().tab_inactive }
pub fn TAB_ACTIVE() -> [f32; 4] { current().read().unwrap().tab_active }
pub fn TAB_HOVER() -> [f32; 4] { current().read().unwrap().tab_hover }
pub fn TAB_FG_ACTIVE() -> Color { current().read().unwrap().tab_fg_active }
pub fn TAB_FG_INACTIVE() -> Color { current().read().unwrap().tab_fg_inactive }
pub fn CLOSE_FG() -> Color { current().read().unwrap().close_fg }
pub fn CLOSE_FG_HOVER() -> Color { current().read().unwrap().close_fg_hover }
pub fn STATUS_BAR_BG() -> [f32; 4] { current().read().unwrap().status_bar_bg }
pub fn STATUS_BAR_FG() -> Color { current().read().unwrap().status_bar_fg }
pub fn BORDER() -> [f32; 4] { current().read().unwrap().border }
pub fn TREE_HOVER() -> [f32; 4] { current().read().unwrap().tree_hover }
pub fn TREE_ACTIVE_FILE() -> [f32; 4] { current().read().unwrap().tree_active_file }
pub fn TREE_SELECTED() -> [f32; 4] { current().read().unwrap().tree_selected }
pub fn PALETTE_BG() -> [f32; 4] { current().read().unwrap().palette_bg }
pub fn PALETTE_BORDER() -> [f32; 4] { current().read().unwrap().palette_border }
pub fn PALETTE_INPUT_BG() -> [f32; 4] { current().read().unwrap().palette_input_bg }
pub fn PALETTE_SELECTED() -> [f32; 4] { current().read().unwrap().palette_selected }
pub fn DIALOG_BTN() -> [f32; 4] { current().read().unwrap().dialog_btn }
pub fn DIALOG_BTN_HOVER() -> [f32; 4] { current().read().unwrap().dialog_btn_hover }
pub fn DIALOG_OVERLAY() -> [f32; 4] { current().read().unwrap().dialog_overlay }
pub fn ACTIVITY_ICON_FG() -> Color { current().read().unwrap().activity_icon_fg }
pub fn ACTIVITY_ICON_ACTIVE() -> Color { current().read().unwrap().activity_icon_active }
pub fn ICON_FOLDER_COLOR() -> Color { current().read().unwrap().icon_folder_color }
pub fn ICON_FILE_COLOR() -> Color { current().read().unwrap().icon_file_color }
// Diff view line backgrounds + hunk header (fixed; VSCode-like, not yet themeable).
pub fn DIFF_ADD_BG() -> [f32; 4] { [0.18, 0.43, 0.24, 0.30] }
pub fn DIFF_DEL_BG() -> [f32; 4] { [0.50, 0.18, 0.18, 0.30] }
pub fn DIFF_HUNK_BG() -> [f32; 4] { [0.20, 0.30, 0.42, 0.28] }
pub fn DIFF_FILLER_BG() -> [f32; 4] { [0.0, 0.0, 0.0, 0.22] } // "no line here" on a side
// Activity-bar count badge (e.g. Source Control changed-file count).
pub fn BADGE_BG() -> [f32; 4] { [0.0, 0.48, 0.80, 1.0] }
pub fn BADGE_FG() -> Color { Color::rgb(0xFF, 0xFF, 0xFF) }
pub fn DIFF_HUNK_FG() -> Color { Color::rgb(0x56, 0x9C, 0xD6) }
pub fn TITLE_BAR_BG() -> [f32; 4] { current().read().unwrap().title_bar_bg }
pub fn TITLE_FG() -> Color { current().read().unwrap().title_fg }
pub fn SEARCH_BG() -> [f32; 4] { current().read().unwrap().search_bg }
pub fn SEARCH_BG_HOVER() -> [f32; 4] { current().read().unwrap().search_bg_hover }
pub fn SEARCH_BORDER() -> [f32; 4] { current().read().unwrap().search_border }
pub fn TITLE_CLOSE_HOVER() -> [f32; 4] { current().read().unwrap().title_close_hover }
pub fn TITLE_BTN_HOVER() -> [f32; 4] { current().read().unwrap().title_btn_hover }
pub fn MENU_HOVER() -> [f32; 4] { current().read().unwrap().menu_hover }
pub fn CONTEXT_BG() -> [f32; 4] { current().read().unwrap().context_bg }
pub fn CONTEXT_BORDER() -> [f32; 4] { current().read().unwrap().context_border }
pub fn CONTEXT_SEL() -> [f32; 4] { current().read().unwrap().context_sel }

// File-type icon colours (Seti-ish), keyed by extension. (Not themed yet.)
pub fn file_icon_color(name: &str) -> Color {
    let ext = name.rsplit('.').next().unwrap_or("");
    match ext {
        "rs" => Color::rgb(0xDE, 0xA5, 0x84),
        "md" => Color::rgb(0x51, 0x9A, 0xBA),
        "toml" | "lock" => Color::rgb(0x9C, 0x9C, 0x9C),
        "json" | "mcp" => Color::rgb(0xCB, 0xCB, 0x41),
        "wgsl" => Color::rgb(0x8F, 0xBC, 0x8F),
        "txt" => Color::rgb(0xB0, 0xB4, 0xBA),
        "png" | "jpg" | "ico" => Color::rgb(0xA0, 0x74, 0xC4),
        _ => Color::rgb(0xC5, 0xC5, 0xC5),
    }
}

// ---- Dimensions / fonts / glyphs ----
//
// SIZE dimensions are runtime functions multiplied by a global UI zoom (VSCode's
// window.zoomLevel) so the whole window scales crisply. Glyph codepoints, font
// families, and timing (ms) stay compile-time const.

/// Global UI zoom factor (1.0 = 100%). Scales every dimension + font size.
fn zoom_cell() -> &'static RwLock<f32> {
    static Z: OnceLock<RwLock<f32>> = OnceLock::new();
    Z.get_or_init(|| RwLock::new(1.0))
}
pub fn ui_zoom() -> f32 {
    *zoom_cell().read().unwrap()
}
/// Scale a raw pixel value by the current UI zoom. Use this for any layout
/// offset/size that should grow with zoom instead of hardcoding a literal.
pub fn zpx(px: f32) -> f32 {
    px * ui_zoom()
}
pub fn set_ui_zoom(z: f32) {
    *zoom_cell().write().unwrap() = z.clamp(0.5, 3.0);
    bump_shape_epoch();
}

// Bumped whenever a change requires every cached text buffer to re-shape (zoom).
// Widgets compare their stored epoch and re-shape (with fresh metrics) when stale.
fn epoch_cell() -> &'static std::sync::atomic::AtomicU64 {
    static E: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    &E
}
pub fn shape_epoch() -> u64 {
    epoch_cell().load(std::sync::atomic::Ordering::Relaxed)
}
fn bump_shape_epoch() {
    epoch_cell().fetch_add(1, std::sync::atomic::Ordering::Relaxed);
}

pub fn SCROLLBAR_WIDTH() -> f32 { 14.0 * ui_zoom() }
pub fn SEARCH_ROW_H() -> f32 { UI_LINE_HEIGHT() } // find-in-files result row height
// Auto-hide overlay scrollbars: held fully visible for HOLD ms after the last
// scroll/hover, then fade to invisible over FADE ms. (Timing, not scaled.)
pub const SCROLLBAR_FADE_HOLD_MS: f32 = 900.0;
pub const SCROLLBAR_FADE_MS: f32 = 300.0;
pub fn DIALOG_BTN_H() -> f32 { 30.0 * ui_zoom() }

// Diagnostic underline colors (quad RGBA), by LSP severity.
pub fn DIAGNOSTIC_ERROR() -> [f32; 4] { [0.94, 0.30, 0.30, 1.0] }
pub fn DIAGNOSTIC_WARNING() -> [f32; 4] { [0.85, 0.65, 0.13, 1.0] }
pub fn DIAGNOSTIC_INFO() -> [f32; 4] { [0.27, 0.52, 0.82, 1.0] }

// Editor font metrics are runtime (driven by `editor.fontSize` / `editor.lineHeight`).
#[allow(non_snake_case)]
pub fn FONT_SIZE() -> f32 { crate::settings::font_size() * ui_zoom() }
#[allow(non_snake_case)]
pub fn LINE_HEIGHT() -> f32 { crate::settings::line_height() * ui_zoom() }
pub fn UI_FONT_SIZE() -> f32 { 13.0 * ui_zoom() }
pub fn UI_LINE_HEIGHT() -> f32 { 22.0 * ui_zoom() }
pub fn ICON_SIZE() -> f32 { 16.0 * ui_zoom() }
pub fn ACTIVITY_ICON_SIZE() -> f32 { 22.0 * ui_zoom() }
pub fn ACTIVITY_CELL() -> f32 { 48.0 * ui_zoom() }

// Editor mono family is runtime (driven by `editor.fontFamily`).
#[allow(non_snake_case)]
pub fn MONO_FAMILY() -> &'static str { crate::settings::mono_family() }
// UI (chrome) family is runtime (driven by `workbench.fontFamily`).
#[allow(non_snake_case)]
pub fn UI_FAMILY() -> &'static str { crate::settings::ui_family() }
// VSCode's own icon font (Codicon, MIT). Bundled at renderer/assets/codicon.ttf
// and loaded into the FontSystem at startup; its internal family name is "codicon".
pub const ICON_FAMILY: &str = "codicon";

// Activity-bar / chrome glyphs — exact Codicon codepoints (match VSCode 1:1).
pub const ICON_FILES: char = '\u{eaf0}';
pub const ICON_SEARCH: char = '\u{ea6d}';
pub const ICON_SOURCE_CONTROL: char = '\u{ea68}';
pub const ICON_RUN: char = '\u{eb91}';
pub const ICON_EXTENSIONS: char = '\u{eae6}';
pub const ICON_CLOSE: char = '\u{ea76}';
pub const ICON_ACCOUNT: char = '\u{eb99}';
pub const ICON_SETTINGS: char = '\u{eb51}';
pub const ICON_CHEVRON_DOWN: char = '\u{eab4}';
pub const ICON_CHEVRON_RIGHT: char = '\u{eab6}';
pub const ICON_CHEVRON_UP: char = '\u{eab7}';
pub const ICON_ADD: char = '\u{ea60}';
pub const ICON_REMOVE: char = '\u{eb3b}'; // codicon "remove" (−), used to unstage
pub const ICON_DISCARD: char = '\u{eae2}'; // codicon "discard" (revert/undo arrow)
pub const ICON_SPLIT_HORIZONTAL: char = '\u{eb56}';
pub const ICON_TRASH: char = '\u{ea81}';
pub const ICON_ELLIPSIS: char = '\u{ea7c}';
pub const ICON_NEW_FILE: char = '\u{ea7f}';
pub const ICON_NEW_FOLDER: char = '\u{ea80}';
pub const ICON_REFRESH: char = '\u{eb37}';
pub const ICON_COLLAPSE_ALL: char = '\u{eac5}';
pub const ICON_LAYOUT_SIDEBAR_LEFT: char = '\u{ebf3}';
pub const ICON_LAYOUT_PANEL: char = '\u{ebf2}';
pub const ICON_LAYOUT_SIDEBAR_RIGHT: char = '\u{ebf4}';

// File-tree glyphs — Codicon codepoints.
pub const ICON_FOLDER_CLOSED: char = '\u{ea83}';
pub const ICON_FOLDER_OPEN: char = '\u{eaf7}';
pub const ICON_FILE: char = '\u{ea7b}';

pub const BLINK_MS: u64 = 530;

pub fn TITLE_BAR_H() -> f32 { 30.0 * ui_zoom() }
pub fn MENU_ITEM_H() -> f32 { 22.0 * ui_zoom() } // = UI_LINE_HEIGHT so rows align with items
// Window controls — Codicon chrome-* glyphs (rendered in ICON_FAMILY).
pub const ICON_MIN: char = '\u{eaba}';
pub const ICON_MAX: char = '\u{eab9}';
pub const ICON_RESTORE: char = '\u{eabb}';
pub const ICON_WIN_CLOSE: char = '\u{eab8}';
pub fn TITLE_BTN_W() -> f32 { 46.0 * ui_zoom() }

pub fn ACTIVITY_BAR_WIDTH() -> f32 { 48.0 * ui_zoom() }
pub fn SIDEBAR_WIDTH() -> f32 { 240.0 * ui_zoom() }
pub fn SIDEBAR_MIN_WIDTH() -> f32 { 120.0 * ui_zoom() }
pub fn SIDEBAR_MAX_WIDTH() -> f32 { 600.0 * ui_zoom() }
pub fn SIDEBAR_RESIZE_HANDLE() -> f32 { 6.0 * ui_zoom() }
pub fn TAB_HEIGHT() -> f32 { 34.0 * ui_zoom() }
pub fn STATUS_BAR_HEIGHT() -> f32 { 22.0 * ui_zoom() }
pub fn GUTTER_WIDTH() -> f32 { 56.0 * ui_zoom() }
pub fn EDITOR_PAD() -> f32 { 12.0 * ui_zoom() }
pub fn CURSOR_WIDTH() -> f32 { 2.0 * ui_zoom() }
pub fn TREE_INDENT() -> f32 { 16.0 * ui_zoom() }
pub fn TREE_ROW_HEIGHT() -> f32 { 22.0 * ui_zoom() }
pub fn SIDEBAR_HEADER_H() -> f32 { 30.0 * ui_zoom() }
pub fn TAB_MIN_WIDTH() -> f32 { 120.0 * ui_zoom() }
pub fn TAB_MAX_WIDTH() -> f32 { 220.0 * ui_zoom() }
pub fn FIND_BAR_HEIGHT() -> f32 { 36.0 * ui_zoom() }
pub fn TERMINAL_HEIGHT() -> f32 { 240.0 * ui_zoom() } // initial/default panel height
pub fn TERMINAL_MIN_HEIGHT() -> f32 { 80.0 * ui_zoom() }
pub fn TERMINAL_MAX_HEIGHT() -> f32 { 700.0 * ui_zoom() }
pub fn TERMINAL_HEADER_H() -> f32 { 32.0 * ui_zoom() } // panel header (tabs + buttons)

/// Bottom-panel tabs (VSCode layout). Index 3 (TERMINAL) is the active stub.
pub const PANEL_TABS: &[&str] = &["PROBLEMS", "OUTPUT", "DEBUG CONSOLE", "TERMINAL", "PORTS"];
pub const PANEL_ACTIVE_TAB: usize = 3;
pub fn PALETTE_WIDTH() -> f32 { 560.0 * ui_zoom() }
pub fn PALETTE_ROW_HEIGHT() -> f32 { 24.0 * ui_zoom() }
pub fn PALETTE_INPUT_HEIGHT() -> f32 { 30.0 * ui_zoom() }
