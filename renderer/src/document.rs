// One edited file: rope text, selection, scroll, undo/redo, glyphon Buffer.
// Edits go through push_and_apply so undo/redo stays consistent.

use std::path::PathBuf;

use glyphon::{Attrs, Buffer, Color, Family, FontSystem, Metrics, Shaping, Wrap};
use ropey::Rope;

use crate::syntax::Lang;
use crate::theme;
use crate::widgets::{Axis, Rect, Scrollbar, ScrollOpts, ScrollView};

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
    // Side-by-side diff: each pane scrolls horizontally on its own (the right pane's
    // long lines must be reachable even when the left side is short). [0]=left/old,
    // [1]=right/new. Cell so the &self render loop can clamp them per frame.
    diff_hscroll: [std::cell::Cell<f32>; 2],
    diff_hbar: [crate::widgets::Scrollbar; 2], // per-pane horizontal scrollbar drag state
    // Repo-relative path + staged flag of a single-file diff, so per-block
    // Stage/Unstage/Revert can build and apply a patch. None ⇒ not a file diff.
    pub diff_path: Option<String>,
    pub diff_staged: bool,
    /// Commit-graph view (Visualize Repository History). Some ⇒ this read-only tab
    /// renders the laid-out graph instead of file text.
    pub graph: Option<crate::graph::Graph>,
    pub folds: std::collections::BTreeMap<usize, usize>, // folded regions: header line → last hidden line
    pub image: Option<String>,           // Some(media key) => this tab renders an image
    pub image_scale: Option<f32>,        // None = fit-to-window; Some(s) = absolute scale
    pub image_pan: (f32, f32),           // pan offset (px) from centered position
    pub feedback: bool,                  // Some => Ctrl+Enter submits it as a GitHub issue
    /// True ⇒ binary / unsupported-encoding file: the editor shows a placeholder
    /// ("not displayed … Open Anyway") instead of garbled text. Cleared by `open_anyway`.
    pub binary: bool,
    /// Display label of the text encoding this doc was decoded with (status bar).
    pub encoding: &'static str,
    pub version: i32,                    // LSP document version (bumped on every edit)
    pub diagnostics: Vec<crate::lsp::Diagnostic>, // current LSP diagnostics for this doc
    pub breakpoints: std::collections::HashSet<usize>, // debug breakpoints (0-based lines)
    pub blame: Vec<crate::git::BlameLine>, // inline git blame, indexed by 0-based line (empty = none/pending)
    pub blame_requested: bool, // a blame fetch has been kicked off for the current content
    pub execution_line: Option<usize>,   // current debug execution line (0-based), if stopped here
    pub lsp_dirty: bool,                 // text changed since the last didChange was sent
    pub lsp_servers: Vec<&'static str>,  // servers a didOpen has been sent to (open-state is per-server)
    hl: Option<crate::highlight::Highlighter>, // tree-sitter (JS/TS) or syntect incremental highlighter (None = no grammar)
    hl_dirty_from: usize,                // lowest line changed since the last highlight (usize::MAX = none)
    semantic: Vec<(usize, usize, Color)>, // Layer-2 LSP semantic tokens (byte range → color)
    expand_stack: Vec<(usize, usize)>, // prior ranges for Expand/Shrink Selection
    // Matching-bracket highlight memo: (caret, version) → pair. Cell because the
    // render loop only holds &Document; the scan reruns only when the key changes.
    bracket_hl_cache: std::cell::Cell<Option<(usize, i32, Option<(usize, usize)>)>>,
    // Detected indentation unit (in columns) for indent guides, keyed by version so
    // it follows the file's own style (2-space, 4-space, tabs) rather than a setting.
    indent_unit_cache: std::cell::Cell<Option<(i32, usize)>>,
    // Cached widest shaped line (px) for the horizontal scroll range, keyed by
    // (version, shape epoch). Avoids an O(lines) scan of both diff buffers per frame.
    maxw_cache: std::cell::Cell<Option<(i32, u64, f32)>>,
    pub info: Option<crate::ui::info_page::InfoPage>, // Some ⇒ designed info page (Welcome / Tips / Shortcuts)
    pub markdown_preview: Option<crate::markdown::Markdown>, // Some ⇒ rendered markdown preview of `rope`
    /// Large-file mode: only a sliding window of lines is shaped into `buffer`
    /// (shaping a 450k-line file whole takes ~50s). Highlighting, LSP, folding and
    /// word wrap are disabled; geometry helpers translate window↔document lines.
    pub large: bool,
    buf_first: usize, // first document line shaped into the buffer
    buf_count: usize, // how many lines the buffer window holds
}

/// Large-file thresholds (lines / bytes) and the shaped-window size.
pub const LARGE_FILE_LINES: usize = 50_000;
pub const LARGE_FILE_BYTES: usize = 8 * 1024 * 1024;
const LARGE_WINDOW_LINES: usize = 1_500;

/// Replace C0/C1 control characters (except tab and newline) with the Unicode
/// replacement char so the shaper never sees a NUL or other unshapeable control
/// byte — cosmic-text panics (`shape.rs` assertion) on those. Used when force-
/// opening binary / unknown-encoding content as editable text.
fn sanitize_for_display(s: &str) -> String {
    s.chars()
        .map(|c| match c {
            '\t' | '\n' | '\r' => c,
            c if c.is_control() => '\u{FFFD}',
            c => c,
        })
        .collect()
}

/// The window's lines as one string (CR stripped — cosmic-text renders a stray
/// `\r` as an extra line break).
fn window_string(rope: &Rope, first: usize, count: usize) -> String {
    let total = rope.len_lines();
    let first = first.min(total.saturating_sub(1));
    let end = (first + count).min(total);
    let lo = rope.line_to_byte(first);
    let hi = if end >= total { rope.len_bytes() } else { rope.line_to_byte(end) };
    rope.byte_slice(lo..hi).to_string().replace('\r', "")
}

