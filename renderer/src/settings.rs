// VSCode-style settings. Aether keeps a read-only Default Settings document (every
// setting + its default + a description, as JSONC) and a user settings.json that
// overrides it — both openable from the command palette, like VSCode's
// "Preferences: Open Settings (JSON)" / "Open Default Settings (JSON)".
//
// This module owns the files on disk and the default content; applying the
// settings to the running editor is a separate concern (not all are wired yet).

use std::path::PathBuf;
use std::sync::{OnceLock, RwLock};

use serde_json::Value;

/// The resolved settings (user values merged over defaults). Cheap to clone.
#[derive(Clone)]
pub struct Settings {
    pub editor_font_size: f32,
    pub editor_line_height: f32,
    pub editor_font_family: String,
    pub editor_tab_size: usize,
    pub editor_insert_spaces: bool,
    pub editor_word_wrap: bool,
    pub editor_cursor_blink: bool,
    pub editor_line_numbers: bool,
    pub editor_rulers: usize,
    pub editor_render_line_highlight: bool,
    pub files_auto_save: bool, // true = afterDelay
    pub files_eol: String,     // "\n" or "\r\n"
    pub files_trim_trailing: bool,
    pub workbench_color_theme: String,
    pub workbench_font_family: String,
    pub workbench_activitybar_visible: bool,
    pub workbench_sidebar_visible: bool,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            editor_font_size: 14.0,
            editor_line_height: 20.0,
            editor_font_family: "Cascadia Code".into(),
            editor_tab_size: 4,
            editor_insert_spaces: true,
            editor_word_wrap: false,
            editor_cursor_blink: true,
            editor_line_numbers: true,
            editor_rulers: 0,
            editor_render_line_highlight: true,
            files_auto_save: false,
            files_eol: "\n".into(),
            files_trim_trailing: false,
            workbench_color_theme: "Aether Dark".into(),
            workbench_font_family: "Segoe UI".into(),
            workbench_activitybar_visible: true,
            workbench_sidebar_visible: true,
        }
    }
}

impl Settings {
    /// Overlay any present keys from a parsed settings JSON object onto self.
    fn overlay(&mut self, v: &Value) {
        if let Some(n) = v["editor.fontSize"].as_f64() {
            self.editor_font_size = (n as f32).clamp(8.0, 48.0);
        }
        if let Some(n) = v["editor.lineHeight"].as_f64() {
            if n > 0.0 {
                self.editor_line_height = n as f32;
            }
        }
        if let Some(s) = v["editor.fontFamily"].as_str() {
            self.editor_font_family = s.to_string();
        }
        if let Some(n) = v["editor.tabSize"].as_u64() {
            self.editor_tab_size = (n as usize).clamp(1, 16);
        }
        if let Some(b) = v["editor.insertSpaces"].as_bool() {
            self.editor_insert_spaces = b;
        }
        if let Some(s) = v["editor.wordWrap"].as_str() {
            self.editor_word_wrap = s == "on";
        }
        if let Some(s) = v["editor.cursorBlinking"].as_str() {
            self.editor_cursor_blink = s != "solid";
        }
        if let Some(b) = v["editor.lineNumbers"].as_bool() {
            self.editor_line_numbers = b;
        }
        if let Some(n) = v["editor.rulers"].as_u64() {
            self.editor_rulers = n as usize;
        }
        if let Some(b) = v["editor.renderLineHighlight"].as_bool() {
            self.editor_render_line_highlight = b;
        }
        if let Some(s) = v["files.autoSave"].as_str() {
            self.files_auto_save = s == "afterDelay";
        }
        if let Some(s) = v["files.eol"].as_str() {
            if s == "\r\n" || s == "\n" {
                self.files_eol = s.to_string();
            }
        }
        if let Some(b) = v["files.trimTrailingWhitespace"].as_bool() {
            self.files_trim_trailing = b;
        }
        if let Some(s) = v["workbench.colorTheme"].as_str() {
            self.workbench_color_theme = s.to_string();
        }
        if let Some(s) = v["workbench.fontFamily"].as_str() {
            self.workbench_font_family = s.to_string();
        }
        if let Some(b) = v["workbench.activityBar.visible"].as_bool() {
            self.workbench_activitybar_visible = b;
        }
        if let Some(b) = v["workbench.sideBar.visible"].as_bool() {
            self.workbench_sidebar_visible = b;
        }
    }
}

