// Reusable UI widgets. Each owns its buffer(s) and renders from a `Rect` supplied
// by the layout, so geometry has a single source of truth shared by hit-testing,
// hover backgrounds, and drawing.

use std::time::{Duration, Instant};

use glyphon::{Attrs, Buffer, Family, FontSystem, Metrics, Shaping, TextArea, TextBounds, Wrap};
use winit::window::CursorIcon;

use crate::icon::IconInstance;
use crate::quad::Quad;
use crate::theme;

pub fn make_ui_buffer(fs: &mut FontSystem, w: f32, h: f32) -> Buffer {
    let mut b = Buffer::new(fs, Metrics::new(theme::UI_FONT_SIZE(), theme::UI_LINE_HEIGHT()));
    b.set_size(fs, Some(w), Some(h));
    b
}

pub fn make_ui_buffer_mono(fs: &mut FontSystem, w: f32, h: f32) -> Buffer {
    let mut b = Buffer::new(fs, Metrics::new(theme::FONT_SIZE(), theme::LINE_HEIGHT()));
    b.set_size(fs, Some(w), Some(h));
    b
}

#[derive(Clone, Copy, Default)]
pub struct Rect {
    pub x: f32,
    pub y: f32,
    pub w: f32,
    pub h: f32,
}

impl Rect {
    pub fn contains(&self, p: (f32, f32)) -> bool {
        p.0 >= self.x && p.0 < self.x + self.w && p.1 >= self.y && p.1 < self.y + self.h
    }
    pub fn quad(&self, color: [f32; 4]) -> Quad {
        Quad::new(self.x, self.y, self.w, self.h, color)
    }
    pub fn rounded_quad(&self, color: [f32; 4], radius: f32) -> Quad {
        Quad::rounded(self.x, self.y, self.w, self.h, color, radius)
    }
    /// The `top` y to place a line of height `line_h` in this rect at the given
    /// vertical alignment. Single source of truth for vertical text placement.
    pub fn text_top(&self, line_h: f32, align: VAlign) -> f32 {
        match align {
            VAlign::Top => self.y,
            VAlign::Center => self.y + (self.h - line_h) * 0.5,
            VAlign::Bottom => self.y + self.h - line_h,
        }
    }
}

/// Vertical alignment of text/content within a rect.
#[derive(Clone, Copy)]
pub enum VAlign {
    Top,
    Center,
    Bottom,
}

/// Reusable icon button. A single `rect` (supplied at draw time from the layout)
/// is the single source of truth for the hit region, the hover background, and
/// the centered glyph — so they can never drift apart. Backed by a one-glyph
/// buffer to sidestep glyphon's multi-line layout quirks.
pub struct IconButton {
    buffer: Buffer,
    size: f32,        // current (zoomed) glyph size
    base: f32,        // unzoomed glyph size (for re-scaling)
    glyph: char,
    family: String,
    epoch: u64,
    pub cursor: CursorIcon,
    pub align: VAlign,
}

impl IconButton {
    pub fn new(fs: &mut FontSystem, glyph: char, family: &str, size: f32) -> Self {
        // `size` is captured at the current zoom; recover the unzoomed base.
        let base = size / theme::ui_zoom();
        let mut buffer = Buffer::new(fs, Metrics::new(size, size + 2.0));
        buffer.set_size(fs, Some(128.0), Some(size + 8.0));
        let mut tmp = [0u8; 4];
        buffer.set_text(
            fs,
            glyph.encode_utf8(&mut tmp),
            Attrs::new().family(Family::Name(family)),
            Shaping::Advanced,
        );
        buffer.shape_until_scroll(fs, false);
        // Buttons get the hand pointer by default; callers can override.
        Self {
            buffer,
            size,
            base,
            glyph,
            family: family.to_string(),
            epoch: theme::shape_epoch(),
            cursor: CursorIcon::Pointer,
            align: VAlign::Center,
        }
    }

    /// Re-shape the glyph at the current zoom (call after a zoom change).
    pub fn reshape(&mut self, fs: &mut FontSystem) {
        if self.epoch == theme::shape_epoch() {
            return;
        }
        self.epoch = theme::shape_epoch();
        self.size = self.base * theme::ui_zoom();
        self.buffer.set_metrics(fs, Metrics::new(self.size, self.size + 2.0));
        self.buffer.set_size(fs, Some(128.0 * theme::ui_zoom()), Some(self.size + 8.0));
        let mut tmp = [0u8; 4];
        self.buffer.set_text(
            fs,
            self.glyph.encode_utf8(&mut tmp),
            Attrs::new().family(Family::Name(&self.family)),
            Shaping::Advanced,
        );
        self.buffer.shape_until_scroll(fs, false);
    }

    pub fn cursor(&self) -> CursorIcon {
        self.cursor
    }

    fn glyph_w(&self) -> f32 {
        self.buffer
            .layout_runs()
            .next()
            .map(|r| r.line_w)
            .unwrap_or(self.size)
    }

    /// Push the button's glyph, centered in `rect` and clipped to it. The hover
    /// background is drawn separately in the bg phase from the same rect, so the
    /// two always align.
    pub fn draw<'a>(&'a self, rect: Rect, color: glyphon::Color, areas: &mut Vec<TextArea<'a>>) {
        let gw = self.glyph_w();
        areas.push(TextArea {
            buffer: &self.buffer,
            left: rect.x + (rect.w - gw) * 0.5,
            top: rect.text_top(self.size, self.align) - 1.0,
            scale: 1.0,
            bounds: TextBounds {
                left: rect.x as i32,
                top: rect.y as i32,
                right: (rect.x + rect.w) as i32,
                bottom: (rect.y + rect.h) as i32,
            },
            default_color: color,
            custom_glyphs: &[],
        });
    }
}

/// Reusable single-line text label. Owns its buffer *and* its last content, so
/// it reshapes only when the text actually changes (no parallel cache string),
/// and draws a TextArea clipped to a supplied rect — that rect being the single
/// source of truth for placement and clipping. Three alignment helpers cover
/// the common cases (left-padded, centered, right-padded).
pub struct TextLabel {
    buffer: Buffer,
    last: String,
    family: String, // family of the last plain `set` (for re-shape on zoom)
    epoch: u64,     // theme::shape_epoch() this buffer was last shaped at
    base_w: f32,    // unzoomed buffer area (scaled by zoom on re-shape)
    base_h: f32,
    pub align: VAlign,
}

impl TextLabel {
    pub fn new(fs: &mut FontSystem, w: f32, h: f32) -> Self {
        let z = theme::ui_zoom();
        Self {
            buffer: make_ui_buffer(fs, w, h),
            last: String::new(),
            family: String::new(),
            epoch: theme::shape_epoch(),
            base_w: w / z,
            base_h: h / z,
            align: VAlign::Center,
        }
    }

    pub fn set(&mut self, fs: &mut FontSystem, text: &str, family: &str) {
        if self.last == text && self.family == family && self.epoch == theme::shape_epoch() {
            return;
        }
        self.last = text.to_string();
        self.family = family.to_string();
        self.shape_plain(fs);
    }

    fn shape_plain(&mut self, fs: &mut FontSystem) {
        let z = theme::ui_zoom();
        self.buffer.set_metrics(fs, Metrics::new(theme::UI_FONT_SIZE(), theme::UI_LINE_HEIGHT()));
        self.buffer.set_size(fs, Some(self.base_w * z), Some(self.base_h * z));
        // Labels are single-line — never wrap (a long string clips at the rect
        // rather than spilling onto a second line into neighbouring widgets).
        self.buffer.set_wrap(fs, Wrap::None);
        self.buffer.set_text(
            fs,
            &self.last,
            Attrs::new().family(Family::Name(&self.family)),
            Shaping::Advanced,
        );
        self.buffer.shape_until_scroll(fs, false);
        self.epoch = theme::shape_epoch();
    }

    /// Re-shape after a zoom change (from the stored plain text). No-op for empty
    /// or rich labels (those re-shape from their per-frame `set`/`set_rich`).
    pub fn reshape(&mut self, fs: &mut FontSystem) {
        if !self.last.is_empty() && !self.family.is_empty() {
            self.shape_plain(fs);
        }
    }

    /// Rich (multi-span, multi-color) variant. `key` is an opaque change-detection
    /// string so we reshape only when the content changes.
    pub fn set_rich(&mut self, fs: &mut FontSystem, key: &str, spans: &[(String, Attrs)], default: Attrs) {
        if self.last == key && self.epoch == theme::shape_epoch() {
            return;
        }
        self.epoch = theme::shape_epoch();
        self.family.clear(); // rich content; not re-shapable from `reshape`
        let z = theme::ui_zoom();
        self.buffer.set_metrics(fs, Metrics::new(theme::UI_FONT_SIZE(), theme::UI_LINE_HEIGHT()));
        self.buffer.set_size(fs, Some(self.base_w * z), Some(self.base_h * z));
        self.buffer.set_rich_text(
            fs,
            spans.iter().map(|(s, a)| (s.as_str(), *a)),
            default,
            Shaping::Advanced,
        );
        self.buffer.shape_until_scroll(fs, false);
        self.last = key.to_string();
    }

    pub fn width(&self) -> f32 {
        self.buffer
            .layout_runs()
            .next()
            .map(|r| r.line_w)
            .unwrap_or(0.0)
    }

    pub fn push<'a>(&'a self, left: f32, rect: Rect, color: glyphon::Color, areas: &mut Vec<TextArea<'a>>) {
        areas.push(TextArea {
            buffer: &self.buffer,
            left,
            top: rect.text_top(theme::UI_LINE_HEIGHT(), self.align),
            scale: 1.0,
            bounds: TextBounds {
                left: rect.x as i32,
                top: rect.y as i32,
                right: (rect.x + rect.w) as i32,
                bottom: (rect.y + rect.h) as i32,
            },
            default_color: color,
            custom_glyphs: &[],
        });
    }

    pub fn draw_left<'a>(&'a self, rect: Rect, pad: f32, color: glyphon::Color, areas: &mut Vec<TextArea<'a>>) {
        self.push(rect.x + pad, rect, color, areas);
    }

    /// `push`, but clipped to `clip` instead of `rect` — for content positioned at
    /// `rect` while scrolled inside a fixed viewport.
    pub fn push_in<'a>(&'a self, left: f32, rect: Rect, clip: Rect, color: glyphon::Color, areas: &mut Vec<TextArea<'a>>) {
        self.push_clipped(left, rect.text_top(theme::UI_LINE_HEIGHT(), self.align), clip, color, areas);
    }

    /// Push with an explicit `top` (for vertical scroll) clipped to `clip`.
    pub fn push_clipped<'a>(&'a self, left: f32, top: f32, clip: Rect, color: glyphon::Color, areas: &mut Vec<TextArea<'a>>) {
        areas.push(TextArea {
            buffer: &self.buffer,
            left,
            top,
            scale: 1.0,
            bounds: TextBounds {
                left: clip.x as i32,
                top: clip.y as i32,
                right: (clip.x + clip.w) as i32,
                bottom: (clip.y + clip.h) as i32,
            },
            default_color: color,
            custom_glyphs: &[],
        });
    }

    pub fn draw_center<'a>(&'a self, rect: Rect, color: glyphon::Color, areas: &mut Vec<TextArea<'a>>) {
        self.push(rect.x + (rect.w - self.width()) * 0.5, rect, color, areas);
    }

    pub fn draw_right<'a>(&'a self, rect: Rect, pad: f32, color: glyphon::Color, areas: &mut Vec<TextArea<'a>>) {
        self.push(rect.x + rect.w - self.width() - pad, rect, color, areas);
    }
}

