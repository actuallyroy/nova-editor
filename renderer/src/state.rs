// Machine-managed session state, persisted to `~/.aether/state.json`.
//
// This is deliberately separate from `settings.json`: settings are a
// human-authored JSONC document (with comments + formatting we must not clobber),
// whereas this file is written by the app on every change. Mirrors VSCode's split
// between user `settings.json` and its internal `state` store.

use std::path::PathBuf;

use serde_json::{json, Value};

use crate::settings::config_dir;

/// Persisted session state restored on the next launch.
#[derive(Clone, Debug, Default)]
pub struct State {
    /// UI zoom level (theme::ui_zoom). `None` ⇒ never set, use the default 1.0.
    pub zoom: Option<f32>,
    /// The last workspace folder the user had open, reopened on launch.
    pub last_workspace: Option<PathBuf>,
    /// Recently-opened workspace folders, newest first (File > Open Recent).
    pub recent: Vec<PathBuf>,
}

fn state_path() -> Option<PathBuf> {
    config_dir().map(|d| d.join("state.json"))
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
        if let Some(arr) = v.get("recentFolders").and_then(|r| r.as_array()) {
            s.recent = arr
                .iter()
                .filter_map(|e| e.as_str())
                .filter(|p| !p.is_empty())
                .map(PathBuf::from)
                .collect();
        }
        s
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
        });
        if let Ok(text) = serde_json::to_string_pretty(&doc) {
            let _ = std::fs::write(&path, text);
        }
    }
}
