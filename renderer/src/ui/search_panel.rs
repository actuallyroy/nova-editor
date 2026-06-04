// Find-in-files (Search) view — a self-contained panel: owns its query/replace
// inputs, results list, option toggles, scrollbar, and all of its state, plus its
// own draw + input. The orchestrator only calls update/draw/on_* when the Search
// sidebar view is active. Cross-cutting actions (opening a match, reloading docs
// after Replace All) are returned as `Intent`s.

use std::collections::HashSet;
use std::sync::mpsc::Sender;
use std::time::Instant;
use std::path::PathBuf;

use glyphon::FontSystem;
use winit::keyboard::{Key, NamedKey};
use winit::window::CursorIcon;

use crate::marketplace::WorkerMsg;
use crate::quad::Quad;
use crate::search::{self, FileMatches, SearchOpts};
use crate::theme;
use crate::ui::Intent;
use crate::widgets::{IconButton, ListView, Rect, ScrollOpts, ScrollView, TextInput, TextLabel};
use arboard::Clipboard;

const OPT: f32 = 18.0; // option-toggle square size

pub struct SearchPanel {
    query: TextInput,
    replace: TextInput,
    list: ListView,
    opt_labels: [TextLabel; 3],
    chevrons: [IconButton; 2], // [expanded, collapsed]
    replace_all_label: TextLabel,
    summary: TextLabel, // "N results in M files"
    last_summary: String,
    scroll: ScrollView,
    opts: SearchOpts,
    results: Vec<FileMatches>,
    collapsed: HashSet<usize>,
    query_active: bool,
    replace_active: bool,
    dragging: Option<u8>, // 0 = query, 1 = replace (text drag-select)
    gen: u64,
    pending: bool,
}

impl SearchPanel {
    pub fn new(fs: &mut FontSystem) -> Self {
        let mk_label = |fs: &mut FontSystem, s: &str| {
            let mut l = TextLabel::new(fs, 40.0, OPT);
            l.set(fs, s, theme::UI_FAMILY());
            l
        };
        let mut query = TextInput::new(fs, theme::SIDEBAR_WIDTH(), 30.0);
        query.set_placeholder(fs, " Search");
        let mut replace = TextInput::new(fs, theme::SIDEBAR_WIDTH(), 30.0);
        replace.set_placeholder(fs, " Replace");
        let mut replace_all_label = TextLabel::new(fs, theme::SIDEBAR_WIDTH(), 24.0);
        replace_all_label.set(fs, "Replace All", theme::UI_FAMILY());
        let ic = theme::ICON_FAMILY;
        Self {
            query,
            replace,
            list: ListView::new(fs, theme::SIDEBAR_WIDTH(), 4000.0, theme::SEARCH_ROW_H(), 10.0),
            opt_labels: [
                mk_label(fs, "Aa"),
                mk_label(fs, "\\b"),
                mk_label(fs, ".*"),
            ],
            chevrons: [
                IconButton::new(fs, theme::ICON_CHEVRON_DOWN, ic, 16.0),
                IconButton::new(fs, theme::ICON_CHEVRON_RIGHT, ic, 16.0),
            ],
            replace_all_label,
            summary: TextLabel::new(fs, theme::SIDEBAR_WIDTH(), 22.0),
            last_summary: String::new(),
            scroll: ScrollView::new(ScrollOpts::vertical()),
            opts: SearchOpts::default(),
            results: Vec::new(),
            collapsed: HashSet::new(),
            query_active: false,
            replace_active: false,
            dragging: None,
            gen: 0,
            pending: bool::default(),
        }
    }