/// A real editable single-line text input. It OWNS its text, a caret byte index,
/// and a selection anchor, plus all edit/navigation ops — so callers route
/// keystrokes/mouse into it instead of keeping a parallel String. When empty it
/// shows `placeholder`. The caret/selection are byte indices into `text`, mapped
/// to pixels via the shaped glyphs (content == text, so indices line up 1:1).
pub struct TextInput {
    buffer: Buffer,
    text: String,
    placeholder: String,
    focused: bool,
    shown: String, // last shaped content (change detection)
    epoch: u64,
    cursor: CursorIcon,
    pub align: VAlign,
    multiline: bool, // wrap + grow vertically; caret tracks the shaped layout
    width: f32,      // content width (for re-applying the wrap width on reshape)
    caret: usize,  // byte index of the caret in `text`
    anchor: usize, // selection anchor; == caret when there's no selection
}

impl TextInput {
    pub fn new(fs: &mut FontSystem, w: f32, h: f32) -> Self {
        Self {
            buffer: make_ui_buffer(fs, w, h),
            text: String::new(),
            placeholder: String::new(),
            focused: false,
            shown: String::new(),
            epoch: theme::shape_epoch(),
            cursor: CursorIcon::Text,
            align: VAlign::Center,
            multiline: false,
            width: w,
            caret: 0,
            anchor: 0,
        }
    }

    /// Enable multi-line editing (word-wrap + top-aligned, layout-aware caret).
    pub fn multiline(mut self, on: bool) -> Self {
        self.multiline = on;
        if on {
            self.align = VAlign::Top;
        }
        self
    }

    /// Re-shape after a zoom change (forces fresh metrics next reshape).
    pub fn rezoom(&mut self, fs: &mut FontSystem) {
        self.reshape(fs);
    }

    pub fn cursor(&self) -> CursorIcon {
        self.cursor
    }

    pub fn text(&self) -> &str {
        &self.text
    }

    pub fn focused(&self) -> bool {
        self.focused
    }

    pub fn focus(&mut self, on: bool) {
        self.focused = on;
    }

    fn reshape(&mut self, fs: &mut FontSystem) {
        let content = if self.text.is_empty() {
            self.placeholder.clone()
        } else {
            self.text.clone()
        };
        if self.shown == content && self.epoch == theme::shape_epoch() {
            return;
        }
        self.epoch = theme::shape_epoch();
        self.buffer.set_metrics(fs, Metrics::new(theme::UI_FONT_SIZE(), theme::UI_LINE_HEIGHT()));
        self.buffer
            .set_wrap(fs, if self.multiline { Wrap::WordOrGlyph } else { Wrap::None });
        // Multi-line: wrap at the field width and lay out plenty of rows (so text
        // past the first screenful is still shaped + visible, not clipped away).
        if self.multiline {
            // `width` is the unzoomed wrap boundary; scale it so the text wraps at
            // the field's actual (zoomed) width instead of in the middle at high zoom.
            self.buffer.set_size(fs, Some(self.width * theme::ui_zoom()), Some(4000.0));
        } else {
            // Single line: unbounded width (no wrap; the draw clips), and a height
            // that tracks the zoomed line height so the line isn't culled at high
            // zoom (the buffer keeps its small creation-time height otherwise).
            self.buffer.set_size(fs, None, Some(theme::UI_LINE_HEIGHT() * 2.0));
        }
        self.buffer.set_text(
            fs,
            &content,
            Attrs::new().family(Family::Name(theme::UI_FAMILY())),
            Shaping::Advanced,
        );
        self.buffer.shape_until_scroll(fs, false);
        self.shown = content;
    }

    /// Absolute byte index where logical line `line` starts in `text`.
    fn line_start_byte(&self, line: usize) -> usize {
        if line == 0 {
            return 0;
        }
        let mut seen = 0;
        for (i, c) in self.text.char_indices() {
            if c == '\n' {
                seen += 1;
                if seen == line {
                    return i + 1;
                }
            }
        }
        self.text.len()
    }

    /// Buffer-local (x, top, height) of the caret. cosmic-text glyph offsets are
    /// per-logical-line, so we resolve the caret's line + column first, then place
    /// it — including on a freshly-inserted empty line (no glyphs yet).
    fn caret_xy(&self) -> (f32, f32, f32) {
        let lh = theme::UI_LINE_HEIGHT();
        let caret = self.caret.min(self.text.len());
        let line = self.text[..caret].matches('\n').count();
        let col = caret - self.line_start_byte(line);
        let (mut last_bottom, mut last_h) = (0.0, lh);
        // A logical line may soft-wrap into several layout runs (same `line_i`).
        // Walk them in order and land on the visual row that actually contains the
        // caret byte, instead of snapping to the first row.
        let mut found_line = false;
        let mut end_of_line = (0.0, 0.0, lh);
        for run in self.buffer.layout_runs() {
            if run.line_i != line {
                if found_line {
                    break; // we've passed this logical line's runs
                }
                last_bottom = run.line_top + run.line_height;
                last_h = run.line_height;
                continue;
            }
            found_line = true;
            // First glyph at/after the caret column → caret sits at its left edge.
            for g in run.glyphs.iter() {
                if g.start as usize >= col {
                    return (g.x, run.line_top, run.line_height);
                }
            }
            // Caret is past every glyph on this row. If it's beyond this row's last
            // byte, a later wrapped row holds it — keep going; otherwise it's the
            // end of this row.
            let row_end = run.glyphs.last().map(|g| g.end as usize);
            end_of_line = (run.line_w, run.line_top, run.line_height);
            match row_end {
                Some(end) if col > end => continue,
                _ => return end_of_line,
            }
        }
        if found_line {
            return end_of_line; // caret at the end of the last wrapped row
        }
        (0.0, last_bottom, last_h) // caret on an empty trailing line
    }

    pub fn set_placeholder(&mut self, fs: &mut FontSystem, s: &str) {
        if self.placeholder != s {
            self.placeholder = s.to_string();
            if self.text.is_empty() {
                self.reshape(fs);
            }
        }
    }

    pub fn set_text(&mut self, fs: &mut FontSystem, s: &str) {
        self.text = s.to_string();
        self.caret = self.text.len();
        self.anchor = self.caret;
        self.reshape(fs);
    }

    pub fn clear(&mut self, fs: &mut FontSystem) {
        self.text.clear();
        self.caret = 0;
        self.anchor = 0;
        self.reshape(fs);
    }

    // ---- selection helpers ----

    pub fn has_selection(&self) -> bool {
        self.caret != self.anchor
    }

    fn sel_range(&self) -> (usize, usize) {
        (self.caret.min(self.anchor), self.caret.max(self.anchor))
    }

    pub fn selected_text(&self) -> &str {
        let (a, b) = self.sel_range();
        &self.text[a..b]
    }

    pub fn select_all(&mut self) {
        self.anchor = 0;
        self.caret = self.text.len();
    }

    fn delete_selection(&mut self) -> bool {
        if !self.has_selection() {
            return false;
        }
        let (a, b) = self.sel_range();
        self.text.replace_range(a..b, "");
        self.caret = a;
        self.anchor = a;
        true
    }

    fn prev_boundary(&self, i: usize) -> usize {
        let mut j = i.saturating_sub(1);
        while j > 0 && !self.text.is_char_boundary(j) {
            j -= 1;
        }
        j
    }

    fn next_boundary(&self, i: usize) -> usize {
        let mut j = (i + 1).min(self.text.len());
        while j < self.text.len() && !self.text.is_char_boundary(j) {
            j += 1;
        }
        j
    }

    // ---- editing ----

    pub fn insert(&mut self, fs: &mut FontSystem, s: &str) {
        self.delete_selection();
        self.text.insert_str(self.caret, s);
        self.caret += s.len();
        self.anchor = self.caret;
        self.reshape(fs);
    }

    pub fn backspace(&mut self, fs: &mut FontSystem) {
        if !self.delete_selection() && self.caret > 0 {
            let p = self.prev_boundary(self.caret);
            self.text.replace_range(p..self.caret, "");
            self.caret = p;
            self.anchor = p;
        }
        self.reshape(fs);
    }

    pub fn delete_forward(&mut self, fs: &mut FontSystem) {
        if !self.delete_selection() && self.caret < self.text.len() {
            let n = self.next_boundary(self.caret);
            self.text.replace_range(self.caret..n, "");
        }
        self.reshape(fs);
    }

    // ---- navigation (extend = hold Shift to grow the selection) ----

    pub fn move_left(&mut self, extend: bool) {
        if self.has_selection() && !extend {
            self.caret = self.sel_range().0;
        } else {
            self.caret = self.prev_boundary(self.caret);
        }
        if !extend {
            self.anchor = self.caret;
        }
    }

    pub fn move_right(&mut self, extend: bool) {
        if self.has_selection() && !extend {
            self.caret = self.sel_range().1;
        } else {
            self.caret = self.next_boundary(self.caret);
        }
        if !extend {
            self.anchor = self.caret;
        }
    }

    pub fn move_home(&mut self, extend: bool) {
        self.caret = 0;
        if !extend {
            self.anchor = 0;
        }
    }

    pub fn move_end(&mut self, extend: bool) {
        self.caret = self.text.len();
        if !extend {
            self.anchor = self.caret;
        }
    }

    // ---- geometry (pixel mapping via shaped glyphs) ----

    /// Width of the shaped content (used for label centering).
    pub fn width(&self) -> f32 {
        self.buffer
            .layout_runs()
            .next()
            .map(|r| r.line_w)
            .unwrap_or(0.0)
    }

    /// X offset (from the text start) of the caret position before `byte`.
    fn x_for_byte(&self, byte: usize) -> f32 {
        if let Some(run) = self.buffer.layout_runs().next() {
            let mut last_end = 0.0;
            for g in run.glyphs.iter() {
                if g.start >= byte {
                    return g.x;
                }
                last_end = g.x + g.w;
            }
            return last_end;
        }
        0.0
    }

    /// Byte index nearest the local x (relative to the text start). Single-line.
    fn byte_at_local_x(&self, local_x: f32) -> usize {
        self.byte_at_local(local_x, 0.0)
    }

    /// Byte index nearest local (x, y) within the shaped layout. For multi-line
    /// inputs this resolves the row by `y` first; single-line uses the only row.
    fn byte_at_local(&self, local_x: f32, local_y: f32) -> usize {
        if self.text.is_empty() {
            return 0;
        }
        for run in self.buffer.layout_runs() {
            let on_row = !self.multiline
                || (local_y >= run.line_top && local_y < run.line_top + run.line_height);
            if !on_row {
                continue;
            }
            let ls = self.line_start_byte(run.line_i);
            for g in run.glyphs.iter() {
                if local_x < g.x + g.w * 0.5 {
                    return ls + g.start as usize;
                }
            }
            // Past the last glyph on this row → end of the row.
            return run.glyphs.last().map(|g| ls + g.end as usize).unwrap_or(ls);
        }
        self.text.len()
    }

    fn local_top(&self, rect: Rect) -> f32 {
        if self.multiline {
            rect.y + theme::zpx(4.0)
        } else {
            rect.text_top(theme::UI_LINE_HEIGHT(), self.align)
        }
    }

    /// Handle a click in this field: focus, then place caret (1 click), select the
    /// word (2), or select all (3+). The single source of truth for click selection
    /// — callers pass the click count and the field handles the rest.
    pub fn on_click(&mut self, rect: Rect, pad_x: f32, x: f32, y: f32, clicks: u32) {
        self.focused = true;
        let b = self.byte_at_local(x - rect.x - pad_x, y - self.local_top(rect));
        match clicks {
            n if n >= 3 => self.select_all(),
            2 => self.select_word_byte(b),
            _ => {
                self.caret = b;
                self.anchor = b;
            }
        }
    }

