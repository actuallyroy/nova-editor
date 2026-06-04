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
    pub bracket_match_bg: [f32; 4],     // fill behind a matched bracket pair
    pub bracket_match_border: [f32; 4], // 1px box around each bracket of the pair
    pub activity_bar_bg: [f32; 4],
    pub activity_bar_active: [f32; 4],
    pub activity_active_border: [f32; 4], // accent stripe on the active view's icon
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
            bg_editor: wgpu::Color { r: 0.063, g: 0.071, b: 0.090, a: 1.0 },
            fg_text: Color::rgb(0xCD, 0xD3, 0xE0),
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
            // ---- Aether dark: cool blue-gray surfaces with an indigo accent. ----
            // Surface elevation: editor (0.063) < panels/tabs < sidebar/title <
            // floating cards. Accent ≈ #6E8CFF used for selection/active states.
            fg_dim: Color::rgb(0x79, 0x82, 0x96),
            fg_active: Color::rgb(0xFF, 0xFF, 0xFF),
            fg_gutter: Color::rgb(0x6B, 0x74, 0x8C),
            fg_gutter_active: Color::rgb(0xC6, 0xCC, 0xDA),
            cursor: [0.55, 0.66, 1.0, 1.0],
            scrollbar_thumb: [0.62, 0.68, 0.85, 0.18],
            scrollbar_thumb_hover: [0.62, 0.68, 0.85, 0.38],
            selection: [0.43, 0.55, 1.0, 0.22],
            line_highlight: [1.0, 1.0, 1.0, 0.035],
            find_match: [0.85, 0.62, 0.20, 0.40],
            // VSCode Dark+ editorBracketMatch: faint green fill, grey box.
            bracket_match_bg: [0.0, 0.39, 0.0, 0.10],
            bracket_match_border: [0.53, 0.53, 0.53, 1.0],
            activity_bar_bg: [0.075, 0.082, 0.106, 1.0],
            activity_bar_active: [1.0, 1.0, 1.0, 0.07],
            activity_active_border: [0.43, 0.55, 1.0, 1.0],
            sidebar_bg: [0.086, 0.094, 0.118, 1.0],
            // Raised vs the editor so the terminal reads as its own surface, with a
            // soft translucent divider on top.
            panel_bg: [0.075, 0.082, 0.106, 1.0],
            panel_border: [1.0, 1.0, 1.0, 0.08],
            tab_bar_bg: [0.071, 0.078, 0.100, 1.0],
            tab_inactive: [0.071, 0.078, 0.100, 1.0],
            tab_active: [0.063, 0.071, 0.090, 1.0],
            tab_hover: [1.0, 1.0, 1.0, 0.05],
            tab_fg_active: Color::rgb(0xFF, 0xFF, 0xFF),
            tab_fg_inactive: Color::rgb(0x9A, 0xA2, 0xB4),
            close_fg: Color::rgb(0x9A, 0xA2, 0xB4),
            close_fg_hover: Color::rgb(0xFF, 0xFF, 0xFF),
            status_bar_bg: [0.086, 0.094, 0.118, 1.0],
            status_bar_fg: Color::rgb(0xC6, 0xCC, 0xDA),
            border: [1.0, 1.0, 1.0, 0.07],
            tree_hover: [1.0, 1.0, 1.0, 0.045],
            tree_active_file: [0.43, 0.55, 1.0, 0.12],
            tree_selected: [0.43, 0.55, 1.0, 0.20],
            palette_bg: [0.110, 0.122, 0.153, 1.0],
            palette_border: [0.30, 0.33, 0.42, 1.0],
            palette_input_bg: [0.063, 0.071, 0.094, 1.0],
            palette_selected: [0.43, 0.55, 1.0, 0.30],
            dialog_btn: [0.18, 0.20, 0.26, 1.0],
            dialog_btn_hover: [0.30, 0.42, 0.85, 1.0],
            dialog_overlay: [0.0, 0.0, 0.0, 0.5],
            activity_icon_fg: Color::rgb(0x9A, 0xA2, 0xB4),
            activity_icon_active: Color::rgb(0xFF, 0xFF, 0xFF),
            icon_folder_color: Color::rgb(0x7E, 0x9C, 0xF0),
            icon_file_color: Color::rgb(0xB8, 0xC0, 0xD2),
            title_bar_bg: [0.071, 0.078, 0.100, 1.0],
            title_fg: Color::rgb(0xC6, 0xCC, 0xDA),
            search_bg: [0.063, 0.071, 0.094, 1.0],
            search_bg_hover: [1.0, 1.0, 1.0, 0.06],
            search_border: [0.27, 0.30, 0.40, 1.0],
            title_close_hover: [0.78, 0.20, 0.24, 1.0],
            title_btn_hover: [1.0, 1.0, 1.0, 0.08],
            menu_hover: [0.43, 0.55, 1.0, 0.16],
            context_bg: [0.110, 0.122, 0.153, 1.0],
            context_border: [0.30, 0.33, 0.42, 1.0],
            context_sel: [0.43, 0.55, 1.0, 0.28],
        }
    }
}