    // ---- Geometry (all derived from the sidebar tree region) ----
    fn query_rect(r: Rect) -> Rect {
        Rect { x: r.x + theme::zpx(10.0), y: r.y + theme::zpx(8.0), w: r.w - theme::zpx(20.0), h: theme::zpx(30.0) }
    }
    fn opt_rects(r: Rect) -> [Rect; 3] {
        let q = Self::query_rect(r);
        let opt = theme::zpx(OPT);
        let (gap, total) = (theme::zpx(3.0), 3.0 * opt + 2.0 * theme::zpx(3.0));
        let start = q.x + q.w - theme::zpx(6.0) - total;
        let y = q.y + (q.h - opt) * 0.5;
        std::array::from_fn(|i| Rect { x: start + i as f32 * (opt + gap), y, w: opt, h: opt })
    }
    fn replace_rect(r: Rect) -> Rect {
        let q = Self::query_rect(r);
        Rect { x: q.x, y: q.y + q.h + theme::zpx(6.0), w: q.w, h: theme::zpx(30.0) }
    }
    fn replace_all_rect(r: Rect) -> Rect {
        let rr = Self::replace_rect(r);
        Rect { x: rr.x, y: rr.y + rr.h + theme::zpx(6.0), w: rr.w, h: theme::zpx(24.0) }
    }
    fn summary_rect(r: Rect) -> Rect {
        let b = Self::replace_all_rect(r);
        Rect { x: r.x + theme::zpx(12.0), y: b.y + b.h + theme::zpx(8.0), w: r.w - theme::zpx(20.0), h: theme::zpx(20.0) }
    }
    fn results_region(r: Rect) -> Rect {
        let b = Self::summary_rect(r);
        let top = b.y + b.h + theme::zpx(2.0);
        Rect { x: r.x, y: top, w: r.w, h: (r.y + r.h - top).max(0.0) }
    }

    pub fn focused(&self) -> bool {
        self.query_active || self.replace_active
    }

    pub fn set_unfocused(&mut self) {
        self.query_active = false;
        self.replace_active = false;
    }

    /// Drop stale results + cancel any in-flight search (used when the workspace
    /// root changes via Open Folder).
    /// Expand the replace row and focus the query (Edit > Replace in Files).
    pub fn show_replace(&mut self) {
        self.replace_active = true;
    }

    pub fn reset(&mut self) {
        self.results.clear();
        self.collapsed.clear();
        self.scroll.scroll_to_y(0.0);
        self.gen += 1;
        self.pending = false;
        self.set_unfocused();
    }

    /// Worker results: append a streamed batch (ignored if from a stale search).
    pub fn ingest(&mut self, gen: u64, files: Vec<FileMatches>) {
        if gen == self.gen {
            self.results.extend(files);
        }
    }
    pub fn search_done(&mut self, gen: u64) {
        if gen == self.gen {
            self.pending = false;
        }
    }
    pub fn pending(&self) -> bool {
        self.pending
    }

    fn rows(&self) -> Vec<search::SearchRow> {
        search::build_rows(&self.results, &self.collapsed)
    }

    fn trigger_find(&mut self, root: PathBuf, tx: &Sender<WorkerMsg>) {
        let query = self.query.text().trim().to_string();
        self.results.clear();
        self.collapsed.clear();
        self.scroll.scroll_to_y(0.0);
        self.gen += 1;
        self.pending = false;
        if query.is_empty() {
            return;
        }
        self.pending = true;
        search::search_async(tx.clone(), self.gen, root, query, self.opts);
    }

    fn do_replace_all(&mut self, root: PathBuf, tx: &Sender<WorkerMsg>, out: &mut Vec<Intent>) {
        let query = self.query.text().trim().to_string();
        let replacement = self.replace.text().to_string();
        if query.is_empty() || self.results.is_empty() {
            return;
        }
        let n = search::replace_all(&self.results, &query, self.opts, &replacement);
        if n > 0 {
            out.push(Intent::ReloadOpenDocs);
            self.trigger_find(root, tx);
        }
    }

    /// Re-shape every owned buffer after a zoom change (inputs/labels/icons keep
    /// their content but need fresh metrics).
    pub fn rezoom(&mut self, fs: &mut FontSystem) {
        self.query.rezoom(fs);
        self.replace.rezoom(fs);
        for l in &mut self.opt_labels {
            l.reshape(fs);
        }
        for c in &mut self.chevrons {
            c.reshape(fs);
        }
        self.replace_all_label.reshape(fs);
        self.summary.reshape(fs);
    }