    /// Extend the selection to a click/drag position (mouse drag).
    pub fn on_drag(&mut self, rect: Rect, pad_x: f32, x: f32, y: f32) {
        self.caret = self.byte_at_local(x - rect.x - pad_x, y - self.local_top(rect));
    }

    /// Place the caret (clearing selection) from a screen-space click.
    pub fn set_caret_from_x(&mut self, rect: Rect, pad_x: f32, x: f32) {
        let b = self.byte_at_local_x(x - rect.x - pad_x);
        self.caret = b;
        self.anchor = b;
    }

    /// Extend the selection to a screen-space x (mouse drag / shift-click).
    pub fn extend_to_x(&mut self, rect: Rect, pad_x: f32, x: f32) {
        self.caret = self.byte_at_local_x(x - rect.x - pad_x);
    }

    /// Select the whole word under a screen-space x (double-click).
    pub fn select_word_at(&mut self, rect: Rect, pad_x: f32, x: f32) {
        let b = self.byte_at_local_x(x - rect.x - pad_x);
        self.select_word_byte(b);
    }

    /// Select the word containing byte `b` (double-click; the single source of word
    /// selection used by both `select_word_at` and `on_click`).
    fn select_word_byte(&mut self, b: usize) {
        let is_word = |c: char| c.is_alphanumeric() || c == '_';
        // Walk left to the word start.
        let mut start = b;
        while start > 0 {
            let p = self.prev_boundary(start);
            if self.text[p..start].chars().next().map(is_word).unwrap_or(false) {
                start = p;
            } else {
                break;
            }
        }
        // Walk right to the word end.
        let mut end = b;
        while end < self.text.len() {
            let n = self.next_boundary(end);
            if self.text[end..n].chars().next().map(is_word).unwrap_or(false) {
                end = n;
            } else {
                break;
            }
        }
        if start == end {
            // Not on a word: just place the caret.
            self.caret = b;
            self.anchor = b;
        } else {
            self.anchor = start;
            self.caret = end;
        }
    }

    /// A thin caret bar at the caret position.
    pub fn caret_quad(&self, rect: Rect, pad_x: f32) -> Quad {
        if self.multiline {
            let (cx, top, lh) = self.caret_xy();
            let h = (lh - 4.0).max(6.0);
            return Quad::new(rect.x + pad_x + cx, rect.y + theme::zpx(4.0) + top + 2.0, 1.5, h, theme::CURSOR());
        }
        let x = rect.x + pad_x + self.x_for_byte(self.caret);
        let h = theme::UI_LINE_HEIGHT() - 6.0;
        let y = rect.text_top(h, self.align);
        Quad::new(x, y, 1.5, h, theme::CURSOR())
    }

    /// Selection highlight rect(s) — one quad per shaped row the selection covers
    /// (so it works for both single-line and multi-line fields).
    pub fn selection_quads(&self, rect: Rect, pad_x: f32, out: &mut Vec<Quad>) {
        if !self.has_selection() {
            return;
        }
        let (a, b) = self.sel_range();
        let left = rect.x + pad_x;
        let top0 = self.local_top(rect);
        for run in self.buffer.layout_runs() {
            let ls = self.line_start_byte(run.line_i);
            let (mut x0, mut x1) = (f32::MAX, f32::MIN);
            for g in run.glyphs.iter() {
                let (gs, ge) = (ls + g.start as usize, ls + g.end as usize);
                if ge <= a || gs >= b {
                    continue;
                }
                x0 = x0.min(g.x);
                x1 = x1.max(g.x + g.w);
            }
            if x1 > x0 {
                let y = top0 + if self.multiline { run.line_top } else { 0.0 };
                out.push(Quad::new(left + x0, y + 1.0, (x1 - x0).max(1.0), run.line_height - 2.0, theme::SELECTION()));
            }
        }
    }

    pub fn draw<'a>(&'a self, rect: Rect, pad_x: f32, color: glyphon::Color, areas: &mut Vec<TextArea<'a>>) {
        let top = if self.multiline {
            rect.y + theme::zpx(4.0)
        } else {
            rect.text_top(theme::UI_LINE_HEIGHT(), self.align)
        };
        areas.push(TextArea {
            buffer: &self.buffer,
            left: rect.x + pad_x,
            top,
            scale: 1.0,
            bounds: TextBounds {
                left: rect.x as i32,
                top: rect.y as i32,
                right: (rect.x + rect.w) as i32,
                bottom: (rect.y + rect.h) as i32,
            },
            default_color: color,
            custom_glyphs: &[],
        });
    }
}

/// VSCode-style header "command center": a centered box with a leading search
/// glyph and a centered label, clickable to open the command palette. Composes
/// the reusable IconButton (search glyph) + TextInput (label) so it inherits
/// their rendering; the box chrome is drawn from the same rect.
pub struct SearchField {
    icon: IconButton,
    label: TextInput,
    cursor: CursorIcon,
}

impl SearchField {
    pub fn new(fs: &mut FontSystem) -> Self {
        let mut icon = IconButton::new(fs, theme::ICON_SEARCH, theme::ICON_FAMILY, 12.0);
        icon.cursor = CursorIcon::Pointer;
        Self {
            icon,
            label: TextInput::new(fs, 700.0, theme::TITLE_BAR_H()),
            cursor: CursorIcon::Pointer,
        }
    }

    pub fn cursor(&self) -> CursorIcon {
        self.cursor
    }

    pub fn set(&mut self, fs: &mut FontSystem, label: &str) {
        self.label.set_placeholder(fs, label);
    }

    /// Box fill + 1px border, drawn from `rect`.
    pub fn draw_bg(&self, rect: Rect, hovered: bool, bg_quads: &mut Vec<Quad>) {
        let fill = if hovered {
            theme::SEARCH_BG_HOVER()
        } else {
            theme::SEARCH_BG()
        };
        // Rounded pill with a 1px border ring (zoom-scaled radius).
        let r = (rect.h * 0.5).min(theme::zpx(8.0));
        bg_quads.push(Rect { x: rect.x - 1.0, y: rect.y - 1.0, w: rect.w + 2.0, h: rect.h + 2.0 }.rounded_quad(theme::SEARCH_BORDER(), r + 1.0));
        bg_quads.push(rect.rounded_quad(fill, r));
    }

    pub fn draw<'a>(&'a self, rect: Rect, areas: &mut Vec<TextArea<'a>>) {
        // Leading search glyph at the left edge.
        let icon_rect = Rect { x: rect.x + theme::zpx(4.0), y: rect.y, w: theme::zpx(22.0), h: rect.h };
        self.icon.draw(icon_rect, theme::TITLE_FG(), areas);
        // Centered label, kept clear of the icon.
        let pad = ((rect.w - self.label.width()) * 0.5).max(theme::zpx(28.0));
        self.label.draw(rect, pad, theme::FG_TEXT(), areas);
    }

    /// Re-shape the icon glyph + label at the current zoom.
    pub fn reshape(&mut self, fs: &mut FontSystem) {
        self.icon.reshape(fs);
        self.label.rezoom(fs);
    }
}

/// Top-left menu bar (File, Edit, ...). Each item is a TextLabel laid out
/// left-to-right; the per-item rects are the single source of truth for hover
/// backgrounds and hit-testing.
const MENU_ITEMS: [&str; 8] = [
    "File", "Edit", "Selection", "View", "Go", "Run", "Terminal", "Help",
];

pub struct MenuBar {
    labels: Vec<TextLabel>,
    cursor: CursorIcon,
}

impl MenuBar {
    pub fn new(fs: &mut FontSystem) -> Self {
        let labels = MENU_ITEMS
            .iter()
            .map(|t| {
                let mut l = TextLabel::new(fs, 240.0, theme::TITLE_BAR_H());
                l.set(fs, t, theme::UI_FAMILY());
                l
            })
            .collect();
        Self {
            labels,
            cursor: CursorIcon::Pointer,
        }
    }

    pub fn cursor(&self) -> CursorIcon {
        self.cursor
    }

    pub fn reshape(&mut self, fs: &mut FontSystem) {
        for l in &mut self.labels {
            l.reshape(fs);
        }
    }

    /// Per-item rects laid out from the left edge of `bar`.
    pub fn item_rects(&self, bar: Rect) -> Vec<Rect> {
        let pad = theme::zpx(9.0);
        let mut x = bar.x + theme::zpx(6.0);
        self.labels
            .iter()
            .map(|l| {
                let w = l.width() + pad * 2.0;
                let r = Rect { x, y: bar.y, w, h: bar.h };
                x += w;
                r
            })
            .collect()
    }

    pub fn item_at(&self, bar: Rect, p: (f32, f32)) -> Option<usize> {
        self.item_rects(bar).iter().position(|r| r.contains(p))
    }

    pub fn draw_bg(&self, bar: Rect, hovered: Option<usize>, bg_quads: &mut Vec<Quad>) {
        if let Some(i) = hovered {
            if let Some(r) = self.item_rects(bar).get(i) {
                bg_quads.push(r.quad(theme::MENU_HOVER()));
            }
        }
    }

    pub fn draw<'a>(&'a self, bar: Rect, areas: &mut Vec<TextArea<'a>>) {
        let rects = self.item_rects(bar);
        for (l, r) in self.labels.iter().zip(rects) {
            l.draw_center(r, theme::TITLE_FG(), areas);
        }
    }
}

/// Reusable line-number gutter. Owns its buffer and rebuilds only when the line
/// count changes. Encapsulates the glyphon "first laid-out line is dropped"
/// quirk in one place (see the known-issue note in `set`).
pub struct Gutter {
    buffer: Buffer,
    last_lines: usize,
    last_rows: usize,
    last_active: usize,
    last_epoch: u64,
}

impl Gutter {
    pub fn new(fs: &mut FontSystem, w: f32) -> Self {
        Self {
            buffer: make_ui_buffer_mono(fs, w, 4000.0),
            last_lines: usize::MAX,
            last_rows: usize::MAX,
            last_active: usize::MAX,
            last_epoch: u64::MAX,
        }
    }

    /// Force a rebuild on next `set_from_buffer` (e.g. after the editor font changed).
    pub fn invalidate(&mut self) {
        self.last_rows = usize::MAX;
    }

    /// Line numbers for a window of a large document: `first..first+count`
    /// (0-based), one row each — large docs never wrap, so rows are uniform. The
    /// draw site offsets by the window's pixel position.
    pub fn set_range(&mut self, fs: &mut FontSystem, first: usize, count: usize, active_line: usize) {
        let epoch = theme::shape_epoch();
        if count == self.last_rows && first == self.last_lines && active_line == self.last_active && epoch == self.last_epoch {
            return;
        }
        let mono = Attrs::new().family(Family::Name(theme::MONO_FAMILY()));
        let dim = mono.color(theme::FG_GUTTER());
        let active = mono.color(theme::FG_GUTTER_ACTIVE());
        let spans: Vec<(String, Attrs)> = (first..first + count)
            .map(|l| {
                let a = if l == active_line { active } else { dim };
                (format!("{:>6} \n", l + 1), a)
            })
            .collect();
        self.buffer.set_metrics(fs, Metrics::new(theme::FONT_SIZE(), theme::LINE_HEIGHT()));
        self.buffer
            .set_size(fs, None, Some(count as f32 * theme::LINE_HEIGHT() + 200.0));
        self.buffer.set_rich_text(
            fs,
            spans.iter().map(|(s, a)| (s.as_str(), *a)),
            mono,
            Shaping::Basic,
        );
        self.buffer.shape_until_scroll(fs, false);
        // Repurpose the change-detection slots: lines = window start, rows = count.
        self.last_lines = first;
        self.last_rows = count;
        self.last_active = active_line;
        self.last_epoch = epoch;
    }

