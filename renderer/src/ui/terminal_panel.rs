// Integrated terminal panel state. Each group is a tab (`+` adds one); within a
// tab, panes are shown side-by-side (split). Only the active group is visible; the
// rest keep running in the background. No shell is ever discarded.
//
// NOTE (refactor staging): this groups the terminal's *state* in one place
// (`self.terminal.*`). The pane/tab/split logic still lives on `App` and the pane
// glyph buffers (`gpu.ui.terminal_panes`/`term_tablist`) + the draw still live in
// `gpu`/`render.rs`, since they need direct `gpu` access. Moving that in is a
// follow-up.

use std::collections::VecDeque;
use std::path::PathBuf;

use crate::layout::Layout;
use crate::ptyhost::client::{Client, Incoming};
use crate::terminal;
use crate::theme;
use crate::widgets::{Axis, Rect, Splitter};
use crate::{
    terminal_content, terminal_grid_size, terminal_header_button_rects, terminal_pane_area,
    terminal_pane_rects, terminal_tab_close_rect, terminal_tablist_rect,
};

pub struct TerminalPanel {
    pub groups: Vec<terminal::Group>,
    pub active: usize,         // active tab (group) index
    pub visible: bool,
    pub focused: bool,
    pub split: Splitter,       // draggable panel height
    pub maximized: bool,       // header maximize toggle (fills the content area)
    /// Workspace root new shells start in (like VSCode). The panel owns this so
    /// spawning doesn't have to thread it through every call; `App` keeps it in
    /// sync via `set_cwd` whenever the workspace root changes.
    cwd: PathBuf,
    /// Connection to the pty-host daemon (lazily established on first terminal use,
    /// spawning the daemon if needed). The daemon owns the shells so they survive a
    /// GUI restart.
    client: Option<Client>,
    /// Local tags for shells whose `Created` reply hasn't arrived yet (FIFO; the
    /// daemon replies in request order on one connection).
    pending: VecDeque<u64>,
    next_tag: u64,
}

impl TerminalPanel {
    pub fn new(cwd: PathBuf) -> Self {
        Self {
            groups: Vec::new(),
            active: 0,
            visible: false,
            focused: false,
            split: Splitter::new(
                theme::TERMINAL_HEIGHT(),
                theme::TERMINAL_MIN_HEIGHT(),
                theme::TERMINAL_MAX_HEIGHT(),
                Axis::Vertical,
            ),
            maximized: false,
            cwd,
            client: None,
            pending: VecDeque::new(),
            next_tag: 1,
        }
    }

    /// Ensure we're connected to the daemon. Returns the daemon's existing terminals
    /// the first time it connects (to re-attach), else an empty list.
    fn ensure_connected(&mut self) -> Vec<crate::ptyhost::TermInfo> {
        if self.client.is_some() {
            return Vec::new();
        }
        match Client::connect_or_spawn() {
            Some((client, terminals)) => {
                self.client = Some(client);
                terminals
            }
            None => Vec::new(),
        }
    }

    fn term_by_id(&mut self, id: crate::ptyhost::TermId) -> Option<&mut terminal::Pane> {
        self.groups.iter_mut().flat_map(|g| g.panes.iter_mut()).find(|p| p.term.id == id)
    }

    fn term_by_tag(&mut self, tag: u64) -> Option<&mut terminal::Pane> {
        self.groups.iter_mut().flat_map(|g| g.panes.iter_mut()).find(|p| p.term.tag == tag)
    }

    /// Drain daemon frames: bind newly-created shells, feed output into grids, and
    /// drop panes whose shell exited. Returns true if anything changed (needs redraw).
    pub fn poll(&mut self) -> bool {
        let Some(client) = self.client.as_ref() else {
            return false;
        };
        let incoming = client.poll();
        if incoming.is_empty() {
            return false;
        }
        let mut exited = Vec::new();
        for inc in incoming {
            match inc {
                Incoming::Created { id, title } => {
                    if let Some(tag) = self.pending.pop_front() {
                        if let Some(p) = self.term_by_tag(tag) {
                            p.term.bind(id, title);
                        }
                    }
                }
                Incoming::Backlog { id, data } | Incoming::Output { id, data } => {
                    if let Some(p) = self.term_by_id(id) {
                        p.term.feed(&data);
                        p.dirty = true;
                    }
                }
                Incoming::Exited { id } => exited.push(id),
            }
        }
        for id in exited {
            self.remove_pane_by_id(id);
        }
        true
    }

