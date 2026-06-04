// One edited file: rope text, selection, scroll, undo/redo, glyphon Buffer.
// Edits go through push_and_apply so undo/redo stays consistent.

use std::path::PathBuf;

use glyphon::{Attrs, Buffer, Color, Family, FontSystem, Metrics, Shaping, Wrap};
use ropey::Rope;

use crate::syntax::Lang;
use crate::theme;
use crate::widgets::{ScrollOpts, ScrollView};

#[derive(Clone, Copy)]
pub struct Selection {
    pub anchor: usize,
    pub head: usize,
    pub desired_col: Option<usize>,
}

impl Selection {
    pub fn caret(byte: usize) -> Self {
        Self {
            anchor: byte,
            head: byte,
            desired_col: None,
        }
    }
    pub fn is_empty(&self) -> bool {
        self.anchor == self.head
    }
    pub fn range(&self) -> (usize, usize) {
        if self.anchor <= self.head {
            (self.anchor, self.head)
        } else {
            (self.head, self.anchor)
        }
    }
}

#[derive(Clone)]
pub enum EditOp {
    Insert(String),
    Delete(String),
}

#[derive(Clone)]
pub struct Edit {
    pub at_byte: usize,
    pub op: EditOp,
    pub sel_before: (usize, usize),
    pub sel_after: (usize, usize),
    /// Undo group (Monaco-style "stack element"): consecutive edits share a group
    /// until an undo stop — Enter, a space after a word, paste, a cursor move, or
    /// undo/redo — starts a new one. Undo/redo applies a whole group at once.
    pub group: u64,
}

pub struct Document {
    pub path: Option<PathBuf>,
    pub name: String,
    pub rope: Rope,
    pub sel: Selection,
    pub scroll: ScrollView, // owns the editor scroll offset + scrollbars (per tab)
    pub dirty: bool,
    history: Vec<Edit>,
    future: Vec<Edit>,
    next_group: u64,    // id for the next undo group (see `Edit::group`)
    pending_stop: bool, // force the next edit into a fresh undo group
    force_join: bool,   // glue the next edit to the current group (replace-selection typing)
    pub buffer: Buffer,
    lang: Lang,
    ext: String,
    wrap_width: Option<f32>, // Some(w) when word-wrap is on (wraps at w px)
    eol: String,             // this file's actual line ending ("\n" or "\r\n")
    pub read_only: bool,     // diff views (and future previews) reject edits
    pub diff: Option<crate::diff::Diff>, // Some => this tab is a git diff view (visible projection)
    pub diff_right: Option<Buffer>,      // side-by-side: `buffer` = old/left, this = new/right
    pub diff_full: Option<crate::diff::Diff>, // combined view: the complete diff (pre-collapse)
    pub diff_collapsed: std::collections::HashSet<usize>, // collapsed file indices (combined view)
    pub folds: std::collections::BTreeMap<usize, usize>, // folded regions: header line → last hidden line
    pub image: Option<String>,           // Some(media key) => this tab renders an image
    pub image_scale: Option<f32>,        // None = fit-to-window; Some(s) = absolute scale
    pub image_pan: (f32, f32),           // pan offset (px) from centered position
    pub feedback: bool,                  // Some => Ctrl+Enter submits it as a GitHub issue
    pub version: i32,                    // LSP document version (bumped on every edit)
    pub diagnostics: Vec<crate::lsp::Diagnostic>, // current LSP diagnostics for this doc
    pub lsp_dirty: bool,                 // text changed since the last didChange was sent
    pub lsp_servers: Vec<&'static str>,  // servers a didOpen has been sent to (open-state is per-server)
    hl: Option<crate::highlight::LineCache>, // syntect incremental highlighter (None = no grammar)
    hl_dirty_from: usize,                // lowest line changed since the last highlight (usize::MAX = none)
    semantic: Vec<(usize, usize, Color)>, // Layer-2 LSP semantic tokens (byte range → color)
    expand_stack: Vec<(usize, usize)>, // prior ranges for Expand/Shrink Selection
    pub info: Option<crate::ui::info_page::InfoPage>, // Some ⇒ designed info page (Welcome / Tips / Shortcuts)
}

/// Set the buffer's metrics/wrap/size and its (rich) text. `spans` are precomputed
/// by the caller (via the syntect `LineCache`); when `None`, falls back to markdown
/// line styling or plain text.
#[allow(clippy::too_many_arguments)]
fn apply_buffer_text(
    buffer: &mut Buffer,
    fs: &mut FontSystem,
    text: &str,
    lines: usize,
    lang: Lang,
    ext: &str,
    wrap_width: Option<f32>,
    spans: Option<Vec<(String, Color)>>,
    semantic: &[(usize, usize, Color)],
) {
    // Pick up the current editor font size / line height (driven by settings).
    buffer.set_metrics(fs, Metrics::new(theme::FONT_SIZE(), theme::LINE_HEIGHT()));
    // editor.wordWrap: Some(width) wraps at that width; None = unbounded (no wrap).
    buffer.set_wrap(fs, if wrap_width.is_some() { Wrap::WordOrGlyph } else { Wrap::None });
    let h = (lines as f32 + 2.0) * theme::LINE_HEIGHT() + 200.0;
    buffer.set_size(fs, wrap_width, Some(h));
    let mono = Attrs::new().family(Family::Name(theme::MONO_FAMILY()));
    // Layer 1 = syntect colors; Layer 2 = LSP semantic-token colors overlaid on top.
    // With no Layer-1 grammar but semantic tokens present, color from semantics over a
    // plain base. Else markdown line styling; else plain.
    let attr_spans: Option<Vec<(String, Attrs<'static>)>> = match spans {
        Some(layer1) => Some(merge_spans(text, layer1, semantic, mono)),
        None if !semantic.is_empty() => {
            Some(merge_spans(text, vec![(text.to_string(), theme::FG_TEXT())], semantic, mono))
        }
        // No bundled syntect grammar: fall back to an installed TextMate grammar
        // (e.g. rainbow-csv), then markdown line styling, then plain.
        None => crate::textmate::spans_for(ext, text)
            .or_else(|| (lang == Lang::Markdown).then(|| md_spans(text))),
    };
    if let Some(spans) = attr_spans {
        buffer.set_rich_text(
            fs,
            spans.iter().map(|(s, a)| (s.as_str(), *a)),
            mono,
            Shaping::Advanced,
        );
    } else {
        buffer.set_text(fs, text, mono, Shaping::Advanced);
    }
    buffer.shape_until_scroll(fs, false);
}