    /// Build line numbers aligned to the editor buffer's VISUAL rows: the number
    /// on each logical line's first row, blanks on wrap-continuation rows. This
    /// keeps numbers aligned whether or not word-wrap is on (with wrap off there's
    /// one visual row per logical line, so it's just 1..n).
    pub fn set_from_buffer(&mut self, fs: &mut FontSystem, src: &Buffer, active_line: usize) {
        // Cheap change-detection: (logical line count, total visual rows, active line,
        // theme epoch) — the active line's number is drawn bright, so a cursor move
        // or theme change must rebuild.
        let mut rows = 0usize;
        let mut lines = 0usize;
        let mut prev = usize::MAX;
        for run in src.layout_runs() {
            rows += 1;
            if run.line_i != prev {
                lines = run.line_i + 1;
                prev = run.line_i;
            }
        }
        let epoch = theme::shape_epoch();
        if rows == self.last_rows && lines == self.last_lines && active_line == self.last_active && epoch == self.last_epoch {
            return;
        }

        // Build per-row colored spans: the active logical line's number bright
        // (fg_gutter_active), the rest dim (fg_gutter). VS Code's active-line gutter.
        // NOTE: line 1's "1" doesn't render on real GPUs (glyphon drops this
        // buffer's first laid-out line) — known minor issue.
        let mono = Attrs::new().family(Family::Name(theme::MONO_FAMILY()));
        let dim = mono.color(theme::FG_GUTTER());
        let active = mono.color(theme::FG_GUTTER_ACTIVE());
        let mut spans: Vec<(String, Attrs)> = Vec::new();
        let mut prev = usize::MAX;
        for run in src.layout_runs() {
            if run.line_i != prev {
                let a = if run.line_i == active_line { active } else { dim };
                spans.push((format!("{:>4} \n", run.line_i + 1), a));
                prev = run.line_i;
            } else {
                spans.push(("\n".to_string(), dim)); // wrap-continuation row → blank
            }
        }
        self.buffer.set_metrics(fs, Metrics::new(theme::FONT_SIZE(), theme::LINE_HEIGHT()));
        self.buffer
            .set_size(fs, None, Some(rows as f32 * theme::LINE_HEIGHT() + 200.0));
        self.buffer.set_rich_text(
            fs,
            spans.iter().map(|(s, a)| (s.as_str(), *a)),
            mono,
            Shaping::Advanced,
        );
        self.buffer.shape_until_scroll(fs, false);
        self.last_lines = lines;
        self.last_rows = rows;
        self.last_active = active_line;
        self.last_epoch = epoch;
    }

    /// Gutter numbers for one side of a side-by-side diff: the old number per row
    /// when `left`, else the new number; blank on filler/hunk rows. Aligns 1:1 with
    /// that side's diff buffer.
    pub fn set_from_diff_side(&mut self, fs: &mut FontSystem, rows: &[crate::diff::DiffRow], left: bool) {
        let mut s = String::with_capacity(rows.len() * 6);
        for r in rows {
            match if left { r.left } else { r.right } {
                Some(v) => s.push_str(&format!("{:>4} \n", v)),
                None => s.push_str("     \n"),
            }
        }
        self.buffer.set_metrics(fs, Metrics::new(theme::FONT_SIZE(), theme::LINE_HEIGHT()));
        self.buffer
            .set_size(fs, None, Some(rows.len() as f32 * theme::LINE_HEIGHT() + 200.0));
        self.buffer.set_text(
            fs,
            &s,
            Attrs::new().family(Family::Name(theme::MONO_FAMILY())),
            Shaping::Advanced,
        );
        self.buffer.shape_until_scroll(fs, false);
        // Force the next set_from_buffer (for a normal doc) to rebuild.
        self.last_rows = usize::MAX;
        self.last_lines = usize::MAX;
    }

    pub fn draw<'a>(&'a self, region: Rect, scroll_y: f32, color: glyphon::Color, areas: &mut Vec<TextArea<'a>>) {
        areas.push(TextArea {
            buffer: &self.buffer,
            left: region.x,
            top: region.y + theme::EDITOR_PAD() - scroll_y,
            scale: 1.0,
            bounds: TextBounds {
                left: region.x as i32,
                top: region.y as i32,
                right: (region.x + region.w) as i32,
                bottom: (region.y + region.h) as i32,
            },
            default_color: color,
            custom_glyphs: &[],
        });
    }

    /// Draw the gutter with line 0 placed at `top`, clipped to `clip` — used to
    /// render fold-aware segments (each visible run of lines at its shifted y).
    pub fn draw_clipped<'a>(&'a self, clip: Rect, top: f32, color: glyphon::Color, areas: &mut Vec<TextArea<'a>>) {
        areas.push(TextArea {
            buffer: &self.buffer,
            left: clip.x,
            top,
            scale: 1.0,
            bounds: TextBounds {
                left: clip.x as i32,
                top: clip.y as i32,
                right: (clip.x + clip.w) as i32,
                bottom: (clip.y + clip.h) as i32,
            },
            default_color: color,
            custom_glyphs: &[],
        });
    }
}

/// Reusable vertical list of fixed-height rows backed by one shared multi-line
/// buffer. Provides the single source of truth for row geometry (`row_rect` /
/// `row_at`) shared by hover/selection backgrounds, hit-testing, and the text
/// draw. Used for the file tree and the command palette list.
pub struct ListView {
    buffer: Buffer,
    last_key: String,
    epoch: u64,
    base_row_h: f32, // unzoomed; row_h() scales by the current UI zoom
    base_pad_x: f32,
    cursor: CursorIcon,
}

impl ListView {
    pub fn new(fs: &mut FontSystem, w: f32, h: f32, row_h: f32, pad_x: f32) -> Self {
        let z = theme::ui_zoom();
        Self {
            buffer: make_ui_buffer(fs, w, h),
            last_key: String::new(),
            epoch: theme::shape_epoch(),
            base_row_h: row_h / z,
            base_pad_x: pad_x / z,
            cursor: CursorIcon::Pointer,
        }
    }

    fn row_h(&self) -> f32 {
        self.base_row_h * theme::ui_zoom()
    }

    pub fn cursor(&self) -> CursorIcon {
        self.cursor
    }

    pub fn set_text(&mut self, fs: &mut FontSystem, key: &str, w: f32, h: f32) {
        if self.last_key == key && self.epoch == theme::shape_epoch() {
            return;
        }
        self.epoch = theme::shape_epoch();
        self.buffer.set_metrics(fs, Metrics::new(theme::UI_FONT_SIZE(), theme::UI_LINE_HEIGHT()));
        // One row per line: never wrap (long names clip at the row, not spill to a
        // second line and misalign the list).
        self.buffer.set_wrap(fs, Wrap::None);
        self.buffer.set_size(fs, Some(w), Some(h));
        self.buffer.set_text(
            fs,
            key,
            Attrs::new().family(Family::Name(theme::UI_FAMILY())),
            Shaping::Advanced,
        );
        self.buffer.shape_until_scroll(fs, false);
        self.last_key = key.to_string();
    }

    pub fn set_rich(&mut self, fs: &mut FontSystem, key: &str, spans: &[(String, Attrs)], w: f32, h: f32) {
        if self.last_key == key && self.epoch == theme::shape_epoch() {
            return;
        }
        self.epoch = theme::shape_epoch();
        self.buffer.set_metrics(fs, Metrics::new(theme::UI_FONT_SIZE(), theme::UI_LINE_HEIGHT()));
        self.buffer.set_wrap(fs, Wrap::None); // one row per line (see set_text)
        self.buffer.set_size(fs, Some(w), Some(h));
        let default = Attrs::new()
            .family(Family::Name(theme::UI_FAMILY()))
            .color(theme::FG_TEXT());
        self.buffer.set_rich_text(
            fs,
            spans.iter().map(|(s, a)| (s.as_str(), *a)),
            default,
            Shaping::Advanced,
        );
        self.buffer.shape_until_scroll(fs, false);
        self.last_key = key.to_string();
    }

    pub fn row_rect(&self, region: Rect, i: usize) -> Rect {
        let rh = self.row_h();
        Rect {
            x: region.x,
            y: region.y + i as f32 * rh,
            w: region.w,
            h: rh,
        }
    }

    /// Row index under `p` within `region`, bounded to `count` rows.
    pub fn row_at(&self, region: Rect, p: (f32, f32), count: usize) -> Option<usize> {
        self.row_at_scrolled(region, 0.0, p, count)
    }

    /// Row under `p` when the list is scrolled by `scroll_y` (content shifted up).
    /// `region` is the visible viewport (used for the bounds check).
    pub fn row_at_scrolled(&self, region: Rect, scroll_y: f32, p: (f32, f32), count: usize) -> Option<usize> {
        if !region.contains(p) {
            return None;
        }
        let idx = ((p.1 - region.y + scroll_y) / self.row_h()) as usize;
        (idx < count).then_some(idx)
    }

    pub fn draw<'a>(&'a self, region: Rect, color: glyphon::Color, areas: &mut Vec<TextArea<'a>>) {
        self.draw_at(region, region.y, color, areas);
    }

    /// Pixel x-span (relative to the buffer's left, i.e. add `clip.x + pad_x`) of a
    /// byte range on row `row`, for drawing match highlights. None if not found.
    pub fn line_x_range(&self, row: usize, byte_start: usize, byte_end: usize) -> Option<(f32, f32)> {
        for run in self.buffer.layout_runs() {
            if run.line_i != row {
                continue;
            }
            let (mut x0, mut x1) = (f32::MAX, f32::MIN);
            for g in run.glyphs {
                if g.end <= byte_start || g.start >= byte_end {
                    continue;
                }
                x0 = x0.min(g.x);
                x1 = x1.max(g.x + g.w);
            }
            return (x1 > x0).then_some((x0, x1));
        }
        None
    }

    /// Left padding (px) the buffer is drawn at within its region.
    pub fn pad_x(&self) -> f32 {
        self.base_pad_x * theme::ui_zoom()
    }

    /// Draw the buffer with row 0 placed at `top`, clipped to `clip`. Lets a
    /// caller render a vertical slice (e.g. rows above/below an inline insert).
    pub fn draw_at<'a>(&'a self, clip: Rect, top: f32, color: glyphon::Color, areas: &mut Vec<TextArea<'a>>) {
        areas.push(TextArea {
            buffer: &self.buffer,
            left: clip.x + self.pad_x(),
            top,
            scale: 1.0,
            bounds: TextBounds {
                left: clip.x as i32,
                top: clip.y as i32,
                right: (clip.x + clip.w) as i32,
                bottom: (clip.y + clip.h) as i32,
            },
            default_color: color,
            custom_glyphs: &[],
        });
    }
}

/// A floating popup menu (right-click context menu): a bordered box of rows,
/// positioned from an anchor and clamped to the window. Wraps a `ListView` for
/// row geometry/hit-test/draw, so item rects are the single source of truth for
/// hover highlight and clicks. The caller maps item index → action.
pub struct Menu {
    list: ListView,
    count: usize,
    base_w: f32, // unzoomed width; width() scales it so items don't wrap when zoomed
    seps: Vec<usize>, // separator row indices — drawn as divider lines, not clickable
}

impl Menu {
    pub fn new(fs: &mut FontSystem, width: f32) -> Self {
        Self {
            list: ListView::new(fs, width, 400.0, theme::MENU_ITEM_H(), 12.0),
            count: 0,
            base_w: width / theme::ui_zoom(),
            seps: Vec::new(),
        }
    }

    fn width(&self) -> f32 {
        self.base_w * theme::ui_zoom()
    }