    /// Remove the pane (and its now-empty group) whose shell exited, like VSCode.
    fn remove_pane_by_id(&mut self, id: crate::ptyhost::TermId) {
        for gi in 0..self.groups.len() {
            if let Some(pi) = self.groups[gi].panes.iter().position(|p| p.term.id == id) {
                self.groups[gi].panes.remove(pi);
                let g = &mut self.groups[gi];
                if !g.panes.is_empty() {
                    g.focused = g.focused.min(g.panes.len() - 1);
                }
                break;
            }
        }
        self.groups.retain(|g| !g.panes.is_empty());
        if self.groups.is_empty() {
            self.visible = false;
            self.focused = false;
            self.maximized = false;
        } else {
            self.active = self.active.min(self.groups.len() - 1);
        }
        self.mark_dirty();
    }

    /// Update the directory new shells will start in (called on Open Folder).
    pub fn set_cwd(&mut self, cwd: PathBuf) {
        self.cwd = cwd;
    }

    /// Requested panel height: huge when maximized (the layout clamps it to leave a
    /// sliver of editor), the splitter size otherwise, None when hidden.
    pub fn panel_height(&self) -> Option<f32> {
        if !self.visible {
            return None;
        }
        Some(if self.maximized { 100_000.0 } else { self.split.size() })
    }

    /// Number of split panes in the active tab (0 when there's no terminal).
    pub fn active_pane_count(&self) -> usize {
        self.groups.get(self.active).map_or(0, |g| g.panes.len())
    }

    /// Spawn a pane sized to fit when the active tab shows `count` side-by-side
    /// panes, with the shell starting in `cwd` (the workspace root).
    fn spawn_pane(&mut self, count: usize, panel: Option<Rect>, cell_w: f32) -> Option<terminal::Pane> {
        self.ensure_connected();
        let client = self.client.as_ref()?;
        let panel = panel?;
        let area = terminal_pane_area(terminal_content(panel), self.groups.len().max(1));
        let rect = terminal_pane_rects(area, count.max(1))
            .into_iter()
            .next()
            .unwrap_or(area);
        let (rows, cols) = terminal_grid_size(rect, cell_w);
        // Ask the daemon to spawn a shell; bind its id when `Created` arrives (poll).
        let tag = self.next_tag;
        self.next_tag += 1;
        client.create(&self.cwd.to_string_lossy(), rows as u16, cols as u16);
        self.pending.push_back(tag);
        let conn = client.conn();
        Some(terminal::Pane::wrap(terminal::Terminal::new_unbound(conn, tag, rows, cols)))
    }

    /// Header `+`: open a new terminal tab (a fresh group). The previous tab keeps
    /// running in the background and stays reachable from the tab list.
    pub fn new_terminal_tab(&mut self, panel: Option<Rect>, cell_w: f32) {
        if let Some(p) = self.spawn_pane(1, panel, cell_w) {
            self.groups.push(terminal::Group::new(p));
            self.active = self.groups.len() - 1;
            self.focused = true;
            self.mark_dirty(); // tab list appearing reflows pane widths
        }
    }

    /// Header split: add a side-by-side pane to the active tab.
    pub fn split_terminal(&mut self, panel: Option<Rect>, cell_w: f32) {
        let count = self.active_pane_count() + 1;
        if let Some(p) = self.spawn_pane(count, panel, cell_w) {
            if let Some(g) = self.groups.get_mut(self.active) {
                g.panes.push(p);
                g.focused = g.panes.len() - 1;
                self.focused = true;
            }
            self.mark_dirty();
        }
    }

    /// Header trash: kill the focused pane; drop the tab if it was its last pane;
    /// hide the panel if that was the last tab.
    pub fn kill_terminal(&mut self) {
        let Some(g) = self.groups.get_mut(self.active) else {
            return;
        };
        if g.panes.is_empty() {
            return;
        }
        let i = g.focused.min(g.panes.len() - 1);
        let id = g.panes[i].term.id;
        g.panes.remove(i);
        if let Some(c) = self.client.as_ref() {
            c.close(id);
        }
        let g = self.groups.get_mut(self.active).expect("active group still valid");
        if g.panes.is_empty() {
            self.groups.remove(self.active);
            if self.groups.is_empty() {
                self.visible = false;
                self.focused = false;
                self.maximized = false;
            } else {
                self.active = self.active.min(self.groups.len() - 1);
            }
        } else {
            g.focused = i.min(g.panes.len() - 1);
        }
        self.mark_dirty();
    }

    /// Switch the visible terminal tab.
    pub fn switch_tab(&mut self, i: usize) {
        if i < self.groups.len() {
            self.active = i;
            self.focused = true;
            self.mark_dirty();
        }
    }