fn store() -> &'static RwLock<Settings> {
    static S: OnceLock<RwLock<Settings>> = OnceLock::new();
    S.get_or_init(|| RwLock::new(Settings::default()))
}

/// The current resolved settings (a clone).
pub fn current() -> Settings {
    store().read().unwrap().clone()
}

// Cheap field accessors for hot paths (avoid cloning the whole struct).
pub fn font_size() -> f32 {
    store().read().unwrap().editor_font_size
}
pub fn line_height() -> f32 {
    let s = store().read().unwrap();
    // Auto-scale: if the configured line height is smaller than the font, derive a
    // proportional one (so a larger font isn't cramped). Set both explicitly to override.
    if s.editor_line_height >= s.editor_font_size {
        s.editor_line_height
    } else {
        (s.editor_font_size * 1.4).round()
    }
}

/// The editor mono font family as a `&'static str` (glyphon needs a stable
/// lifetime). The family string is leaked once per reload — bounded, since
/// settings reload only on startup / save.
fn mono_family_cell() -> &'static RwLock<&'static str> {
    static M: OnceLock<RwLock<&'static str>> = OnceLock::new();
    M.get_or_init(|| RwLock::new("Cascadia Code"))
}
pub fn mono_family() -> &'static str {
    *mono_family_cell().read().unwrap()
}
fn set_mono_family(name: &str) {
    let leaked: &'static str = Box::leak(name.to_string().into_boxed_str());
    *mono_family_cell().write().unwrap() = leaked;
}

/// The UI (chrome) font family as a `&'static str` (driven by `workbench.fontFamily`).
fn ui_family_cell() -> &'static RwLock<&'static str> {
    static M: OnceLock<RwLock<&'static str>> = OnceLock::new();
    M.get_or_init(|| RwLock::new("Segoe UI"))
}
pub fn ui_family() -> &'static str {
    *ui_family_cell().read().unwrap()
}
fn set_ui_family(name: &str) {
    let leaked: &'static str = Box::leak(name.to_string().into_boxed_str());
    *ui_family_cell().write().unwrap() = leaked;
}

// Cheap accessors for behavior settings (read on demand).
pub fn word_wrap() -> bool {
    store().read().unwrap().editor_word_wrap
}
pub fn line_numbers() -> bool {
    store().read().unwrap().editor_line_numbers
}
pub fn render_line_highlight() -> bool {
    store().read().unwrap().editor_render_line_highlight
}
pub fn rulers() -> usize {
    store().read().unwrap().editor_rulers
}
pub fn activitybar_visible() -> bool {
    store().read().unwrap().workbench_activitybar_visible
}
pub fn auto_save() -> bool {
    store().read().unwrap().files_auto_save
}
pub fn eol() -> String {
    store().read().unwrap().files_eol.clone()
}
pub fn trim_trailing() -> bool {
    store().read().unwrap().files_trim_trailing
}

/// Re-read the user settings file, merge over defaults, and store. Call at startup
/// and whenever settings.json is saved. Returns the resolved settings.
pub fn reload() -> Settings {
    let mut s = Settings::default();
    if let Some(path) = config_dir().map(|d| d.join("settings.json")) {
        if let Ok(text) = std::fs::read_to_string(&path) {
            if let Ok(v) = serde_json::from_str::<Value>(&strip_jsonc(&text)) {
                s.overlay(&v);
            }
        }
    }
    set_mono_family(&s.editor_font_family);
    set_ui_family(&s.workbench_font_family);
    *store().write().unwrap() = s.clone();
    s
}