    /// Full rows: `(label, shortcut-hint, is_separator)`. Hints render dim after the
    /// label; separator rows draw as divider lines and don't hover or click.
    pub fn set_entries(&mut self, fs: &mut FontSystem, rows: &[(&str, &str, bool)]) {
        let label_attrs = glyphon::Attrs::new()
            .family(glyphon::Family::Name(theme::UI_FAMILY()))
            .color(theme::FG_TEXT());
        let hint_attrs = glyphon::Attrs::new()
            .family(glyphon::Family::Name(theme::UI_FAMILY()))
            .color(theme::FG_DIM());
        let mut key = String::from("M\n");
        let mut spans: Vec<(String, glyphon::Attrs<'static>)> = Vec::new();
        self.seps.clear();
        for (i, (label, hint, sep)) in rows.iter().enumerate() {
            if *sep {
                self.seps.push(i);
                spans.push(("\n".to_string(), label_attrs));
                key.push_str("--\n");
                continue;
            }
            spans.push((format!(" {}", label), label_attrs));
            if hint.is_empty() {
                spans.push(("\n".to_string(), label_attrs));
            } else {
                spans.push((format!("    {}\n", hint), hint_attrs));
            }
            key.push_str(label);
            key.push(' ');
            key.push_str(hint);
            key.push('\n');
        }
        self.list.set_rich(fs, &key, &spans, self.width(), 4000.0);
        self.count = rows.len();
    }

    /// The popup box rect for `anchor`, clamped within the window `win`.
    pub fn rect(&self, anchor: (f32, f32), win: (f32, f32)) -> Rect {
        let h = self.count as f32 * theme::MENU_ITEM_H() + theme::zpx(8.0);
        let w = self.width();
        Rect {
            x: anchor.0.min(win.0 - w - theme::zpx(4.0)).max(0.0),
            y: anchor.1.min(win.1 - h - theme::zpx(4.0)).max(0.0),
            w,
            h,
        }
    }

    fn inner(&self, menu: Rect) -> Rect {
        Rect { x: menu.x, y: menu.y + theme::zpx(4.0), w: menu.w, h: menu.h - theme::zpx(8.0) }
    }

    pub fn item_at(&self, menu: Rect, p: (f32, f32)) -> Option<usize> {
        self.list
            .row_at(self.inner(menu), p, self.count)
            .filter(|i| !self.seps.contains(i))
    }

    pub fn draw_bg(&self, menu: Rect, hovered: Option<usize>, quads: &mut Vec<Quad>) {
        let r = theme::zpx(8.0);
        // Soft drop shadow so the menu floats above the content.
        for i in 1..=5 {
            let s = i as f32 * theme::zpx(2.0);
            let a = 0.14 * (1.0 - (i as f32 - 1.0) / 5.0);
            quads.push(
                Rect { x: menu.x - s, y: menu.y - s + theme::zpx(2.0), w: menu.w + s * 2.0, h: menu.h + s * 2.0 }
                    .rounded_quad([0.0, 0.0, 0.0, a], r + s),
            );
        }
        quads.push(
            Rect { x: menu.x - 1.0, y: menu.y - 1.0, w: menu.w + 2.0, h: menu.h + 2.0 }
                .rounded_quad(theme::CONTEXT_BORDER(), r + 1.0),
        );
        quads.push(menu.rounded_quad(theme::CONTEXT_BG(), r));
        // Divider lines for separator rows.
        for &i in &self.seps {
            let row = self.list.row_rect(self.inner(menu), i);
            let y = row.y + row.h * 0.5;
            quads.push(Quad::new(row.x + theme::zpx(8.0), y, row.w - theme::zpx(16.0), 1.0, [1.0, 1.0, 1.0, 0.10]));
        }
        if let Some(i) = hovered {
            if !self.seps.contains(&i) {
                let row = self.list.row_rect(self.inner(menu), i);
                let pill = Rect { x: row.x + theme::zpx(4.0), y: row.y + theme::zpx(1.0), w: row.w - theme::zpx(8.0), h: (row.h - theme::zpx(2.0)).max(2.0) };
                quads.push(pill.rounded_quad(theme::CONTEXT_SEL(), theme::zpx(5.0)));
            }
        }
    }

    pub fn draw<'a>(&'a self, menu: Rect, areas: &mut Vec<TextArea<'a>>) {
        self.list.draw(self.inner(menu), theme::FG_TEXT(), areas);
    }
}

/// A centered modal dialog: a message, 2–3 buttons (right-aligned), and an
/// optional "don't ask again" checkbox (bottom-left). Owns its text buffers;
/// the caller maps button index → action. All geometry (box, buttons, checkbox)
/// lives here as the single source of truth for draw + hit-test.
pub struct Dialog {
    message: TextLabel,
    buttons: Vec<TextLabel>,
    check: TextLabel,
    width: f32,
}

impl Dialog {
    pub fn new(fs: &mut FontSystem) -> Self {
        Self {
            // Wide buffer so the (single-line) message is measured in full and never
            // clipped — the box sizes itself to this width.
            message: TextLabel::new(fs, 4000.0, 60.0),
            buttons: Vec::new(),
            check: TextLabel::new(fs, 4000.0, 24.0),
            width: 460.0,
        }
    }

    pub fn set(&mut self, fs: &mut FontSystem, message: &str, buttons: &[&str], check: Option<&str>) {
        self.message.set(fs, message, theme::UI_FAMILY());
        if self.buttons.len() != buttons.len() {
            self.buttons = buttons.iter().map(|_| TextLabel::new(fs, 200.0, theme::DIALOG_BTN_H())).collect();
        }
        for (b, l) in self.buttons.iter_mut().zip(buttons) {
            b.set(fs, l, theme::UI_FAMILY());
        }
        if let Some(c) = check {
            self.check.set(fs, c, theme::UI_FAMILY());
        }
    }

    pub fn box_rect(&self, win: (f32, f32), has_check: bool) -> Rect {
        let h = theme::zpx(if has_check { 176.0 } else { 148.0 });
        // Size to the message (so long prompts don't clip), with a sensible minimum.
        // Everything scales with the UI zoom.
        let pad = theme::zpx(40.0);
        let w = (self.message.width() + pad * 2.0).max(theme::zpx(self.width));
        Rect { x: (win.0 - w) * 0.5, y: (win.1 - h) * 0.5, w, h }
    }

    pub fn button_rects(&self, b: Rect) -> Vec<Rect> {
        let bw = theme::zpx(100.0);
        let bh = theme::DIALOG_BTN_H();
        let gap = theme::zpx(10.0);
        let edge = theme::zpx(16.0);
        let n = self.buttons.len();
        let y = b.y + b.h - bh - edge;
        (0..n)
            .map(|i| Rect {
                x: b.x + b.w - edge - (n - i) as f32 * (bw + gap) + gap,
                y,
                w: bw,
                h: bh,
            })
            .collect()
    }

    fn check_box(&self, b: Rect) -> Rect {
        let s = theme::zpx(18.0);
        Rect { x: b.x + theme::zpx(18.0), y: b.y + b.h - theme::zpx(47.0), w: s, h: s }
    }

    pub fn button_at(&self, b: Rect, p: (f32, f32)) -> Option<usize> {
        self.button_rects(b).iter().position(|r| r.contains(p))
    }

    /// True if `p` hit the checkbox or its label.
    pub fn check_hit(&self, b: Rect, p: (f32, f32)) -> bool {
        let cb = self.check_box(b);
        Rect { x: cb.x, y: cb.y - theme::zpx(2.0), w: theme::zpx(220.0), h: cb.h + theme::zpx(4.0) }.contains(p)
    }

    pub fn draw_bg(
        &self,
        b: Rect,
        win: (f32, f32),
        hovered: Option<usize>,
        checked: bool,
        has_check: bool,
        quads: &mut Vec<Quad>,
    ) {
        quads.push(Rect { x: 0.0, y: 0.0, w: win.0, h: win.1 }.quad(theme::DIALOG_OVERLAY()));
        let radius = theme::zpx(12.0);
        // Soft drop shadow behind the card.
        for i in 1..=6 {
            let s = i as f32 * theme::zpx(2.5);
            let a = 0.18 * (1.0 - (i as f32 - 1.0) / 6.0);
            quads.push(
                Rect { x: b.x - s, y: b.y - s + theme::zpx(4.0), w: b.w + s * 2.0, h: b.h + s * 2.0 }
                    .rounded_quad([0.0, 0.0, 0.0, a], radius + s),
            );
        }
        quads.push(Rect { x: b.x - 1.0, y: b.y - 1.0, w: b.w + 2.0, h: b.h + 2.0 }.rounded_quad(theme::PALETTE_BORDER(), radius + 1.0));
        quads.push(b.rounded_quad(theme::PALETTE_BG(), radius));
        for (i, r) in self.button_rects(b).iter().enumerate() {
            let c = if hovered == Some(i) { theme::DIALOG_BTN_HOVER() } else { theme::DIALOG_BTN() };
            quads.push(r.rounded_quad(c, theme::zpx(6.0)));
        }
        if has_check {
            let cb = self.check_box(b);
            quads.push(Rect { x: cb.x - 1.0, y: cb.y - 1.0, w: cb.w + 2.0, h: cb.h + 2.0 }.rounded_quad(theme::PALETTE_BORDER(), theme::zpx(4.0)));
            quads.push(cb.rounded_quad(if checked { theme::DIALOG_BTN_HOVER() } else { theme::PALETTE_INPUT_BG() }, theme::zpx(3.0)));
        }
    }