    /// Tab-list × button: kill an entire tab (all its panes); hide the panel if it
    /// was the last tab.
    pub fn kill_tab(&mut self, i: usize) {
        if i >= self.groups.len() {
            return;
        }
        let removed = self.groups.remove(i);
        if let Some(c) = self.client.as_ref() {
            for p in &removed.panes {
                c.close(p.term.id);
            }
        }
        if self.groups.is_empty() {
            self.visible = false;
            self.focused = false;
            self.maximized = false;
        } else {
            self.active = self.active.min(self.groups.len() - 1);
        }
        self.mark_dirty();
    }

    /// Header maximize: grow the panel to fill the whole content area (toggle).
    pub fn toggle_max(&mut self) {
        self.maximized = !self.maximized;
        self.mark_dirty();
    }

    /// Mark every pane in every tab as needing a reshape (after a layout change).
    pub fn mark_dirty(&mut self) {
        for g in &mut self.groups {
            for p in &mut g.panes {
                p.dirty = true;
            }
        }
    }

    /// Show/hide the integrated terminal. Returns true if a first tab must be
    /// spawned (caller computes the panel rect *after* this flips `visible`, since
    /// the panel only has a height once visible — then calls `spawn_initial`).
    pub fn toggle(&mut self) -> bool {
        self.visible = !self.visible;
        self.focused = self.visible;
        self.visible && self.groups.is_empty()
    }

    /// On first open: re-attach to any shells the daemon kept alive from a previous
    /// session (one tab each), else spawn a fresh first tab.
    pub fn spawn_initial(&mut self, panel: Option<Rect>, cell_w: f32) {
        let existing = self.ensure_connected();
        if !existing.is_empty() {
            let Some(panel) = panel else { return };
            if let Some(client) = self.client.as_ref() {
                // Size each re-attached terminal to a single full-width pane.
                let area = terminal_pane_area(terminal_content(panel), existing.len().max(1));
                let rect = terminal_pane_rects(area, 1).into_iter().next().unwrap_or(area);
                let (rows, cols) = terminal_grid_size(rect, cell_w);
                for info in existing {
                    let term = terminal::Terminal::new_bound(client.conn(), info.id, rows, cols, info.title);
                    client.attach(info.id);
                    self.groups.push(terminal::Group::new(terminal::Pane::wrap(term)));
                }
                self.active = 0;
                self.mark_dirty();
            }
            return;
        }
        if let Some(p) = self.spawn_pane(1, panel, cell_w) {
            self.groups.push(terminal::Group::new(p));
            self.active = 0;
        }
    }

    // ---- Input (the panel owns its region's press/scroll/drag/hover) ----

    /// A pane scrollbar thumb/track press (overlay, claimed before region handlers).
    pub fn pane_scroll_press(&mut self, pt: (f32, f32)) -> bool {
        if !self.visible {
            return false;
        }
        if let Some(g) = self.groups.get_mut(self.active) {
            for i in 0..g.panes.len() {
                if g.panes[i].scroll.press(pt) {
                    g.panes[i].dirty = true;
                    g.focused = i;
                    return true;
                }
            }
        }
        false
    }

