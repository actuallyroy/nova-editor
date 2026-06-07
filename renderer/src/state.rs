// Machine-managed session state, persisted to `~/.aether/state.json`.
//
// This is deliberately separate from `settings.json`: settings are a
// human-authored JSONC document (with comments + formatting we must not clobber),
// whereas this file is written by the app on every change. Mirrors VSCode's split
// between user `settings.json` and its internal `state` store.

use std::collections::HashMap;
use std::path::PathBuf;

use serde_json::{json, Value};

use crate::settings::config_dir;

/// Per-workspace window/layout snapshot, restored when that folder is reopened
/// so the editor comes back visually as the user left it (open files, which
/// panels were showing, the terminal height, the window size, …).
#[derive(Clone, Debug)]
pub struct Session {
    pub files: Vec<PathBuf>,        // open editor tabs, in order
    pub active: Option<usize>,      // active tab index into `files`
    pub sidebar_visible: bool,
    pub sidebar_view: String,       // "explorer" | "search" | "scm" | "debug" | "extensions"
    pub sidebar_width: f32,         // 0 ⇒ keep the default
    pub terminal_visible: bool,
    pub terminal_maximized: bool,
    pub terminal_height: f32,       // panel splitter size; 0 ⇒ default
    pub panel_tab: usize,           // active bottom-panel tab
    pub right_visible: bool,        // AI chat sidebar
    pub right_width: f32,           // 0 ⇒ default
    pub window: Option<(u32, u32)>, // physical inner size
}

impl Default for Session {
    fn default() -> Self {
        Self {
            files: Vec::new(),
            active: None,
            sidebar_visible: true,
            sidebar_view: "explorer".into(),
            sidebar_width: 0.0,
            terminal_visible: false,
            terminal_maximized: false,
            terminal_height: 0.0,
            panel_tab: 0,
            right_visible: false,
            right_width: 0.0,
            window: None,
        }
    }
}

/// Persisted session state restored on the next launch.
#[derive(Clone, Debug, Default)]
pub struct State {
    /// UI zoom level (theme::ui_zoom). `None` ⇒ never set, use the default 1.0.
    pub zoom: Option<f32>,
    /// The last workspace folder the user had open, reopened on launch.
    pub last_workspace: Option<PathBuf>,
    /// Recently-opened workspace folders, newest first (File > Open Recent).
    pub recent: Vec<PathBuf>,
    /// Source Control tree (true) vs flat-list (false) view, restored on launch.
    pub scm_tree_view: bool,
    /// Source Control GRAPH accordion expanded (true) vs collapsed (false). Defaults
    /// to collapsed so the CHANGES list isn't pushed up on launch.
    pub scm_graph_open: bool,
    /// Window/layout snapshot per workspace path (absolute).
    pub sessions: HashMap<String, Session>,
}

impl Session {
    fn to_json(&self) -> Value {
        json!({
            "files": self.files.iter().map(|p| p.to_string_lossy().to_string()).collect::<Vec<_>>(),
            "active": self.active,
            "sidebarVisible": self.sidebar_visible,
            "sidebarView": self.sidebar_view,
            "sidebarWidth": self.sidebar_width,
            "terminalVisible": self.terminal_visible,
            "terminalMaximized": self.terminal_maximized,
            "terminalHeight": self.terminal_height,
            "panelTab": self.panel_tab,
            "rightVisible": self.right_visible,
            "rightWidth": self.right_width,
            "window": self.window.map(|(w, h)| json!([w, h])),
        })
    }

    fn from_json(v: &Value) -> Session {
        let mut s = Session::default();
        if let Some(arr) = v.get("files").and_then(|f| f.as_array()) {
            s.files = arr.iter().filter_map(|e| e.as_str()).filter(|p| !p.is_empty()).map(PathBuf::from).collect();
        }
        s.active = v.get("active").and_then(|a| a.as_u64()).map(|a| a as usize);
        if let Some(b) = v.get("sidebarVisible").and_then(|b| b.as_bool()) { s.sidebar_visible = b; }
        if let Some(t) = v.get("sidebarView").and_then(|t| t.as_str()) { s.sidebar_view = t.to_string(); }
        if let Some(w) = v.get("sidebarWidth").and_then(|w| w.as_f64()) { s.sidebar_width = w as f32; }
        if let Some(b) = v.get("terminalVisible").and_then(|b| b.as_bool()) { s.terminal_visible = b; }
        if let Some(b) = v.get("terminalMaximized").and_then(|b| b.as_bool()) { s.terminal_maximized = b; }
        if let Some(h) = v.get("terminalHeight").and_then(|h| h.as_f64()) { s.terminal_height = h as f32; }
        if let Some(t) = v.get("panelTab").and_then(|t| t.as_u64()) { s.panel_tab = t as usize; }
        if let Some(b) = v.get("rightVisible").and_then(|b| b.as_bool()) { s.right_visible = b; }
        if let Some(w) = v.get("rightWidth").and_then(|w| w.as_f64()) { s.right_width = w as f32; }
        if let Some(arr) = v.get("window").and_then(|w| w.as_array()) {
            if let (Some(w), Some(h)) = (arr.first().and_then(|x| x.as_u64()), arr.get(1).and_then(|x| x.as_u64())) {
                s.window = Some((w as u32, h as u32));
            }
        }
        s
    }
}