/// Fill `buffer` with a plain-text window (`count` lines). Basic shaping — large
/// files skip rich styling, and logs are overwhelmingly ASCII.
fn shape_window(buffer: &mut Buffer, fs: &mut FontSystem, text: &str, count: usize) {
    buffer.set_metrics(fs, Metrics::new(theme::FONT_SIZE(), theme::LINE_HEIGHT()));
    buffer.set_wrap(fs, Wrap::None);
    buffer.set_size(fs, None, Some((count as f32 + 2.0) * theme::LINE_HEIGHT() + 200.0));
    let mono = Attrs::new().family(Family::Name(theme::MONO_FAMILY()));
    buffer.set_text(fs, text, mono, Shaping::Basic);
    buffer.shape_until_scroll(fs, false);
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
        // Large-file mode (VSCode's largeFileOptimizations): shaping every line of a
        // 450k-line log takes ~50s, so huge docs render through a sliding window of
        // shaped lines instead, with highlighting/LSP/folding disabled.
        let line_count = display.matches('\n').count() + 1;
        let large = line_count > LARGE_FILE_LINES || display.len() > LARGE_FILE_BYTES;
        let rope = Rope::from_str(&contents);
        // Layer-1 highlighter: a syntect grammar for this file type (None → plain/markdown).
        let mut hl = if large { None } else { crate::highlight::Highlighter::new(&ext) };
        let buf_count = if large { LARGE_WINDOW_LINES.min(rope.len_lines()) } else { 0 };
        if large {
            shape_window(&mut buffer, fs, &window_string(&rope, 0, buf_count), buf_count);
        } else {
            let spans = hl.as_mut().map(|h| h.highlight(&display, 0));
            apply_buffer_text(&mut buffer, fs, &display, display.matches('\n').count(), lang, &ext, wrap_width, spans, &[]);
        }
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
            rope,
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
            diff_hscroll: [std::cell::Cell::new(0.0), std::cell::Cell::new(0.0)],
            diff_hbar: [Scrollbar::new(Axis::Horizontal), Scrollbar::new(Axis::Horizontal)],
            diff_path: None,
            diff_staged: false,
            graph: None,
            folds: std::collections::BTreeMap::new(),
            image: None,
            image_scale: None,
            image_pan: (0.0, 0.0),
            feedback: false,
            binary: false,
            encoding: "UTF-8",
            version: 0,
            diagnostics: Vec::new(),
            breakpoints: std::collections::HashSet::new(),
            blame: Vec::new(),
            blame_requested: false,
            execution_line: None,
            lsp_dirty: false,
            lsp_servers: Vec::new(),
            hl,
            hl_dirty_from: usize::MAX,
            semantic: Vec::new(),
            expand_stack: Vec::new(),
            bracket_hl_cache: std::cell::Cell::new(None),
            indent_unit_cache: std::cell::Cell::new(None),
            maxw_cache: std::cell::Cell::new(None),
            info: None,
            markdown_preview: None,
            large,
            buf_first: 0,
            buf_count,
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

    /// A read-only rendered Markdown preview of `source`. `name` is the tab title.
    /// The source text is kept in `rope` (re-rendered on width/zoom changes).
    pub fn new_markdown_preview(name: String, source: String, fs: &mut FontSystem) -> Self {
        let mut d = Document::new(None, source, fs);
        d.name = name;
        d.read_only = true;
        d.markdown_preview = Some(crate::markdown::Markdown::new(fs));
        d
    }

    /// A placeholder tab for a binary / unsupported-encoding file: no text is shaped,
    /// the editor draws the "not displayed … Open Anyway" overlay. `open_anyway`
    /// reloads it as lossy UTF-8 text.
    pub fn new_binary(path: PathBuf, fs: &mut FontSystem) -> Self {
        let mut d = Document::new(Some(path), String::new(), fs);
        d.read_only = true;
        d.binary = true;
        d
    }

    /// Force-load a binary placeholder's bytes as lossy UTF-8 text (VSCode's "Open
    /// Anyway"). Turns the tab into a normal editable document.
    pub fn open_anyway(&mut self, fs: &mut FontSystem) {
        let Some(path) = self.path.clone() else { return };
        let Ok(bytes) = std::fs::read(&path) else { return };
        let text = sanitize_for_display(&String::from_utf8_lossy(&bytes));
        *self = Document::new(Some(path), text, fs);
    }

    /// Re-read the file from disk and decode it with `encoding` (VSCode's "Reopen
    /// with Encoding"). Replaces content; records the label for the status bar.
    pub fn reopen_with_encoding(&mut self, encoding: &'static str, fs: &mut FontSystem) {
        let Some(path) = self.path.clone() else { return };
        let Ok(bytes) = std::fs::read(&path) else { return };
        let text = sanitize_for_display(&crate::encoding::decode(encoding, &bytes));
        *self = Document::new(Some(path), text, fs);
        self.encoding = encoding;
    }

    /// A read-only commit-graph tab (Visualize Repository History). The buffer holds
    /// one line per commit (inline refs + subject + author/date); the renderer draws
    /// the lane graph to the left and offsets this text past it.
    pub fn new_graph(graph: crate::graph::Graph, fs: &mut FontSystem) -> Self {
        let mut text = String::new();
        for r in &graph.rows {
            let refs: String = r.refs.iter().map(|rf| format!("‹{}› ", rf.label)).collect();
            // Truncate the subject so rows don't run long; full message shows on hover.
            let subject: String = if r.subject.chars().count() > 72 {
                let s: String = r.subject.chars().take(71).collect();
                format!("{s}…")
            } else {
                r.subject.clone()
            };
            text.push_str(&format!("{}  {}{}    {} · {}\n", r.short, refs, subject, r.author, r.when));
        }
        let mut d = Document::new(None, text, fs);
        d.name = graph.title.clone();
        d.read_only = true;
        d.graph = Some(graph);
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
            diff_hscroll: [std::cell::Cell::new(0.0), std::cell::Cell::new(0.0)],
            diff_hbar: [Scrollbar::new(Axis::Horizontal), Scrollbar::new(Axis::Horizontal)],
            diff_path: None,
            diff_staged: false,
            graph: None,
            folds: std::collections::BTreeMap::new(),
            image: Some(key),
            image_scale: None,
            image_pan: (0.0, 0.0),
            feedback: false,
            binary: false,
            encoding: "UTF-8",
            version: 0,
            diagnostics: Vec::new(),
            breakpoints: std::collections::HashSet::new(),
            blame: Vec::new(),
            blame_requested: false,
            execution_line: None,
            lsp_dirty: false,
            lsp_servers: Vec::new(),
            hl: None,
            hl_dirty_from: usize::MAX,
            semantic: Vec::new(),
            expand_stack: Vec::new(),
            bracket_hl_cache: std::cell::Cell::new(None),
            indent_unit_cache: std::cell::Cell::new(None),
            maxw_cache: std::cell::Cell::new(None),
            info: None,
            markdown_preview: None,
            large: false,
            buf_first: 0,
            buf_count: 0,
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
        // and `diff` start from the fully-expanded projection. Single-file diffs with
        // collapsed unchanged regions likewise keep the full diff and start from the
        // gap-projected view (separator rows in place of hidden runs).
        let (visible, full) = if diff.combined {
            (crate::diff::project(&diff, &std::collections::HashSet::new()), Some(diff))
        } else if !diff.gaps.is_empty() {
            (crate::diff::project_gaps(&diff), Some(diff))
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
            diff_hscroll: [std::cell::Cell::new(0.0), std::cell::Cell::new(0.0)],
            diff_hbar: [Scrollbar::new(Axis::Horizontal), Scrollbar::new(Axis::Horizontal)],
            diff_path: None,
            diff_staged: false,
            graph: None,
            folds: std::collections::BTreeMap::new(),
            image: None,
            image_scale: None,
            image_pan: (0.0, 0.0),
            feedback: false,
            binary: false,
            encoding: "UTF-8",
            version: 0,
            diagnostics: Vec::new(),
            breakpoints: std::collections::HashSet::new(),
            blame: Vec::new(),
            blame_requested: false,
            execution_line: None,
            lsp_dirty: false,
            lsp_servers: Vec::new(),
            hl: None,
            hl_dirty_from: usize::MAX,
            semantic: Vec::new(),
            expand_stack: Vec::new(),
            bracket_hl_cache: std::cell::Cell::new(None),
            indent_unit_cache: std::cell::Cell::new(None),
            maxw_cache: std::cell::Cell::new(None),
            info: None,
            markdown_preview: None,
            large: false,
            buf_first: 0,
            buf_count: 0,
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

    /// Expand a collapsed unchanged region in a single-file diff. `lines` is how
    /// many hidden rows to reveal (`usize::MAX` = all of them); `from_top` reveals
    /// from the top edge of the gap (drag-down) or the bottom (drag-up). Rebuilds
    /// the visible buffers from the re-projected diff.
    pub fn expand_diff_gap(&mut self, gap_idx: usize, lines: usize, from_top: bool, fs: &mut FontSystem) {
        let Some(full) = self.diff_full.as_mut() else { return };
        let Some(gap) = full.gaps.get_mut(gap_idx) else { return };
        let avail = gap.hidden();
        if avail == 0 {
            return;
        }
        let take = lines.min(avail);
        if from_top {
            gap.top += take;
        } else {
            gap.bot += take;
        }
        self.rebuild_diff_view(fs);
    }

    /// A gap's current (reveal-from-top, reveal-from-bottom, total span).
    pub fn diff_gap_info(&self, gap_idx: usize) -> Option<(usize, usize, usize)> {
        let g = self.diff_full.as_ref()?.gaps.get(gap_idx)?;
        Some((g.top, g.bot, g.end - g.start))
    }

    /// Set a gap's reveal-from-top and -from-bottom (clamped so they can't overlap)
    /// and rebuild the visible buffers. Drag-down grows `top`, drag-up grows `bot`.
    pub fn set_diff_gap_reveal(&mut self, gap_idx: usize, top: usize, bot: usize, fs: &mut FontSystem) {
        let Some(full) = self.diff_full.as_mut() else { return };
        let Some(gap) = full.gaps.get_mut(gap_idx) else { return };
        let span = gap.end - gap.start;
        let bot = bot.min(span);
        let top = top.min(span - bot);
        if top == gap.top && bot == gap.bot {
            return;
        }
        gap.top = top;
        gap.bot = bot;
        self.rebuild_diff_view(fs);
    }

    /// Re-project the full single-file diff (with its current gap state) and rebuild
    /// both pane buffers + the left rope from it.
    fn rebuild_diff_view(&mut self, fs: &mut FontSystem) {
        let Some(full) = self.diff_full.as_ref() else { return };
        let vis = crate::diff::project_gaps(full);
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

    /// The gap index of a `Gap` separator row in the visible diff, if `vis_row` is
    /// one (its index is carried in the row's `file` field).
    pub fn diff_gap_at_row(&self, vis_row: usize) -> Option<usize> {
        let d = self.diff.as_ref()?;
        let row = d.rows.get(vis_row)?;
        (row.kind == crate::diff::RowKind::Gap).then_some(row.file)
    }

    /// The gap index under screen-y `y` in a diff `region`. Diff rows are uniform
    /// height, so derive the row arithmetically (the windowed buffer mis-maps under
    /// `Buffer::hit`). Used by click/drag/cursor.
    pub fn diff_gap_at_y(&self, region: Rect, y: f32) -> Option<usize> {
        self.diff_full.as_ref()?;
        let lh = theme::LINE_HEIGHT().max(1.0);
        let rel = y - (region.y + theme::EDITOR_PAD()) + self.scroll_y();
        if rel < 0.0 {
            return None;
        }
        self.diff_gap_at_row((rel / lh) as usize)
    }

    /// The change block `[start, end)` (visible rows) under screen-y `y`, if `y`
    /// falls on an Add/Del row. Drives the per-block Stage/Revert hover buttons.
    pub fn diff_block_at_y(&self, region: Rect, y: f32) -> Option<(usize, usize)> {
        let d = self.diff.as_ref()?;
        self.diff_path.as_ref()?;
        let lh = theme::LINE_HEIGHT().max(1.0);
        let rel = y - (region.y + theme::EDITOR_PAD()) + self.scroll_y();
        if rel < 0.0 {
            return None;
        }
        let row = (rel / lh) as usize;
        crate::diff::change_blocks(d).into_iter().find(|&(s, e)| row >= s && row < e)
    }

    /// Build the patch for a visible change block starting at row `vbs`, mapping it
    /// to the full diff so context/line-numbers are correct even next to a gap.
    pub fn diff_block_patch(&self, vbs: usize) -> Option<String> {
        let vis = self.diff.as_ref()?;
        let path = self.diff_path.as_ref()?;
        let full = self.diff_full.as_ref().unwrap_or(vis);
        let r0 = vis.rows.get(vbs)?;
        let is_change = |k| matches!(k, crate::diff::RowKind::Add | crate::diff::RowKind::Del);
        let del = r0.kind == crate::diff::RowKind::Del;
        let key = if del { r0.left } else { r0.right };
        key?;
        let fi = full
            .rows
            .iter()
            .position(|r| r.kind == r0.kind && (if del { r.left } else { r.right }) == key)?;
        let mut fbs = fi;
        while fbs > 0 && is_change(full.rows[fbs - 1].kind) {
            fbs -= 1;
        }
        let mut fbe = fi;
        while fbe < full.rows.len() && is_change(full.rows[fbe].kind) {
            fbe += 1;
        }
        crate::diff::block_patch(full, path, fbs, fbe)
    }

    /// Full commit message of the graph row under screen-y `y` (uniform row height).
    pub fn graph_message_at_y(&self, region: Rect, y: f32) -> Option<&str> {
        let g = self.graph.as_ref()?;
        let lh = theme::LINE_HEIGHT().max(1.0);
        let rel = y - (region.y + theme::EDITOR_PAD()) + self.scroll_y();
        if rel < 0.0 {
            return None;
        }
        g.rows.get((rel / lh) as usize).map(|r| r.message.as_str())
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
        if self.large {
            return; // large-file mode: wrap stays off (uniform line heights)
        }
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

    /// Window the diff buffers to the visible viewport so glyphon only processes
    /// visible rows (otherwise it iterates every line of both panes each frame — the
    /// multi-file-diff scroll lag). Sets each buffer's vertical scroll to `scroll_y`
    /// and its height to `visible_h`; cosmic-text's layout iterator then skips rows
    /// above (line_y < 0) and stops below (line_y > height). After this, a run's
    /// `line_top` is viewport-relative, so diff draw code adds it to the pane top
    /// directly (no `- scroll_y`). No-op for non-diff docs.
    pub fn window_diff(&mut self, fs: &mut FontSystem, scroll_y: f32, visible_h: f32) {
        if self.diff_right.is_none() {
            return;
        }
        let scroll = glyphon::cosmic_text::Scroll::new(0, scroll_y.max(0.0), 0.0);
        self.buffer.set_scroll(scroll);
        if self.buffer.size().1 != Some(visible_h) {
            let w = self.buffer.size().0;
            self.buffer.set_size(fs, w, Some(visible_h));
        }
        if let Some(right) = self.diff_right.as_mut() {
            right.set_scroll(scroll);
            if right.size().1 != Some(visible_h) {
                let rw = right.size().0;
                right.set_size(fs, rw, Some(visible_h));
            }
        }
    }

    /// Widest shaped line in pixels (for horizontal scrolling).
    pub fn max_line_width(&self) -> f32 {
        // Cache by (version, shape epoch): scanning every line of both diff buffers
        // each frame is the multi-file-diff scroll lag. Large-file mode isn't cached —
        // its windowed buffer reshapes (and its width changes) as you scroll.
        // Diffs window their buffers (only visible rows are shaped), so a cache keyed
        // by `version` would freeze the width to the first visible screenful and miss
        // longer lines scrolled into view — recompute each frame like large mode. Both
        // only ever scan the visible window, so this stays cheap.
        let windowed = self.large || self.diff_right.is_some();
        let key = (self.version, theme::shape_epoch());
        if !windowed {
            if let Some((v, e, w)) = self.maxw_cache.get() {
                if (v, e) == key {
                    return w;
                }
            }
        }
        let left = self.buffer.layout_runs().map(|r| r.line_w).fold(0.0_f32, f32::max);
        // In a side-by-side diff the right pane has its own buffer; the widest line
        // across both panes drives the horizontal scroll range.
        let w = match self.diff_right.as_ref() {
            Some(right) => right.layout_runs().map(|r| r.line_w).fold(left, f32::max),
            None => left,
        };
        if !windowed {
            self.maxw_cache.set(Some((key.0, key.1, w)));
        }
        w
    }

    /// Current scroll offset (px). Backed by the document's `ScrollView`.
    pub fn scroll_x(&self) -> f32 {
        self.scroll.offset().0
    }
    pub fn scroll_y(&self) -> f32 {
        self.scroll.offset().1
    }

    // ---- Side-by-side diff: independent per-pane horizontal scroll ----

    /// Current horizontal offset (px) of diff pane `pane` (0=left, 1=right).
    pub fn diff_hx(&self, pane: usize) -> f32 {
        self.diff_hscroll[pane].get()
    }

    /// Widest shaped line (px, + padding) in diff pane `pane` — its scroll cap.
    pub fn diff_pane_content_w(&self, pane: usize) -> f32 {
        let widest = |buf: &Buffer| buf.layout_runs().map(|r| r.line_w).fold(0.0_f32, f32::max);
        let w = if pane == 0 {
            widest(&self.buffer)
        } else {
            self.diff_right.as_ref().map_or(0.0, widest)
        };
        w + theme::EDITOR_PAD() * 2.0
    }

    /// Clamp a pane's horizontal offset to its content given the pane's visible
    /// width — called each frame (content width shifts as rows window in/out).
    pub fn diff_clamp_h(&self, pane: usize, view_w: f32) {
        let max = (self.diff_pane_content_w(pane) - view_w).max(0.0);
        self.diff_hscroll[pane].set(self.diff_hscroll[pane].get().clamp(0.0, max));
    }

    /// Wheel-scroll a diff pane horizontally by `dx` px (clamped). Returns true if
    /// it moved.
    pub fn diff_hwheel(&self, pane: usize, dx: f32, view_w: f32) -> bool {
        let max = (self.diff_pane_content_w(pane) - view_w).max(0.0);
        let before = self.diff_hscroll[pane].get();
        let next = (before - dx).clamp(0.0, max);
        self.diff_hscroll[pane].set(next);
        next != before
    }

    /// Press on a diff pane's horizontal scrollbar track; begins a drag if hit.
    pub fn diff_hbar_press(&mut self, p: (f32, f32), pane: usize, track: Rect, view_w: f32) -> bool {
        let content = self.diff_pane_content_w(pane);
        let cur = self.diff_hscroll[pane].get();
        if let Some(s) = self.diff_hbar[pane].press_track(p, track, content, view_w, cur) {
            self.diff_hscroll[pane].set(s.clamp(0.0, (content - view_w).max(0.0)));
            return true;
        }
        false
    }

    pub fn diff_hbar_dragging(&self) -> bool {
        self.diff_hbar.iter().any(|b| b.is_dragging())
    }

    /// Continue a diff scrollbar drag; returns true if a thumb moved.
    pub fn diff_hbar_drag(&mut self, p: (f32, f32), panes: [Rect; 2]) -> bool {
        let mut moved = false;
        for pane in 0..2 {
            if self.diff_hbar[pane].is_dragging() {
                let content = self.diff_pane_content_w(pane);
                let track = Self::diff_htrack(panes[pane]);
                if let Some(s) = self.diff_hbar[pane].drag(p, track, content, panes[pane].w) {
                    self.diff_hscroll[pane].set(s.clamp(0.0, (content - panes[pane].w).max(0.0)));
                    moved = true;
                }
            }
        }
        moved
    }

    pub fn diff_release_hbars(&mut self) {
        for b in &mut self.diff_hbar {
            b.release();
        }
    }

    /// The horizontal scrollbar track strip at the bottom of a diff pane's text rect.
    pub fn diff_htrack(pane_text: Rect) -> Rect {
        let h = theme::SCROLLBAR_WIDTH();
        Rect { x: pane_text.x, y: pane_text.y + pane_text.h - h, w: pane_text.w, h }
    }

    /// Draw a pane's horizontal scrollbar thumb (returns the quad, if any).
    pub fn diff_hthumb(&self, pane: usize, pane_text: Rect) -> Option<Rect> {
        let track = Self::diff_htrack(pane_text);
        self.diff_hbar[pane].thumb(track, self.diff_pane_content_w(pane), pane_text.w, self.diff_hscroll[pane].get())
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
        // Large docs shape a window: buffer line = doc line - window start, and the
        // returned y gets the window's pixel offset added back. Lines outside the
        // window fall through to the uniform-height estimate below.
        let off = self.buf_offset_px();
        let Some(local) = line.checked_sub(self.buf_first).filter(|l| !self.large || *l < self.buf_count) else {
            return (0.0, line as f32 * theme::LINE_HEIGHT(), theme::LINE_HEIGHT());
        };
        let mut last_top = line as f32 * theme::LINE_HEIGHT();
        let mut last_h = theme::LINE_HEIGHT();
        let mut last_end_x = 0.0f32;
        for run in self.buffer.layout_runs() {
            if run.line_i != local {
                continue;
            }
            last_top = run.line_top + off;
            last_h = run.line_height;
            let mut run_end = 0.0f32;
            for g in run.glyphs.iter() {
                if (g.start as usize) >= col_byte {
                    return (g.x, run.line_top + off, run.line_height);
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
        let off = self.buf_offset_px();
        let Some(local) = line.checked_sub(self.buf_first).filter(|l| !self.large || *l < self.buf_count) else {
            return (line as f32 * theme::LINE_HEIGHT(), theme::LINE_HEIGHT());
        };
        let mut top: Option<f32> = None;
        let mut bottom = 0.0f32;
        for run in self.buffer.layout_runs() {
            if run.line_i == local {
                if top.is_none() {
                    top = Some(run.line_top + off);
                }
                bottom = run.line_top + off + run.line_height;
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
        if self.large {
            return None; // large-file mode: folding off (uniform line geometry)
        }
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
        if self.large {
            // Large-file mode: reshape only the shaped window (an edit can't change
            // styling — there is none — so rebuilding the window is enough).
            self.hl_dirty_from = usize::MAX;
            return self.reshape_window(fs);
        }
        let t0 = std::time::Instant::now();
        let text = self.rope.to_string().replace('\r', "");
        let lines = self.rope.len_lines();
        let t_str = t0.elapsed();
        let t1 = std::time::Instant::now();
        // Layer-1 highlight, incrementally from the lowest edited line (usize::MAX =
        // no text change since last highlight → returns cached spans, no re-tokenize).
        let dirty = std::mem::replace(&mut self.hl_dirty_from, usize::MAX);
        if let Some(hl) = self.hl.as_mut() {
            let (s, e) = hl.refresh(&text, dirty);
            // Fast path: a text edit whose tokenization converged — rebuild ONLY
            // the re-tokenized buffer lines instead of re-setting and re-shaping
            // the whole document. set_rich_text on a ~6000-line file costs
            // hundreds of ms PER KEYSTROKE (#36); swapping a few BufferLines is
            // near-free and keeps every other line's shape/layout caches.
            if dirty != usize::MAX && self.reshape_lines_partial(fs, s, e) {
                crate::perf::log(&format!(
                    "reshape-partial({lines} lines, rows {s}..{e}): total {:?}",
                    t1.elapsed()
                ));
                return;
            }
        }
        let spans = self.hl.as_ref().map(|h| h.flatten());
        apply_buffer_text(&mut self.buffer, fs, &text, lines, self.lang, &self.ext, self.wrap_width, spans, &self.semantic);
        crate::perf::log(&format!("reshape({lines} lines): to_string {:?}, highlight+shape {:?}", t_str, t1.elapsed()));
    }

    /// Rebuild buffer lines `[start, end)` in place from the highlight cache,
    /// keeping every other line's shape/layout caches. Returns false when the
    /// preconditions don't hold (line-count/metrics drift, wrap, missing cache)
    /// and the caller must run the full `apply_buffer_text` instead.
    fn reshape_lines_partial(&mut self, fs: &mut FontSystem, start: usize, end: usize) -> bool {
        use glyphon::cosmic_text::LineEnding;
        use glyphon::AttrsList;
        let Some(hl) = self.hl.as_ref() else { return false };
        // Both counts are display lines the cosmic-text way (a trailing '\n'
        // makes the ROPE one longer — never compare against it). When the edit
        // changed the line count, the rebuilt range is SPLICED into the buffer:
        // the shifted tail keeps its shape/layout caches.
        let total = hl.line_count();
        let buf_total = self.buffer.lines.len();
        if start >= end || self.wrap_width.is_some() {
            return false;
        }
        let delta = total as isize - buf_total as isize;
        let old_end = end as isize - delta;
        if old_end < start as isize || old_end as usize > buf_total {
            return false; // rebuilt range doesn't map cleanly — full reshape
        }
        // Metrics changes (zoom, font settings) need the full pass.
        let want = Metrics::new(theme::FONT_SIZE(), theme::LINE_HEIGHT());
        if self.buffer.metrics() != want {
            return false;
        }
        // Display-text offsets per line, from the cache's own span lengths (cheap
        // usize sums; also validates every line is cached).
        let mut starts = Vec::with_capacity(total + 1);
        starts.push(0usize);
        let mut acc = 0usize;
        for i in 0..total {
            let Some(sp) = hl.line_spans(i) else { return false };
            acc += sp.iter().map(|(s, _)| s.len()).sum::<usize>();
            starts.push(acc);
        }
        let mono = Attrs::new().family(Family::Name(theme::MONO_FAMILY()));
        let build = |i: usize| -> (String, glyphon::AttrsList, LineEnding) {
            let spans = hl.line_spans(i).unwrap_or(&[]);
            let line_text: String = spans.iter().map(|(s, _)| s.as_str()).collect();
            let (ls, le) = (starts[i], starts[i + 1]);
            // Semantic (Layer-2) ranges intersecting this line, rebased to it.
            let sem: Vec<(usize, usize, Color)> = self
                .semantic
                .iter()
                .filter(|(a, b, _)| *a < le && *b > ls)
                .map(|(a, b, c)| ((*a).max(ls) - ls, (*b).min(le) - ls, *c))
                .collect();
            let merged = merge_spans(&line_text, spans.to_vec(), &sem, mono);
            let has_nl = line_text.ends_with('\n');
            let text = line_text.trim_end_matches('\n').to_string();
            let mut al = AttrsList::new(mono);
            let mut off = 0usize;
            for (s, a) in &merged {
                let seg_end = (off + s.len()).min(text.len());
                if seg_end > off {
                    al.add_span(off..seg_end, *a);
                }
                off += s.len();
            }
            let ending = if has_nl { LineEnding::Lf } else { LineEnding::None };
            (text, al, ending)
        };
        let end = end.min(total);
        if delta == 0 {
            for i in start..end {
                let (text, al, ending) = build(i);
                self.buffer.lines[i].set_text(text, ending, al);
            }
        } else {
            // Line count changed (Enter / paste / line delete): splice the
            // rebuilt range over the old one; the tail shifts intact.
            let new_lines: Vec<glyphon::BufferLine> = (start..end)
                .map(|i| {
                    let (text, al, ending) = build(i);
                    glyphon::BufferLine::new(text, ending, al, Shaping::Advanced)
                })
                .collect();
            self.buffer.lines.splice(start..old_end as usize, new_lines);
            // The buffer's height tracks the line count (full-shape invariant).
            let h = (total as f32 + 2.0) * theme::LINE_HEIGHT() + 200.0;
            self.buffer.set_size(fs, self.wrap_width, Some(h));
        }
        self.buffer.set_redraw(true);
        self.buffer.shape_until_scroll(fs, false);
        true
    }

    // ---- Large-file shaped window ----

    /// First document line currently shaped into the buffer (0 for normal docs).
    pub fn buf_first_line(&self) -> usize {
        self.buf_first
    }

    /// How many document lines the shaped window holds.
    pub fn buf_window_lines(&self) -> usize {
        self.buf_count
    }

    /// Pixel offset of the shaped window's top from the document's top.
    pub fn buf_offset_px(&self) -> f32 {
        self.buf_first as f32 * theme::LINE_HEIGHT()
    }

    /// Rebuild the shaped window at its current position (clamped to the rope).
    fn reshape_window(&mut self, fs: &mut FontSystem) {
        let total = self.rope.len_lines();
        self.buf_first = self.buf_first.min(total.saturating_sub(1));
        self.buf_count = LARGE_WINDOW_LINES.min(total - self.buf_first);
        let text = window_string(&self.rope, self.buf_first, self.buf_count);
        shape_window(&mut self.buffer, fs, &text, self.buf_count);
    }

    /// Keep the shaped window covering the viewport: when the visible range drifts
    /// near (or past) the window's edges, re-center and reshape. Called per frame
    /// by the renderer for large docs; a no-op while the view stays inside.
    pub fn ensure_window(&mut self, fs: &mut FontSystem, first_visible: usize, visible: usize) {
        if !self.large {
            return;
        }
        let total = self.rope.len_lines();
        let slack = visible.max(50); // re-center before the edge enters the viewport
        let end = self.buf_first + self.buf_count;
        let lo_ok = self.buf_first == 0 || first_visible >= self.buf_first + slack;
        let hi_ok = end >= total || first_visible + visible + slack <= end;
        if lo_ok && hi_ok && self.buf_count > 0 {
            return;
        }
        let half = LARGE_WINDOW_LINES.saturating_sub(visible) / 2;
        self.buf_first = first_visible.saturating_sub(half).min(total.saturating_sub(1));
        self.reshape_window(fs);
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

    /// Apply a fresh semantic-token set and recolor ONLY the lines whose tokens
    /// actually changed. A full `set_semantic` + `reshape` costs hundreds of ms on
    /// a big file, and rust-analyzer re-delivers tokens continuously while typing
    /// — full reshapes per delivery froze the editor in waves (#36).
    pub fn apply_semantic_tokens(&mut self, toks: &[(u32, u32, u32, Color)], fs: &mut FontSystem) {
        use std::collections::BTreeMap;
        let new: Vec<(usize, usize, Color)> = toks
            .iter()
            .map(|&(line, start, len, c)| (self.lsp_byte(line, start), self.lsp_byte(line, start + len), c))
            .collect();
        let old = std::mem::replace(&mut self.semantic, new);
        if self.large {
            return; // large mode draws plain text — nothing to recolor
        }
        // Per-line buckets of both sets; a line is dirty when its buckets differ.
        let mut buckets: BTreeMap<usize, (Vec<(usize, usize, Color)>, Vec<(usize, usize, Color)>)> =
            BTreeMap::new();
        let max_b = self.rope.len_bytes();
        for (which, set) in [old.as_slice(), self.semantic.as_slice()].into_iter().enumerate() {
            for &(a, b, c) in set {
                let l = self.rope.byte_to_line(a.min(max_b));
                let e = buckets.entry(l).or_default();
                if which == 0 {
                    e.0.push((a, b, c));
                } else {
                    e.1.push((a, b, c));
                }
            }
        }
        let changed: Vec<usize> = buckets
            .into_iter()
            .filter(|(_, (o, n))| o != n)
            .map(|(l, _)| l)
            .collect();
        let (Some(&lo), Some(&hi)) = (changed.first(), changed.last()) else {
            return; // identical token sets — nothing to redraw
        };
        // A tight dirty band (typical while typing) recolors in place; a sweeping
        // change (first delivery, big refactor) takes the full reshape.
        if hi - lo < 400 && self.reshape_lines_partial(fs, lo, hi + 1) {
            return;
        }
        self.reshape(fs);
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
        self.shift_breakpoints(op, at_byte);
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

    /// Toggle a breakpoint on a 0-based line.
    pub fn toggle_breakpoint(&mut self, line: usize) {
        if !self.breakpoints.remove(&line) {
            self.breakpoints.insert(line);
        }
    }

    /// Shift breakpoint + execution lines when an edit adds/removes newlines before
    /// them, so they stay glued to their code (line-granular; columns don't matter).
    fn shift_breakpoints(&mut self, op: &EditOp, at_byte: usize) {
        if self.breakpoints.is_empty() && self.execution_line.is_none() {
            return;
        }
        let at = at_byte.min(self.rope.len_bytes());
        let edit_line = self.rope.byte_to_line(at);
        let (s, deleting) = match op {
            EditOp::Insert(s) => (s, false),
            EditOp::Delete(s) => (s, true),
        };
        let nl = s.matches('\n').count() as i64;
        if nl == 0 {
            return; // single-line edit: no line numbers move
        }
        let delta = if deleting { -nl } else { nl };
        let shift = |line: usize| -> usize {
            if line > edit_line {
                (line as i64 + delta).max(edit_line as i64) as usize
            } else {
                line
            }
        };
        self.breakpoints = self.breakpoints.iter().map(|&l| shift(l)).collect();
        self.execution_line = self.execution_line.map(shift);
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

    /// Apply a batch of LSP TextEdits (formatting / rename) as ONE undo step.
    /// Edits are sorted descending and applied bottom-up so earlier edits don't
    /// shift later ranges (the LSP guarantees ranges don't overlap).
    pub fn apply_text_edits(&mut self, edits: &[crate::lsp::TextEdit], fs: &mut FontSystem) {
        if self.read_only || edits.is_empty() {
            return;
        }
        let mut sorted: Vec<&crate::lsp::TextEdit> = edits.iter().collect();
        sorted.sort_by_key(|e| (e.start_line, e.start_char));
        let caret = self.sel.head;
        self.break_undo_group();
        let mut first = true;
        for e in sorted.iter().rev() {
            let lo = self.lsp_byte(e.start_line, e.start_char);
            let hi = self.lsp_byte(e.end_line, e.end_char).max(lo);
            if !first {
                self.force_join = true;
            }
            if hi > lo {
                let old = self.slice_str(lo, hi);
                self.push_and_apply(EditOp::Delete(old), lo, Selection::caret(lo));
                self.force_join = true;
            }
            if !e.new_text.is_empty() {
                self.push_and_apply(EditOp::Insert(e.new_text.clone()), lo, Selection::caret(lo + e.new_text.len()));
            }
            first = false;
        }
        // Keep the caret near where it was (clamped — exact tracking through a
        // reformat isn't meaningful).
        self.sel = Selection::caret(caret.min(self.rope.len_bytes()));
        self.reshape(fs);
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
            // Mark the highlight dirty like any edit — otherwise the tokenizer
            // cache keeps the pre-undo lines and the buffer rebuilds from STALE
            // spans (wrong text on screen after a structural undo).
            let line = self.rope.byte_to_line(edit.at_byte.min(self.rope.len_bytes()));
            self.hl_dirty_from = self.hl_dirty_from.min(line);
            self.sel = Selection {
                anchor: edit.sel_before.0,
                head: edit.sel_before.1,
                desired_col: None,
            };
            self.future.push(edit);
        }
        self.pending_stop = true; // typing after an undo starts a fresh group
        self.dirty = true;
        // The text changed: language servers need a didChange like any edit.
        self.version += 1;
        self.lsp_dirty = true;
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
            let line = self.rope.byte_to_line(edit.at_byte.min(self.rope.len_bytes()));
            self.hl_dirty_from = self.hl_dirty_from.min(line);
            self.sel = Selection {
                anchor: edit.sel_after.0,
                head: edit.sel_after.1,
                desired_col: None,
            };
            self.history.push(edit);
        }
        self.pending_stop = true;
        self.dirty = true;
        self.version += 1;
        self.lsp_dirty = true;
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
        self.hl = crate::highlight::Highlighter::new(&ext);
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
    /// Byte range of the identifier at `byte` (alnum/underscore run), if any.
    pub fn word_at(&self, byte: usize) -> Option<(usize, usize)> {
        let total = self.rope.len_chars();
        if total == 0 {
            return None;
        }
        let mut ci = self.rope.byte_to_char(byte.min(self.rope.len_bytes()));
        if ci >= total {
            ci = total - 1;
        }
        let is_word = |c: char| c.is_alphanumeric() || c == '_';
        if !is_word(self.rope.char(ci)) {
            // Directly after a word (caret at its end) still counts.
            if ci == 0 || !is_word(self.rope.char(ci - 1)) {
                return None;
            }
            ci -= 1;
        }
        let mut start = ci;
        while start > 0 && is_word(self.rope.char(start - 1)) {
            start -= 1;
        }
        let mut end = ci;
        while end < total && is_word(self.rope.char(end)) {
            end += 1;
        }
        Some((self.rope.char_to_byte(start), self.rope.char_to_byte(end)))
    }

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

    /// The bracket at/just beside `head` (the char after the caret, else the char
    /// before — VSCode's adjacency rule) plus its match, as byte offsets
    /// (bracket_at_caret, matching_bracket). Shared by Go to Bracket and the
    /// matching-pair highlight. Naive scan (ignores brackets inside strings).
    pub fn bracket_pair_at(&self, head: usize) -> Option<(usize, usize)> {
        let pairs: &[(char, char)] = &[('(', ')'), ('[', ']'), ('{', '}')];
        let head = head.min(self.rope.len_bytes());
        let hc = self.rope.byte_to_char(head);
        let total = self.rope.len_chars();
        let cand = [(hc, false), (hc.saturating_sub(1), true)];
        for (ci, before) in cand {
            if ci >= total || (before && hc == 0) {
                continue;
            }
            let c = self.rope.char(ci);
            let Some((pi, is_open)) = pairs.iter().enumerate().find_map(|(i, (o, cl))| {
                (c == *o).then_some((i, true)).or((c == *cl).then_some((i, false)))
            }) else {
                continue;
            };
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
                            return Some((self.rope.char_to_byte(ci), self.rope.char_to_byte(j)));
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
                            return Some((self.rope.char_to_byte(ci), self.rope.char_to_byte(j)));
                        }
                        d -= 1;
                    }
                }
            }
            return None; // on a bracket but unmatched — nothing to pair with
        }
        None
    }

    /// The quote at/just beside `head` plus its matching quote on the same line, as
    /// byte offsets. Quotes don't nest — they pair up in order along the line
    /// (0–1, 2–3, …), skipping backslash-escaped ones — so the partner is whichever
    /// slot ours shares. None if the quote is unbalanced (odd count).
    pub fn quote_pair_at(&self, head: usize) -> Option<(usize, usize)> {
        let quotes = ['"', '\'', '`'];
        let head = head.min(self.rope.len_bytes());
        let hc = self.rope.byte_to_char(head);
        let total = self.rope.len_chars();
        for (ci, before) in [(hc, false), (hc.saturating_sub(1), true)] {
            if ci >= total || (before && hc == 0) {
                continue;
            }
            let q = self.rope.char(ci);
            if !quotes.contains(&q) {
                continue;
            }
            let byte = self.rope.char_to_byte(ci);
            let line = self.rope.byte_to_line(byte);
            // Unescaped `q` byte positions on this line, in order.
            let mut positions: Vec<usize> = Vec::new();
            let mut b = self.rope.line_to_byte(line);
            let mut escaped = false;
            for ch in self.rope.line(line).chars() {
                if ch == q && !escaped {
                    positions.push(b);
                }
                escaped = ch == '\\' && !escaped;
                b += ch.len_utf8();
            }
            let idx = positions.iter().position(|&p| p == byte)?;
            let partner = if idx % 2 == 0 { positions.get(idx + 1) } else { positions.get(idx - 1) };
            return partner.map(|&p| (byte, p));
        }
        None
    }

    /// The file's indentation unit in columns, detected from its own content (so
    /// indent guides follow 2-space / 4-space / tab styles, not a global setting).
    /// The most common single-step indent *increase* wins; falls back to `tab` when
    /// nothing is indented. Cached per version (a full scan is capped to 2000 lines).
    pub fn indent_unit(&self, tab: usize) -> usize {
        if let Some((v, u)) = self.indent_unit_cache.get() {
            if v == self.version {
                return u;
            }
        }
        let tab = tab.max(1);
        let mut tally = [0u32; 9]; // counts for indent increments 1..=8 columns
        let mut prev = 0usize;
        for line in 0..self.rope.len_lines().min(2000) {
            let (mut cols, mut blank) = (0usize, true);
            for c in self.rope.line(line).chars() {
                match c {
                    ' ' => cols += 1,
                    '\t' => cols += tab - (cols % tab),
                    '\n' | '\r' => break, // blank line — don't disturb `prev`
                    _ => {
                        blank = false;
                        break;
                    }
                }
            }
            if blank {
                continue;
            }
            if cols > prev {
                let d = cols - prev;
                if (1..=8).contains(&d) {
                    tally[d] += 1;
                }
            }
            prev = cols;
        }
        let unit = (1..=8).filter(|&d| tally[d] > 0).max_by_key(|&d| tally[d]).unwrap_or(tab);
        self.indent_unit_cache.set(Some((self.version, unit)));
        unit
    }

    /// Matching pair to highlight for the current caret: a bracket pair when the
    /// caret is beside a bracket, else a quote pair when beside a quote. None while
    /// a selection is active or the caret isn't beside either. Cached per (caret,
    /// version) — the render loop asks every frame but the scan only runs when the
    /// caret actually moved or the text changed.
    pub fn bracket_highlight(&self) -> Option<(usize, usize)> {
        if !self.sel.is_empty() {
            return None;
        }
        let head = self.sel.head.min(self.rope.len_bytes());
        if let Some((h, v, r)) = self.bracket_hl_cache.get() {
            if h == head && v == self.version {
                return r;
            }
        }
        let r = self.bracket_pair_at(head).or_else(|| self.quote_pair_at(head));
        self.bracket_hl_cache.set(Some((head, self.version, r)));
        r
    }

    /// Jump to the bracket matching the one at/under the caret; if the caret isn't
    /// on a bracket, jump to the close of the nearest enclosing pair (VSCode).
    pub fn goto_bracket(&mut self) {
        let head = self.sel.head.min(self.rope.len_bytes());
        if let Some((_, m)) = self.bracket_pair_at(head) {
            self.place(m, false);
            return;
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

    #[test]
    fn bracket_pair_highlight() {
        let mut fs = glyphon::FontSystem::new();
        //                   0123456789012345 6
        let mut d = doc("fn f(aa, (bb)) {}", &mut fs);
        // Caret before "(" of f(...): pair is (4, 13).
        d.place(4, false);
        assert_eq!(d.bracket_highlight(), Some((4, 13)));
        // Caret just AFTER ")" — the char-before rule still pairs it.
        d.place(14, false);
        assert_eq!(d.bracket_highlight(), Some((13, 4)));
        // Inner pair wins when the caret is beside it.
        d.place(9, false);
        assert_eq!(d.bracket_highlight(), Some((9, 12)));
        // Not beside any bracket → no highlight.
        d.place(7, false);
        assert_eq!(d.bracket_highlight(), None);
        // A selection suppresses the highlight.
        d.place(4, false);
        d.place(5, true);
        assert_eq!(d.bracket_highlight(), None);
        // The cache invalidates on edit: deleting ")" unmatches the "(".
        let mut d2 = doc("(x)", &mut fs);
        d2.place(0, false);
        assert_eq!(d2.bracket_highlight(), Some((0, 2)));
        d2.place(3, false);
        d2.backspace(&mut fs); // "(x"
        d2.place(0, false);
        assert_eq!(d2.bracket_highlight(), None);
    }

    #[test]
    fn quote_pair_highlight() {
        let mut fs = glyphon::FontSystem::new();
        //                   0123456789012345678
        let mut d = doc("let s = \"hello\" + x;", &mut fs);
        // Caret before the opening quote (byte 8) → pairs with the closing quote (14).
        d.place(8, false);
        assert_eq!(d.bracket_highlight(), Some((8, 14)));
        // Caret after the closing quote → char-before rule pairs it back.
        d.place(15, false);
        assert_eq!(d.bracket_highlight(), Some((14, 8)));
        // Inside the string (not beside a quote) → nothing.
        d.place(11, false);
        assert_eq!(d.bracket_highlight(), None);
        // Escaped quote is skipped when pairing.
        let mut d2 = doc("x = \"a\\\"b\";", &mut fs); // x = "a\"b";
        d2.place(4, false); // before opening "
        // Bytes: " at 4, \ at 6, escaped " at 7, real closing " at 9.
        assert_eq!(d2.bracket_highlight(), Some((4, 9)));
    }

    // Diagnostic timing for huge files (run with --ignored --nocapture). Mirrors a
    // 450k-line log: open (Document::new), one edit reshape, and the per-frame
    // metric scans.
    #[test]
    #[ignore]
    fn big_file_timing() {
        let mut fs = glyphon::FontSystem::new();
        let line = "2026-06-04 12:00:00 INFO some.module - processed request id=12345 status=ok elapsed=12ms\n";
        let text: String = line.repeat(450_000);
        eprintln!("file: {} MB, {} lines", text.len() / 1_048_576, 450_000);

        let t = std::time::Instant::now();
        let mut d = Document::new(Some(std::path::PathBuf::from("t.log")), text, &mut fs);
        eprintln!("open (Document::new): {:?}", t.elapsed());

        let t = std::time::Instant::now();
        let w = d.max_line_width();
        eprintln!("max_line_width (per frame): {:?} -> {w}", t.elapsed());

        let t = std::time::Instant::now();
        d.place(100, false);
        d.insert_str("x", &mut fs);
        eprintln!("single-keystroke edit (reshape): {:?}", t.elapsed());

        let t = std::time::Instant::now();
        let text2 = d.text();
        eprintln!("text() for LSP/completion: {:?} ({} MB)", t.elapsed(), text2.len() / 1_048_576);

        // Window mechanics: scrolling to the middle re-centers the shaped window.
        assert!(d.large, "450k lines must enter large-file mode");
        let t = std::time::Instant::now();
        d.ensure_window(&mut fs, 225_000, 50);
        eprintln!("window jump to line 225k: {:?}", t.elapsed());
        assert!(d.buf_first_line() <= 225_000 && 225_000 < d.buf_first_line() + d.buf_window_lines());
        // Geometry round-trips through the window offset.
        let b = d.rope.line_to_byte(225_000);
        let (_, y, _) = d.byte_visual(b);
        assert_eq!(y, 225_000.0 * theme::LINE_HEIGHT());
    }

    // Large-file mode invariants that must hold at normal sizes too: small docs
    // never enter it, and window geometry is the identity for them.
    #[test]
    fn small_docs_stay_normal() {
        let mut fs = glyphon::FontSystem::new();
        let d = doc("fn main() {}\n", &mut fs);
        assert!(!d.large);
        assert_eq!(d.buf_first_line(), 0);
        assert_eq!(d.buf_offset_px(), 0.0);
    }

    #[test]
    fn text_edits_apply_as_one_undo_step() {
        let mut fs = glyphon::FontSystem::new();
        let mut d = doc("let foo = 1;\nprint(foo);\n", &mut fs);
        let edit = |sl, sc, el, ec, t: &str| crate::lsp::TextEdit {
            start_line: sl,
            start_char: sc,
            end_line: el,
            end_char: ec,
            new_text: t.into(),
        };
        // Rename foo → counter at both sites (server order is arbitrary).
        d.apply_text_edits(
            &[edit(1, 6, 1, 9, "counter"), edit(0, 4, 0, 7, "counter")],
            &mut fs,
        );
        assert_eq!(d.text(), "let counter = 1;\nprint(counter);\n");
        // One undo restores both occurrences.
        d.undo(&mut fs);
        assert_eq!(d.text(), "let foo = 1;\nprint(foo);\n");
        // Pure insertion (formatting adds an indent) and pure deletion both work.
        d.apply_text_edits(&[edit(1, 0, 1, 0, "    ")], &mut fs);
        assert_eq!(d.text(), "let foo = 1;\n    print(foo);\n");
        d.apply_text_edits(&[edit(1, 0, 1, 4, "")], &mut fs);
        assert_eq!(d.text(), "let foo = 1;\nprint(foo);\n");
        // word_at: identifier under / just after the caret.
        assert_eq!(d.word_at(5), Some((4, 7))); // inside "foo"
        assert_eq!(d.word_at(7), Some((4, 7))); // right after "foo"
        assert_eq!(d.word_at(3), Some((0, 3))); // after "let"
    }

    /// Every buffer line's text must equal the document's display line — the
    /// invariant the partial reshape / splice paths must never break.
    #[cfg(test)]
    fn assert_buffer_matches(d: &Document) {
        let display = d.rope.to_string().replace('\r', "");
        let mut doc_lines: Vec<&str> = display.split('\n').collect();
        // cosmic-text doesn't create a line after a trailing newline.
        if display.ends_with('\n') {
            doc_lines.pop();
        }
        if doc_lines.is_empty() {
            doc_lines.push("");
        }
        assert_eq!(
            d.buffer.lines.len(),
            doc_lines.len(),
            "buffer line count vs document"
        );
        for (i, want) in doc_lines.iter().enumerate() {
            assert_eq!(d.buffer.lines[i].text(), *want, "buffer line {i} text");
        }
    }

    // Structural edits (Enter / line deletion) must stay fast AND splice the
    // buffer correctly — every line of the buffer equals the document after.
    #[test]
    fn enter_and_delete_line_splice_correctly() {
        let mut fs = glyphon::FontSystem::new();
        let mut src = String::new();
        for i in 0..2000 {
            src.push_str(&format!("fn f_{i}() {{ let v = {i}; }}\n"));
        }
        let mut d = Document::new(Some(std::path::PathBuf::from("big.rs")), src, &mut fs);
        d.reshape(&mut fs);
        // Enter in the middle of line 1000.
        let mid = d.rope.line_to_byte(1000) + 10;
        d.place(mid, false);
        let t0 = std::time::Instant::now();
        d.insert_str("\n", &mut fs);
        let dt = t0.elapsed();
        assert_buffer_matches(&d);
        assert!(dt < std::time::Duration::from_millis(150), "Enter took {dt:?}");
        // Undo (removes the line) must splice back.
        d.undo(&mut fs);
        assert_buffer_matches(&d);
        // Paste with multiple newlines.
        d.place(d.rope.line_to_byte(500), false);
        d.insert_str("// a\n// b\n// c\n", &mut fs);
        assert_buffer_matches(&d);
        // Delete a whole line (selection across the newline).
        let ls = d.rope.line_to_byte(500);
        let le = d.rope.line_to_byte(501);
        d.sel = Selection { anchor: ls, head: le, desired_col: None };
        d.delete_selection(&mut fs);
        assert_buffer_matches(&d);
    }

    // Semantic-token deliveries recolor in place without breaking the buffer.
    #[test]
    fn semantic_tokens_apply_partially() {
        let mut fs = glyphon::FontSystem::new();
        let mut src = String::new();
        for i in 0..1000 {
            src.push_str(&format!("fn g_{i}() {{}}\n"));
        }
        let mut d = Document::new(Some(std::path::PathBuf::from("t.rs")), src, &mut fs);
        d.reshape(&mut fs);
        let color = Color::rgb(10, 200, 10);
        // First delivery: tokens on lines 10..14.
        let toks: Vec<(u32, u32, u32, Color)> = (10..14).map(|l| (l, 3, 4, color)).collect();
        d.apply_semantic_tokens(&toks, &mut fs);
        assert_buffer_matches(&d);
        // Second delivery: one line's token moves — only that band recolors.
        let toks2: Vec<(u32, u32, u32, Color)> =
            vec![(10, 3, 4, color), (11, 4, 4, color), (12, 3, 4, color), (13, 3, 4, color)];
        let t0 = std::time::Instant::now();
        d.apply_semantic_tokens(&toks2, &mut fs);
        let dt = t0.elapsed();
        assert_buffer_matches(&d);
        assert!(dt < std::time::Duration::from_millis(50), "semantic apply took {dt:?}");
    }

    // #36 regression: one keystroke in a ~6000-line .rs must stay interactive.
    // (The freeze was syntect re-parsing to EOF; convergence fixed it. This
    // guards the whole insert path — rope edit + incremental highlight + full
    // buffer reshape.)
    #[test]
    fn keystroke_in_large_rust_file_is_fast() {
        let mut fs = glyphon::FontSystem::new();
        let mut src = String::new();
        for i in 0..6000 {
            src.push_str(&format!("fn func_{i}() {{ let x = {i}; println!(\"{{}}\", x); }}\n"));
        }
        let mut d = Document::new(Some(std::path::PathBuf::from("big.rs")), src, &mut fs);
        d.reshape(&mut fs); // initial full highlight+shape (not measured)
        let mid = d.rope.line_to_byte(3000);
        d.place(mid, false);
        let t0 = std::time::Instant::now();
        d.insert_str("x", &mut fs);
        let dt = t0.elapsed();
        // Generous bound for CI variance; the pre-fix cost was multiple SECONDS.
        assert!(dt < std::time::Duration::from_millis(250), "keystroke took {dt:?}");
    }
}