    // ---- Shaping ----
    pub fn update(&mut self, fs: &mut FontSystem, region: Rect) {
        let rows = self.rows();
        let key: String = rows.iter().map(|r| r.text.as_str()).collect::<Vec<_>>().join("\n");
        self.list.set_text(fs, &key, 4000.0, 12000.0);
        // Total-occurrence summary across all files (re-shaped only when it changes).
        let total: usize = self
            .results
            .iter()
            .map(|f| f.lines.iter().map(|l| l.ranges.len()).sum::<usize>())
            .sum();
        let files = self.results.len();
        let summary = if total == 0 {
            String::new()
        } else {
            let fw = if files == 1 { "file" } else { "files" };
            let rw = if total == 1 { "result" } else { "results" };
            format!("{} {} in {} {}", total, rw, files, fw)
        };
        if summary != self.last_summary {
            self.summary.set(fs, &summary, theme::UI_FAMILY());
            self.last_summary = summary;
        }
        let region = Self::results_region(region);
        self.scroll
            .set_metrics(region, (region.w, rows.len() as f32 * theme::SEARCH_ROW_H()));
    }

    // ---- Drawing (split to match the renderer's quad-phase then text-phase) ----

    /// Quad phase: chrome, toggle highlights, input selection/caret, match
    /// highlights, and the scrollbar overlay.
    pub fn draw_quads(
        &self,
        region: Rect,
        blink: bool,
        now: Instant,
        bg: &mut Vec<Quad>,
        fg: &mut Vec<Quad>,
    ) {
        let ir = theme::zpx(7.0); // input corner radius (zoom-scaled so it stays round)
        let q = Self::query_rect(region);
        let border = Rect { x: q.x - 1.0, y: q.y - 1.0, w: q.w + 2.0, h: q.h + 2.0 };
        bg.push(border.rounded_quad(theme::SEARCH_BORDER(), ir + 1.0));
        bg.push(q.rounded_quad(theme::SEARCH_BG(), ir));
        let on = [self.opts.case_sensitive, self.opts.whole_word, self.opts.regex];
        for (i, r) in Self::opt_rects(region).iter().enumerate() {
            if on[i] {
                bg.push(r.rounded_quad(theme::ACCENT(), theme::zpx(4.0)));
            }
        }
        let rr = Self::replace_rect(region);
        let rb = Rect { x: rr.x - 1.0, y: rr.y - 1.0, w: rr.w + 2.0, h: rr.h + 2.0 };
        bg.push(rb.rounded_quad(theme::SEARCH_BORDER(), ir + 1.0));
        bg.push(rr.rounded_quad(theme::SEARCH_BG(), ir));
        bg.push(Self::replace_all_rect(region).rounded_quad(theme::DIALOG_BTN(), theme::zpx(6.0)));

        if self.query_active {
            self.query.selection_quads(q, theme::zpx(6.0), bg);
            if blink {
                fg.push(self.query.caret_quad(q, theme::zpx(6.0)));
            }
        }
        if self.replace_active {
            self.replace.selection_quads(rr, theme::zpx(6.0), bg);
            if blink {
                fg.push(self.replace.caret_quad(rr, theme::zpx(6.0)));
            }
        }

        let region2 = Self::results_region(region);
        let scroll = self.scroll.offset().1;
        let pad = self.list.pad_x();
        for (ri, row) in self.rows().iter().enumerate() {
            if row.ranges.is_empty() {
                continue;
            }
            let y = region2.y + ri as f32 * theme::SEARCH_ROW_H() - scroll;
            if y + theme::SEARCH_ROW_H() < region2.y || y > region2.y + region2.h {
                continue;
            }
            for &(s, e) in &row.ranges {
                if let Some((x0, x1)) = self.list.line_x_range(ri, s, e) {
                    let qx = region2.x + pad + x0;
                    let right = region2.x + region2.w;
                    let w = (x1 - x0).min((right - qx).max(0.0));
                    if w > 0.0 {
                        bg.push(Quad::new(qx, y, w, theme::SEARCH_ROW_H(), theme::FIND_MATCH()));
                    }
                }
            }
        }
        self.scroll.draw(now, fg);
    }