/// True if `path` is the user settings file (so a save can trigger a reload).
/// Tolerant of path-form differences: matches `…/.aether/settings.json` by name.
pub fn is_user_settings(path: &std::path::Path) -> bool {
    let is_name = path.file_name().map(|n| n == "settings.json").unwrap_or(false);
    let in_config = path
        .parent()
        .and_then(|p| p.file_name())
        .map(|n| n == ".aether")
        .unwrap_or(false);
    is_name && in_config
}

/// Strip `//` line and `/* */` block comments from JSONC (string-aware).
fn strip_jsonc(src: &str) -> String {
    let b = src.as_bytes();
    let mut out = String::with_capacity(src.len());
    let mut i = 0;
    let mut in_str = false;
    while i < b.len() {
        let c = b[i];
        if in_str {
            out.push(c as char);
            if c == b'\\' && i + 1 < b.len() {
                out.push(b[i + 1] as char);
                i += 2;
                continue;
            }
            if c == b'"' {
                in_str = false;
            }
            i += 1;
        } else if c == b'"' {
            in_str = true;
            out.push('"');
            i += 1;
        } else if c == b'/' && i + 1 < b.len() && b[i + 1] == b'/' {
            while i < b.len() && b[i] != b'\n' {
                i += 1;
            }
        } else if c == b'/' && i + 1 < b.len() && b[i + 1] == b'*' {
            i += 2;
            while i + 1 < b.len() && !(b[i] == b'*' && b[i + 1] == b'/') {
                i += 1;
            }
            i += 2;
        } else {
            out.push(c as char);
            i += 1;
        }
    }
    out
}

/// `~/.aether`, created if missing. None if no home dir.
pub fn config_dir() -> Option<PathBuf> {
    let home = std::env::var_os("USERPROFILE").or_else(|| std::env::var_os("HOME"))?;
    let dir = PathBuf::from(home).join(".aether");
    std::fs::create_dir_all(&dir).ok()?;
    Some(dir)
}

/// One-time migration of the legacy `~/.nova` config dir (settings, installed
/// extensions, session state) to `~/.aether`. Runs at startup BEFORE anything
/// touches `config_dir()` (which would otherwise create an empty `~/.aether`).
/// No-op if `~/.aether` already exists or there's no legacy dir to move.
pub fn migrate_legacy_config_dir() {
    let Some(home) = std::env::var_os("USERPROFILE").or_else(|| std::env::var_os("HOME")) else {
        return;
    };
    let home = PathBuf::from(home);
    let legacy = home.join(".nova");
    let current = home.join(".aether");
    if current.exists() || !legacy.exists() {
        return;
    }
    // Prefer an atomic rename; if that fails (e.g. across filesystems) leave the
    // legacy dir in place — a fresh ~/.aether will be created on demand.
    let _ = std::fs::rename(&legacy, &current);
}

/// The default settings document (JSONC): every setting, its default, and a
/// one-line description. Mirrors VSCode's key namespace where it maps to Aether.
pub fn default_settings_jsonc() -> &'static str {
    r#"// Default Settings — Aether's built-in configuration and defaults.
// This file is a reference; override any of these in your user settings
// (Preferences: Open Settings (JSON)). Do not edit this file — changes here are
// not saved.
{
    // --- Editor ---

    // Controls the font size in pixels.
    "editor.fontSize": 14,

    // Controls the line height. Use 0 to compute from the font size.
    "editor.lineHeight": 20,

    // The monospace font family used in the editor.
    "editor.fontFamily": "Cascadia Code",

    // The number of spaces a tab is equal to.
    "editor.tabSize": 4,

    // Insert spaces when pressing Tab.
    "editor.insertSpaces": true,

    // Controls how lines should wrap: "off" or "on".
    "editor.wordWrap": "off",

    // Controls the cursor animation style: "blink" or "solid".
    "editor.cursorBlinking": "blink",

    // Render a vertical line after a certain number of monospace characters.
    // Use 0 to disable.
    "editor.rulers": 0,

    // Highlight the current line.
    "editor.renderLineHighlight": true,

    // Show line numbers in the gutter.
    "editor.lineNumbers": true,

    // --- Files ---

    // Controls auto save of editors: "off" or "afterDelay".
    "files.autoSave": "off",

    // Default end-of-line for NEW files: "\n" (LF) or "\r\n" (CRLF). Existing files
    // keep their own line ending (shown in the status bar) when saved.
    "files.eol": "\n",

    // Trim trailing whitespace on save.
    "files.trimTrailingWhitespace": false,

    // --- Workbench / appearance ---

    // The color theme. Install a theme extension to add more.
    "workbench.colorTheme": "Aether Dark",

    // The UI (chrome) font family.
    "workbench.fontFamily": "Segoe UI",

    // Show the activity bar on the left.
    "workbench.activityBar.visible": true,

    // Controls the visibility of the sidebar on startup.
    "workbench.sideBar.visible": true
}
"#
}