fn current() -> &'static RwLock<Theme> {
    static T: OnceLock<RwLock<Theme>> = OnceLock::new();
    T.get_or_init(|| {
        let mut t = Theme::dark();
        ensure_legible(&mut t); // floor the default too (it never goes through set())
        RwLock::new(t)
    })
}

/// Replace the active theme (e.g. after loading a VSCode color theme).
pub fn set(mut theme: Theme) {
    // Guarantee legibility for EVERY theme — built-in or loaded — in one place, so no
    // dim/secondary foreground can slip through on any palette.
    ensure_legible(&mut theme);
    *current().write().unwrap() = theme;
    // Force every color-baking text widget (sidebar metadata, gutter, tab labels,
    // rich labels) to re-shape with the new palette — otherwise they keep the colors
    // they were first shaped with and the theme only half-applies.
    bump_shape_epoch();
}

/// Force a minimum contrast on every dim/secondary UI foreground against the surface
/// it's drawn on, blending toward the matching bright color when too faint. Runs for
/// all themes (including the built-in dark), so dim chrome text is never illegible.
/// Syntax/markdown colors are intentionally untouched — that's the theme's code palette.
fn ensure_legible(t: &mut Theme) {
    let eb = (
        (t.bg_editor.r * 255.0) as u8,
        (t.bg_editor.g * 255.0) as u8,
        (t.bg_editor.b * 255.0) as u8,
        1.0,
    );
    // fg_dim/fg_gutter appear on the editor AND the sidebar/panels — floor them against
    // the DARKEST of those surfaces so dim text (e.g. the SOURCE CONTROL / CHANGES
    // section headers, which live on the sidebar) is legible everywhere, not just the editor.
    let darkest = [eb, quad_tuple(t.sidebar_bg), quad_tuple(t.panel_bg)]
        .into_iter()
        .min_by(|a, b| luminance(*a).partial_cmp(&luminance(*b)).unwrap())
        .unwrap_or(eb);
    // Target ~7:1 (WCAG AAA) for secondary text — 4.5 left mid-greys like #858585
    // technically passing but reading as "dim". Higher floor = comfortably legible.
    t.fg_dim = legible(t.fg_dim, darkest, t.fg_text, 7.0);
    t.fg_active = legible(t.fg_active, eb, t.fg_text, 7.0);
    t.fg_gutter = legible(t.fg_gutter, darkest, t.fg_text, 6.0);
    t.close_fg = legible(t.close_fg, darkest, t.fg_text, 6.0);
    t.tab_fg_inactive = legible(t.tab_fg_inactive, quad_tuple(t.tab_bar_bg), t.tab_fg_active, 6.0);
    t.tab_fg_active = legible(t.tab_fg_active, quad_tuple(t.tab_active), t.fg_text, 7.0);
    t.activity_icon_fg = legible(t.activity_icon_fg, quad_tuple(t.activity_bar_bg), t.activity_icon_active, 6.0);
    t.status_bar_fg = legible(t.status_bar_fg, quad_tuple(t.status_bar_bg), t.fg_text, 6.0);
    t.title_fg = legible(t.title_fg, quad_tuple(t.title_bar_bg), t.fg_text, 6.0);
}