    /// Text phase: query/replace text, toggle captions, button caption, and the
    /// results list (match lines + brighter file headers + chevrons).
    pub fn draw_text<'b>(&'b self, region: Rect, areas: &mut Vec<glyphon::TextArea<'b>>) {
        let q = Self::query_rect(region);
        let rr = Self::replace_rect(region);
        let on = [self.opts.case_sensitive, self.opts.whole_word, self.opts.regex];
        let qc = if self.query.text().is_empty() { theme::FG_DIM() } else { theme::FG_TEXT() };
        self.query.draw(q, theme::zpx(6.0), qc, areas);
        let rc = if self.replace.text().is_empty() { theme::FG_DIM() } else { theme::FG_TEXT() };
        self.replace.draw(rr, theme::zpx(6.0), rc, areas);
        for (i, r) in Self::opt_rects(region).iter().enumerate() {
            let lbl = &self.opt_labels[i];
            let left = r.x + (r.w - lbl.width()) * 0.5;
            let color = if on[i] { theme::FG_ACTIVE() } else { theme::FG_DIM() };
            lbl.push(left, *r, color, areas);
        }
        let ba = Self::replace_all_rect(region);
        let bl = &self.replace_all_label;
        bl.push(ba.x + (ba.w - bl.width()) * 0.5, ba, theme::FG_TEXT(), areas);

        // Total-occurrence summary line above the results.
        if !self.last_summary.is_empty() {
            let sr = Self::summary_rect(region);
            self.summary.push(sr.x, sr, theme::FG_DIM(), areas);
        }