    pub fn draw<'a>(&'a self, b: Rect, has_check: bool, areas: &mut Vec<TextArea<'a>>) {
        let pad = theme::zpx(18.0);
        let msg = Rect { x: b.x + pad, y: b.y + theme::zpx(20.0), w: b.w - pad * 2.0, h: theme::UI_LINE_HEIGHT() };
        self.message.draw_left(msg, 0.0, theme::FG_TEXT(), areas);
        for (lab, r) in self.buttons.iter().zip(self.button_rects(b)) {
            lab.draw_center(r, theme::FG_TEXT(), areas);
        }
        if has_check {
            let cb = self.check_box(b);
            let lr = Rect { x: cb.x + cb.w + theme::zpx(8.0), y: cb.y - theme::zpx(2.0), w: theme::zpx(220.0), h: theme::UI_LINE_HEIGHT() };
            self.check.draw_left(lr, 0.0, theme::FG_DIM(), areas);
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Axis {
    Vertical,
    Horizontal,
}

/// A scrollbar for one axis. Stateless about content (computes the thumb from
/// content/viewport/scroll each call) but owns its drag state. The thumb rect is
/// the single source of truth shared by drawing, hit-testing, and drag math.
pub struct Scrollbar {
    dragging: bool,
    grab: f32, // cursor offset within the thumb at drag start
    axis: Axis,
}

impl Scrollbar {
    pub fn new(axis: Axis) -> Self {
        Self { dragging: false, grab: 0.0, axis }
    }

    pub fn is_dragging(&self) -> bool {
        self.dragging
    }

    /// Thumb rect within `track`, or None when everything fits (no scrollbar).
    pub fn thumb(&self, track: Rect, content: f32, view: f32, scroll: f32) -> Option<Rect> {
        if content <= view {
            return None;
        }
        let max_scroll = (content - view).max(1.0);
        let t = (scroll / max_scroll).clamp(0.0, 1.0);
        match self.axis {
            Axis::Vertical => {
                let len = track.h;
                let th = (view / content * len).max(24.0).min(len);
                Some(Rect { x: track.x + 2.0, y: track.y + t * (len - th), w: track.w - 4.0, h: th })
            }
            Axis::Horizontal => {
                let len = track.w;
                let tw = (view / content * len).max(24.0).min(len);
                Some(Rect { x: track.x + t * (len - tw), y: track.y + 2.0, w: tw, h: track.h - 4.0 })
            }
        }
    }

    /// Start a drag if `p` is on the thumb.
    pub fn press(&mut self, p: (f32, f32), track: Rect, content: f32, view: f32, scroll: f32) -> bool {
        if let Some(th) = self.thumb(track, content, view, scroll) {
            if th.contains(p) {
                self.dragging = true;
                self.grab = match self.axis {
                    Axis::Vertical => p.1 - th.y,
                    Axis::Horizontal => p.0 - th.x,
                };
                return true;
            }
        }
        false
    }

    /// Press anywhere in the `track`. If on the thumb, drag from where it was
    /// grabbed; otherwise jump the thumb to center on the click (scroll-to-here),
    /// then drag. Returns the resulting scroll offset, or None if not in the track
    /// (or nothing overflows). Either way a drag begins so the user can keep moving.
    pub fn press_track(&mut self, p: (f32, f32), track: Rect, content: f32, view: f32, scroll: f32) -> Option<f32> {
        if content <= view || !track.contains(p) {
            return None;
        }
        let max_scroll = (content - view).max(1.0);
        let (len, cursor, origin) = match self.axis {
            Axis::Vertical => (track.h, p.1, track.y),
            Axis::Horizontal => (track.w, p.0, track.x),
        };
        let thumb = (view / content * len).max(24.0).min(len);
        let cur_start = origin + (scroll / max_scroll).clamp(0.0, 1.0) * (len - thumb);
        self.dragging = true;
        if cursor >= cur_start && cursor <= cur_start + thumb {
            self.grab = cursor - cur_start; // grabbed the thumb where it sits
            Some(scroll)
        } else {
            self.grab = thumb * 0.5; // center the thumb under the cursor, then drag
            let usable = (len - thumb).max(1.0);
            let t = ((cursor - self.grab - origin) / usable).clamp(0.0, 1.0);
            Some(t * max_scroll)
        }
    }

    /// While dragging, map the cursor to a new scroll offset.
    pub fn drag(&self, p: (f32, f32), track: Rect, content: f32, view: f32) -> Option<f32> {
        if !self.dragging || content <= view {
            return None;
        }
        let max_scroll = content - view;
        let (len, cursor, origin) = match self.axis {
            Axis::Vertical => (track.h, p.1, track.y),
            Axis::Horizontal => (track.w, p.0, track.x),
        };
        let thumb = (view / content * len).max(24.0).min(len);
        let usable = (len - thumb).max(1.0);
        let t = ((cursor - self.grab - origin) / usable).clamp(0.0, 1.0);
        Some(t * max_scroll)
    }

    pub fn release(&mut self) {
        self.dragging = false;
    }
}

/// A draggable divider that owns a resizable dimension (e.g. the sidebar width
/// or the terminal-panel height) plus its clamp range and drag state.
/// Self-contained: the rest of the app just asks for `size()` and forwards mouse
/// events; the handle geometry, hit-test, and clamping all live here.
///
/// Axis-aware so it's reused for both orientations:
/// - `Axis::Horizontal` resizes a *width* — handle straddles the region's right
///   edge, ColResize cursor, size grows as the cursor moves right (`cursor - origin`).
/// - `Axis::Vertical` resizes a *height* — handle straddles the region's top
///   edge, RowResize cursor, size grows as the cursor moves up (`origin - cursor`).
pub struct Splitter {
    size: f32,
    min: f32,
    max: f32,
    dragging: bool,
    axis: Axis,
    from_end: bool, // panel anchored to the far edge (right sidebar): handle + drag flip
}

impl Splitter {
    pub fn new(size: f32, min: f32, max: f32, axis: Axis) -> Self {
        Self {
            size,
            min,
            max,
            dragging: false,
            axis,
            from_end: false,
        }
    }

    /// A splitter whose panel is anchored to the FAR edge (right sidebar): the
    /// handle sits on the region's near (left) edge and the size is measured
    /// back from `origin` (the window's right edge) while dragging.
    pub fn new_from_end(size: f32, min: f32, max: f32, axis: Axis) -> Self {
        Self {
            size,
            min,
            max,
            dragging: false,
            axis,
            from_end: true,
        }
    }

    pub fn size(&self) -> f32 {
        self.size
    }

    /// Scale the size + bounds by `factor` (for a zoom change), so a sidebar/panel that
    /// was N px at one zoom keeps its proportion at the new zoom instead of staying a
    /// fixed pixel width (which looks tiny once the content scales up).
    pub fn scale(&mut self, factor: f32) {
        if factor > 0.0 && (factor - 1.0).abs() > f32::EPSILON {
            self.size *= factor;
            self.min *= factor;
            self.max *= factor;
        }
    }

    /// Update the clamp bounds (e.g. when the window or zoom changes) and re-clamp
    /// the current size into them. The construction-time bounds are otherwise fixed,
    /// which would cap a zoom-scaled panel at its zoom-1 limit.
    pub fn set_bounds(&mut self, min: f32, max: f32) {
        self.min = min;
        self.max = max.max(min);
        self.size = self.size.clamp(self.min, self.max);
    }

    pub fn cursor(&self) -> CursorIcon {
        match self.axis {
            Axis::Horizontal => CursorIcon::ColResize,
            Axis::Vertical => CursorIcon::RowResize,
        }
    }

    pub fn is_dragging(&self) -> bool {
        self.dragging
    }

    /// Thin hit strip straddling the active edge of `region`: the right edge for
    /// a horizontal (width) splitter (the LEFT edge when `from_end`), the top
    /// edge for a vertical (height) one.
    pub fn handle_rect(&self, region: Rect) -> Rect {
        let half = theme::SIDEBAR_RESIZE_HANDLE() * 0.5;
        match self.axis {
            Axis::Horizontal => Rect {
                x: if self.from_end { region.x - half } else { region.x + region.w - half },
                y: region.y,
                w: theme::SIDEBAR_RESIZE_HANDLE(),
                h: region.h,
            },
            Axis::Vertical => Rect {
                x: region.x,
                y: region.y - half,
                w: region.w,
                h: theme::SIDEBAR_RESIZE_HANDLE(),
            },
        }
    }

    /// Begin a drag if `p` lands on the handle. Returns true if a drag started.
    pub fn press(&mut self, p: (f32, f32), region: Rect) -> bool {
        if self.handle_rect(region).contains(p) {
            self.dragging = true;
            true
        } else {
            false
        }
    }

    /// While dragging, set the size from the cursor along this splitter's axis;
    /// `origin` is the fixed edge the size is measured from (left edge for a
    /// width splitter, bottom edge for a height splitter). Returns true if the
    /// size changed.
    pub fn drag(&mut self, cursor: f32, origin: f32) -> bool {
        if !self.dragging {
            return false;
        }
        let raw = match self.axis {
            Axis::Horizontal if self.from_end => origin - cursor, // measured back from the right edge
            Axis::Horizontal => cursor - origin,
            Axis::Vertical => origin - cursor,
        };
        let new = raw.clamp(self.min, self.max);
        if (new - self.size).abs() > 0.5 {
            self.size = new;
            true
        } else {
            false
        }
    }

    pub fn release(&mut self) {
        self.dragging = false;
    }
}

/// Which axes a `ScrollView` scrolls, plus terminal-style bottom pinning.
#[derive(Clone, Copy)]
pub struct ScrollOpts {
    pub vertical: bool,
    pub horizontal: bool,
    pub stick_to_end: bool,
}

impl ScrollOpts {
    pub fn vertical() -> Self {
        Self { vertical: true, horizontal: false, stick_to_end: false }
    }
    pub fn both() -> Self {
        Self { vertical: true, horizontal: true, stick_to_end: false }
    }
}

/// A reusable scroll container. It owns the scroll offset, optional vertical/
/// horizontal scrollbars (built on `Scrollbar`), clamping, wheel + thumb-drag input,
/// hover state, and a VSCode-style auto-hide overlay fade. Callers report `viewport`
/// + `content` each frame via `set_metrics`, draw their content shifted by `-offset()`,
/// and forward mouse events — no scroll math leaks out, and the thumb reserves no width.
///
/// This is the single place overflow/scrollbar behavior lives, so every region
/// (editor, terminal, lists, README) gets identical, debuggable scrolling.
pub struct ScrollView {
    offset: (f32, f32),   // scroll position, px from the top-left of the content
    content: (f32, f32),  // content extent (set each frame)
    viewport: Rect,       // visible region (set each frame)
    vbar: Option<Scrollbar>,
    hbar: Option<Scrollbar>,
    stick_to_end: bool,
    at_end: bool,         // currently pinned to the bottom (for stick_to_end)
    last_active: Instant, // last scroll/hover, drives the auto-hide fade
    hovered: bool,
}

impl ScrollView {
    pub fn new(opts: ScrollOpts) -> Self {
        Self {
            offset: (0.0, 0.0),
            content: (0.0, 0.0),
            viewport: Rect::default(),
            vbar: opts.vertical.then(|| Scrollbar::new(Axis::Vertical)),
            hbar: opts.horizontal.then(|| Scrollbar::new(Axis::Horizontal)),
            stick_to_end: opts.stick_to_end,
            at_end: opts.stick_to_end, // start pinned to the bottom
            last_active: Instant::now(),
            hovered: false,
        }
    }

    fn max_x(&self) -> f32 {
        (self.content.0 - self.viewport.w).max(0.0)
    }
    fn max_y(&self) -> f32 {
        (self.content.1 - self.viewport.h).max(0.0)
    }

    pub fn offset(&self) -> (f32, f32) {
        self.offset
    }

    /// True when scrolled to the bottom (live view for the terminal).
    pub fn at_end(&self) -> bool {
        self.offset.1 >= self.max_y() - 0.5
    }

    /// Store this frame's geometry and clamp the offset. With `stick_to_end`, the
    /// view stays pinned to the bottom until the user scrolls away from it.
    pub fn set_metrics(&mut self, viewport: Rect, content: (f32, f32)) {
        self.viewport = viewport;
        self.content = content;
        if self.stick_to_end && self.at_end {
            self.offset.1 = self.max_y();
        }
        self.clamp();
    }

    fn clamp(&mut self) {
        self.offset.0 = if self.hbar.is_some() { self.offset.0.clamp(0.0, self.max_x()) } else { 0.0 };
        self.offset.1 = if self.vbar.is_some() { self.offset.1.clamp(0.0, self.max_y()) } else { 0.0 };
        self.at_end = self.offset.1 >= self.max_y() - 0.5;
    }

    /// Wheel: `dy > 0` (wheel up) scrolls toward the top. Returns true if it moved.
    pub fn on_wheel(&mut self, dx: f32, dy: f32) -> bool {
        let before = self.offset;
        if self.vbar.is_some() {
            self.offset.1 = (self.offset.1 - dy).clamp(0.0, self.max_y());
        }
        if self.hbar.is_some() {
            self.offset.0 = (self.offset.0 - dx).clamp(0.0, self.max_x());
        }
        self.at_end = self.offset.1 >= self.max_y() - 0.5;
        if self.offset != before {
            self.last_active = Instant::now();
            true
        } else {
            false
        }
    }

    /// Move the vertical offset so `y` (content px) is the top of the view.
    pub fn scroll_to_y(&mut self, y: f32) {
        self.offset.1 = y.clamp(0.0, self.max_y());
        self.at_end = self.offset.1 >= self.max_y() - 0.5;
        self.last_active = Instant::now();
    }

    /// Pin to the bottom (used by the terminal when the user types).
    pub fn scroll_to_end(&mut self) {
        self.offset.1 = self.max_y();
        self.at_end = true;
        self.last_active = Instant::now();
    }

    fn vtrack(&self) -> Rect {
        Rect {
            x: self.viewport.x + self.viewport.w - theme::SCROLLBAR_WIDTH(),
            y: self.viewport.y,
            w: theme::SCROLLBAR_WIDTH(),
            h: self.viewport.h,
        }
    }

    /// The vertical scrollbar track rect (for overview markers like find matches).
    pub fn vtrack_rect(&self) -> Rect {
        self.vtrack()
    }
    fn htrack(&self) -> Rect {
        Rect {
            x: self.viewport.x,
            y: self.viewport.y + self.viewport.h - theme::SCROLLBAR_WIDTH(),
            w: self.viewport.w - theme::SCROLLBAR_WIDTH(),
            h: theme::SCROLLBAR_WIDTH(),
        }
    }

    /// Press on a scrollbar track: drags the thumb, or jumps to the click point if
    /// the track was clicked outside the thumb. Returns true if it claimed the press.
    pub fn press(&mut self, p: (f32, f32)) -> bool {
        let (vt, ht) = (self.vtrack(), self.htrack());
        let ((cw, ch), (vw, vh), (ox, oy)) =
            (self.content, (self.viewport.w, self.viewport.h), self.offset);
        let mut hit = false;
        if let Some(b) = self.vbar.as_mut() {
            if let Some(s) = b.press_track(p, vt, ch, vh, oy) {
                self.offset.1 = s;
                hit = true;
            }
        }
        if !hit {
            if let Some(b) = self.hbar.as_mut() {
                if let Some(s) = b.press_track(p, ht, cw, vw, ox) {
                    self.offset.0 = s;
                    hit = true;
                }
            }
        }
        if hit {
            self.at_end = self.offset.1 >= self.max_y() - 0.5;
            self.last_active = Instant::now();
        }
        hit
    }

    /// While a thumb is held, map the cursor to a new offset. Returns true if moved.
    pub fn drag(&mut self, p: (f32, f32)) -> bool {
        let (vt, ht) = (self.vtrack(), self.htrack());
        let ((cw, ch), (vw, vh)) = (self.content, (self.viewport.w, self.viewport.h));
        let mut moved = false;
        if let Some(b) = self.vbar.as_ref() {
            if b.is_dragging() {
                if let Some(y) = b.drag(p, vt, ch, vh) {
                    self.offset.1 = y;
                    moved = true;
                }
            }
        }
        if let Some(b) = self.hbar.as_ref() {
            if b.is_dragging() {
                if let Some(x) = b.drag(p, ht, cw, vw) {
                    self.offset.0 = x;
                    moved = true;
                }
            }
        }
        if moved {
            self.at_end = self.offset.1 >= self.max_y() - 0.5;
            self.last_active = Instant::now();
        }
        moved
    }

    pub fn release(&mut self) {
        if let Some(b) = self.vbar.as_mut() {
            b.release();
        }
        if let Some(b) = self.hbar.as_mut() {
            b.release();
        }
    }

    pub fn is_dragging(&self) -> bool {
        self.vbar.as_ref().map_or(false, |b| b.is_dragging())
            || self.hbar.as_ref().map_or(false, |b| b.is_dragging())
    }

    /// Update hover state (pointer inside the viewport); the rising edge wakes the
    /// fade. Returns true if the hover state changed (caller should redraw).
    pub fn hover(&mut self, inside: bool) -> bool {
        let changed = inside != self.hovered;
        if inside && !self.hovered {
            self.last_active = Instant::now();
        }
        self.hovered = inside;
        changed
    }

    /// The cursor to show over a thumb (`Default`, matching the editor), else None.
    pub fn cursor(&self, p: (f32, f32)) -> Option<CursorIcon> {
        if let Some(b) = self.vbar.as_ref() {
            if let Some(th) = b.thumb(self.vtrack(), self.content.1, self.viewport.h, self.offset.1) {
                if th.contains(p) {
                    return Some(CursorIcon::Default);
                }
            }
        }
        if let Some(b) = self.hbar.as_ref() {
            if let Some(th) = b.thumb(self.htrack(), self.content.0, self.viewport.w, self.offset.0) {
                if th.contains(p) {
                    return Some(CursorIcon::Default);
                }
            }
        }
        None
    }

    fn alpha(&self, now: Instant) -> f32 {
        if self.hovered || self.is_dragging() {
            return 1.0;
        }
        let ms = now.saturating_duration_since(self.last_active).as_millis() as f32;
        if ms <= theme::SCROLLBAR_FADE_HOLD_MS {
            1.0
        } else {
            (1.0 - (ms - theme::SCROLLBAR_FADE_HOLD_MS) / theme::SCROLLBAR_FADE_MS).clamp(0.0, 1.0)
        }
    }

    /// Push the thumb quad(s) as an auto-hiding overlay (fades out when idle).
    pub fn draw(&self, now: Instant, fg_quads: &mut Vec<Quad>) {
        let a = self.alpha(now);
        if a <= 0.0 {
            return;
        }
        let hot = self.hovered || self.is_dragging();
        let base = if hot { theme::SCROLLBAR_THUMB_HOVER() } else { theme::SCROLLBAR_THUMB() };
        let color = [base[0], base[1], base[2], base[3] * a];
        if let Some(b) = self.vbar.as_ref() {
            if let Some(th) = b.thumb(self.vtrack(), self.content.1, self.viewport.h, self.offset.1) {
                fg_quads.push(th.quad(color));
            }
        }
        if let Some(b) = self.hbar.as_ref() {
            if let Some(th) = b.thumb(self.htrack(), self.content.0, self.viewport.w, self.offset.0) {
                fg_quads.push(th.quad(color));
            }
        }
    }

    /// While a fade is still in progress (and content overflows), the next frame time
    /// to animate it; None when nothing is changing. Drives an event-loop WaitUntil.
    pub fn next_wake(&self, now: Instant) -> Option<Instant> {
        if self.hovered || self.is_dragging() {
            return None;
        }
        if self.max_x() <= 0.0 && self.max_y() <= 0.0 {
            return None; // nothing scrollable, no thumb to fade
        }
        let total = (theme::SCROLLBAR_FADE_HOLD_MS + theme::SCROLLBAR_FADE_MS) as u64;
        let end = self.last_active + Duration::from_millis(total);
        if now < end {
            Some(now + Duration::from_millis(16))
        } else {
            None
        }
    }
}

/// A self-contained extensions-list row: a clickable entry showing an icon +
/// name + publisher·category + description. It owns its text buffers and derives
/// every sub-rect from its bounds (no magic offsets leak to the caller). There's
/// no inline action button — clicking the row opens the detail page, which is
/// where Install lives (VSCode-style).
pub struct ExtensionRow {
    name: TextLabel,
    meta: TextLabel,
    desc: TextLabel,
    icon_color: [f32; 4],
    icon_uv: Option<[f32; 4]>, // atlas UV rect when a real icon is available
}

impl ExtensionRow {
    /// Row height, scaled by zoom (the three text lines + icon must fit at 200%).
    pub fn height() -> f32 {
        84.0 * theme::ui_zoom()
    }
    fn pad_x() -> f32 {
        12.0 * theme::ui_zoom()
    }
    fn icon() -> f32 {
        42.0 * theme::ui_zoom()
    }
    fn gap() -> f32 {
        12.0 * theme::ui_zoom()
    }
    fn top() -> f32 {
        9.0 * theme::ui_zoom()
    }

    pub fn new(fs: &mut FontSystem, name: &str, meta: &str, desc: &str, icon_uv: Option<[f32; 4]>) -> Self {
        let mut nl = TextLabel::new(fs, theme::SIDEBAR_WIDTH(), theme::UI_LINE_HEIGHT());
        nl.align = VAlign::Center;
        nl.set(fs, name, theme::UI_FAMILY());
        let mut ml = TextLabel::new(fs, theme::SIDEBAR_WIDTH(), theme::UI_LINE_HEIGHT());
        ml.align = VAlign::Center;
        ml.set(fs, meta, theme::UI_FAMILY());
        let mut dl = TextLabel::new(fs, 800.0, theme::UI_LINE_HEIGHT());
        dl.align = VAlign::Center;
        dl.set(fs, desc, theme::UI_FAMILY());
        Self {
            name: nl,
            meta: ml,
            desc: dl,
            icon_color: icon_color(name),
            icon_uv,
        }
    }

    /// The textured-icon instance for this row, if it has a real icon.
    pub fn icon_instance(&self, bounds: Rect) -> Option<IconInstance> {
        let r = Self::icon_rect(bounds);
        self.icon_uv.map(|uv| IconInstance { rect: [r.x, r.y, r.w, r.h], uv })
    }

    fn icon_rect(b: Rect) -> Rect {
        let icon = Self::icon();
        Rect { x: b.x + Self::pad_x(), y: b.y + (b.h - icon) * 0.5, w: icon, h: icon }
    }
    fn text_x(b: Rect) -> f32 {
        b.x + Self::pad_x() + Self::icon() + Self::gap()
    }
    fn line_rect(b: Rect, n: f32) -> Rect {
        let x = Self::text_x(b);
        Rect { x, y: b.y + Self::top() + n * theme::UI_LINE_HEIGHT(), w: b.x + b.w - Self::pad_x() - x, h: theme::UI_LINE_HEIGHT() }
    }

    /// Re-shape the row's label text after a zoom change.
    fn reshape(&mut self, fs: &mut FontSystem) {
        self.name.reshape(fs);
        self.meta.reshape(fs);
        self.desc.reshape(fs);
    }

    /// True if `p` is on this row.
    pub fn hit(&self, bounds: Rect, p: (f32, f32)) -> bool {
        bounds.contains(p)
    }

    /// Background quads (hover highlight + placeholder icon tile) — quad pass.
    pub fn draw_quads(&self, bounds: Rect, hovered: bool, quads: &mut Vec<Quad>) {
        if hovered {
            quads.push(bounds.quad(theme::TREE_HOVER()));
        }
        // Colored placeholder only when there's no real icon (the atlas draws those).
        if self.icon_uv.is_none() {
            quads.push(Self::icon_rect(bounds).quad(self.icon_color));
        }
    }

    /// Text areas (name, meta, description) — glyph pass.
    pub fn draw_text<'a>(&'a self, bounds: Rect, areas: &mut Vec<TextArea<'a>>) {
        self.name.draw_left(Self::line_rect(bounds, 0.0), 0.0, theme::FG_TEXT(), areas);
        self.meta.draw_left(Self::line_rect(bounds, 1.0), 0.0, theme::FG_DIM(), areas);
        self.desc.draw_left(Self::line_rect(bounds, 2.0), 0.0, theme::FG_DIM(), areas);
    }
}

/// A stable, distinct placeholder color for an extension icon (real PNG icons
/// aren't rendered yet). Derived from the name so it stays consistent per row.
pub(crate) fn icon_color(name: &str) -> [f32; 4] {
    const PALETTE: [[f32; 4]; 8] = [
        [0.20, 0.45, 0.78, 1.0],
        [0.55, 0.36, 0.72, 1.0],
        [0.20, 0.62, 0.50, 1.0],
        [0.78, 0.45, 0.22, 1.0],
        [0.70, 0.27, 0.40, 1.0],
        [0.36, 0.58, 0.28, 1.0],
        [0.30, 0.52, 0.66, 1.0],
        [0.62, 0.55, 0.25, 1.0],
    ];
    let h = name.bytes().fold(0u32, |a, b| a.wrapping_mul(31).wrapping_add(b as u32));
    PALETTE[(h as usize) % PALETTE.len()]
}

/// A vertical list of `ExtensionRow`s. Owns the rows, stacks them at a fixed row
/// height (single source of truth for row bounds shared by hit-test and draw),
/// and routes hover/click to the row under the cursor.
/// Row spec: (name, meta, description, icon atlas UV).
pub type ExtSpec = (String, String, String, Option<[f32; 4]>);

pub struct ExtensionList {
    rows: Vec<ExtensionRow>,
}

impl ExtensionList {
    pub fn new() -> Self {
        Self { rows: Vec::new() }
    }