    /// Press in the terminal content/header: tab list (× kills / row switches), pane
    /// focus, or a header icon-button action. Returns true if consumed. Clicking
    /// outside the panel while visible just drops focus (not consumed).
    pub fn content_press(&mut self, pt: (f32, f32), layout: &Layout, cell_w: f32, clicks: u32) -> bool {
        if !self.visible {
            return false;
        }
        let Some(panel) = layout.terminal_panel else { return false };
        let content = terminal_content(panel);
        if content.contains(pt) {
            // The right-side tab list: × kills that tab, the row body switches.
            if let Some(tl) = terminal_tablist_rect(content, self.groups.len()) {
                if tl.contains(pt) {
                    let idx = ((pt.1 - tl.y) / theme::TREE_ROW_HEIGHT()) as usize;
                    if idx < self.groups.len() {
                        if terminal_tab_close_rect(tl, idx).contains(pt) {
                            self.kill_tab(idx);
                        } else {
                            self.switch_tab(idx);
                        }
                    }
                    return true;
                }
            }
            // Otherwise focus whichever split pane was clicked, and begin a text
            // selection at the clicked cell.
            let area = terminal_pane_area(content, self.groups.len());
            let rects = terminal_pane_rects(area, self.active_pane_count());
            if let Some(i) = rects.iter().position(|r| r.contains(pt)) {
                if let Some(g) = self.groups.get_mut(self.active) {
                    g.focused = i;
                    if let Some(pane) = g.panes.get_mut(i) {
                        let (line, col) = Self::cell_at(rects[i], pt, cell_w, pane);
                        match clicks {
                            n if n >= 3 => {
                                // Triple-click: select the whole line.
                                let end = pane.term.line_chars(line).len();
                                pane.sel = Some(((line, 0), (line, end)));
                                pane.sel_dragging = false;
                            }
                            2 => {
                                // Double-click: select the word (run of non-whitespace).
                                let chars = pane.term.line_chars(line);
                                let (s, e) = word_bounds(&chars, col);
                                pane.sel = Some(((line, s), (line, e)));
                                pane.sel_dragging = false;
                            }
                            _ => {
                                pane.sel = Some(((line, col), (line, col)));
                                pane.sel_dragging = true;
                            }
                        }
                        pane.dirty = true;
                    }
                }
            }
            self.focused = true;
            return true;
        }
        // Header strip (above content): right-side icon buttons.
        if panel.contains(pt) {
            let btns = terminal_header_button_rects(panel);
            if let Some(i) = btns.iter().position(|r| r.contains(pt)) {
                match i {
                    0 => self.new_terminal_tab(Some(panel), cell_w), // + new tab
                    1 => self.split_terminal(Some(panel), cell_w),   // ⊟ split active tab
                    2 => self.kill_terminal(),                       // 🗑 kill focused pane
                    4 => self.toggle_max(),                          // ⌃ maximize/restore
                    5 => {
                        self.toggle(); // × hide panel (groups exist, so no spawn)
                    }
                    _ => {} // 3 more — menu infra TBD
                }
            }
            return true;
        }
        self.focused = false; // clicked elsewhere while visible
        false
    }

    /// Map a point inside pane `rect` to a `(line, col)` in that pane's combined
    /// buffer. Mirrors the cell geometry used by the renderer (8px/4px insets).
    fn cell_at(rect: Rect, pt: (f32, f32), cell_w: f32, pane: &terminal::Pane) -> (usize, usize) {
        let line_h = theme::LINE_HEIGHT();
        let x0 = rect.x + theme::zpx(8.0);
        let y0 = rect.y + theme::zpx(4.0);
        let col = ((pt.0 - x0) / cell_w).floor().max(0.0) as usize;
        let vis_row = ((pt.1 - y0) / line_h).floor().max(0.0) as usize;
        let (cols, _) = pane.term.dims();
        let top_line = (pane.scroll.offset().1 / line_h).round() as usize;
        let total = pane.term.total_lines();
        let line = (top_line + vis_row).min(total.saturating_sub(1));
        (line, col.min(cols))
    }

    /// Continue a text selection drag in whichever pane is selecting. Returns true if
    /// a selection drag was active (so the caller redraws).
    pub fn selection_drag(&mut self, pt: (f32, f32), layout: &Layout, cell_w: f32) -> bool {
        if !self.visible {
            return false;
        }
        let Some(panel) = layout.terminal_panel else { return false };
        let content = terminal_content(panel);
        let area = terminal_pane_area(content, self.groups.len());
        let rects = terminal_pane_rects(area, self.active_pane_count());
        // Find the dragging pane and the rect it lives in.
        let mut target: Option<usize> = None;
        if let Some(g) = self.groups.get(self.active) {
            target = g.panes.iter().position(|p| p.sel_dragging);
        }
        let Some(i) = target else { return false };
        let rect = rects.get(i).copied().unwrap_or(content);
        // Clamp the point into the pane so dragging past an edge selects to it.
        let clamped = (
            pt.0.clamp(rect.x, rect.x + rect.w - 1.0),
            pt.1.clamp(rect.y, rect.y + rect.h - 1.0),
        );
        if let Some(g) = self.groups.get_mut(self.active) {
            if let Some(p) = g.panes.get_mut(i) {
                let head = Self::cell_at(rect, clamped, cell_w, p);
                if let Some((_, h)) = p.sel.as_mut() {
                    if *h != head {
                        *h = head;
                        p.dirty = true;
                    }
                }
            }
        }
        true
    }

    /// End any in-progress selection drag; drop a zero-width selection (plain click).
    pub fn selection_release(&mut self) {
        for g in &mut self.groups {
            for p in &mut g.panes {
                if p.sel_dragging {
                    p.sel_dragging = false;
                    if let Some((a, b)) = p.sel {
                        if a == b {
                            p.sel = None;
                        }
                    }
                    p.dirty = true;
                }
            }
        }
    }