/// Blend `fg` toward `bg` by `t` (0 = fg, 1 = bg) — used to derive a dim foreground
/// that tracks the theme instead of falling back to the dark-theme grey.
fn blend(fg: (u8, u8, u8, f32), bg: (u8, u8, u8, f32), t: f32) -> Color {
    let mix = |a: u8, b: u8| ((a as f32) * (1.0 - t) + (b as f32) * t).round() as u8;
    Color::rgb(mix(fg.0, bg.0), mix(fg.1, bg.1), mix(fg.2, bg.2))
}

fn rgb_tuple(c: Color) -> (u8, u8, u8, f32) {
    (c.r(), c.g(), c.b(), 1.0)
}
fn quad_tuple(q: [f32; 4]) -> (u8, u8, u8, f32) {
    ((q[0] * 255.0) as u8, (q[1] * 255.0) as u8, (q[2] * 255.0) as u8, 1.0)
}
/// WCAG relative luminance of an sRGB color.
fn luminance((r, g, b, _): (u8, u8, u8, f32)) -> f32 {
    let lin = |c: u8| {
        let c = c as f32 / 255.0;
        if c <= 0.03928 { c / 12.92 } else { ((c + 0.055) / 1.055).powf(2.4) }
    };
    0.2126 * lin(r) + 0.7152 * lin(g) + 0.0722 * lin(b)
}
/// WCAG contrast ratio between two colors (1.0 = identical, 21 = black/white).
fn contrast_ratio(a: (u8, u8, u8, f32), b: (u8, u8, u8, f32)) -> f32 {
    let (la, lb) = (luminance(a), luminance(b));
    let (hi, lo) = if la > lb { (la, lb) } else { (lb, la) };
    (hi + 0.05) / (lo + 0.05)
}
/// Guarantee `fg` reads against `bg`: if its contrast is below `min`, blend it toward
/// `toward` (the matching bright foreground) just enough to clear the floor. This is
/// what keeps every theme's dim/inactive foregrounds (gutter, inactive tabs, icons,
/// labels) legible without per-element tuning — applied uniformly in `load_vscode`.
fn legible(fg: Color, bg: (u8, u8, u8, f32), toward: Color, min: f32) -> Color {
    let fg_t = rgb_tuple(fg);
    if contrast_ratio(fg_t, bg) >= min {
        return fg;
    }
    let to_t = rgb_tuple(toward);
    for step in 1..=10 {
        let c = blend(fg_t, to_t, step as f32 / 10.0);
        if contrast_ratio(rgb_tuple(c), bg) >= min {
            return c;
        }
    }
    toward
}