    pub fn len(&self) -> usize {
        self.rows.len()
    }

    /// Rebuild from `(name, meta, desc, icon_uv)` specs (call when the extension
    /// data changes — after a scan, a search, or an install).
    pub fn rebuild(&mut self, fs: &mut FontSystem, specs: &[ExtSpec]) {
        self.rows = specs
            .iter()
            .map(|(n, m, d, uv)| ExtensionRow::new(fs, n, m, d, *uv))
            .collect();
    }

    /// Total stacked height of all rows (for scroll clamping).
    pub fn content_height(&self) -> f32 {
        self.rows.len() as f32 * ExtensionRow::height()
    }

    /// Re-shape every row's text after a zoom change.
    pub fn rezoom(&mut self, fs: &mut FontSystem) {
        for r in &mut self.rows {
            r.reshape(fs);
        }
    }

    fn row_bounds(&self, region: Rect, scroll: f32, i: usize) -> Rect {
        Rect {
            x: region.x,
            y: region.y - scroll + i as f32 * ExtensionRow::height(),
            w: region.w,
            h: ExtensionRow::height(),
        }
    }

    fn visible(b: Rect, region: Rect) -> bool {
        b.y + b.h > region.y && b.y < region.y + region.h
    }

    /// The row index under `p`, or None.
    pub fn hit(&self, region: Rect, scroll: f32, p: (f32, f32)) -> Option<usize> {
        if !region.contains(p) {
            return None;
        }
        (0..self.rows.len()).find(|&i| {
            let b = self.row_bounds(region, scroll, i);
            Self::visible(b, region) && self.rows[i].hit(b, p)
        })
    }