    /// Select the entire scrollback + screen of the focused pane. Returns true if a
    /// selection was made (so the caller redraws).
    pub fn select_all(&mut self) -> bool {
        if let Some(g) = self.groups.get_mut(self.active) {
            let f = g.focused;
            if let Some(p) = g.panes.get_mut(f) {
                let total = p.term.total_lines();
                if total == 0 {
                    return false;
                }
                let last = total - 1;
                let last_len = p.term.line_chars(last).len();
                p.sel = Some(((0, 0), (last, last_len)));
                p.sel_dragging = false;
                p.dirty = true;
                return true;
            }
        }
        false
    }

    /// The focused pane's selected text, if any.
    pub fn selection_text(&self) -> Option<String> {
        let g = self.groups.get(self.active)?;
        g.panes.get(g.focused).and_then(|p| p.selection_text())
    }

    /// Clear the focused pane's selection (e.g. on keyboard input). Returns true if
    /// there was one (so the caller redraws).
    pub fn clear_focused_selection(&mut self) -> bool {
        if let Some(g) = self.groups.get_mut(self.active) {
            let f = g.focused;
            if let Some(p) = g.panes.get_mut(f) {
                if p.clear_selection() {
                    p.dirty = true;
                    return true;
                }
            }
        }
        false
    }

    /// Mouse wheel over a terminal pane → scroll its scrollback. Returns true if
    /// consumed (cursor was over the terminal content).
    pub fn on_scroll(&mut self, pt: (f32, f32), layout: &Layout, dy: f32) -> bool {
        if !self.visible {
            return false;
        }
        let Some(panel) = layout.terminal_panel else { return false };
        let content = terminal_content(panel);
        if !content.contains(pt) {
            return false;
        }
        let area = terminal_pane_area(content, self.groups.len());
        let rects = terminal_pane_rects(area, self.active_pane_count());
        if let Some(i) = rects.iter().position(|r| r.contains(pt)) {
            if let Some(g) = self.groups.get_mut(self.active) {
                let pane = &mut g.panes[i];
                if pane.term.is_alt() {
                    // A full-screen app owns scrolling — forward the wheel to it
                    // (a few notches per tick) instead of Aether's empty scrollback.
                    let up = dy > 0.0;
                    for _ in 0..3 {
                        pane.term.forward_wheel(up, 1, 1);
                    }
                } else if pane.scroll.on_wheel(0.0, dy) {
                    pane.dirty = true;
                }
            }
        }
        true
    }

    /// Continue a pane scrollbar drag. Returns true if a drag was active.
    pub fn pane_scroll_drag(&mut self, pt: (f32, f32)) -> bool {
        if let Some(g) = self.groups.get_mut(self.active) {
            if let Some(p) = g.panes.iter_mut().find(|p| p.scroll.is_dragging()) {
                if p.scroll.drag(pt) {
                    p.dirty = true;
                }
                return true;
            }
        }
        false
    }

    /// Release any in-progress pane scrollbar drags.
    pub fn release_scrolls(&mut self) {
        for g in &mut self.groups {
            for p in &mut g.panes {
                p.scroll.release();
            }
        }
    }

    /// Drive each visible pane's scrollbar hover (auto-hide fade) and report whether
    /// the pointer is over a thumb. Returns (redraw_needed, over_scroll_thumb).
    pub fn hover_panes(&mut self, p: (f32, f32), layout: &Layout) -> (bool, bool) {
        let mut changed = false;
        let mut over_thumb = false;
        if self.visible {
            if let Some(panel) = layout.terminal_panel {
                let area = terminal_pane_area(terminal_content(panel), self.groups.len());
                let rects = terminal_pane_rects(area, self.active_pane_count());
                if let Some(g) = self.groups.get_mut(self.active) {
                    for (i, pane) in g.panes.iter_mut().enumerate() {
                        let inside = rects.get(i).map_or(false, |r| r.contains(p));
                        if pane.scroll.hover(inside) {
                            changed = true;
                        }
                        if inside && pane.scroll.cursor(p).is_some() {
                            over_thumb = true;
                        }
                    }
                }
            }
        }
        (changed, over_thumb)
    }
}

/// Word boundaries (start, end-exclusive) around `col` in `chars`: the run of
/// non-whitespace under the cursor. On a space (or past the end) selects just that
/// cell. Treating any non-space as a word keeps paths/URLs/flags selectable whole.
fn word_bounds(chars: &[char], col: usize) -> (usize, usize) {
    if col >= chars.len() || chars[col].is_whitespace() {
        return (col, col + 1);
    }
    let mut s = col;
    while s > 0 && !chars[s - 1].is_whitespace() {
        s -= 1;
    }
    let mut e = col + 1;
    while e < chars.len() && !chars[e].is_whitespace() {
        e += 1;
    }
    (s, e)
}