/// Merge Layer-1 (syntect) colored spans with Layer-2 (LSP semantic) byte-range
/// overrides into final `(text, Attrs)` runs. Fast path when there are no semantic
/// tokens (just attach the mono family to each Layer-1 color); otherwise resolve a
/// per-byte color array (semantic wins) and run-length-encode it on char boundaries.
fn merge_spans(
    text: &str,
    layer1: Vec<(String, Color)>,
    semantic: &[(usize, usize, Color)],
    mono: Attrs<'static>,
) -> Vec<(String, Attrs<'static>)> {
    if semantic.is_empty() {
        return layer1.into_iter().map(|(s, c)| (s, mono.color(c))).collect();
    }
    let mut colors: Vec<Color> = Vec::with_capacity(text.len());
    for (s, c) in &layer1 {
        for _ in 0..s.len() {
            colors.push(*c);
        }
    }
    colors.resize(text.len(), theme::FG_TEXT());
    for &(b0, b1, c) in semantic {
        for i in b0.min(text.len())..b1.min(text.len()) {
            colors[i] = c;
        }
    }
    let mut out: Vec<(String, Attrs<'static>)> = Vec::new();
    let mut start = 0usize;
    let mut cur = colors.first().copied().unwrap_or_else(theme::FG_TEXT);
    for (idx, _) in text.char_indices() {
        let c = colors[idx];
        if c != cur {
            out.push((text[start..idx].to_string(), mono.color(cur)));
            start = idx;
            cur = c;
        }
    }
    if start < text.len() {
        out.push((text[start..].to_string(), mono.color(cur)));
    }
    out
}

/// Line-level markdown highlighting (headings, quotes, rules, list markers,
/// fenced code). Returns (text, attrs) spans for set_rich_text.
fn md_spans(text: &str) -> Vec<(String, Attrs<'static>)> {
    let mono = |c| Attrs::new().family(Family::Name(theme::MONO_FAMILY())).color(c);
    let mut out: Vec<(String, Attrs)> = Vec::new();
    let mut in_fence = false;
    for line in text.split_inclusive('\n') {
        let body = line.trim_end_matches('\n');
        let trimmed = body.trim_start();
        if trimmed.starts_with("```") {
            in_fence = !in_fence;
            out.push((line.to_string(), mono(theme::MD_CODE())));
            continue;
        }
        if in_fence {
            out.push((line.to_string(), mono(theme::MD_CODE())));
        } else if trimmed.starts_with('#') {
            out.push((line.to_string(), mono(theme::MD_HEADING())));
        } else if trimmed.starts_with('>') {
            out.push((line.to_string(), mono(theme::MD_QUOTE())));
        } else if !trimmed.is_empty()
            && trimmed.chars().all(|c| c == '-' || c == '*' || c == '_' || c == ' ')
            && trimmed.chars().filter(|&c| c == '-' || c == '*' || c == '_').count() >= 3
        {
            out.push((line.to_string(), mono(theme::MD_RULE())));
        } else if trimmed.starts_with("* ") || trimmed.starts_with("- ") || trimmed.starts_with("+ ") {
            let indent = body.len() - trimmed.len();
            out.push((body[..indent + 1].to_string(), mono(theme::MD_LIST())));
            out.push((format!("{}\n", &body[indent + 1..]), mono(theme::FG_TEXT())));
        } else {
            out.push((line.to_string(), mono(theme::FG_TEXT())));
        }
    }
    out
}

impl Document {
    pub fn new(path: Option<PathBuf>, contents: String, fs: &mut FontSystem) -> Self {
        let ext = path
            .as_ref()
            .and_then(|p| p.extension())
            .map(|e| e.to_string_lossy().to_lowercase())
            .unwrap_or_default();
        let lang = Lang::from_ext(&ext);
        let mut buffer = Buffer::new(fs, Metrics::new(theme::FONT_SIZE(), theme::LINE_HEIGHT()));
        // Strip CR for display: cosmic-text renders a stray \r (from CRLF files)
        // as an extra line break, double-spacing the whole document. The rope
        // keeps the original \r\n so saving preserves line endings.
        let display = contents.replace('\r', "");
        let wrap_width = None;
        // Detect the file's actual EOL from its content; new/empty files fall back
        // to the files.eol default. The status bar shows this (truthful), and save
        // preserves it — changing files.eol only affects new files.
        let eol = if contents.contains("\r\n") {
            "\r\n".to_string()
        } else if contents.contains('\n') {
            "\n".to_string()
        } else {
            crate::settings::eol()
        };
        // Layer-1 highlighter: a syntect grammar for this file type (None → plain/markdown).
        let mut hl = crate::highlight::LineCache::new(&ext);
        let spans = hl.as_mut().map(|h| h.highlight(&display, 0));
        apply_buffer_text(&mut buffer, fs, &display, display.matches('\n').count(), lang, &ext, wrap_width, spans, &[]);
        let name = match &path {
            Some(p) => p
                .file_name()
                .map(|f| f.to_string_lossy().into_owned())
                .unwrap_or_else(|| "Untitled".into()),
            None => "Untitled".into(),
        };
        Self {
            path,
            name,
            rope: Rope::from_str(&contents),
            sel: Selection::caret(0),
            scroll: ScrollView::new(ScrollOpts::both()),
            dirty: false,
            history: Vec::new(),
            next_group: 0,
            pending_stop: true,
            force_join: false,
            future: Vec::new(),
            buffer,
            lang,
            ext,
            wrap_width,
            eol,
            read_only: false,
            diff: None,
            diff_right: None,
            diff_full: None,
            diff_collapsed: std::collections::HashSet::new(),
            folds: std::collections::BTreeMap::new(),
            image: None,
            image_scale: None,
            image_pan: (0.0, 0.0),
            feedback: false,
            version: 0,
            diagnostics: Vec::new(),
            lsp_dirty: false,
            lsp_servers: Vec::new(),
            hl,
            hl_dirty_from: usize::MAX,
            semantic: Vec::new(),
            expand_stack: Vec::new(),
            info: None,
        }
    }

    /// A read-only tab that renders a hand-designed info page (Welcome / Tips /
    /// Keyboard Shortcuts) instead of text. The page owns its layout + geometry.
    pub fn new_info(page: crate::ui::info_page::InfoPage, fs: &mut FontSystem) -> Self {
        let mut d = Document::new(None, String::new(), fs);
        d.name = page.title.clone();
        d.read_only = true;
        d.info = Some(page);
        d
    }

    /// An editable "Feedback" tab. The user types a report and presses Ctrl+Enter
    /// to file it as a GitHub issue (first line = title, rest = body).
    pub fn new_feedback(fs: &mut FontSystem) -> Self {
        let template = "<!-- First line = issue title, the rest = body. Ctrl+Enter to submit to GitHub. -->\n\n";
        let mut d = Document::new(None, template.to_string(), fs);
        d.name = "Feedback".to_string();
        d.feedback = true;
        let end = d.rope.len_bytes();
        d.sel = Selection::caret(end);
        d
    }

    /// A read-only image tab. The picture itself is uploaded to `gpu.media` under
    /// `key` and drawn in the editor region by the renderer; this Document just
    /// carries the key + name so it behaves like a normal (read-only) tab.
    pub fn new_image(path: PathBuf, key: String, fs: &mut FontSystem) -> Self {
        let name = path
            .file_name()
            .map(|f| f.to_string_lossy().into_owned())
            .unwrap_or_else(|| "Image".into());
        let buffer = Buffer::new(fs, Metrics::new(theme::FONT_SIZE(), theme::LINE_HEIGHT()));
        Self {
            path: Some(path),
            name,
            rope: Rope::new(),
            sel: Selection::caret(0),
            scroll: ScrollView::new(ScrollOpts::both()),
            dirty: false,
            history: Vec::new(),
            next_group: 0,
            pending_stop: true,
            force_join: false,
            future: Vec::new(),
            buffer,
            lang: Lang::PlainText,
            ext: String::new(),
            wrap_width: None,
            eol: "\n".to_string(),
            read_only: true,
            diff: None,
            diff_right: None,
            diff_full: None,
            diff_collapsed: std::collections::HashSet::new(),
            folds: std::collections::BTreeMap::new(),
            image: Some(key),
            image_scale: None,
            image_pan: (0.0, 0.0),
            feedback: false,
            version: 0,
            diagnostics: Vec::new(),
            lsp_dirty: false,
            lsp_servers: Vec::new(),
            hl: None,
            hl_dirty_from: usize::MAX,
            semantic: Vec::new(),
            expand_stack: Vec::new(),
            info: None,
        }
    }

    /// Zoom the image so the pixel under `cursor` stays fixed (cursor-centred).
    pub fn image_zoom_at(&mut self, cursor: (f32, f32), region: crate::widgets::Rect, iw: f32, ih: f32, factor: f32) {
        let fit = crate::render::image_fit_scale(iw, ih, region);
        let old = self.image_scale.unwrap_or(fit);
        let new = (old * factor).clamp(0.02, 64.0);
        let (cx, cy) = (region.x + region.w * 0.5, region.y + region.h * 0.5);
        let t_old = (cx - iw * old * 0.5 + self.image_pan.0, cy - ih * old * 0.5 + self.image_pan.1);
        let img = ((cursor.0 - t_old.0) / old, (cursor.1 - t_old.1) / old);
        let t_new = (cursor.0 - img.0 * new, cursor.1 - img.1 * new);
        self.image_pan = (t_new.0 - (cx - iw * new * 0.5), t_new.1 - (cy - ih * new * 0.5));
        self.image_scale = Some(new);
    }

    pub fn image_fit(&mut self) {
        self.image_scale = None;
        self.image_pan = (0.0, 0.0);
    }

    pub fn image_actual(&mut self) {
        self.image_scale = Some(1.0);
        self.image_pan = (0.0, 0.0);
    }

    pub fn image_pan_by(&mut self, dx: f32, dy: f32) {
        self.image_pan.0 += dx;
        self.image_pan.1 += dy;
    }

    /// A read-only side-by-side git diff view, shown as its own tab. The main
    /// `buffer` holds the old (left) side, `diff_right` the new (right) side — both
    /// plain (no syntax); `diff.rows` drives the per-row backgrounds and gutters.
    pub fn new_diff(diff: crate::diff::Diff, fs: &mut FontSystem) -> Self {
        let mk = |fs: &mut FontSystem, text: &str| {
            let mut b = Buffer::new(fs, Metrics::new(theme::FONT_SIZE(), theme::LINE_HEIGHT()));
            let display = text.replace('\r', "");
            apply_buffer_text(&mut b, fs, &display, display.matches('\n').count(), Lang::PlainText, "", None, None, &[]);
            b
        };
        // Combined views keep the full diff so collapse can re-project; the buffers
        // and `diff` start from the fully-expanded projection.
        let (visible, full) = if diff.combined {
            (crate::diff::project(&diff, &std::collections::HashSet::new()), Some(diff))
        } else {
            (diff, None)
        };
        let buffer = mk(fs, &visible.left_text);
        let diff_right = Some(mk(fs, &visible.right_text));
        Self {
            path: None,
            name: visible.title.clone(),
            rope: Rope::from_str(&visible.left_text),
            sel: Selection::caret(0),
            scroll: ScrollView::new(ScrollOpts::both()),
            dirty: false,
            history: Vec::new(),
            next_group: 0,
            pending_stop: true,
            force_join: false,
            future: Vec::new(),
            buffer,
            lang: Lang::PlainText,
            ext: String::new(),
            wrap_width: None,
            eol: "\n".to_string(),
            read_only: true,
            diff: Some(visible),
            diff_right,
            diff_full: full,
            diff_collapsed: std::collections::HashSet::new(),
            folds: std::collections::BTreeMap::new(),
            image: None,
            image_scale: None,
            image_pan: (0.0, 0.0),
            feedback: false,
            version: 0,
            diagnostics: Vec::new(),
            lsp_dirty: false,
            lsp_servers: Vec::new(),
            hl: None,
            hl_dirty_from: usize::MAX,
            semantic: Vec::new(),
            expand_stack: Vec::new(),
            info: None,
        }
    }

    /// If visible `line` is a combined-diff file header, the file index it toggles.
    pub fn diff_file_at_line(&self, line: usize) -> Option<usize> {
        let d = self.diff.as_ref()?;
        if !d.combined {
            return None;
        }
        let row = d.rows.get(line)?;
        (row.kind == crate::diff::RowKind::File).then_some(row.file)
    }

    /// Collapse/expand one file in a combined diff and rebuild the side-by-side
    /// panes from the new projection.
    pub fn toggle_diff_file(&mut self, file_idx: usize, fs: &mut FontSystem) {
        let Some(full) = self.diff_full.as_ref() else {
            return;
        };
        if !self.diff_collapsed.insert(file_idx) {
            self.diff_collapsed.remove(&file_idx);
        }
        let vis = crate::diff::project(full, &self.diff_collapsed);
        let mk = |fs: &mut FontSystem, text: &str| {
            let mut b = Buffer::new(fs, Metrics::new(theme::FONT_SIZE(), theme::LINE_HEIGHT()));
            let display = text.replace('\r', "");
            apply_buffer_text(&mut b, fs, &display, display.matches('\n').count(), Lang::PlainText, "", None, None, &[]);
            b
        };
        self.buffer = mk(fs, &vis.left_text);
        self.diff_right = Some(mk(fs, &vis.right_text));
        self.rope = Rope::from_str(&vis.left_text);
        self.diff = Some(vis);
    }

    /// This file's line ending: "\n" or "\r\n".
    pub fn eol(&self) -> &str {
        &self.eol
    }

    /// The file's lowercased extension (e.g. "rs") — for go-to-symbol language rules.
    pub fn ext(&self) -> &str {
        &self.ext
    }

    /// Toggle word-wrap: `Some(width)` wraps the buffer at that pixel width, `None`
    /// disables wrapping. Reshapes only when the value changes.
    pub fn set_wrap(&mut self, fs: &mut FontSystem, width: Option<f32>) {
        let changed = match (self.wrap_width, width) {
            (Some(a), Some(b)) => (a - b).abs() > 0.5,
            (None, None) => false,
            _ => true,
        };
        if changed {
            self.wrap_width = width;
            self.reshape(fs);
        }
    }

    /// Widest shaped line in pixels (for horizontal scrolling).
    pub fn max_line_width(&self) -> f32 {
        let left = self.buffer.layout_runs().map(|r| r.line_w).fold(0.0_f32, f32::max);
        // In a side-by-side diff the right pane has its own buffer; the widest line
        // across both panes drives the (shared) horizontal scroll range.
        match self.diff_right.as_ref() {
            Some(right) => right.layout_runs().map(|r| r.line_w).fold(left, f32::max),
            None => left,
        }
    }

    /// Current scroll offset (px). Backed by the document's `ScrollView`.
    pub fn scroll_x(&self) -> f32 {
        self.scroll.offset().0
    }
    pub fn scroll_y(&self) -> f32 {
        self.scroll.offset().1
    }

    // ---- Visual geometry (single source of truth for wrap-aware positions). ----
    // A logical line can span several visual rows when word-wrap is on; these
    // resolve buffer-local positions from the shaped layout so the caret, the
    // current-line highlight, and anything else stay consistent.

    /// Buffer-local (x, y, height) of the caret. y is the top of the visual row the
    /// caret is on; x is the offset within that row.
    pub fn caret_visual(&self) -> (f32, f32, f32) {
        self.byte_visual(self.sel.head)
    }

    /// Buffer-local (x, top, height) of an arbitrary byte offset (the caret math,
    /// generalized — used e.g. for the drag-and-drop insertion caret).
    pub fn byte_visual(&self, byte: usize) -> (f32, f32, f32) {
        let byte = byte.min(self.rope.len_bytes());
        let line = self.rope.byte_to_line(byte);
        let col_byte = byte - self.rope.line_to_byte(line);
        let mut last_top = line as f32 * theme::LINE_HEIGHT();
        let mut last_h = theme::LINE_HEIGHT();
        let mut last_end_x = 0.0f32;
        for run in self.buffer.layout_runs() {
            if run.line_i != line {
                continue;
            }
            last_top = run.line_top;
            last_h = run.line_height;
            let mut run_end = 0.0f32;
            for g in run.glyphs.iter() {
                if (g.start as usize) >= col_byte {
                    return (g.x, run.line_top, run.line_height);
                }
                run_end = g.x + g.w;
            }
            last_end_x = run_end;
        }
        (last_end_x, last_top, last_h)
    }

    /// Buffer-local (top, height) covering all visual rows of a logical line — used
    /// to highlight the current line even when it wraps across several rows.
    pub fn line_visual_bounds(&self, line: usize) -> (f32, f32) {
        let mut top: Option<f32> = None;
        let mut bottom = 0.0f32;
        for run in self.buffer.layout_runs() {
            if run.line_i == line {
                if top.is_none() {
                    top = Some(run.line_top);
                }
                bottom = run.line_top + run.line_height;
            }
        }
        match top {
            Some(t) => (t, (bottom - t).max(theme::LINE_HEIGHT())),
            None => (line as f32 * theme::LINE_HEIGHT(), theme::LINE_HEIGHT()),
        }
    }

    // ---- Code folding (indentation-based) ----

    /// Leading-whitespace width of a line, or None for a blank line.
    fn line_indent(&self, line: usize) -> Option<usize> {
        if line >= self.rope.len_lines() {
            return None;
        }
        let s = self.rope.line(line).to_string();
        if s.trim().is_empty() {
            return None;
        }
        Some(s.chars().take_while(|c| *c == ' ' || *c == '\t').count())
    }

    /// The last line of the indentation-based fold starting at `header`, if any.
    pub fn fold_range(&self, header: usize) -> Option<usize> {
        let total = self.rope.len_lines();
        let base = self.line_indent(header)?;
        let mut end = header;
        let mut i = header + 1;
        while i < total {
            match self.line_indent(i) {
                None => {}                          // blank line: keep scanning
                Some(ind) if ind > base => end = i, // deeper: part of the region
                Some(_) => break,                   // same/less indent: region ends
            }
            i += 1;
        }
        (end > header).then_some(end)
    }

    pub fn is_foldable(&self, line: usize) -> bool {
        self.fold_range(line).is_some()
    }
    pub fn is_folded(&self, header: usize) -> bool {
        self.folds.contains_key(&header)
    }
    /// True if `line` sits inside a collapsed region (not the header itself).
    pub fn is_line_hidden(&self, line: usize) -> bool {
        self.folds.iter().any(|(&h, &e)| line > h && line <= e)
    }
    /// Count of hidden lines strictly above `line` (drives vertical placement).
    /// Counts unique hidden lines (robust to overlapping/nested fold ranges).
    pub fn hidden_above(&self, line: usize) -> usize {
        if self.folds.is_empty() {
            return 0;
        }
        let cap = line.min(self.rope.len_lines());
        (0..cap).filter(|&l| self.is_line_hidden(l)).count()
    }

    /// Expand any folds that hide `line` (e.g. when navigating to a search match
    /// inside a collapsed region) so it becomes visible.
    pub fn reveal_line(&mut self, line: usize) {
        let headers: Vec<usize> = self
            .folds
            .iter()
            .filter(|(&h, &e)| line > h && line <= e)
            .map(|(&h, _)| h)
            .collect();
        for h in headers {
            self.folds.remove(&h);
        }
    }

    /// Fold or unfold the region whose header is `header`.
    pub fn toggle_fold(&mut self, header: usize) {
        if self.folds.remove(&header).is_some() {
            return;
        }
        if let Some(end) = self.fold_range(header) {
            // Keep nested child folds intact — the union-based fold logic renders
            // overlapping ranges correctly, so unfolding this parent restores the
            // children's collapsed state (like VSCode).
            self.folds.insert(header, end);
            // Pull the caret out of a region that just collapsed.
            let (cl, _) = self.head_line_col();
            if cl > header && cl <= end {
                let byte = self.rope.line_to_byte(header);
                self.place(byte, false);
            }
        }
    }

    /// Map a visible-row index (counting only non-hidden lines, top to bottom) to
    /// its rope line. Robust to overlapping/nested folds.
    pub fn visible_index_to_line(&self, vidx: usize) -> usize {
        let total = self.rope.len_lines();
        if self.folds.is_empty() {
            return vidx.min(total.saturating_sub(1));
        }
        let mut seen = 0usize;
        for l in 0..total {
            if self.is_line_hidden(l) {
                continue;
            }
            if seen == vidx {
                return l;
            }
            seen += 1;
        }
        total.saturating_sub(1)
    }

    /// Convert a click's compressed y (px, fold-collapsed space) to the real buffer
    /// y, so hit-testing lands on the right line when folds are present.
    pub fn expand_visual_y(&self, vy: f32) -> f32 {
        if self.folds.is_empty() || vy <= 0.0 {
            return vy;
        }
        let lh = theme::LINE_HEIGHT();
        let vidx = (vy / lh).floor() as usize;
        let frac = vy - vidx as f32 * lh;
        self.visible_index_to_line(vidx) as f32 * lh + frac
    }

    /// First visible line at or after `line` (skips collapsed regions, moving down).
    pub fn first_visible_from(&self, line: usize, down: bool) -> usize {
        let total = self.rope.len_lines();
        let mut l = line.min(total.saturating_sub(1));
        while self.is_line_hidden(l) {
            if down {
                if l + 1 >= total {
                    break;
                }
                l += 1;
            } else if l == 0 {
                break;
            } else {
                l -= 1;
            }
        }
        l
    }

    pub fn reshape(&mut self, fs: &mut FontSystem) {
        let t0 = std::time::Instant::now();
        let text = self.rope.to_string().replace('\r', "");
        let lines = self.rope.len_lines();
        let t_str = t0.elapsed();
        let t1 = std::time::Instant::now();
        // Layer-1 highlight, incrementally from the lowest edited line (usize::MAX =
        // no text change since last highlight → returns cached spans, no re-tokenize).
        let dirty = std::mem::replace(&mut self.hl_dirty_from, usize::MAX);
        let spans = self.hl.as_mut().map(|h| h.highlight(&text, dirty));
        apply_buffer_text(&mut self.buffer, fs, &text, lines, self.lang, &self.ext, self.wrap_width, spans, &self.semantic);
        crate::perf::log(&format!("reshape({lines} lines): to_string {:?}, highlight+shape {:?}", t_str, t1.elapsed()));
    }

    /// Force a full re-highlight on the next reshape (e.g. after a theme change).
    pub fn invalidate_highlight(&mut self) {
        self.hl_dirty_from = 0;
    }

    /// Store Layer-2 semantic tokens `(line, start_utf16, len_utf16, color)`, mapped
    /// to byte ranges. They overlay the syntect colors on the next reshape.
    pub fn set_semantic(&mut self, toks: &[(u32, u32, u32, Color)]) {
        self.semantic = toks
            .iter()
            .map(|&(line, start, len, c)| (self.lsp_byte(line, start), self.lsp_byte(line, start + len), c))
            .collect();
    }

    /// Replace the entire document from an external on-disk change (e.g. Replace
    /// All). Resets undo history, clamps the selection, and marks the doc clean.
    pub fn set_text_external(&mut self, text: &str, fs: &mut FontSystem) {
        self.rope = Rope::from_str(text);
        self.history.clear();
        self.future.clear();
        let max = self.rope.len_bytes();
        self.sel.anchor = self.sel.anchor.min(max);
        self.sel.head = self.sel.head.min(max);
        self.dirty = false;
        self.reshape(fs);
    }

    fn apply_op(&mut self, op: &EditOp, at_byte: usize) {
        // Keep diagnostic squiggles anchored to their text while the server's
        // re-publish is pending (it can lag to the next save). Positions are shifted
        // against the PRE-edit rope. Undo/redo also pass through here with inverse
        // ops, so the shifts cancel correctly.
        self.shift_diagnostics(op, at_byte);
        let at_char = self.rope.byte_to_char(at_byte);
        match op {
            EditOp::Insert(s) => {
                self.rope.insert(at_char, s);
            }
            EditOp::Delete(s) => {
                let end_char = at_char + s.chars().count();
                self.rope.remove(at_char..end_char);
            }
        }
    }

    /// Shift stored diagnostic positions for an edit at `at_byte` (VSCode keeps
    /// squiggles glued to their text between server publishes; without this they sit
    /// at stale coordinates, underlining whatever scrolls into them). LSP positions
    /// are (line, UTF-16 col); cols are computed in UTF-16 to match.
    fn shift_diagnostics(&mut self, op: &EditOp, at_byte: usize) {
        if self.diagnostics.is_empty() {
            return;
        }
        let at = at_byte.min(self.rope.len_bytes());
        let line = self.rope.byte_to_line(at);
        let line_start = self.rope.line_to_byte(line);
        let utf16 = |s: &str| s.encode_utf16().count() as i64;
        let col = utf16(&self.rope.byte_slice(line_start..at).to_string());
        let (l, c) = (line as i64, col);
        let (s, deleting) = match op {
            EditOp::Insert(s) => (s, false),
            EditOp::Delete(s) => (s, true),
        };
        let k = s.matches('\n').count() as i64; // newlines in the edited text
        let t = utf16(s.rsplit('\n').next().unwrap_or("")); // utf16 len after the last newline
        // End of the affected span (deletes only).
        let (l2, c2) = if k == 0 { (l, c + t) } else { (l + k, t) };
        for d in &mut self.diagnostics {
            for (is_end, pl, pc) in [
                (false, &mut d.start_line, &mut d.start_char),
                (true, &mut d.end_line, &mut d.end_char),
            ] {
                let (mut a, mut b) = (*pl as i64, *pc as i64);
                if !deleting {
                    // Insert at (l, c): positions after it slide right/down. Ties differ:
                    // inserting AT a range's start pushes the range right, but inserting
                    // AT its (exclusive) end does not extend it.
                    let hit = if is_end { b > c } else { b >= c };
                    if a == l && hit {
                        if k == 0 {
                            b += t;
                        } else {
                            a += k;
                            b = b - c + t;
                        }
                    } else if a > l {
                        a += k;
                    }
                } else {
                    // Delete [ (l,c) .. (l2,c2) ): positions after the span slide back;
                    // positions inside it clamp to the span start.
                    if a > l2 || (a == l2 && b >= c2) {
                        if a == l2 {
                            a = l;
                            b = c + (b - c2);
                        } else {
                            a -= k;
                        }
                    } else if a > l || (a == l && b > c) {
                        a = l;
                        b = c;
                    }
                }
                *pl = a.max(0) as u32;
                *pc = b.max(0) as u32;
            }
        }
    }

    /// True when `op` should extend the current undo group (Monaco's rules): same
    /// kind, the caret never moved in between (the new edit starts exactly where the
    /// last one ended), single keystrokes only, Enter starts its own group, and a
    /// space typed after a word starts the next group ("hello world" undoes word-wise).
    fn joins_undo_group(&self, op: &EditOp, sel_before: (usize, usize)) -> bool {
        if self.force_join {
            return true;
        }
        if self.pending_stop || !self.future.is_empty() {
            return false;
        }
        let Some(prev) = self.history.last() else {
            return false;
        };
        if sel_before != prev.sel_after {
            return false; // cursor moved (click/arrows) between the edits
        }
        match (&prev.op, op) {
            (EditOp::Insert(p), EditOp::Insert(s)) => {
                if s.len() > 4 || p.len() > 200 || s.contains('\n') || p.ends_with('\n') {
                    return false;
                }
                let s_ws = s.chars().all(char::is_whitespace);
                let p_ends_word = p.chars().last().map_or(false, |c| !c.is_whitespace());
                !(s_ws && p_ends_word) // space after a word → new group
            }
            (EditOp::Delete(p), EditOp::Delete(s)) => {
                s.len() <= 4 && p.len() <= 200 && !s.contains('\n') && !p.contains('\n')
            }
            _ => false,
        }
    }

    /// Force the next edit into a fresh undo group (called around paste, etc.).
    pub fn break_undo_group(&mut self) {
        self.pending_stop = true;
    }

    fn push_and_apply(&mut self, op: EditOp, at_byte: usize, sel_after: Selection) {
        let sel_before = (self.sel.anchor, self.sel.head);
        let sel_after_t = (sel_after.anchor, sel_after.head);
        let group = if self.joins_undo_group(&op, sel_before) {
            self.history.last().map(|e| e.group).unwrap_or(0)
        } else {
            self.next_group += 1;
            self.next_group
        };
        self.pending_stop = false;
        self.force_join = false;
        self.apply_op(&op, at_byte);
        self.history.push(Edit {
            at_byte,
            op,
            sel_before,
            sel_after: sel_after_t,
            group,
        });
        self.future.clear();
        self.sel = sel_after;
        self.dirty = true;
        // The expand/shrink stack holds byte ranges; edits invalidate them.
        self.expand_stack.clear();
        // Folds are keyed by line number; an edit can shift lines, so drop them
        // rather than hide the wrong range. (Cheap to re-fold.)
        if !self.folds.is_empty() {
            self.folds.clear();
        }
        // LSP: the text changed — bump the version and flag a pending didChange
        // (App sends it debounced from the idle tick).
        self.version += 1;
        self.lsp_dirty = true;
        // Highlight: re-tokenize only from the edited line forward on the next reshape.
        let edited_line = self.rope.byte_to_line(at_byte.min(self.rope.len_bytes()));
        self.hl_dirty_from = self.hl_dirty_from.min(edited_line);
    }

    /// `file://` URI for this document, if it has a path.
    pub fn uri(&self) -> Option<String> {
        self.path.as_deref().map(crate::lsp::path_to_uri)
    }

    /// LSP language id (e.g. "javascript"), if a server serves this file type.
    pub fn language_id(&self) -> Option<&'static str> {
        crate::lsp::language_id(&self.ext)
    }

    /// Full document text (for LSP full-text sync).
    pub fn text(&self) -> String {
        self.rope.to_string()
    }

    /// Absolute byte offset for an LSP position `(line, utf16_char)`. LSP characters
    /// are UTF-16 code units within the line; the rope is UTF-8, so walk the line.
    pub fn lsp_byte(&self, line: u32, ch: u32) -> usize {
        let line = (line as usize).min(self.rope.len_lines().saturating_sub(1));
        let line_start = self.rope.line_to_byte(line);
        let mut utf16 = 0u32;
        let mut byte = 0usize;
        for c in self.rope.line(line).chars() {
            if utf16 >= ch || c == '\n' {
                break;
            }
            utf16 += c.len_utf16() as u32;
            byte += c.len_utf8();
        }
        line_start + byte
    }

    /// LSP position `(line, utf16_col)` for an absolute byte offset — the inverse
    /// of `lsp_byte` (definition/references requests send the caret this way).
    pub fn lsp_pos(&self, byte: usize) -> (u32, u32) {
        let b = byte.min(self.rope.len_bytes());
        let line = self.rope.byte_to_line(b);
        let line_start = self.rope.line_to_byte(line);
        let col: usize = self
            .rope
            .byte_slice(line_start..b)
            .chars()
            .map(|c| c.len_utf16())
            .sum();
        (line as u32, col as u32)
    }

    /// The diagnostic message under a buffer-relative point (for hover tooltips),
    /// if the point lands within a diagnostic's range. `buf_x/buf_y` are relative to
    /// the text's top-left (caller subtracts the editor pad + adds scroll).
    pub fn diagnostic_at(&self, buf_x: f32, buf_y: f32) -> Option<crate::lsp::DiagHover> {
        if self.diagnostics.is_empty() {
            return None;
        }
        let hit = self.buffer.hit(buf_x, buf_y)?;
        let line = hit.line;
        if line >= self.rope.len_lines() {
            return None;
        }
        let byte = self.rope.line_to_byte(line) + hit.index.min(self.rope.line(line).len_bytes());
        let matched: Vec<&crate::lsp::Diagnostic> = self
            .diagnostics
            .iter()
            .filter(|d| {
                let (lo, hi) = self.diag_byte_range(d);
                byte >= lo && byte < hi.max(lo + 1)
            })
            .collect();
        if matched.is_empty() {
            return None;
        }
        let message = matched.iter().map(|d| d.message.trim()).collect::<Vec<_>>().join("\n");
        // Prefer the diagnostic that carries a docs link for the source/code/href.
        let primary = matched.iter().find(|d| d.code_href.is_some()).copied().unwrap_or(matched[0]);
        Some(crate::lsp::DiagHover {
            message,
            source: primary.source.clone(),
            code: primary.code.clone(),
            href: primary.code_href.clone(),
        })
    }

    /// Absolute byte (lo, hi) range of a diagnostic, for highlight rendering.
    pub fn diag_byte_range(&self, d: &crate::lsp::Diagnostic) -> (usize, usize) {
        (self.lsp_byte(d.start_line, d.start_char), self.lsp_byte(d.end_line, d.end_char))
    }

    pub fn insert_str(&mut self, s: &str, fs: &mut FontSystem) {
        if self.read_only {
            return;
        }
        if !self.sel.is_empty() {
            self.delete_selection_no_reshape();
            // The delete + insert came from one keystroke (typing over a selection):
            // keep them in one undo group so a single undo restores both.
            self.force_join = true;
        }
        let head = self.sel.head;
        let new_head = head + s.len();
        self.push_and_apply(
            EditOp::Insert(s.to_string()),
            head,
            Selection::caret(new_head),
        );
        self.reshape(fs);
    }

    /// Move the selected text to `target` (drag-and-drop). One undo step; the moved
    /// text stays selected at its new home (VSCode behavior). No-op if `target` is
    /// inside the selection.
    pub fn move_selection_to(&mut self, target: usize, fs: &mut FontSystem) {
        if self.read_only || self.sel.is_empty() {
            return;
        }
        let (lo, hi) = self.sel.range();
        if target >= lo && target <= hi {
            return;
        }
        let text = self.rope.slice(self.rope.byte_to_char(lo)..self.rope.byte_to_char(hi)).to_string();
        self.delete_selection_no_reshape();
        self.force_join = true; // delete + insert = one undo group
        let dest = if target > hi { target - (hi - lo) } else { target };
        self.sel = Selection::caret(dest);
        self.push_and_apply(
            EditOp::Insert(text.clone()),
            dest,
            Selection { anchor: dest, head: dest + text.len(), desired_col: None },
        );
        self.reshape(fs);
    }

    fn delete_selection_no_reshape(&mut self) {
        let (lo, hi) = self.sel.range();
        if lo == hi {
            return;
        }
        let lo_char = self.rope.byte_to_char(lo);
        let hi_char = self.rope.byte_to_char(hi);
        let removed = self.rope.slice(lo_char..hi_char).to_string();
        self.push_and_apply(EditOp::Delete(removed), lo, Selection::caret(lo));
    }

    pub fn delete_selection(&mut self, fs: &mut FontSystem) {
        if self.read_only || self.sel.is_empty() {
            return;
        }
        self.delete_selection_no_reshape();
        self.reshape(fs);
    }

    pub fn backspace(&mut self, fs: &mut FontSystem) {
        if self.read_only {
            return;
        }
        if !self.sel.is_empty() {
            self.delete_selection(fs);
            return;
        }
        if self.sel.head == 0 {
            return;
        }
        let end_char = self.rope.byte_to_char(self.sel.head);
        let start_char = end_char - 1;
        let start_byte = self.rope.char_to_byte(start_char);
        let removed = self.rope.slice(start_char..end_char).to_string();
        self.push_and_apply(EditOp::Delete(removed), start_byte, Selection::caret(start_byte));
        self.reshape(fs);
    }

    pub fn delete_forward(&mut self, fs: &mut FontSystem) {
        if self.read_only {
            return;
        }
        if !self.sel.is_empty() {
            self.delete_selection(fs);
            return;
        }
        let start_char = self.rope.byte_to_char(self.sel.head);
        if start_char >= self.rope.len_chars() {
            return;
        }
        let removed = self.rope.slice(start_char..start_char + 1).to_string();
        let head = self.sel.head;
        self.push_and_apply(EditOp::Delete(removed), head, Selection::caret(head));
        self.reshape(fs);
    }

    pub fn undo(&mut self, fs: &mut FontSystem) -> bool {
        if self.read_only {
            return false;
        }
        // Undo the entire top group (a typing run is one step, like VSCode).
        let Some(group) = self.history.last().map(|e| e.group) else {
            return false;
        };
        while self.history.last().map_or(false, |e| e.group == group) {
            let edit = self.history.pop().expect("checked above");
            let inverse_op = match &edit.op {
                EditOp::Insert(s) => EditOp::Delete(s.clone()),
                EditOp::Delete(s) => EditOp::Insert(s.clone()),
            };
            self.apply_op(&inverse_op, edit.at_byte);
            self.sel = Selection {
                anchor: edit.sel_before.0,
                head: edit.sel_before.1,
                desired_col: None,
            };
            self.future.push(edit);
        }
        self.pending_stop = true; // typing after an undo starts a fresh group
        self.dirty = true;
        self.reshape(fs);
        true
    }

    pub fn redo(&mut self, fs: &mut FontSystem) -> bool {
        if self.read_only {
            return false;
        }
        // Redo the entire top group, mirroring undo.
        let Some(group) = self.future.last().map(|e| e.group) else {
            return false;
        };
        while self.future.last().map_or(false, |e| e.group == group) {
            let edit = self.future.pop().expect("checked above");
            self.apply_op(&edit.op, edit.at_byte);
            self.sel = Selection {
                anchor: edit.sel_after.0,
                head: edit.sel_after.1,
                desired_col: None,
            };
            self.history.push(edit);
        }
        self.pending_stop = true;
        self.dirty = true;
        self.reshape(fs);
        true
    }

    /// Assign a path to an (untitled) document — used by Save As. Re-derives the
    /// language/extension/tab-name and rebuilds the syntect highlighter so the
    /// buffer picks up syntax colors for the new file type, then reshapes.
    pub fn set_path(&mut self, path: PathBuf, fs: &mut FontSystem) {
        let ext = path
            .extension()
            .map(|e| e.to_string_lossy().to_lowercase())
            .unwrap_or_default();
        self.name = path
            .file_name()
            .map(|f| f.to_string_lossy().into_owned())
            .unwrap_or_else(|| "Untitled".into());
        self.lang = Lang::from_ext(&ext);
        self.hl = crate::highlight::LineCache::new(&ext);
        self.ext = ext;
        self.path = Some(path);
        self.hl_dirty_from = 0;
        self.reshape(fs);
    }

    pub fn save(&mut self) -> std::io::Result<bool> {
        let Some(path) = self.path.clone() else {
            return Ok(false);
        };
        let mut text = self.rope.to_string();
        // files.trimTrailingWhitespace — strip trailing spaces/tabs per line.
        if crate::settings::trim_trailing() {
            let trimmed: Vec<&str> = text.split('\n').map(|l| l.trim_end_matches([' ', '\t'])).collect();
            text = trimmed.join("\n");
        }
        // Preserve THIS file's line ending (detected on open / set for new files).
        if self.eol == "\r\n" {
            text = text.replace("\r\n", "\n").replace('\n', "\r\n");
        }
        std::fs::write(&path, text)?;
        self.dirty = false;
        Ok(true)
    }

    pub fn head_line_col(&self) -> (usize, usize) {
        let line = self.rope.byte_to_line(self.sel.head);
        let line_start = self.rope.line_to_byte(line);
        (line, self.sel.head - line_start)
    }

    /// Caret byte offset (selection head). Used by code completion to find the
    /// identifier prefix ending at the cursor.
    pub fn caret_byte(&self) -> usize {
        self.sel.head
    }

    /// Replace the bytes from `start` to the caret with `text` — used to accept a
    /// completion that replaces the typed prefix. No-op if `start` is past the caret.
    pub fn replace_prefix(&mut self, start: usize, text: &str, fs: &mut FontSystem) {
        if self.read_only {
            return;
        }
        let head = self.sel.head;
        if start > head {
            return;
        }
        self.sel = Selection { anchor: start, head, desired_col: None };
        self.insert_str(text, fs);
    }

    pub fn place(&mut self, byte: usize, extend: bool) {
        self.sel.head = byte;
        if !extend {
            self.sel.anchor = byte;
        }
        self.sel.desired_col = None;
    }

    pub fn move_left(&mut self, extend: bool) {
        if self.sel.head == 0 {
            return;
        }
        let char_idx = self.rope.byte_to_char(self.sel.head);
        let new_byte = self.rope.char_to_byte(char_idx - 1);
        self.place(new_byte, extend);
    }

    pub fn move_right(&mut self, extend: bool) {
        let char_idx = self.rope.byte_to_char(self.sel.head);
        if char_idx >= self.rope.len_chars() {
            return;
        }
        let new_byte = self.rope.char_to_byte(char_idx + 1);
        self.place(new_byte, extend);
    }

    pub fn move_to_line(&mut self, line: usize, extend: bool) {
        let total = self.rope.len_lines();
        let line = line.min(total.saturating_sub(1));
        let line_start_char = self.rope.line_to_char(line);
        let line_slice = self.rope.line(line);
        let mut line_chars = line_slice.len_chars();
        if line_slice
            .chars()
            .last()
            .map(|c| c == '\n')
            .unwrap_or(false)
        {
            line_chars = line_chars.saturating_sub(1);
        }
        let (_, cur_col) = self.head_line_col();
        let want = self.sel.desired_col.unwrap_or(cur_col);
        let target_col = want.min(line_chars);
        let new_byte = self.rope.char_to_byte(line_start_char + target_col);
        self.sel.head = new_byte;
        if !extend {
            self.sel.anchor = new_byte;
        }
        self.sel.desired_col = Some(want);
    }

    pub fn move_up(&mut self, extend: bool) {
        let (line, _) = self.head_line_col();
        if line == 0 {
            return;
        }
        // Skip over any collapsed region above.
        let target = self.first_visible_from(line - 1, false);
        self.move_to_line(target, extend);
    }

    pub fn move_down(&mut self, extend: bool) {
        let (line, _) = self.head_line_col();
        let target = self.first_visible_from(line + 1, true);
        self.move_to_line(target, extend);
    }

    pub fn move_home(&mut self, extend: bool) {
        let (line, _) = self.head_line_col();
        let byte = self.rope.line_to_byte(line);
        self.place(byte, extend);
    }

    pub fn move_end(&mut self, extend: bool) {
        let (line, _) = self.head_line_col();
        let line_slice = self.rope.line(line);
        let mut len_chars = line_slice.len_chars();
        if line_slice
            .chars()
            .last()
            .map(|c| c == '\n')
            .unwrap_or(false)
        {
            len_chars = len_chars.saturating_sub(1);
        }
        let line_start_char = self.rope.line_to_char(line);
        let byte = self.rope.char_to_byte(line_start_char + len_chars);
        self.place(byte, extend);
    }

    /// Select the word (alphanumeric/underscore run) under `byte`; if the click
    /// is on a non-word char, select just that char.
    pub fn select_word(&mut self, byte: usize) {
        let total = self.rope.len_chars();
        if total == 0 {
            return;
        }
        let mut ci = self.rope.byte_to_char(byte.min(self.rope.len_bytes()));
        if ci >= total {
            ci = total - 1;
        }
        let is_word = |c: char| c.is_alphanumeric() || c == '_';
        let here = self.rope.char(ci);
        if !is_word(here) {
            self.sel.anchor = self.rope.char_to_byte(ci);
            self.sel.head = self.rope.char_to_byte((ci + 1).min(total));
            self.sel.desired_col = None;
            return;
        }
        let mut start = ci;
        while start > 0 && is_word(self.rope.char(start - 1)) {
            start -= 1;
        }
        let mut end = ci;
        while end < total && is_word(self.rope.char(end)) {
            end += 1;
        }
        self.sel.anchor = self.rope.char_to_byte(start);
        self.sel.head = self.rope.char_to_byte(end);
        self.sel.desired_col = None;
    }

    /// Byte offset of the previous word boundary (skips whitespace, then a run
    /// of word chars or a run of punctuation).
    pub fn prev_word(&self, byte: usize) -> usize {
        let is_word = |c: char| c.is_alphanumeric() || c == '_';
        let mut ci = self.rope.byte_to_char(byte.min(self.rope.len_bytes()));
        while ci > 0 && self.rope.char(ci - 1).is_whitespace() {
            ci -= 1;
        }
        if ci > 0 && is_word(self.rope.char(ci - 1)) {
            while ci > 0 && is_word(self.rope.char(ci - 1)) {
                ci -= 1;
            }
        } else {
            while ci > 0 {
                let c = self.rope.char(ci - 1);
                if c.is_whitespace() || is_word(c) {
                    break;
                }
                ci -= 1;
            }
        }
        self.rope.char_to_byte(ci)
    }

    /// Byte offset of the next word boundary.
    pub fn next_word(&self, byte: usize) -> usize {
        let is_word = |c: char| c.is_alphanumeric() || c == '_';
        let total = self.rope.len_chars();
        let mut ci = self.rope.byte_to_char(byte.min(self.rope.len_bytes()));
        if ci < total && is_word(self.rope.char(ci)) {
            while ci < total && is_word(self.rope.char(ci)) {
                ci += 1;
            }
        } else if ci < total && !self.rope.char(ci).is_whitespace() {
            while ci < total {
                let c = self.rope.char(ci);
                if c.is_whitespace() || is_word(c) {
                    break;
                }
                ci += 1;
            }
        }
        while ci < total && self.rope.char(ci).is_whitespace() && self.rope.char(ci) != '\n' {
            ci += 1;
        }
        self.rope.char_to_byte(ci)
    }

    pub fn move_word_left(&mut self, extend: bool) {
        let b = self.prev_word(self.sel.head);
        self.place(b, extend);
    }

    pub fn move_word_right(&mut self, extend: bool) {
        let b = self.next_word(self.sel.head);
        self.place(b, extend);
    }

    pub fn delete_word_back(&mut self, fs: &mut FontSystem) {
        if self.read_only {
            return;
        }
        if !self.sel.is_empty() {
            self.delete_selection(fs);
            return;
        }
        let start = self.prev_word(self.sel.head);
        let end = self.sel.head;
        if start >= end {
            return;
        }
        let lo_char = self.rope.byte_to_char(start);
        let hi_char = self.rope.byte_to_char(end);
        let removed = self.rope.slice(lo_char..hi_char).to_string();
        self.push_and_apply(EditOp::Delete(removed), start, Selection::caret(start));
        self.reshape(fs);
    }

    /// Select the whole line under `byte`, including its trailing newline.
    pub fn select_line(&mut self, byte: usize) {
        let line = self.rope.byte_to_line(byte.min(self.rope.len_bytes()));
        let start = self.rope.line_to_byte(line);
        let end = if line + 1 < self.rope.len_lines() {
            self.rope.line_to_byte(line + 1)
        } else {
            self.rope.len_bytes()
        };
        self.sel.anchor = start;
        self.sel.head = end;
        self.sel.desired_col = None;
    }

    pub fn select_all(&mut self) {
        self.sel.anchor = 0;
        self.sel.head = self.rope.len_bytes();
        self.sel.desired_col = None;
    }

    pub fn selected_text(&self) -> Option<String> {
        let (lo, hi) = self.sel.range();
        if lo == hi {
            return None;
        }
        let lo_char = self.rope.byte_to_char(lo);
        let hi_char = self.rope.byte_to_char(hi);
        Some(self.rope.slice(lo_char..hi_char).to_string())
    }

    // ---- Line & selection operations (Edit / Selection menus) ----

    fn slice_str(&self, lo: usize, hi: usize) -> String {
        self.rope.slice(self.rope.byte_to_char(lo)..self.rope.byte_to_char(hi)).to_string()
    }

    /// `(first_line, last_line)` covered by the selection. A selection ending at
    /// column 0 of a line doesn't include that line (VSCode).
    fn sel_lines(&self) -> (usize, usize) {
        let (lo, hi) = self.sel.range();
        let fl = self.rope.byte_to_line(lo);
        let mut ll = self.rope.byte_to_line(hi.min(self.rope.len_bytes()));
        if ll > fl && hi == self.rope.line_to_byte(ll) {
            ll -= 1;
        }
        (fl, ll)
    }

    /// Byte span of full lines `fl..=ll` (start of `fl` .. start of `ll+1`, i.e.
    /// including the trailing newline when there is one).
    fn line_span(&self, fl: usize, ll: usize) -> (usize, usize) {
        let bs = self.rope.line_to_byte(fl);
        let be = if ll + 1 >= self.rope.len_lines() {
            self.rope.len_bytes()
        } else {
            self.rope.line_to_byte(ll + 1)
        };
        (bs, be)
    }

    /// Move the selected lines up or down by one line, as one undo step. The
    /// selection rides along with the moved text (VSCode Alt+Up/Down).
    pub fn move_lines(&mut self, down: bool, fs: &mut FontSystem) {
        if self.read_only {
            return;
        }
        let (fl, ll) = self.sel_lines();
        let (bs, be) = self.line_span(fl, ll);
        let len = self.rope.len_bytes();
        let sel = self.sel;
        self.break_undo_group();
        if down {
            if be >= len {
                return; // already at the bottom
            }
            let (_, ne) = self.line_span(ll + 1, ll + 1);
            let next = self.slice_str(be, ne);
            let (del_at, del, ins) = if next.ends_with('\n') {
                (be, next.clone(), next)
            } else {
                // The next line is the unterminated last line: move the block's own
                // trailing newline with it so the file still ends without one. The
                // deletion sits at/after the block's end, so selection bytes inside
                // the block only shift by the insert below.
                (be - 1, format!("\n{next}"), format!("{next}\n"))
            };
            self.push_and_apply(EditOp::Delete(del), del_at, Selection::caret(del_at));
            self.force_join = true;
            let d = ins.len();
            let after = Selection {
                anchor: sel.anchor + d,
                head: sel.head + d,
                desired_col: None,
            };
            self.push_and_apply(EditOp::Insert(ins), bs, after);
        } else {
            if fl == 0 {
                return; // already at the top
            }
            let ps = self.rope.line_to_byte(fl - 1);
            let prev = self.slice_str(ps, bs); // always ends with '\n'
            let pl = prev.len();
            self.push_and_apply(EditOp::Delete(prev.clone()), ps, Selection::caret(ps));
            self.force_join = true;
            let block_ends_nl = be > bs && self.rope.byte(be - pl - 1) == b'\n';
            let (ins_at, ins) = if block_ends_nl {
                (be - pl, prev)
            } else {
                // Block is the unterminated tail: re-attach the moved line below it
                // with a leading newline instead of its trailing one.
                (be - pl, format!("\n{}", &prev[..pl - 1]))
            };
            let after = Selection {
                anchor: sel.anchor.saturating_sub(pl),
                head: sel.head.saturating_sub(pl),
                desired_col: None,
            };
            self.push_and_apply(EditOp::Insert(ins), ins_at, after);
        }
        self.reshape(fs);
    }

    /// Duplicate the selected lines above/below (VSCode Copy Line Up/Down). The
    /// caret stays on the upper copy for "up" and rides to the lower copy for "down".
    pub fn copy_lines(&mut self, down: bool, fs: &mut FontSystem) {
        if self.read_only {
            return;
        }
        let (fl, ll) = self.sel_lines();
        let (bs, be) = self.line_span(fl, ll);
        let block = self.slice_str(bs, be);
        let ins = if block.ends_with('\n') { block } else { format!("{block}\n") };
        let sel = self.sel;
        let d = ins.len();
        let after = if down {
            Selection { anchor: sel.anchor + d, head: sel.head + d, desired_col: None }
        } else {
            sel
        };
        self.break_undo_group();
        self.push_and_apply(EditOp::Insert(ins), bs, after);
        self.reshape(fs);
    }

    /// Duplicate the selection after itself (selection moves to the copy); with no
    /// selection, behaves like Copy Line Down.
    pub fn duplicate_selection(&mut self, fs: &mut FontSystem) {
        if self.read_only {
            return;
        }
        if self.sel.is_empty() {
            return self.copy_lines(true, fs);
        }
        let (lo, hi) = self.sel.range();
        let text = self.slice_str(lo, hi);
        let d = text.len();
        self.break_undo_group();
        self.push_and_apply(
            EditOp::Insert(text),
            hi,
            Selection { anchor: hi, head: hi + d, desired_col: None },
        );
        self.reshape(fs);
    }

    /// Toggle the language's line comment on every selected line, as one undo step.
    /// Adds at the block's minimum indent; removes a trailing space after the token.
    /// Languages without a line token (HTML/CSS) fall back to a block comment.
    pub fn toggle_line_comment(&mut self, fs: &mut FontSystem) {
        if self.read_only {
            return;
        }
        let Some((line_tok, _)) = comment_tokens(&self.ext) else { return };
        let Some(tok) = line_tok else {
            return self.toggle_block_comment(fs);
        };
        let (fl, ll) = self.sel_lines();
        // Removing only when every non-blank line is commented (VSCode).
        let mut all = true;
        let mut any = false;
        let mut min_indent = usize::MAX;
        for l in fl..=ll {
            let line = self.rope.line(l).to_string();
            let content = line.trim_end_matches(['\n', '\r']);
            let t = content.trim_start();
            if t.is_empty() {
                continue;
            }
            any = true;
            min_indent = min_indent.min(content.len() - t.len());
            if !t.starts_with(tok) {
                all = false;
            }
        }
        if !any {
            return;
        }
        let removing = all;
        // Selection endpoints as (line, col) so they survive the byte shifts.
        let pos = |b: usize| {
            let l = self.rope.byte_to_line(b.min(self.rope.len_bytes()));
            (l, b - self.rope.line_to_byte(l))
        };
        let (mut a, mut h) = (pos(self.sel.anchor), pos(self.sel.head));
        self.break_undo_group();
        let mut first = true;
        for l in (fl..=ll).rev() {
            let ls = self.rope.line_to_byte(l);
            let line = self.rope.line(l).to_string();
            let content = line.trim_end_matches(['\n', '\r']);
            let t = content.trim_start();
            if t.is_empty() {
                continue;
            }
            let indent = content.len() - t.len();
            let keep = self.sel;
            if removing {
                let mut del = tok.len();
                if t[tok.len()..].starts_with(' ') {
                    del += 1;
                }
                let text = content[indent..indent + del].to_string();
                if !first {
                    self.force_join = true;
                }
                self.push_and_apply(EditOp::Delete(text), ls + indent, keep);
                for p in [&mut a, &mut h] {
                    if p.0 == l && p.1 > indent {
                        p.1 = p.1.saturating_sub(del).max(indent);
                    }
                }
            } else {
                let text = format!("{tok} ");
                let d = text.len();
                if !first {
                    self.force_join = true;
                }
                self.push_and_apply(EditOp::Insert(text), ls + min_indent, keep);
                for p in [&mut a, &mut h] {
                    if p.0 == l && p.1 >= min_indent {
                        p.1 += d;
                    }
                }
            }
            first = false;
        }
        let back = |(l, c): (usize, usize)| {
            let ls = self.rope.line_to_byte(l);
            let ll = self.rope.line(l).len_bytes();
            ls + c.min(ll)
        };
        self.sel = Selection { anchor: back(a), head: back(h), desired_col: None };
        self.reshape(fs);
    }

    /// Wrap the selection in (or strip) the language's block comment, one undo step.
    /// An empty selection wraps the caret's whole line.
    pub fn toggle_block_comment(&mut self, fs: &mut FontSystem) {
        if self.read_only {
            return;
        }
        let Some((_, block)) = comment_tokens(&self.ext) else { return };
        let Some((open, close)) = block else { return };
        if self.sel.is_empty() {
            // Wrap the caret's line content (sans indent / newline).
            let l = self.rope.byte_to_line(self.sel.head.min(self.rope.len_bytes()));
            let ls = self.rope.line_to_byte(l);
            let line = self.rope.line(l).to_string();
            let content = line.trim_end_matches(['\n', '\r']);
            let indent = content.len() - content.trim_start().len();
            self.sel = Selection {
                anchor: ls + indent,
                head: ls + content.len(),
                desired_col: None,
            };
            if self.sel.is_empty() {
                return; // blank line: nothing to wrap
            }
        }
        let (lo, hi) = self.sel.range();
        let text = self.slice_str(lo, hi);
        let inner = text.trim();
        self.break_undo_group();
        if inner.starts_with(open) && inner.ends_with(close) && inner.len() >= open.len() + close.len() {
            // Strip: delete the close token (plus one leading space) first so the
            // open token's offsets stay valid, then the open token.
            let os = lo + (text.len() - text.trim_start().len());
            let ce = lo + text.trim_end().len();
            let mut cs = ce - close.len();
            if self.slice_str(cs - 1, cs) == " " {
                cs -= 1;
            }
            let mut oe = os + open.len();
            if self.slice_str(oe, oe + 1) == " " {
                oe += 1;
            }
            self.push_and_apply(EditOp::Delete(self.slice_str(cs, ce)), cs, self.sel);
            self.force_join = true;
            self.push_and_apply(EditOp::Delete(self.slice_str(os, oe)), os, self.sel);
            let d = oe - os;
            self.sel = Selection { anchor: lo, head: cs - d, desired_col: None };
        } else {
            let close_t = format!(" {close}");
            let open_t = format!("{open} ");
            self.push_and_apply(EditOp::Insert(close_t), hi, self.sel);
            self.force_join = true;
            let d = open_t.len();
            self.push_and_apply(
                EditOp::Insert(open_t),
                lo,
                Selection { anchor: lo + d, head: hi + d, desired_col: None },
            );
        }
        self.reshape(fs);
    }

    /// Innermost bracket pair strictly enclosing `[lo, hi)`. Returns the pair's
    /// (open_byte, close_byte). Naive scan (ignores brackets inside strings).
    fn enclosing_bracket(&self, lo: usize, hi: usize) -> Option<(usize, usize)> {
        let pairs: &[(char, char)] = &[('(', ')'), ('[', ']'), ('{', '}')];
        let lo_c = self.rope.byte_to_char(lo);
        // Walk backwards from lo to the nearest unmatched open bracket.
        let mut depth = [0i32; 3];
        let mut idx = lo_c;
        let mut open: Option<(usize, usize)> = None; // (pair idx, char idx)
        while idx > 0 {
            idx -= 1;
            let c = self.rope.char(idx);
            for (pi, (o, cl)) in pairs.iter().enumerate() {
                if c == *cl {
                    depth[pi] += 1;
                } else if c == *o {
                    if depth[pi] == 0 {
                        open = Some((pi, idx));
                    } else {
                        depth[pi] -= 1;
                    }
                }
            }
            if open.is_some() {
                break;
            }
        }
        let (pi, oc) = open?;
        // Walk forwards from the open bracket to its matching close.
        let (o, cl) = pairs[pi];
        let mut d = 0i32;
        let total = self.rope.len_chars();
        let mut j = oc;
        while j + 1 < total {
            j += 1;
            let c = self.rope.char(j);
            if c == o {
                d += 1;
            } else if c == cl {
                if d == 0 {
                    let ob = self.rope.char_to_byte(oc);
                    let cb = self.rope.char_to_byte(j);
                    if cb >= hi {
                        return Some((ob, cb));
                    }
                    return None; // pair closes before the range ends — not enclosing
                }
                d -= 1;
            }
        }
        None
    }

    /// Grow the selection: word → bracket contents → brackets included → outer
    /// brackets → full lines → whole document. Each step is undone by
    /// `shrink_selection`.
    pub fn expand_selection(&mut self) {
        let cur = self.sel.range();
        if self.sel.is_empty() {
            self.expand_stack.push(cur);
            self.select_word(self.sel.head);
            return;
        }
        let (lo, hi) = cur;
        if let Some((ob, cb)) = self.enclosing_bracket(lo, hi) {
            let content = (ob + 1, cb);
            let next = if cur != content && content.0 <= lo && content.1 >= hi {
                content
            } else {
                (ob, cb + 1) // already the contents → include the brackets
            };
            if next != cur {
                self.expand_stack.push(cur);
                self.sel = Selection { anchor: next.0, head: next.1, desired_col: None };
                return;
            }
        }
        let (fl, ll) = self.sel_lines();
        let (bs, be) = self.line_span(fl, ll);
        if (bs, be) != cur {
            self.expand_stack.push(cur);
            self.sel = Selection { anchor: bs, head: be, desired_col: None };
            return;
        }
        let all = (0, self.rope.len_bytes());
        if all != cur {
            self.expand_stack.push(cur);
            self.sel = Selection { anchor: all.0, head: all.1, desired_col: None };
        }
    }

    /// Undo one `expand_selection` step.
    pub fn shrink_selection(&mut self) {
        if let Some((a, h)) = self.expand_stack.pop() {
            let max = self.rope.len_bytes();
            self.sel = Selection { anchor: a.min(max), head: h.min(max), desired_col: None };
        }
    }

    /// Jump to the bracket matching the one at/under the caret; if the caret isn't
    /// on a bracket, jump to the close of the nearest enclosing pair (VSCode).
    pub fn goto_bracket(&mut self) {
        let pairs: &[(char, char)] = &[('(', ')'), ('[', ']'), ('{', '}')];
        let head = self.sel.head.min(self.rope.len_bytes());
        let hc = self.rope.byte_to_char(head);
        let total = self.rope.len_chars();
        // The bracket "at" the caret: the char after it, else the char before.
        let cand = [(hc, false), (hc.saturating_sub(1), true)];
        for (ci, before) in cand {
            if ci >= total || (before && hc == 0) {
                continue;
            }
            let c = self.rope.char(ci);
            if let Some((pi, is_open)) = pairs
                .iter()
                .enumerate()
                .find_map(|(i, (o, cl))| {
                    (c == *o).then_some((i, true)).or((c == *cl).then_some((i, false)))
                })
            {
                let (o, cl) = pairs[pi];
                let mut d = 0i32;
                if is_open {
                    let mut j = ci;
                    while j + 1 < total {
                        j += 1;
                        let ch = self.rope.char(j);
                        if ch == o {
                            d += 1;
                        } else if ch == cl {
                            if d == 0 {
                                self.place(self.rope.char_to_byte(j), false);
                                return;
                            }
                            d -= 1;
                        }
                    }
                } else {
                    let mut j = ci;
                    while j > 0 {
                        j -= 1;
                        let ch = self.rope.char(j);
                        if ch == cl {
                            d += 1;
                        } else if ch == o {
                            if d == 0 {
                                self.place(self.rope.char_to_byte(j), false);
                                return;
                            }
                            d -= 1;
                        }
                    }
                }
                return;
            }
        }
        if let Some((_, cb)) = self.enclosing_bracket(head, head) {
            self.place(cb, false);
        }
    }
}

