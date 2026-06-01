// One edited file: rope text, selection, scroll, undo/redo, glyphon Buffer.
// Edits go through push_and_apply so undo/redo stays consistent.

use std::path::PathBuf;

use glyphon::{Attrs, Buffer, Color, Family, FontSystem, Metrics, Shaping, Wrap};
use ropey::Rope;

use crate::syntax::{self, Lang};
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
    pub buffer: Buffer,
    lang: Lang,
    ext: String,
    wrap_width: Option<f32>, // Some(w) when word-wrap is on (wraps at w px)
    eol: String,             // this file's actual line ending ("\n" or "\r\n")
    pub read_only: bool,     // diff views (and future previews) reject edits
    pub diff: Option<crate::diff::Diff>, // Some => this tab is a git diff view
    pub diff_right: Option<Buffer>,      // side-by-side: `buffer` = old/left, this = new/right
    pub image: Option<String>,           // Some(media key) => this tab renders an image
    pub image_scale: Option<f32>,        // None = fit-to-window; Some(s) = absolute scale
    pub image_pan: (f32, f32),           // pan offset (px) from centered position
    pub feedback: bool,                  // Some => Ctrl+Enter submits it as a GitHub issue
    pub version: i32,                    // LSP document version (bumped on every edit)
    pub diagnostics: Vec<crate::lsp::Diagnostic>, // current LSP diagnostics for this doc
    pub lsp_dirty: bool,                 // text changed since the last didChange was sent
    pub lsp_open: bool,                  // a didOpen has been sent to a server for this doc
    hl: Option<crate::highlight::LineCache>, // syntect incremental highlighter (None = no grammar)
    hl_dirty_from: usize,                // lowest line changed since the last highlight (usize::MAX = none)
    semantic: Vec<(usize, usize, Color)>, // Layer-2 LSP semantic tokens (byte range → color)
}

/// Set the buffer's metrics/wrap/size and its (rich) text. `spans` are precomputed
/// by the caller (via the syntect `LineCache`); when `None`, falls back to markdown
/// line styling or plain text.
fn apply_buffer_text(
    buffer: &mut Buffer,
    fs: &mut FontSystem,
    text: &str,
    lines: usize,
    lang: Lang,
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
        None => (lang == Lang::Markdown).then(|| md_spans(text)),
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
        apply_buffer_text(&mut buffer, fs, &display, display.matches('\n').count(), lang, wrap_width, spans, &[]);
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
            future: Vec::new(),
            buffer,
            lang,
            ext,
            wrap_width,
            eol,
            read_only: false,
            diff: None,
            diff_right: None,
            image: None,
            image_scale: None,
            image_pan: (0.0, 0.0),
            feedback: false,
            version: 0,
            diagnostics: Vec::new(),
            lsp_dirty: false,
            lsp_open: false,
            hl,
            hl_dirty_from: usize::MAX,
            semantic: Vec::new(),
        }
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
            future: Vec::new(),
            buffer,
            lang: Lang::PlainText,
            ext: String::new(),
            wrap_width: None,
            eol: "\n".to_string(),
            read_only: true,
            diff: None,
            diff_right: None,
            image: Some(key),
            image_scale: None,
            image_pan: (0.0, 0.0),
            feedback: false,
            version: 0,
            diagnostics: Vec::new(),
            lsp_dirty: false,
            lsp_open: false,
            hl: None,
            hl_dirty_from: usize::MAX,
            semantic: Vec::new(),
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
            apply_buffer_text(&mut b, fs, &display, display.matches('\n').count(), Lang::PlainText, None, None, &[]);
            b
        };
        let buffer = mk(fs, &diff.left_text);
        let diff_right = Some(mk(fs, &diff.right_text));
        Self {
            path: None,
            name: diff.title.clone(),
            rope: Rope::from_str(&diff.left_text),
            sel: Selection::caret(0),
            scroll: ScrollView::new(ScrollOpts::both()),
            dirty: false,
            history: Vec::new(),
            future: Vec::new(),
            buffer,
            lang: Lang::PlainText,
            ext: String::new(),
            wrap_width: None,
            eol: "\n".to_string(),
            read_only: true,
            diff: Some(diff),
            diff_right,
            image: None,
            image_scale: None,
            image_pan: (0.0, 0.0),
            feedback: false,
            version: 0,
            diagnostics: Vec::new(),
            lsp_dirty: false,
            lsp_open: false,
            hl: None,
            hl_dirty_from: usize::MAX,
            semantic: Vec::new(),
        }
    }

    /// This file's line ending: "\n" or "\r\n".
    pub fn eol(&self) -> &str {
        &self.eol
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
        let (line, col_byte) = self.head_line_col();
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
        apply_buffer_text(&mut self.buffer, fs, &text, lines, self.lang, self.wrap_width, spans, &self.semantic);
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

    fn push_and_apply(&mut self, op: EditOp, at_byte: usize, sel_after: Selection) {
        let sel_before = (self.sel.anchor, self.sel.head);
        let sel_after_t = (sel_after.anchor, sel_after.head);
        self.apply_op(&op, at_byte);
        self.history.push(Edit {
            at_byte,
            op,
            sel_before,
            sel_after: sel_after_t,
        });
        self.future.clear();
        self.sel = sel_after;
        self.dirty = true;
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
        let Some(edit) = self.history.pop() else {
            return false;
        };
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
        self.dirty = true;
        self.reshape(fs);
        true
    }

    pub fn redo(&mut self, fs: &mut FontSystem) -> bool {
        if self.read_only {
            return false;
        }
        let Some(edit) = self.future.pop() else {
            return false;
        };
        self.apply_op(&edit.op, edit.at_byte);
        self.sel = Selection {
            anchor: edit.sel_after.0,
            head: edit.sel_after.1,
            desired_col: None,
        };
        self.history.push(edit);
        self.dirty = true;
        self.reshape(fs);
        true
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
        self.move_to_line(line - 1, extend);
    }

    pub fn move_down(&mut self, extend: bool) {
        let (line, _) = self.head_line_col();
        self.move_to_line(line + 1, extend);
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

    pub fn find_next(&mut self, needle: &str, from_byte: usize) -> Option<usize> {
        if needle.is_empty() {
            return None;
        }
        let text = self.rope.to_string();
        let start = from_byte.min(text.len());
        if let Some(rel) = text[start..].find(needle) {
            return Some(start + rel);
        }
        text[..start].find(needle)
    }

    pub fn find_prev(&mut self, needle: &str, from_byte: usize) -> Option<usize> {
        if needle.is_empty() {
            return None;
        }
        let text = self.rope.to_string();
        let end = from_byte.min(text.len());
        if let Some(pos) = text[..end].rfind(needle) {
            return Some(pos);
        }
        text.rfind(needle)
    }
}