        let region2 = Self::results_region(region);
        let scroll = self.scroll.offset().1;
        self.list.draw_at(region2, region2.y - scroll, theme::FG_TEXT(), areas);
        for (ri, row) in self.rows().iter().enumerate() {
            if row.line.is_some() {
                continue;
            }
            let y = region2.y + ri as f32 * theme::SEARCH_ROW_H() - scroll;
            if y + theme::SEARCH_ROW_H() < region2.y || y > region2.y + region2.h {
                continue;
            }
            let top = y.max(region2.y);
            let band = Rect {
                x: region2.x,
                y: top,
                w: region2.w,
                h: ((y + theme::SEARCH_ROW_H()) - top).max(0.0),
            };
            self.list.draw_at(band, region2.y - scroll, theme::FG_ACTIVE(), areas);
            let chev = &self.chevrons[self.collapsed.contains(&row.file) as usize];
            let cr = Rect { x: region2.x + theme::zpx(4.0), y, w: theme::zpx(16.0), h: theme::SEARCH_ROW_H() };
            chev.draw(cr, theme::FG_DIM(), areas);
        }
    }

    // ---- Input ----
    pub fn cursor(&self, p: (f32, f32), region: Rect) -> Option<CursorIcon> {
        if Self::opt_rects(region).iter().any(|r| r.contains(p))
            || Self::results_region(region).contains(p)
            || Self::replace_all_rect(region).contains(p)
        {
            return Some(CursorIcon::Pointer);
        }
        if Self::query_rect(region).contains(p) || Self::replace_rect(region).contains(p) {
            return Some(CursorIcon::Text);
        }
        None
    }

    pub fn hover(&mut self, p: (f32, f32), region: Rect) -> bool {
        let inside = Self::results_region(region).contains(p);
        self.scroll.hover(inside)
    }

    pub fn next_wake(&self, now: Instant) -> Option<Instant> {
        self.scroll.next_wake(now)
    }

    pub fn on_wheel(&mut self, p: (f32, f32), region: Rect, dy: f32) -> bool {
        if Self::results_region(region).contains(p) {
            self.scroll.on_wheel(0.0, dy);
            return true;
        }
        false
    }

    /// Mouse press inside the sidebar while the Search view is active.
    pub fn on_press(
        &mut self,
        pt: (f32, f32),
        region: Rect,
        double: bool,
        _fs: &mut FontSystem,
        root: PathBuf,
        tx: &Sender<WorkerMsg>,
        out: &mut Vec<Intent>,
    ) -> bool {
        // Option toggles (inside the query box).
        if let Some(i) = Self::opt_rects(region).iter().position(|r| r.contains(pt)) {
            match i {
                0 => self.opts.case_sensitive = !self.opts.case_sensitive,
                1 => self.opts.whole_word = !self.opts.whole_word,
                _ => self.opts.regex = !self.opts.regex,
            }
            self.trigger_find(root, tx);
            return true;
        }
        // Scrollbar thumb/track.
        if self.scroll.press(pt) {
            return true;
        }
        if Self::replace_all_rect(region).contains(pt) {
            self.do_replace_all(root, tx, out);
            return true;
        }
        // Query / replace boxes — focus + caret + begin drag-select.
        let q = Self::query_rect(region);
        if q.contains(pt) {
            self.query_active = true;
            self.replace_active = false;
            if double {
                self.query.select_word_at(q, theme::zpx(6.0), pt.0);
            } else {
                self.query.set_caret_from_x(q, theme::zpx(6.0), pt.0);
            }
            self.dragging = Some(0);
            return true;
        }
        let rr = Self::replace_rect(region);
        if rr.contains(pt) {
            self.replace_active = true;
            self.query_active = false;
            if double {
                self.replace.select_word_at(rr, theme::zpx(6.0), pt.0);
            } else {
                self.replace.set_caret_from_x(rr, theme::zpx(6.0), pt.0);
            }
            self.dragging = Some(1);
            return true;
        }
        // Results list — toggle a file header or open a match.
        let region2 = Self::results_region(region);
        if region2.contains(pt) {
            let row = ((pt.1 - region2.y + self.scroll.offset().1) / theme::SEARCH_ROW_H()) as usize;
            if let Some(sr) = self.rows().get(row) {
                match sr.line {
                    Some(line) => {
                        if let Some(f) = self.results.get(sr.file) {
                            out.push(Intent::OpenFile { path: f.path.clone(), line, col: sr.col });
                        }
                    }
                    None => {
                        if !self.collapsed.insert(sr.file) {
                            self.collapsed.remove(&sr.file);
                        }
                    }
                }
            }
            return true;
        }
        // Defocus inputs on any other click in the sidebar.
        self.set_unfocused();
        true
    }

    pub fn on_drag(&mut self, pt: (f32, f32), region: Rect) -> bool {
        if let Some(which) = self.dragging {
            match which {
                0 => self.query.extend_to_x(Self::query_rect(region), theme::zpx(6.0), pt.0),
                _ => self.replace.extend_to_x(Self::replace_rect(region), theme::zpx(6.0), pt.0),
            }
            return true;
        }
        if self.scroll.is_dragging() {
            self.scroll.drag(pt);
            return true;
        }
        false
    }

    pub fn on_release(&mut self) {
        self.dragging = None;
        self.scroll.release();
    }

    /// Keyboard while a search input is focused. Returns true if consumed.
    pub fn on_key(
        &mut self,
        event: &winit::event::KeyEvent,
        ctrl: bool,
        shift: bool,
        fs: &mut FontSystem,
        clip: Option<&mut Clipboard>,
        root: PathBuf,
        tx: &Sender<WorkerMsg>,
        out: &mut Vec<Intent>,
    ) -> bool {
        if self.query_active {
            if matches!(event.logical_key.as_ref(), Key::Named(NamedKey::Escape)) {
                self.query.clear(fs);
                self.results.clear();
                self.scroll.scroll_to_y(0.0);
                return true;
            }
            match crate::edit_input(&mut self.query, fs, clip, event, ctrl, shift) {
                Some(changed) => {
                    if changed {
                        self.trigger_find(root, tx);
                    }
                    return true;
                }
                None => return !ctrl,
            }
        }
        if self.replace_active {
            match event.logical_key.as_ref() {
                Key::Named(NamedKey::Escape) => {
                    self.replace_active = false;
                    return true;
                }
                Key::Named(NamedKey::Enter) => {
                    self.do_replace_all(root, tx, out);
                    return true;
                }
                _ => {}
            }
            match crate::edit_input(&mut self.replace, fs, clip, event, ctrl, shift) {
                Some(_) => return true,
                None => return !ctrl,
            }
        }
        false
    }
}