fn state_path() -> Option<PathBuf> {
    config_dir().map(|d| d.join("state.json"))
}

fn breakpoints_path() -> Option<PathBuf> {
    config_dir().map(|d| d.join("breakpoints.json"))
}

/// Persist debug breakpoints as `{ "<abs path>": [1-based lines] }` (best-effort).
pub fn save_breakpoints(map: &[(String, Vec<i64>)]) {
    let Some(path) = breakpoints_path() else { return };
    let obj: serde_json::Map<String, Value> =
        map.iter().map(|(p, lines)| (p.clone(), json!(lines))).collect();
    let _ = std::fs::write(&path, serde_json::to_string_pretty(&Value::Object(obj)).unwrap_or_default());
}

/// Load persisted breakpoints (abs path → 1-based lines). Empty on any error.
pub fn load_breakpoints() -> Vec<(String, Vec<i64>)> {
    let Some(path) = breakpoints_path() else { return Vec::new() };
    let Ok(text) = std::fs::read_to_string(&path) else { return Vec::new() };
    let Ok(Value::Object(obj)) = serde_json::from_str::<Value>(&text) else { return Vec::new() };
    obj.into_iter()
        .map(|(k, v)| {
            let lines = v.as_array().map(|a| a.iter().filter_map(|x| x.as_i64()).collect()).unwrap_or_default();
            (k, lines)
        })
        .collect()
}

impl State {
    /// Load state from disk, falling back to defaults on any error (missing file,
    /// malformed JSON, …) — persisted state is best-effort and never fatal.
    pub fn load() -> State {
        let mut s = State::default();
        let Some(path) = state_path() else { return s };
        let Ok(text) = std::fs::read_to_string(&path) else { return s };
        let Ok(v) = serde_json::from_str::<Value>(&text) else { return s };
        if let Some(z) = v.get("zoom").and_then(|z| z.as_f64()) {
            s.zoom = Some((z as f32).clamp(0.5, 3.0));
        }
        if let Some(p) = v.get("lastWorkspace").and_then(|p| p.as_str()) {
            if !p.is_empty() {
                s.last_workspace = Some(PathBuf::from(p));
            }
        }
        if let Some(t) = v.get("scmTreeView").and_then(|t| t.as_bool()) {
            s.scm_tree_view = t;
        }
        if let Some(g) = v.get("scmGraphOpen").and_then(|g| g.as_bool()) {
            s.scm_graph_open = g;
        }
        if let Some(arr) = v.get("recentFolders").and_then(|r| r.as_array()) {
            s.recent = arr
                .iter()
                .filter_map(|e| e.as_str())
                .filter(|p| !p.is_empty())
                .map(PathBuf::from)
                .collect();
        }
        if let Some(obj) = v.get("sessions").and_then(|s| s.as_object()) {
            s.sessions = obj.iter().map(|(k, val)| (k.clone(), Session::from_json(val))).collect();
        }
        s
    }

    /// The saved session for `ws` (absolute workspace path), if any.
    pub fn session_for(&self, ws: &std::path::Path) -> Option<&Session> {
        self.sessions.get(&ws.to_string_lossy().to_string())
    }

    /// Record/replace the session for `ws` and persist immediately.
    pub fn save_session(ws: &std::path::Path, session: Session) {
        let mut st = State::load();
        st.sessions.insert(ws.to_string_lossy().to_string(), session);
        st.save();
    }

    /// Move `folder` to the front of the recent list (dedup, capped at 10).
    pub fn touch_recent(&mut self, folder: &PathBuf) {
        self.recent.retain(|p| p != folder);
        self.recent.insert(0, folder.clone());
        self.recent.truncate(10);
    }

    /// Write the current state to disk (best-effort — errors are ignored).
    pub fn save(&self) {
        let Some(path) = state_path() else { return };
        let doc = json!({
            "zoom": self.zoom.unwrap_or(1.0),
            "lastWorkspace": self.last_workspace
                .as_ref()
                .map(|p| p.to_string_lossy().to_string())
                .unwrap_or_default(),
            "recentFolders": self.recent
                .iter()
                .map(|p| p.to_string_lossy().to_string())
                .collect::<Vec<_>>(),
            "scmTreeView": self.scm_tree_view,
            "scmGraphOpen": self.scm_graph_open,
            "sessions": self.sessions.iter().map(|(k, v)| (k.clone(), v.to_json())).collect::<serde_json::Map<_, _>>(),
        });
        if let Ok(text) = serde_json::to_string_pretty(&doc) {
            let _ = std::fs::write(&path, text);
        }
    }
}