/// `(line_token, block_pair)` for a file extension, VSCode's language defaults.
/// `None` line token with a `Some` block pair (HTML/CSS) means "toggle line
/// comment" wraps the line in the block pair instead.
pub fn comment_tokens(ext: &str) -> Option<(Option<&'static str>, Option<(&'static str, &'static str)>)> {
    Some(match ext {
        "rs" | "js" | "jsx" | "ts" | "tsx" | "mjs" | "cjs" | "c" | "h" | "cpp" | "hpp" | "cc"
        | "java" | "go" | "swift" | "kt" | "kts" | "cs" | "scala" | "dart" | "json" | "jsonc"
        | "mcp" | "scss" | "less" | "proto" | "zig" | "glsl" | "wgsl" | "vert" | "frag" => {
            (Some("//"), Some(("/*", "*/")))
        }
        "py" | "sh" | "bash" | "zsh" | "fish" | "rb" | "pl" | "yaml" | "yml" | "toml" | "conf"
        | "ini" | "cfg" | "env" | "dockerfile" | "makefile" | "mk" | "cmake" | "r" | "ex"
        | "exs" | "tf" | "nix" | "gitignore" => (Some("#"), None),
        "lua" | "sql" => (Some("--"), None),
        "html" | "htm" | "xml" | "svg" | "vue" | "md" | "markdown" => {
            (None, Some(("<!--", "-->")))
        }
        "css" => (None, Some(("/*", "*/"))),
        "vim" => (Some("\""), None),
        "el" | "lisp" | "clj" | "cljs" => (Some(";;"), None),
        "hs" | "elm" => (Some("--"), Some(("{-", "-}"))),
        _ => return None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::lsp::{Diagnostic, Severity};

    fn diag(sl: u32, sc: u32, el: u32, ec: u32) -> Diagnostic {
        Diagnostic {
            start_line: sl,
            start_char: sc,
            end_line: el,
            end_char: ec,
            severity: Severity::Warning,
            message: String::new(),
            source: None,
            code: None,
            code_href: None,
        }
    }

    // Squiggles must stay glued to their text between server publishes: inserting
    // lines above shifts them down, same-line inserts shift columns, deletes shift
    // back, and undo round-trips exactly.
    #[test]
    fn diagnostics_follow_edits() {
        let mut fs = glyphon::FontSystem::new();
        let mut d = Document::new(None, "line0\nline1\nline2\n".into(), &mut fs);
        d.diagnostics = vec![diag(2, 0, 2, 5)]; // underlines "line2"

        // Newline inserted at the top → range moves down a line.
        d.place(0, false);
        d.insert_str("\n", &mut fs);
        assert_eq!((d.diagnostics[0].start_line, d.diagnostics[0].end_line), (3, 3));

        // Undo (inverse delete) → back where it was.
        d.undo(&mut fs);
        assert_eq!((d.diagnostics[0].start_line, d.diagnostics[0].end_line), (2, 2));

        // Same-line insert BEFORE the range → columns shift right.
        let b = d.rope.line_to_byte(2);
        d.place(b, false);
        d.insert_str("xx", &mut fs);
        assert_eq!((d.diagnostics[0].start_char, d.diagnostics[0].end_char), (2, 7));
        assert_eq!(d.diagnostics[0].start_line, 2);

        // Insert AFTER the range on the same line → untouched.
        let after = d.rope.line_to_byte(2) + 7;
        d.place(after, false);
        d.insert_str("yy", &mut fs);
        assert_eq!((d.diagnostics[0].start_char, d.diagnostics[0].end_char), (2, 7));
    }

    // Monaco-style undo grouping: a typed run is ONE undo step; spaces start the
    // next word's group; Enter stands alone; cursor moves break the run; redo
    // replays a whole group.
    #[test]
    fn undo_groups_typed_runs() {
        let mut fs = glyphon::FontSystem::new();
        let mut d = Document::new(None, String::new(), &mut fs);
        for ch in ["d", "a", "f", "d", "s", "f"] {
            d.insert_str(ch, &mut fs);
        }
        assert_eq!(d.text(), "dafdsf");
        d.undo(&mut fs);
        assert_eq!(d.text(), "", "quickly typed run undoes as one step");
        d.redo(&mut fs);
        assert_eq!(d.text(), "dafdsf", "redo replays the whole group");

        // "hello world": space starts the next group → undo peels "( world)" then "hello".
        let mut d = Document::new(None, String::new(), &mut fs);
        for ch in "hello world".chars() {
            d.insert_str(&ch.to_string(), &mut fs);
        }
        d.undo(&mut fs);
        assert_eq!(d.text(), "hello", "undo removes the second word group");
        d.undo(&mut fs);
        assert_eq!(d.text(), "");

        // A cursor move breaks the run.
        let mut d = Document::new(None, String::new(), &mut fs);
        d.insert_str("ab", &mut fs);
        d.place(1, false); // move between 'a' and 'b'
        d.insert_str("x", &mut fs);
        d.undo(&mut fs);
        assert_eq!(d.text(), "ab", "edit after a cursor move undoes alone");
    }

    // ---- Line & selection ops ----

    fn doc(text: &str, fs: &mut glyphon::FontSystem) -> Document {
        Document::new(Some(std::path::PathBuf::from("t.rs")), text.into(), fs)
    }

    #[test]
    fn move_lines_up_down_roundtrip() {
        let mut fs = glyphon::FontSystem::new();
        let mut d = doc("aa\nbb\ncc", &mut fs);
        // caret on "bb"; move down swaps with "cc" (no trailing newline case)
        d.place(3, false);
        d.move_lines(true, &mut fs);
        assert_eq!(d.text(), "aa\ncc\nbb");
        assert_eq!(d.rope.byte_to_line(d.sel.head), 2); // caret rode along
        // one undo restores both edits
        d.undo(&mut fs);
        assert_eq!(d.text(), "aa\nbb\ncc");
        // move up swaps with "aa"
        d.place(3, false);
        d.move_lines(false, &mut fs);
        assert_eq!(d.text(), "bb\naa\ncc");
        assert_eq!(d.rope.byte_to_line(d.sel.head), 0);
        // moving the unterminated last line up keeps the file unterminated
        let mut d2 = doc("aa\nbb", &mut fs);
        d2.place(4, false);
        d2.move_lines(false, &mut fs);
        assert_eq!(d2.text(), "bb\naa");
        // edges are no-ops
        d2.place(0, false);
        d2.move_lines(false, &mut fs);
        assert_eq!(d2.text(), "bb\naa");
        d2.place(4, false);
        d2.move_lines(true, &mut fs);
        assert_eq!(d2.text(), "bb\naa");
    }

    #[test]
    fn copy_and_duplicate_lines() {
        let mut fs = glyphon::FontSystem::new();
        let mut d = doc("aa\nbb", &mut fs);
        d.place(0, false);
        d.copy_lines(true, &mut fs); // copy down: caret rides to the lower copy
        assert_eq!(d.text(), "aa\naa\nbb");
        assert_eq!(d.rope.byte_to_line(d.sel.head), 1);
        d.undo(&mut fs);
        assert_eq!(d.text(), "aa\nbb");
        d.copy_lines(false, &mut fs); // copy up: caret stays on the upper copy
        assert_eq!(d.text(), "aa\naa\nbb");
        assert_eq!(d.rope.byte_to_line(d.sel.head), 0);
        // duplicate a selection: copy goes after, selection moves to it
        let mut d2 = doc("hello", &mut fs);
        d2.sel = Selection { anchor: 0, head: 5, desired_col: None };
        d2.duplicate_selection(&mut fs);
        assert_eq!(d2.text(), "hellohello");
        assert_eq!(d2.sel.range(), (5, 10));
    }

    #[test]
    fn line_comment_toggles() {
        let mut fs = glyphon::FontSystem::new();
        let mut d = doc("fn main() {\n    let x = 1;\n    let y = 2;\n}\n", &mut fs);
        // select both let-lines and comment them at the common indent
        let lo = d.rope.line_to_byte(1);
        let hi = d.rope.line_to_byte(3);
        d.sel = Selection { anchor: lo, head: hi, desired_col: None };
        d.toggle_line_comment(&mut fs);
        assert_eq!(d.text(), "fn main() {\n    // let x = 1;\n    // let y = 2;\n}\n");
        // toggling again removes — and one undo would restore the whole block
        d.toggle_line_comment(&mut fs);
        assert_eq!(d.text(), "fn main() {\n    let x = 1;\n    let y = 2;\n}\n");
        d.undo(&mut fs);
        assert_eq!(d.text(), "fn main() {\n    // let x = 1;\n    // let y = 2;\n}\n");
        // caret column survives a single-line toggle
        let mut d2 = doc("let x = 1;", &mut fs);
        d2.place(5, false);
        d2.toggle_line_comment(&mut fs);
        assert_eq!(d2.text(), "// let x = 1;");
        assert_eq!(d2.sel.head, 8); // shifted by "// "
        // hash languages
        let mut d3 = Document::new(Some(std::path::PathBuf::from("t.py")), "x = 1".into(), &mut fs);
        d3.toggle_line_comment(&mut fs);
        assert_eq!(d3.text(), "# x = 1");
    }

    #[test]
    fn block_comment_toggles() {
        let mut fs = glyphon::FontSystem::new();
        let mut d = doc("abc", &mut fs);
        d.sel = Selection { anchor: 0, head: 3, desired_col: None };
        d.toggle_block_comment(&mut fs);
        assert_eq!(d.text(), "/* abc */");
        // strip it back off (selection covers the whole comment now? select all)
        d.select_all();
        d.toggle_block_comment(&mut fs);
        assert_eq!(d.text(), "abc");
        // empty selection wraps the caret line; html uses <!-- -->
        let mut d2 = Document::new(Some(std::path::PathBuf::from("t.html")), "<b>hi</b>".into(), &mut fs);
        d2.place(2, false);
        d2.toggle_line_comment(&mut fs); // html has no line token → block fallback
        assert_eq!(d2.text(), "<!-- <b>hi</b> -->");
    }

    #[test]
    fn expand_shrink_and_goto_bracket() {
        let mut fs = glyphon::FontSystem::new();
        let mut d = doc("fn f(aa, (bb)) {}", &mut fs);
        // caret inside "bb"
        d.place(11, false);
        d.expand_selection();
        assert_eq!(d.sel.range(), (10, 12)); // word "bb"
        d.expand_selection();
        assert_eq!(d.sel.range(), (9, 13)); // "(bb)" — contents == word, so include brackets
        d.expand_selection();
        assert_eq!(d.sel.range(), (5, 13)); // contents of f(...)
        d.expand_selection();
        assert_eq!(d.sel.range(), (4, 14)); // include f's parens
        d.shrink_selection();
        assert_eq!(d.sel.range(), (5, 13));
        d.shrink_selection();
        assert_eq!(d.sel.range(), (9, 13));
        // goto bracket: caret on "(" jumps to its ")"
        d.place(4, false);
        d.goto_bracket();
        assert_eq!(d.sel.head, 13);
        d.goto_bracket(); // and back (caret sits on ")")
        assert_eq!(d.sel.head, 4);
        // not on a bracket: jump to the enclosing close
        d.place(7, false);
        d.goto_bracket();
        assert_eq!(d.sel.head, 13);
    }
}