/// Perceived-luminance test for picking light-vs-dark-appropriate derivations.
fn is_light((r, g, b, _): (u8, u8, u8, f32)) -> bool {
    (0.299 * r as f32 + 0.587 * g as f32 + 0.114 * b as f32) > 140.0
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
        // First key present wins (VS Code falls back across related keys).
        let rgb_any = |keys: &[&str]| keys.iter().find_map(|k| rgb(k));

        // ---- Foundational fg/bg, used to derive anything the theme omits ----
        let editor_fg = rgb_any(&["editor.foreground", "foreground"]).unwrap_or((0xD4, 0xD4, 0xD4, 1.0));
        let editor_bg = rgb("editor.background").unwrap_or((0x14, 0x14, 0x14, 1.0));
        let to_color = |(r, g, b, _): (u8, u8, u8, f32)| Color::rgb(r, g, b);
        let to_quad = |(r, g, b, a): (u8, u8, u8, f32)| [r as f32 / 255.0, g as f32 / 255.0, b as f32 / 255.0, a];

        let mut col = |keys: &[&str], dst: &mut Color| {
            if let Some(c) = rgb_any(keys) {
                *dst = to_color(c);
            }
        };
        // Foreground text colors (the contrast-critical ones).
        col(&["editor.foreground", "foreground"], &mut t.fg_text);
        col(&["foreground", "editor.foreground"], &mut t.fg_active);
        col(&["sideBar.foreground", "foreground"], &mut t.fg_text); // tree/list text
        col(&["statusBar.foreground"], &mut t.status_bar_fg);
        col(&["titleBar.activeForeground", "foreground"], &mut t.title_fg);
        col(&["tab.activeForeground", "foreground"], &mut t.tab_fg_active);
        // Inactive tab label: theme's color blended 40% toward the active tab color so
        // it stays readable (Dracula's tab.inactiveForeground is very dim on its tab bar).
        let tab_active = rgb_any(&["tab.activeForeground", "foreground"]).unwrap_or(editor_fg);
        t.tab_fg_inactive = match rgb_any(&["tab.inactiveForeground", "descriptionForeground"]) {
            Some(c) => blend(c, tab_active, 0.40),
            None => blend(editor_fg, editor_bg, 0.40),
        };
        // Line numbers: theme's gutter color blended 35% toward the foreground so the
        // inactive numbers stay legible (Dracula's editorLineNumber.foreground is dim);
        // the active line uses fg_gutter_active (bright) below.
        t.fg_gutter = match rgb("editorLineNumber.foreground") {
            Some(c) => blend(c, editor_fg, 0.35),
            None => blend(editor_fg, editor_bg, 0.50),
        };
        col(&["editorLineNumber.activeForeground", "editor.foreground"], &mut t.fg_gutter_active);
        // Activity-bar icons: active is bright; inactive starts from the theme's
        // inactiveForeground but is blended 40% toward the active color so it stays
        // clearly legible (themes like Dracula make it very dim — ~2.6:1).
        let icon_active = rgb_any(&["activityBar.foreground", "icon.foreground", "foreground"]).unwrap_or(editor_fg);
        t.activity_icon_active = to_color(icon_active);
        t.activity_icon_fg = match rgb_any(&["activityBar.inactiveForeground", "icon.foreground", "descriptionForeground"]) {
            Some(inactive) => blend(inactive, icon_active, 0.40),
            None => blend(editor_fg, editor_bg, 0.45), // dim, but legible
        };
        col(&["tab.activeForeground"], &mut t.close_fg_hover);
        // Dim/secondary text: prefer the theme's descriptionForeground, else blend
        // the foreground ~45% toward the background so it's never the dark-theme grey.
        // Secondary text: prefer the theme's descriptionForeground, else blend the
        // foreground only ~28% toward bg (45% was too faint — labels vanished).
        t.fg_dim = rgb("descriptionForeground")
            .map(to_color)
            .unwrap_or_else(|| blend(editor_fg, editor_bg, 0.28));
        t.close_fg = t.fg_dim;

        let mut quad = |keys: &[&str], dst: &mut [f32; 4]| {
            if let Some(c) = rgb_any(keys) {
                *dst = to_quad(c);
            }
        };
        // Editor background drives the clear color + a faintly-raised panel default.
        let (br, bg_, bb, _) = editor_bg;
        t.bg_editor = wgpu::Color { r: br as f64 / 255.0, g: bg_ as f64 / 255.0, b: bb as f64 / 255.0, a: 1.0 };
        t.tab_active = [br as f32 / 255.0, bg_ as f32 / 255.0, bb as f32 / 255.0, 1.0];
        let raise = if is_light(editor_bg) { -0.03 } else { 0.02 };
        t.panel_bg = [
            (br as f32 / 255.0 + raise).clamp(0.0, 1.0),
            (bg_ as f32 / 255.0 + raise).clamp(0.0, 1.0),
            (bb as f32 / 255.0 + raise).clamp(0.0, 1.0),
            1.0,
        ];

        quad(&["panel.background"], &mut t.panel_bg);
        quad(&["panel.border", "editorGroup.border", "contrastBorder"], &mut t.panel_border);
        quad(&["activityBar.background"], &mut t.activity_bar_bg);
        quad(&["activityBar.activeBackground"], &mut t.activity_bar_active);
        // The active-view accent stripe: VS Code's activityBar.activeBorder (Dracula's
        // pink). Fall back to the active icon color so it's always visible.
        if let Some(c) = rgb("activityBar.activeBorder") {
            t.activity_active_border = to_quad(c);
        } else if let Some(c) = rgb_any(&["activityBar.foreground", "focusBorder", "foreground"]) {
            t.activity_active_border = to_quad(c);
        }
        quad(&["sideBar.background"], &mut t.sidebar_bg);
        quad(&["editorGroupHeader.tabsBackground", "editor.background"], &mut t.tab_bar_bg);
        quad(&["tab.inactiveBackground"], &mut t.tab_inactive);
        quad(&["tab.activeBackground"], &mut t.tab_active);
        quad(&["tab.hoverBackground", "list.hoverBackground"], &mut t.tab_hover);
        quad(&["statusBar.background"], &mut t.status_bar_bg);
        quad(&["titleBar.activeBackground"], &mut t.title_bar_bg);
        quad(&["editor.selectionBackground"], &mut t.selection);
        quad(&["editor.lineHighlightBackground"], &mut t.line_highlight);
        quad(&["editor.findMatchHighlightBackground", "editor.findMatchBackground"], &mut t.find_match);
        quad(&["editorBracketMatch.background"], &mut t.bracket_match_bg);
        quad(&["editorBracketMatch.border"], &mut t.bracket_match_border);
        quad(&["editorCursor.foreground", "editor.foreground"], &mut t.cursor);
        quad(&["scrollbarSlider.background"], &mut t.scrollbar_thumb);
        quad(&["scrollbarSlider.hoverBackground"], &mut t.scrollbar_thumb_hover);
        quad(&["list.hoverBackground"], &mut t.tree_hover);
        quad(&["list.inactiveSelectionBackground", "list.activeSelectionBackground"], &mut t.tree_active_file);
        quad(&["list.activeSelectionBackground"], &mut t.tree_selected);
        quad(&["editorWidget.border", "panel.border", "contrastBorder"], &mut t.border);
        // Quick pick / palette + dialogs.
        quad(&["quickInput.background", "editorWidget.background", "menu.background"], &mut t.palette_bg);
        quad(&["quickInput.border", "editorWidget.border", "focusBorder"], &mut t.palette_border);
        quad(&["input.background"], &mut t.palette_input_bg);
        quad(&["quickInputList.focusBackground", "list.activeSelectionBackground"], &mut t.palette_selected);
        quad(&["button.background", "list.activeSelectionBackground"], &mut t.dialog_btn);
        quad(&["button.hoverBackground", "button.background"], &mut t.dialog_btn_hover);
        // Header search box + menus.
        quad(&["input.background"], &mut t.search_bg);
        quad(&["input.border", "focusBorder"], &mut t.search_border);
        quad(&["menu.background", "editorWidget.background"], &mut t.context_bg);
        quad(&["menu.border", "editorWidget.border"], &mut t.context_border);
        quad(&["menu.selectionBackground", "list.activeSelectionBackground"], &mut t.context_sel);
        quad(&["menu.selectionBackground", "list.hoverBackground"], &mut t.menu_hover);
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
pub fn BRACKET_MATCH_BG() -> [f32; 4] { current().read().unwrap().bracket_match_bg }
pub fn BRACKET_MATCH_BORDER() -> [f32; 4] { current().read().unwrap().bracket_match_border }
pub fn ACTIVITY_BAR_BG() -> [f32; 4] { current().read().unwrap().activity_bar_bg }
pub fn ACTIVITY_BAR_ACTIVE() -> [f32; 4] { current().read().unwrap().activity_bar_active }
pub fn ACTIVITY_ACTIVE_BORDER() -> [f32; 4] { current().read().unwrap().activity_active_border }
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
/// The signature accent (opaque) — derived from the selection color so loaded
/// VSCode themes track their own accent. Used for primary buttons, focus borders,
/// badges, and the current-match marker.
pub fn ACCENT() -> [f32; 4] {
    let c = current().read().unwrap().palette_selected;
    [c[0], c[1], c[2], 1.0]
}
/// A dimmer accent for large filled surfaces (e.g. the Commit button) so white
/// text on top stays comfortable.
pub fn ACCENT_DIM() -> [f32; 4] {
    let c = current().read().unwrap().palette_selected;
    [c[0] * 0.72, c[1] * 0.72, c[2] * 0.86, 1.0]
}
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
/// Icon glyph + color for a file row, by well-known filename then extension. The
/// single source of truth for file icons everywhere (explorer, SCM, tabs). The
/// glyph is a Codicon category icon; the color carries the language/type signal.
pub fn file_icon(name: &str) -> (char, Color) {
    let lower = name.to_ascii_lowercase();
    let c = |r, g, b| Color::rgb(r, g, b);
    // Well-known whole filenames take priority over the extension.
    match lower.as_str() {
        "dockerfile" | ".dockerignore" => return (ICON_FILE_CODE, c(0x4F, 0x9E, 0xE0)),
        ".gitignore" | ".gitattributes" | ".gitmodules" | ".git" => return (ICON_GEAR_FILE, c(0xE0, 0x6C, 0x4E)),
        "makefile" | "cmakelists.txt" | ".editorconfig" => return (ICON_GEAR_FILE, c(0x9C, 0x9C, 0x9C)),
        "license" | "license.md" | "license.txt" | "copying" => return (ICON_KEY, c(0xD4, 0xB8, 0x6A)),
        _ => {}
    }
    let ext = lower.rsplit('.').next().unwrap_or("");
    match ext {
        "rs" => (ICON_FILE_CODE, c(0xDE, 0xA5, 0x84)),
        "ts" | "tsx" => (ICON_FILE_CODE, c(0x3F, 0x8E, 0xD0)),
        "js" | "jsx" | "mjs" | "cjs" => (ICON_FILE_CODE, c(0xE8, 0xD4, 0x4D)),
        "py" | "pyw" | "pyi" => (ICON_FILE_CODE, c(0x5A, 0x9F, 0xD4)),
        "go" => (ICON_FILE_CODE, c(0x4F, 0xC3, 0xE0)),
        "c" | "h" | "cpp" | "cc" | "cxx" | "hpp" | "hxx" => (ICON_FILE_CODE, c(0x6F, 0x9F, 0xD8)),
        "cs" => (ICON_FILE_CODE, c(0x6E, 0xC4, 0x8F)),
        "java" | "kt" | "kts" => (ICON_FILE_CODE, c(0xC9, 0x7B, 0x4E)),
        "rb" => (ICON_RUBY, c(0xCC, 0x42, 0x3A)),
        "php" => (ICON_FILE_CODE, c(0x8A, 0x8F, 0xD0)),
        "swift" => (ICON_FILE_CODE, c(0xE0, 0x6C, 0x4E)),
        "sh" | "bash" | "zsh" | "fish" | "bat" | "cmd" | "ps1" => (ICON_TERMINAL_FILE, c(0x89, 0xD1, 0x85)),
        "html" | "htm" | "xhtml" => (ICON_FILE_CODE, c(0xE3, 0x6C, 0x4E)),
        "css" | "scss" | "sass" | "less" => (ICON_FILE_CODE, c(0x42, 0xA5, 0xF5)),
        "vue" | "svelte" => (ICON_FILE_CODE, c(0x6B, 0xC4, 0x8F)),
        "wgsl" | "glsl" | "hlsl" | "shader" | "metal" => (ICON_FILE_CODE, c(0x8F, 0xBC, 0x8F)),
        "json" | "jsonc" | "json5" | "mcp" => (ICON_JSON, c(0xCB, 0xCB, 0x41)),
        "md" | "markdown" | "mdx" | "rst" => (ICON_MARKDOWN, c(0x51, 0x9A, 0xBA)),
        "toml" | "yaml" | "yml" | "ini" | "cfg" | "conf" | "env" | "properties" | "xml" => {
            (ICON_GEAR_FILE, c(0x9C, 0x9C, 0x9C))
        }
        "lock" => (ICON_LOCK_FILE, c(0x9C, 0x9C, 0x9C)),
        "png" | "jpg" | "jpeg" | "gif" | "ico" | "bmp" | "webp" | "svg" | "avif" => {
            (ICON_FILE_MEDIA, c(0xA0, 0x74, 0xC4))
        }
        "zip" | "tar" | "gz" | "tgz" | "rar" | "7z" | "xz" | "bz2" => (ICON_FILE_ZIP, c(0xB0, 0x90, 0x4A)),
        "pdf" => (ICON_FILE_PDF, c(0xD0, 0x65, 0x4E)),
        "db" | "sqlite" | "sqlite3" | "sql" => (ICON_DATABASE, c(0x6B, 0xA5, 0xC4)),
        "exe" | "dll" | "so" | "dylib" | "o" | "a" | "bin" | "wasm" | "class" => {
            (ICON_FILE_BINARY, c(0x9C, 0x9C, 0x9C))
        }
        "csv" | "tsv" => (ICON_FILE_TEXT, c(0x89, 0xD1, 0x85)),
        "txt" | "log" | "text" => (ICON_FILE_TEXT, c(0xB0, 0xB4, 0xBA)),
        _ => (ICON_FILE, c(0xC5, 0xC5, 0xC5)),
    }
}

/// Icon + color for a symbol-kind string (from the symbol extractor or LSP),
/// VSCode's scheme: methods purple, variables/fields blue, types orange, misc gray.
pub fn symbol_icon(kind: &str) -> (char, Color) {
    let purple = Color::rgb(0xB1, 0x80, 0xD7);
    let blue = Color::rgb(0x75, 0xBE, 0xFF);
    let orange = Color::rgb(0xEE, 0x9D, 0x28);
    let gray = Color::rgb(0xC1, 0xC5, 0xCE);
    match kind {
        "fn" | "func" | "function" | "def" | "method" => (ICON_SYM_METHOD, purple),
        "let" | "var" | "local" | "variable" => (ICON_SYM_VARIABLE, blue),
        "const" | "static" | "constant" => (ICON_SYM_CONSTANT, gray),
        "class" => (ICON_SYM_CLASS, orange),
        "struct" => (ICON_SYM_STRUCT, blue),
        "enum" => (ICON_SYM_ENUM, orange),
        "interface" | "trait" => (ICON_SYM_INTERFACE, blue),
        "field" | "property" | "prop" => (ICON_SYM_FIELD, blue),
        "type" | "typedef" => (ICON_SYM_CLASS, orange),
        "keyword" => (ICON_SYM_KEYWORD, gray),
        "snippet" => (ICON_SYM_SNIPPET, gray),
        _ => (ICON_SYM_MISC, gray),
    }
}

/// Folder glyph (open/closed) + a color tinted by well-known folder names. We have
/// only generic folder glyphs in codicon, so the name signal is carried by color.
pub fn folder_icon(name: &str, open: bool) -> (char, Color) {
    let g = if open { ICON_FOLDER_OPEN } else { ICON_FOLDER_CLOSED };
    let base = current().read().unwrap().icon_folder_color;
    let c = |r, gg, b| Color::rgb(r, gg, b);
    let col = match name.to_ascii_lowercase().as_str() {
        ".git" => c(0xE0, 0x6C, 0x4E),
        "node_modules" | "vendor" | "target" | "dist" | "build" | "out" | ".next" | "bin" | "obj" => {
            c(0x7A, 0x7E, 0x86)
        }
        ".github" | ".vscode" | ".idea" | ".cargo" => c(0x6B, 0x8A, 0xB0),
        "assets" | "images" | "img" | "media" | "public" | "static" => c(0xA0, 0x74, 0xC4),
        "test" | "tests" | "__tests__" | "spec" | "specs" => c(0x89, 0xD1, 0x85),
        _ => base,
    };
    (g, col)
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
pub const ICON_LIST_TREE: char = '\u{eb86}'; // codicon "list-tree" — view-as-tree toggle
pub const ICON_LIST_FLAT: char = '\u{eb84}'; // codicon "list-flat" — view-as-list toggle
pub const ICON_STASH: char = '\u{ec26}'; // codicon "git-stash"
pub const ICON_OPEN_CHANGES: char = '\u{eafd}'; // codicon "git-compare" — open diff
// Find/replace widget glyphs.
pub const ICON_ARROW_UP: char = '\u{eaa1}'; // previous match
pub const ICON_ARROW_DOWN: char = '\u{ea9a}'; // next match
pub const ICON_REPLACE: char = '\u{eb3d}'; // replace current
pub const ICON_REPLACE_ALL: char = '\u{eb3c}'; // replace all
pub const ICON_CASE: char = '\u{eab1}'; // match case (Aa)
pub const ICON_WORD: char = '\u{eb7e}'; // whole word (ab)
pub const ICON_REGEX: char = '\u{eb38}'; // use regex (.*)
pub const ICON_LAYOUT_SIDEBAR_LEFT: char = '\u{ebf3}';
pub const ICON_LAYOUT_PANEL: char = '\u{ebf2}';
pub const ICON_LAYOUT_SIDEBAR_RIGHT: char = '\u{ebf4}';

// File-tree glyphs — Codicon codepoints.
pub const ICON_FOLDER_CLOSED: char = '\u{ea83}';
pub const ICON_FOLDER_OPEN: char = '\u{eaf7}';
pub const ICON_FILE: char = '\u{ea7b}';
// File-type category glyphs (verified against the bundled codicon.ttf cmap). We
// don't ship per-language logos, so the glyph marks the broad category and the
// color carries the language signal (VSCode "Minimal" icon theme style).
pub const ICON_FILE_CODE: char = '\u{eae9}';
pub const ICON_FILE_MEDIA: char = '\u{eaea}';
pub const ICON_FILE_ZIP: char = '\u{eaef}';
pub const ICON_FILE_PDF: char = '\u{eaeb}';
pub const ICON_FILE_BINARY: char = '\u{eae8}';
pub const ICON_FILE_TEXT: char = '\u{ec5e}';
pub const ICON_JSON: char = '\u{eb0f}';
pub const ICON_MARKDOWN: char = '\u{eb1d}';
pub const ICON_GEAR_FILE: char = '\u{eaf8}';
pub const ICON_LOCK_FILE: char = '\u{ea75}';
pub const ICON_TERMINAL_FILE: char = '\u{ea85}';
pub const ICON_DATABASE: char = '\u{eace}';
pub const ICON_RUBY: char = '\u{eb48}';
pub const ICON_KEY: char = '\u{eb11}';
// Symbol-kind glyphs (verified codicon codepoints) — the `@` symbol palette and the
// completion popup show these instead of textual [const]/[let] tags.
pub const ICON_SYM_METHOD: char = '\u{ea8c}';
pub const ICON_SYM_VARIABLE: char = '\u{ea88}';
pub const ICON_SYM_CONSTANT: char = '\u{eb5d}';
pub const ICON_SYM_CLASS: char = '\u{eb5b}';
pub const ICON_SYM_STRUCT: char = '\u{ea91}';
pub const ICON_SYM_ENUM: char = '\u{ea95}';
pub const ICON_SYM_INTERFACE: char = '\u{eb61}';
pub const ICON_SYM_FIELD: char = '\u{eb5f}';
pub const ICON_SYM_KEYWORD: char = '\u{eb62}';
pub const ICON_SYM_SNIPPET: char = '\u{eb66}';
pub const ICON_SYM_MISC: char = '\u{eb63}';

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
pub fn TERMINAL_HEIGHT() -> f32 { 240.0 * ui_zoom() } // initial/default panel height
pub fn TERMINAL_MIN_HEIGHT() -> f32 { 80.0 * ui_zoom() }
pub fn TERMINAL_MAX_HEIGHT() -> f32 { 700.0 * ui_zoom() }
pub fn TERMINAL_HEADER_H() -> f32 { 32.0 * ui_zoom() } // panel header (tabs + buttons)

/// Bottom-panel tabs (VSCode layout). Index 3 (TERMINAL) is the active stub.
pub const PANEL_TABS: &[&str] = &["PROBLEMS", "OUTPUT", "DEBUG CONSOLE", "TERMINAL", "PORTS"];
pub const PANEL_ACTIVE_TAB: usize = 3;
pub fn PALETTE_WIDTH() -> f32 { 620.0 * ui_zoom() }
// Row height matches the UI line height so the selection pill aligns with the text.
pub fn PALETTE_ROW_HEIGHT() -> f32 { UI_LINE_HEIGHT() }
pub fn PALETTE_INPUT_HEIGHT() -> f32 { 40.0 * ui_zoom() }
