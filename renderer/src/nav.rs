// Editor navigation history (Go > Back / Forward / Last Edit Location) — a
// VSCode-style jump list. Locations are recorded when the user *leaves* a spot
// via a jump (palette goto, tab switch, go-to-definition), then Back/Forward
// walk the trail. Documents are keyed by path (or tab name for untitled/diff
// tabs) so entries survive tab index churn.

use std::path::PathBuf;

use crate::workspace::Workspace;

#[derive(Clone, PartialEq, Debug)]
pub struct NavLoc {
    pub path: Option<PathBuf>,
    pub name: String, // resolves pathless tabs (Untitled / diff views)
    pub line: usize,
}

#[derive(Default)]
pub struct NavState {
    back: Vec<NavLoc>,
    fwd: Vec<NavLoc>,
    prev_active: Option<usize>,    // last seen active tab (tab-switch detection)
    edit_seen: Option<(usize, i32)>, // (active idx, doc version) at the last tick
    pub last_edit: Option<NavLoc>, // where the most recent edit happened
}

impl NavState {
    /// Location of the active document's caret.
    pub fn capture(ws: &Workspace) -> Option<NavLoc> {
        ws.active.and_then(|i| Self::capture_idx(ws, i))
    }

    fn capture_idx(ws: &Workspace, idx: usize) -> Option<NavLoc> {
        let d = ws.documents.get(idx)?;
        let line = d.rope.byte_to_line(d.caret_byte().min(d.rope.len_bytes()));
        Some(NavLoc { path: d.path.clone(), name: d.name.clone(), line })
    }

    /// Record the current location before an explicit jump (palette goto,
    /// go-to-definition). Any new navigation clears the forward trail.
    pub fn mark(&mut self, ws: &Workspace) {
        if let Some(loc) = Self::capture(ws) {
            if self.back.last() != Some(&loc) {
                self.back.push(loc);
                if self.back.len() > 100 {
                    self.back.remove(0);
                }
            }
            self.fwd.clear();
        }
    }

    /// Per-frame bookkeeping: a tab switch records where the old tab's caret was
    /// (so Back returns there); an edit updates Last Edit Location.
    pub fn tick(&mut self, ws: &Workspace) {
        if ws.active != self.prev_active {
            if let Some(pi) = self.prev_active {
                if let Some(loc) = Self::capture_idx(ws, pi) {
                    if self.back.last() != Some(&loc) {
                        self.back.push(loc);
                        if self.back.len() > 100 {
                            self.back.remove(0);
                        }
                    }
                    self.fwd.clear();
                }
            }
            self.prev_active = ws.active;
            self.edit_seen = None;
        }
        if let Some((i, d)) = ws.active.and_then(|i| ws.documents.get(i).map(|d| (i, d))) {
            match self.edit_seen {
                Some((si, sv)) if si == i => {
                    if d.version != sv {
                        self.last_edit = Self::capture(ws);
                        self.edit_seen = Some((i, d.version));
                    }
                }
                _ => self.edit_seen = Some((i, d.version)),
            }
        }
    }

    /// Resync after a programmatic tab switch (Back/Forward themselves) so the
    /// next `tick` doesn't re-record the move as a fresh jump.
    pub fn note_switch(&mut self, ws: &Workspace) {
        self.prev_active = ws.active;
        self.edit_seen = None;
    }

    /// Pop the previous location; the current one goes onto the forward trail.
    pub fn back(&mut self, ws: &Workspace) -> Option<NavLoc> {
        let target = self.back.pop()?;
        if let Some(cur) = Self::capture(ws) {
            self.fwd.push(cur);
        }
        Some(target)
    }

    /// Inverse of `back`.
    pub fn forward(&mut self, ws: &Workspace) -> Option<NavLoc> {
        let target = self.fwd.pop()?;
        if let Some(cur) = Self::capture(ws) {
            self.back.push(cur);
        }
        Some(target)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ws_with(names: &[&str], fs: &mut glyphon::FontSystem) -> Workspace {
        let mut ws = Workspace::new(std::env::temp_dir());
        for n in names {
            let mut d = crate::document::Document::new(None, format!("{n}\nline1\nline2\n"), fs);
            d.name = n.to_string();
            ws.documents.push(d);
        }
        ws.active = Some(0);
        ws
    }

    #[test]
    fn tab_switch_records_and_back_walks() {
        let mut fs = glyphon::FontSystem::new();
        let mut ws = ws_with(&["a", "b"], &mut fs);
        let mut nav = NavState::default();
        nav.tick(&ws); // baseline
        ws.active = Some(1);
        nav.tick(&ws); // switch recorded: back = [a@0]
        let t = nav.back(&ws).expect("has back target");
        assert_eq!(t.name, "a");
        // current (b) went to the forward trail
        let f = nav.forward(&ws).expect("has fwd target");
        assert_eq!(f.name, "b");
    }

    #[test]
    fn edits_update_last_edit_location() {
        let mut fs = glyphon::FontSystem::new();
        let mut ws = ws_with(&["a"], &mut fs);
        let mut nav = NavState::default();
        nav.tick(&ws);
        assert!(nav.last_edit.is_none());
        let line2 = ws.documents[0].rope.line_to_byte(2);
        ws.documents[0].place(line2, false);
        ws.documents[0].insert_str("x", &mut fs);
        nav.tick(&ws);
        let le = nav.last_edit.clone().expect("edit recorded");
        assert_eq!((le.name.as_str(), le.line), ("a", 2));
    }
}