    pub fn draw_quads(&self, region: Rect, scroll: f32, hovered: Option<usize>, quads: &mut Vec<Quad>) {
        for (i, row) in self.rows.iter().enumerate() {
            let b = self.row_bounds(region, scroll, i);
            if Self::visible(b, region) {
                row.draw_quads(b, hovered == Some(i), quads);
            }
        }
    }

    pub fn draw_text<'a>(&'a self, region: Rect, scroll: f32, areas: &mut Vec<TextArea<'a>>) {
        for (i, row) in self.rows.iter().enumerate() {
            let b = self.row_bounds(region, scroll, i);
            if Self::visible(b, region) {
                row.draw_text(b, areas);
            }
        }
    }

    /// Textured-icon instances for the visible rows that have a real icon.
    pub fn icon_instances(&self, region: Rect, scroll: f32, out: &mut Vec<IconInstance>) {
        for (i, row) in self.rows.iter().enumerate() {
            let b = self.row_bounds(region, scroll, i);
            if Self::visible(b, region) {
                if let Some(inst) = row.icon_instance(b) {
                    out.push(inst);
                }
            }
        }
    }
}

/// A floating tooltip card (e.g. the diagnostic hover). Wraps its text to a max
/// width, measures its own content size from the shaped runs, and draws a bordered
/// box + text anchored near a point and clamped to stay on screen. Owns its buffer
/// and reshapes only when the text changes — single source of truth for both sizing
/// and drawing, so callers just hand it an anchor and a screen rect.
/// VS Code-style link blue for the clickable rule id.
const LINK_BLUE: glyphon::Color = glyphon::Color::rgb(0x3b, 0x8e, 0xea);

pub struct HoverCard {
    buffer: Buffer,
    last: String, // change-detection key (message + source/code/href)
    epoch: u64,
    base_max_w: f32, // unzoomed max content width (wrap boundary)
    href: Option<String>,
    link_w: f32, // pixel width of the link (last) line, for hit-testing
}

impl HoverCard {
    pub fn new(fs: &mut FontSystem) -> Self {
        let max_w = 420.0;
        Self {
            buffer: make_ui_buffer(fs, max_w, 400.0),
            last: String::new(),
            epoch: theme::shape_epoch(),
            base_max_w: max_w,
            href: None,
            link_w: 0.0,
        }
    }

    /// Populate from a diagnostic hover: the message, then a `source(rule)` line —
    /// the rule rendered as a blue link when the server provided a docs URL.
    pub fn set(&mut self, fs: &mut FontSystem, hover: &crate::lsp::DiagHover) {
        let key = format!("{}|{:?}|{:?}|{:?}", hover.message, hover.source, hover.code, hover.href);
        if self.last == key && self.epoch == theme::shape_epoch() {
            return;
        }
        self.epoch = theme::shape_epoch();
        self.last = key;
        self.href = hover.href.clone();

        let base = Attrs::new().family(Family::Name(theme::UI_FAMILY()));
        let mut spans: Vec<(String, Attrs)> = vec![(hover.message.clone(), base.color(theme::FG_TEXT()))];
        // `source(rule)` footer line, e.g. "eslint(no-unused-vars)".
        let src = hover.source.clone().unwrap_or_default();
        let code = hover.code.clone().unwrap_or_default();
        let footer = match (src.is_empty(), code.is_empty()) {
            (false, false) => format!("{src}({code})"),
            (false, true) => src,
            (true, false) => code,
            (true, true) => String::new(),
        };
        let has_link = self.href.is_some();
        if !footer.is_empty() {
            spans.push(("\n".to_string(), base.color(theme::FG_DIM())));
            let color = if has_link { LINK_BLUE } else { theme::FG_DIM() };
            spans.push((footer, base.color(color)));
        }

        let z = theme::ui_zoom();
        self.buffer.set_metrics(fs, Metrics::new(theme::UI_FONT_SIZE(), theme::UI_LINE_HEIGHT()));
        self.buffer.set_wrap(fs, Wrap::WordOrGlyph);
        self.buffer.set_size(fs, Some(self.base_max_w * z), Some(4000.0));
        self.buffer.set_rich_text(
            fs,
            spans.iter().map(|(s, a)| (s.as_str(), *a)),
            base,
            Shaping::Advanced,
        );
        self.buffer.shape_until_scroll(fs, false);
        // Width of the last (link) line, for the underline + click hit-test.
        self.link_w = if has_link {
            self.buffer.layout_runs().last().map(|r| r.line_w).unwrap_or(0.0)
        } else {
            0.0
        };
    }

    /// Measured content size (px) from the wrapped runs.
    fn content_size(&self) -> (f32, f32) {
        let mut w = 0.0f32;
        let mut rows = 0;
        for run in self.buffer.layout_runs() {
            w = w.max(run.line_w);
            rows += 1;
        }
        (w, rows.max(1) as f32 * theme::UI_LINE_HEIGHT())
    }

    fn pad() -> f32 {
        theme::zpx(8.0)
    }

    /// The card's rect for a cursor `anchor`, clamped inside `screen`. Prefers to
    /// sit just above the anchor (VS Code style); flips below if there's no room.
    pub fn rect(&self, anchor: (f32, f32), screen: Rect) -> Rect {
        let pad = Self::pad();
        let (cw, ch) = self.content_size();
        let w = cw + pad * 2.0;
        let h = ch + pad * 2.0;
        let gap = theme::zpx(6.0);
        let mut x = anchor.0;
        let mut y = anchor.1 - h - gap;
        if y < screen.y {
            y = anchor.1 + theme::zpx(18.0); // below the line if it won't fit above
        }
        let edge = theme::zpx(4.0);
        if x + w > screen.x + screen.w {
            x = screen.x + screen.w - w - edge;
        }
        x = x.max(screen.x + edge);
        y = y.min(screen.y + screen.h - h - edge).max(screen.y + edge);
        Rect { x, y, w, h }
    }

    /// Screen rect of the clickable link (the last line), if there's a docs URL.
    pub fn link_rect(&self, card: Rect) -> Option<Rect> {
        self.href.as_ref()?;
        let lh = theme::UI_LINE_HEIGHT();
        let pad = Self::pad();
        let y = card.y + card.h - pad - lh;
        Some(Rect { x: card.x + pad, y, w: self.link_w.max(1.0), h: lh })
    }

    /// The docs URL to open when the link is clicked.
    pub fn href(&self) -> Option<&str> {
        self.href.as_deref()
    }

    pub fn draw_quads(&self, r: Rect, quads: &mut Vec<Quad>) {
        quads.push(Rect { x: r.x - 1.0, y: r.y - 1.0, w: r.w + 2.0, h: r.h + 2.0 }.quad(theme::PALETTE_BORDER()));
        quads.push(r.quad(theme::PALETTE_BG()));
        // Underline the link line.
        if let Some(lr) = self.link_rect(r) {
            let uy = lr.y + theme::UI_LINE_HEIGHT() - theme::zpx(2.0);
            quads.push(Rect { x: lr.x, y: uy, w: lr.w, h: theme::zpx(1.0).max(1.0) }.quad([0.23, 0.56, 0.92, 1.0]));
        }
    }

    pub fn draw_text<'a>(&'a self, r: Rect, areas: &mut Vec<TextArea<'a>>) {
        let pad = Self::pad();
        areas.push(TextArea {
            buffer: &self.buffer,
            left: r.x + pad,
            top: r.y + pad,
            scale: 1.0,
            bounds: TextBounds {
                left: r.x as i32,
                top: r.y as i32,
                right: (r.x + r.w) as i32,
                bottom: (r.y + r.h) as i32,
            },
            default_color: theme::FG_TEXT(),
            custom_glyphs: &[],
        });
    }
}

/// Apply a common editing/navigation key to a focused text input. Returns `None`
/// if the key wasn't consumed, or `Some(text_changed)` if it was (so callers can
/// re-filter only when content actually changed). Shared by every input so
/// selection, clipboard, and caret movement behave identically.
pub(crate) fn edit_input(
    input: &mut TextInput,
    fs: &mut FontSystem,
    clip: Option<&mut arboard::Clipboard>,
    event: &winit::event::KeyEvent,
    ctrl: bool,
    shift: bool,
) -> Option<bool> {
    use winit::keyboard::{Key, KeyCode, NamedKey, PhysicalKey};
    if ctrl {
        // Match the physical key (Ctrl can turn the logical key into a control char).
        if let PhysicalKey::Code(code) = event.physical_key {
            match code {
                KeyCode::KeyA => {
                    input.select_all();
                    return Some(false);
                }
                KeyCode::KeyC => {
                    if let Some(cb) = clip {
                        let _ = cb.set_text(input.selected_text().to_string());
                    }
                    return Some(false);
                }
                KeyCode::KeyX => {
                    if input.has_selection() {
                        if let Some(cb) = clip {
                            let _ = cb.set_text(input.selected_text().to_string());
                        }
                        input.backspace(fs);
                        return Some(true);
                    }
                    return Some(false);
                }
                KeyCode::KeyV => {
                    if let Some(cb) = clip {
                        if let Ok(t) = cb.get_text() {
                            let t: String = t.chars().filter(|c| *c != '\n' && *c != '\r').collect();
                            input.insert(fs, &t);
                            return Some(true);
                        }
                    }
                    return Some(false);
                }
                _ => return None,
            }
        }
        return None;
    }
    match event.logical_key.as_ref() {
        Key::Named(NamedKey::ArrowLeft) => {
            input.move_left(shift);
            Some(false)
        }
        Key::Named(NamedKey::ArrowRight) => {
            input.move_right(shift);
            Some(false)
        }
        Key::Named(NamedKey::Home) => {
            input.move_home(shift);
            Some(false)
        }
        Key::Named(NamedKey::End) => {
            input.move_end(shift);
            Some(false)
        }
        Key::Named(NamedKey::Delete) => {
            input.delete_forward(fs);
            Some(true)
        }
        Key::Named(NamedKey::Backspace) => {
            input.backspace(fs);
            Some(true)
        }
        _ => {
            if let Some(t) = event.text.as_ref() {
                let s: &str = t;
                if !s.chars().any(|c| c.is_control()) {
                    input.insert(fs, s);
                    return Some(true);
                }
            }
            None
        }
    }
}
