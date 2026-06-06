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
use crate::widgets::{IconButton, IconList, IconRow, Rect, ScrollOpts, ScrollView, TextInput, TextLabel};
use arboard::Clipboard;

const OPT: f32 = 18.0; // option-toggle square size

pub struct SearchPanel {
    query: TextInput,
    replace: TextInput,
    include: TextInput, // "files to include" glob filter
    exclude: TextInput, // "files to exclude" glob filter
    list: IconList,     // results tree (same reusable component as the explorer)
    file_icons: std::collections::HashMap<char, IconButton>, // file-type icon overlays
    view_tree: bool,    // true = folder tree, false = flat file list (toolbar toggle)
    opt_labels: [TextLabel; 3],
    // Section toolbar: [refresh, clear, new search editor, collapse all, tree/list].
    tool_btns: [IconButton; 5],
    hovered_tool: Option<usize>,
    filters_btn: IconButton, // "…" toggle that shows/hides the include/exclude inputs
    filters_open: bool,
    replace_all_label: TextLabel,
    summary: TextLabel, // "N results in M files"
    last_summary: String,
    scroll: ScrollView,
    opts: SearchOpts,
    results: Vec<FileMatches>,
    collapsed: HashSet<String>, // collapsed tree groups, by dir/file key
    query_active: bool,
    replace_active: bool,
    include_active: bool,
    exclude_active: bool,
    dragging: Option<u8>, // 0 = query, 1 = replace, 2 = include, 3 = exclude
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
        // Reserve room on the right for the Aa/\b/.* toggles drawn inside the box
        // (3*OPT + 2 gaps + margin) so text/caret never slide under them.
        let mut query = TextInput::new(fs, theme::SIDEBAR_WIDTH(), 30.0).right_pad(3.0 * OPT + 2.0 * 3.0 + 10.0);
        query.set_placeholder(fs, " Search");
        let mut replace = TextInput::new(fs, theme::SIDEBAR_WIDTH(), 30.0);
        replace.set_placeholder(fs, " Replace");
        let mut include = TextInput::new(fs, theme::SIDEBAR_WIDTH(), 30.0);
        include.set_placeholder(fs, " files to include");
        let mut exclude = TextInput::new(fs, theme::SIDEBAR_WIDTH(), 30.0);
        exclude.set_placeholder(fs, " files to exclude");
        let mut replace_all_label = TextLabel::new(fs, theme::SIDEBAR_WIDTH(), 24.0);
        replace_all_label.set(fs, "Replace All", theme::UI_FAMILY());
        let ic = theme::ICON_FAMILY;
        Self {
            query,
            replace,
            include,
            exclude,
            list: IconList::new(fs, theme::SIDEBAR_WIDTH(), 4000.0, theme::SEARCH_ROW_H(), 10.0),
            file_icons: std::collections::HashMap::new(),
            view_tree: true,
            opt_labels: [
                mk_label(fs, "Aa"),
                mk_label(fs, "\\b"),
                mk_label(fs, ".*"),
            ],
            tool_btns: [
                IconButton::new(fs, theme::ICON_REFRESH, ic, 16.0),
                IconButton::new(fs, theme::ICON_CLOSE, ic, 16.0),
                IconButton::new(fs, theme::ICON_NEW_FILE, ic, 16.0),
                IconButton::new(fs, theme::ICON_COLLAPSE_ALL, ic, 16.0),
                IconButton::new(fs, theme::ICON_LIST_TREE, ic, 16.0), // tree/list toggle
            ],
            hovered_tool: None,
            filters_btn: IconButton::new(fs, theme::ICON_ELLIPSIS, ic, 16.0),
            filters_open: false,
            replace_all_label,
            summary: TextLabel::new(fs, theme::SIDEBAR_WIDTH(), 22.0),
            last_summary: String::new(),
            scroll: ScrollView::new(ScrollOpts::vertical()),
            opts: SearchOpts::default(),
            results: Vec::new(),
            collapsed: HashSet::new(),
            query_active: false,
            replace_active: false,
            include_active: false,
            exclude_active: false,
            dragging: None,
            gen: 0,
            pending: bool::default(),
        }
    }

    // ---- Geometry. `r` is the full sidebar (header row + body), so the toolbar
    // icons can sit in the header next to the "SEARCH" title, like the Explorer. ----
    /// The action icons (refresh / clear / new search editor / collapse-all),
    /// right-aligned in the header row beside the "SEARCH" title.
    fn tool_btn_rects(&self, r: Rect) -> [Rect; 5] {
        let sz = theme::zpx(20.0);
        let gap = theme::zpx(4.0);
        let total = 5.0 * sz + 4.0 * gap;
        let start = r.x + r.w - theme::zpx(10.0) - total;
        let y = r.y + (theme::SIDEBAR_HEADER_H() - sz) * 0.5;
        std::array::from_fn(|i| Rect { x: start + i as f32 * (sz + gap), y, w: sz, h: sz })
    }
    fn query_rect(&self, r: Rect) -> Rect {
        // Just below the header row; leave room on the right for the "…" toggle.
        let y = r.y + theme::SIDEBAR_HEADER_H() + theme::zpx(6.0);
        Rect { x: r.x + theme::zpx(10.0), y, w: r.w - theme::zpx(20.0) - theme::zpx(26.0), h: theme::zpx(30.0) }
    }
    /// The "…" toggle button to the right of the query box (shows include/exclude).
    fn filters_toggle_rect(&self, r: Rect) -> Rect {
        let q = self.query_rect(r);
        let sz = theme::zpx(22.0);
        Rect { x: q.x + q.w + theme::zpx(4.0), y: q.y + (q.h - sz) * 0.5, w: sz, h: sz }
    }
    fn opt_rects(&self, r: Rect) -> [Rect; 3] {
        let q = self.query_rect(r);
        let opt = theme::zpx(OPT);
        let (gap, total) = (theme::zpx(3.0), 3.0 * opt + 2.0 * theme::zpx(3.0));
        let start = q.x + q.w - theme::zpx(6.0) - total;
        let y = q.y + (q.h - opt) * 0.5;
        std::array::from_fn(|i| Rect { x: start + i as f32 * (opt + gap), y, w: opt, h: opt })
    }
    fn replace_rect(&self, r: Rect) -> Rect {
        let q = self.query_rect(r);
        Rect { x: q.x, y: q.y + q.h + theme::zpx(6.0), w: q.w, h: theme::zpx(30.0) }
    }
    fn include_rect(&self, r: Rect) -> Rect {
        let rr = self.replace_rect(r);
        // Collapsed (zero-height, no gap) when the filters section is hidden, so the
        // Replace All button and results move up to fill the space.
        let (gap, h) = if self.filters_open { (theme::zpx(8.0), theme::zpx(30.0)) } else { (0.0, 0.0) };
        Rect { x: rr.x, y: rr.y + rr.h + gap, w: rr.w, h }
    }
    fn exclude_rect(&self, r: Rect) -> Rect {
        let ir = self.include_rect(r);
        let (gap, h) = if self.filters_open { (theme::zpx(6.0), theme::zpx(30.0)) } else { (0.0, 0.0) };
        Rect { x: ir.x, y: ir.y + ir.h + gap, w: ir.w, h }
    }
    fn replace_all_rect(&self, r: Rect) -> Rect {
        let er = self.exclude_rect(r);
        Rect { x: er.x, y: er.y + er.h + theme::zpx(8.0), w: er.w, h: theme::zpx(24.0) }
    }
    fn summary_rect(&self, r: Rect) -> Rect {
        let b = self.replace_all_rect(r);
        Rect { x: r.x + theme::zpx(12.0), y: b.y + b.h + theme::zpx(8.0), w: r.w - theme::zpx(20.0), h: theme::zpx(20.0) }
    }
    fn results_region(&self, r: Rect) -> Rect {
        let b = self.summary_rect(r);
        let top = b.y + b.h + theme::zpx(2.0);
        Rect { x: r.x, y: top, w: r.w, h: (r.y + r.h - top).max(0.0) }
    }

    pub fn focused(&self) -> bool {
        self.query_active || self.replace_active
    }

    pub fn set_unfocused(&mut self) {
        self.query_active = false;
        self.replace_active = false;
        self.include_active = false;
        self.exclude_active = false;
    }

    /// Focus exactly one of the four inputs (0 query, 1 replace, 2 include, 3 exclude).
    fn focus_only(&mut self, which: u8) {
        self.query_active = which == 0;
        self.replace_active = which == 1;
        self.include_active = which == 2;
        self.exclude_active = which == 3;
    }

    /// Flatten the current results into a VSCode-style search-editor body:
    /// one `path` header per file, then `  line: text` rows.
    fn results_as_text(&self) -> String {
        let mut out = String::new();
        for f in &self.results {
            out.push_str(&f.rel);
            out.push('\n');
            for l in &f.lines {
                out.push_str(&format!("  {}: {}\n", l.line, l.text.trim_end()));
            }
            out.push('\n');
        }
        out
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

    fn rows(&self) -> Vec<search::TreeRow> {
        if self.view_tree {
            search::build_tree_rows(&self.results, &self.collapsed)
        } else {
            search::build_flat_rows(&self.results, &self.collapsed)
        }
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
        let filters = search::Filters::new(self.include.text().trim(), self.exclude.text().trim());
        search::search_async(tx.clone(), self.gen, root, query, self.opts, filters);
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
        self.include.rezoom(fs);
        self.exclude.rezoom(fs);
        for l in &mut self.opt_labels {
            l.reshape(fs);
        }
        self.list.reshape_icons(fs);
        for b in self.file_icons.values_mut() {
            b.reshape(fs);
        }
        for b in &mut self.tool_btns {
            b.reshape(fs);
        }
        self.filters_btn.reshape(fs);
        self.replace_all_label.reshape(fs);
        self.summary.reshape(fs);
    }

    // ---- Shaping ----
    pub fn update(&mut self, fs: &mut FontSystem, region: Rect) {
        let rows = self.rows();
        // Build the results as a folder tree via the shared IconList component:
        //  - Dir rows: the chevron is the row icon (no file icon), like the explorer's
        //    folders; depth indents the hierarchy.
        //  - File rows: the file-type icon is the row icon; a chevron is overlaid to
        //    its left in draw_text (so files show both, VSCode-style).
        //  - Match rows: no icon, just the matched text.
        // The collapse keys are folded into the cache key so the tree reshapes when a
        // group expands/collapses (even if the visible labels would otherwise match).
        use search::RowKind;
        let key: String = rows
            .iter()
            .map(|r| format!("{}:{}:{}", r.depth, r.kind as u8, r.label))
            .chain(self.collapsed.iter().cloned())
            .collect::<Vec<_>>()
            .join("\n");
        // Indent multiplier: IconList indents 2 spaces per depth, which is too tight
        // to read as a tree, so each tree level spans INDENT levels of the component.
        const INDENT: usize = 2;
        let icon_rows: Vec<IconRow> = rows
            .iter()
            .map(|r| match r.kind {
                // Dirs and files both use the chevron as their (indent-anchored) icon
                // so every level lines up; the file-type icon is overlaid after it in
                // draw_text. The label's leading spaces reserve room for that overlay.
                RowKind::Dir => {
                    let glyph = if self.collapsed.contains(&r.key) { theme::ICON_CHEVRON_RIGHT } else { theme::ICON_CHEVRON_DOWN };
                    IconRow { depth: r.depth * INDENT, icon: Some((glyph, theme::FG_DIM(), 1.0)), label: vec![(r.label.clone(), theme::FG_TEXT())] }
                }
                RowKind::File => {
                    let glyph = if self.collapsed.contains(&r.key) { theme::ICON_CHEVRON_RIGHT } else { theme::ICON_CHEVRON_DOWN };
                    IconRow { depth: r.depth * INDENT, icon: Some((glyph, theme::FG_DIM(), 1.0)), label: vec![(format!("   {}", r.label), theme::FG_ACTIVE())] }
                }
                RowKind::Match => IconRow { depth: r.depth * INDENT, icon: None, label: vec![(r.label.clone(), theme::FG_TEXT())] },
            })
            .collect();
        self.list.set_rows(fs, &key, &icon_rows, 4000.0, 12000.0);
        // Ensure the file-type icon overlay cache has a glyph for every file row.
        for r in rows.iter().filter(|r| r.kind == RowKind::File) {
            let glyph = theme::file_icon(&r.rel).0;
            self.file_icons
                .entry(glyph)
                .or_insert_with(|| IconButton::new(fs, glyph, theme::ICON_FAMILY, 14.0));
        }
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
        let region = self.results_region(region);
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
        // Toolbar: a soft hover background behind the icon under the pointer.
        if let Some(h) = self.hovered_tool {
            let br = self.tool_btn_rects(region)[h];
            bg.push(br.rounded_quad(theme::TITLE_BTN_HOVER(), theme::zpx(4.0)));
        }
        let input_box = |r: Rect, bg: &mut Vec<Quad>| {
            let border = Rect { x: r.x - 1.0, y: r.y - 1.0, w: r.w + 2.0, h: r.h + 2.0 };
            bg.push(border.rounded_quad(theme::SEARCH_BORDER(), ir + 1.0));
            bg.push(r.rounded_quad(theme::SEARCH_BG(), ir));
        };
        let q = self.query_rect(region);
        input_box(q, bg);
        let on = [self.opts.case_sensitive, self.opts.whole_word, self.opts.regex];
        for (i, r) in self.opt_rects(region).iter().enumerate() {
            if on[i] {
                bg.push(r.rounded_quad(theme::ACCENT(), theme::zpx(4.0)));
            }
        }
        // The "…" toggle gets an active background when the filters are open.
        if self.filters_open {
            bg.push(self.filters_toggle_rect(region).rounded_quad(theme::TITLE_BTN_HOVER(), theme::zpx(4.0)));
        }
        let rr = self.replace_rect(region);
        input_box(rr, bg);
        let inc = self.include_rect(region);
        let exc = self.exclude_rect(region);
        if self.filters_open {
            input_box(inc, bg);
            input_box(exc, bg);
        }
        bg.push(self.replace_all_rect(region).rounded_quad(theme::DIALOG_BTN(), theme::zpx(6.0)));

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
        if self.include_active {
            self.include.selection_quads(inc, theme::zpx(6.0), bg);
            if blink {
                fg.push(self.include.caret_quad(inc, theme::zpx(6.0)));
            }
        }
        if self.exclude_active {
            self.exclude.selection_quads(exc, theme::zpx(6.0), bg);
            if blink {
                fg.push(self.exclude.caret_quad(exc, theme::zpx(6.0)));
            }
        }

        let region2 = self.results_region(region);
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
            // IconList prepends 2 spaces per component depth, and each tree level is
            // INDENT(2) component depths, so a match row at tree depth d has 4*d
            // leading spaces; shift the byte ranges past them.
            let pre = row.depth * 4;
            for &(s, e) in &row.ranges {
                if let Some((x0, x1)) = self.list.line_x_range(ri, s + pre, e + pre) {
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
        // Toolbar icons (brighter under the pointer; the tree/list toggle [4] also
        // lights up while tree view is active).
        for (i, r) in self.tool_btn_rects(region).iter().enumerate() {
            let active = self.hovered_tool == Some(i) || (i == 4 && self.view_tree);
            let color = if active { theme::FG_ACTIVE() } else { theme::FG_DIM() };
            self.tool_btns[i].draw(*r, color, areas);
        }
        let q = self.query_rect(region);
        let rr = self.replace_rect(region);
        let inc = self.include_rect(region);
        let exc = self.exclude_rect(region);
        let on = [self.opts.case_sensitive, self.opts.whole_word, self.opts.regex];
        let qc = if self.query.text().is_empty() { theme::FG_DIM() } else { theme::FG_TEXT() };
        self.query.draw(q, theme::zpx(6.0), qc, areas);
        let rc = if self.replace.text().is_empty() { theme::FG_DIM() } else { theme::FG_TEXT() };
        self.replace.draw(rr, theme::zpx(6.0), rc, areas);
        // "…" filters toggle (brighter when open).
        let tcol = if self.filters_open { theme::FG_ACTIVE() } else { theme::FG_DIM() };
        self.filters_btn.draw(self.filters_toggle_rect(region), tcol, areas);
        if self.filters_open {
            let icc = if self.include.text().is_empty() { theme::FG_DIM() } else { theme::FG_TEXT() };
            self.include.draw(inc, theme::zpx(6.0), icc, areas);
            let exc_c = if self.exclude.text().is_empty() { theme::FG_DIM() } else { theme::FG_TEXT() };
            self.exclude.draw(exc, theme::zpx(6.0), exc_c, areas);
        }
        for (i, r) in self.opt_rects(region).iter().enumerate() {
            let lbl = &self.opt_labels[i];
            let left = r.x + (r.w - lbl.width()) * 0.5;
            let color = if on[i] { theme::FG_ACTIVE() } else { theme::FG_DIM() };
            lbl.push(left, *r, color, areas);
        }
        let ba = self.replace_all_rect(region);
        let bl = &self.replace_all_label;
        bl.push(ba.x + (ba.w - bl.width()) * 0.5, ba, theme::FG_TEXT(), areas);

        // Total-occurrence summary line above the results.
        if !self.last_summary.is_empty() {
            let sr = self.summary_rect(region);
            self.summary.push(sr.x, sr, theme::FG_DIM(), areas);
        }

        // The IconList draws each row's text + centered chevron overlay together;
        // per-row label colors already distinguish bright file headers from match
        // lines, so a single slice covers the whole results tree.
        let region2 = self.results_region(region);
        let scroll = self.scroll.offset().1;
        self.list.draw_slice(region2, region2.y - scroll, theme::FG_TEXT(), areas);
        // Every collapsible row uses the chevron as its IconList icon (so depths line
        // up); file rows additionally show their file-type icon, overlaid just right
        // of the chevron in the leading spaces reserved in the label.
        for (ri, row) in self.rows().iter().enumerate() {
            if row.kind != search::RowKind::File {
                continue;
            }
            let y = region2.y + ri as f32 * theme::SEARCH_ROW_H() - scroll;
            if y + theme::SEARCH_ROW_H() < region2.y || y > region2.y + region2.h {
                continue;
            }
            // Position the file-type icon just right of the chevron (the IconList icon).
            let chev_right = self
                .list
                .icon_x_span(ri)
                .map(|(_, x1)| region2.x + self.list.pad_x() + x1)
                .unwrap_or(region2.x + theme::zpx(20.0));
            if let Some(ib) = self.file_icons.get(&theme::file_icon(&row.rel).0) {
                let fr = Rect { x: chev_right + theme::zpx(2.0), y, w: theme::zpx(16.0), h: theme::SEARCH_ROW_H() };
                ib.draw(fr, theme::file_icon(&row.rel).1, areas);
            }
        }
    }

    // ---- Input ----
    pub fn cursor(&self, p: (f32, f32), region: Rect) -> Option<CursorIcon> {
        if self.opt_rects(region).iter().any(|r| r.contains(p))
            || self.tool_btn_rects(region).iter().any(|r| r.contains(p))
            || self.filters_toggle_rect(region).contains(p)
            || self.results_region(region).contains(p)
            || self.replace_all_rect(region).contains(p)
        {
            return Some(CursorIcon::Pointer);
        }
        if self.query_rect(region).contains(p)
            || self.replace_rect(region).contains(p)
            || self.include_rect(region).contains(p)
            || self.exclude_rect(region).contains(p)
        {
            return Some(CursorIcon::Text);
        }
        None
    }

    pub fn hover(&mut self, p: (f32, f32), region: Rect) -> bool {
        let prev = self.hovered_tool;
        self.hovered_tool = self.tool_btn_rects(region).iter().position(|r| r.contains(p));
        let inside = self.results_region(region).contains(p);
        self.scroll.hover(inside) || self.hovered_tool != prev
    }

    pub fn next_wake(&self, now: Instant) -> Option<Instant> {
        self.scroll.next_wake(now)
    }

    pub fn on_wheel(&mut self, p: (f32, f32), region: Rect, dy: f32) -> bool {
        // Wheel over an input box scrolls its (single-line) text horizontally.
        if self.hwheel(p, region, dy) {
            return true;
        }
        if self.results_region(region).contains(p) {
            self.scroll.on_wheel(0.0, dy);
            return true;
        }
        false
    }

    /// Scroll the input box under `p` horizontally by `d` px (shared by vertical and
    /// horizontal wheel routing). Returns true if an input consumed it.
    pub fn hwheel(&self, p: (f32, f32), region: Rect, d: f32) -> bool {
        for (rect, input) in [
            (self.query_rect(region), &self.query),
            (self.replace_rect(region), &self.replace),
            (self.include_rect(region), &self.include),
            (self.exclude_rect(region), &self.exclude),
        ] {
            if rect.h > 0.0 && rect.contains(p) {
                input.scroll_h(-d);
                return true;
            }
        }
        false
    }

    /// Mouse press inside the sidebar while the Search view is active.
    pub fn on_press(
        &mut self,
        pt: (f32, f32),
        region: Rect,
        clicks: u32,
        _fs: &mut FontSystem,
        root: PathBuf,
        tx: &Sender<WorkerMsg>,
        out: &mut Vec<Intent>,
    ) -> bool {
        // "…" toggle: show/hide the include/exclude filter inputs.
        if self.filters_toggle_rect(region).contains(pt) {
            self.filters_open = !self.filters_open;
            if !self.filters_open {
                self.include_active = false;
                self.exclude_active = false;
            }
            return true;
        }
        // Toolbar: refresh / clear results / new search editor / collapse all.
        if let Some(i) = self.tool_btn_rects(region).iter().position(|r| r.contains(pt)) {
            match i {
                0 => self.trigger_find(root, tx), // refresh: re-run the search
                1 => {
                    // Clear: drop the query + results (keeps the filters).
                    self.query.clear(_fs);
                    self.reset();
                }
                2 => {
                    // New search editor: dump the current results into an Untitled doc.
                    let text = self.results_as_text();
                    if !text.is_empty() {
                        out.push(Intent::OpenSearchEditor { text });
                    }
                }
                3 => {
                    // Collapse every directory + file group in the tree.
                    self.collapsed = search::all_group_keys(&self.results);
                }
                _ => {
                    // Toggle between the folder tree and the flat file list.
                    self.view_tree = !self.view_tree;
                    self.collapsed.clear();
                }
            }
            self.set_unfocused();
            return true;
        }
        // Option toggles (inside the query box).
        if let Some(i) = self.opt_rects(region).iter().position(|r| r.contains(pt)) {
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
        if self.replace_all_rect(region).contains(pt) {
            self.do_replace_all(root, tx, out);
            return true;
        }
        // Query / replace / include / exclude boxes — focus + caret + drag-select.
        // Each box routes its press through the TextInput component so single /
        // word / line (1/2/3-click) selection is identical everywhere.
        let q = self.query_rect(region);
        if q.contains(pt) {
            self.focus_only(0);
            self.query.on_click(q, theme::zpx(6.0), pt.0, pt.1, clicks);
            self.dragging = Some(0);
            return true;
        }
        let rr = self.replace_rect(region);
        if rr.contains(pt) {
            self.focus_only(1);
            self.replace.on_click(rr, theme::zpx(6.0), pt.0, pt.1, clicks);
            self.dragging = Some(1);
            return true;
        }
        let inc = self.include_rect(region);
        if inc.contains(pt) {
            self.focus_only(2);
            self.include.on_click(inc, theme::zpx(6.0), pt.0, pt.1, clicks);
            self.dragging = Some(2);
            return true;
        }
        let exc = self.exclude_rect(region);
        if exc.contains(pt) {
            self.focus_only(3);
            self.exclude.on_click(exc, theme::zpx(6.0), pt.0, pt.1, clicks);
            self.dragging = Some(3);
            return true;
        }
        // Results list — toggle a file header or open a match.
        let region2 = self.results_region(region);
        if region2.contains(pt) {
            let row = ((pt.1 - region2.y + self.scroll.offset().1) / theme::SEARCH_ROW_H()) as usize;
            if let Some(sr) = self.rows().get(row) {
                match sr.kind {
                    // A match line opens the file at that line.
                    search::RowKind::Match => {
                        if let (Some(line), Some(f)) = (sr.line, self.results.get(sr.file)) {
                            out.push(Intent::OpenFile { path: f.path.clone(), line, col: sr.col });
                        }
                    }
                    // A directory or file group toggles collapsed.
                    _ => {
                        if !self.collapsed.insert(sr.key.clone()) {
                            self.collapsed.remove(&sr.key);
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
                0 => self.query.extend_to_x(self.query_rect(region), theme::zpx(6.0), pt.0),
                1 => self.replace.extend_to_x(self.replace_rect(region), theme::zpx(6.0), pt.0),
                2 => self.include.extend_to_x(self.include_rect(region), theme::zpx(6.0), pt.0),
                _ => self.exclude.extend_to_x(self.exclude_rect(region), theme::zpx(6.0), pt.0),
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
        // Include / exclude globs: editing them re-runs the search (they scope it).
        if self.include_active || self.exclude_active {
            let input = if self.include_active { &mut self.include } else { &mut self.exclude };
            if matches!(event.logical_key.as_ref(), Key::Named(NamedKey::Escape)) {
                input.clear(fs);
                self.trigger_find(root, tx);
                return true;
            }
            match crate::edit_input(input, fs, clip, event, ctrl, shift) {
                Some(changed) => {
                    if changed {
                        self.trigger_find(root, tx);
                    }
                    return true;
                }
                None => return !ctrl,
            }
        }
        false
    }
}