/// A fresh user settings.json template.
fn user_settings_template() -> &'static str {
    r#"{
    // Override default settings here. See the Default Settings document
    // (Preferences: Open Default Settings (JSON)) for all available keys.
    "editor.fontSize": 14
}
"#
}

/// Write the default settings to `~/.aether/defaultSettings.jsonc` (overwriting, so
/// it always reflects the current build) and return its path.
pub fn default_settings_path() -> Option<PathBuf> {
    let path = config_dir()?.join("defaultSettings.jsonc");
    std::fs::write(&path, default_settings_jsonc()).ok()?;
    Some(path)
}

/// Path to the user settings file, creating it with a template if absent.
pub fn user_settings_path() -> Option<PathBuf> {
    let path = config_dir()?.join("settings.json");
    if !path.exists() {
        std::fs::write(&path, user_settings_template()).ok()?;
    }
    Some(path)
}

/// Persist `workbench.colorTheme` into the user settings file, preserving comments
/// and other keys (line-based: rewrite the existing line, else insert one). Best-effort.
/// Persist `files.autoSave` into the user settings (File > Auto Save toggle) and
/// apply it to the live store.
pub fn set_auto_save(on: bool) {
    let value = if on { "afterDelay" } else { "off" };
    set_user_setting("files.autoSave", &format!("\"{value}\""));
    store().write().unwrap().files_auto_save = on;
}

/// Rewrite (or insert) one `"key": value` line in the user settings.json,
/// preserving the rest of the hand-authored document untouched.
fn set_user_setting(key: &str, value_json: &str) {
    let Some(path) = user_settings_path() else { return };
    let text = std::fs::read_to_string(&path).unwrap_or_default();
    let needle = format!("\"{key}\"");
    let mut found = false;
    let mut lines: Vec<String> = text
        .lines()
        .map(|l| {
            if l.contains(&needle) {
                found = true;
                let indent: String = l.chars().take_while(|c| c.is_whitespace()).collect();
                let comma = if l.trim_end().ends_with(',') { "," } else { "" };
                format!("{indent}\"{key}\": {value_json}{comma}")
            } else {
                l.to_string()
            }
        })
        .collect();
    if !found {
        if let Some(pos) = lines.iter().position(|l| l.contains('{')) {
            lines.insert(pos + 1, format!("  \"{key}\": {value_json},"));
        }
    }
    let _ = std::fs::write(&path, lines.join("\n"));
}

pub fn set_color_theme(label: &str) {
    let Some(path) = user_settings_path() else { return };
    let text = std::fs::read_to_string(&path).unwrap_or_default();
    let mut found = false;
    let mut lines: Vec<String> = text
        .lines()
        .map(|l| {
            if l.contains("\"workbench.colorTheme\"") {
                found = true;
                let indent: String = l.chars().take_while(|c| c.is_whitespace()).collect();
                let comma = if l.trim_end().ends_with(',') { "," } else { "" };
                format!("{indent}\"workbench.colorTheme\": \"{label}\"{comma}")
            } else {
                l.to_string()
            }
        })
        .collect();
    if !found {
        if let Some(pos) = lines.iter().position(|l| l.contains('{')) {
            lines.insert(pos + 1, format!("  \"workbench.colorTheme\": \"{label}\","));
        }
    }
    let _ = std::fs::write(&path, lines.join("\n"));
}
