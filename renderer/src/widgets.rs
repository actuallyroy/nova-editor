// Reusable UI widgets. Each owns its buffer(s) and renders from a `Rect` supplied
// by the layout, so geometry has a single source of truth shared by hit-testing,
// hover backgrounds, and drawing.

use glyphon::{Attrs, Buffer, Family, FontSystem, Metrics, Shaping, TextArea, TextBounds};
use winit::window::CursorIcon;

use crate::icon::IconInstance;
use crate::quad::Quad;
use crate::theme;

pub fn make_ui_buffer(fs: &mut FontSystem, w: f32, h: f32) -> Buffer {
    let mut b = Buffer::new(fs, Metrics::new(theme::UI_FONT_SIZE, theme::UI_LINE_HEIGHT));
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
    size: f32,
    pub cursor: CursorIcon,
    pub align: VAlign,
}

impl IconButton {
    pub fn new(fs: &mut FontSystem, glyph: char, family: &str, size: f32) -> Self {
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
            cursor: CursorIcon::Pointer,
            align: VAlign::Center,
        }
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
    pub align: VAlign,
}

impl TextLabel {
    pub fn new(fs: &mut FontSystem, w: f32, h: f32) -> Self {
        Self {
            buffer: make_ui_buffer(fs, w, h),
            last: String::new(),
            align: VAlign::Center,
        }
    }

    pub fn set(&mut self, fs: &mut FontSystem, text: &str, family: &str) {
        if self.last == text {
            return;
        }
        self.buffer.set_text(
            fs,
            text,
            Attrs::new().family(Family::Name(family)),
            Shaping::Advanced,
        );
        self.buffer.shape_until_scroll(fs, false);
        self.last = text.to_string();
    }

    /// Rich (multi-span, multi-color) variant. `key` is an opaque change-detection
    /// string so we reshape only when the content changes.
    pub fn set_rich(&mut self, fs: &mut FontSystem, key: &str, spans: &[(String, Attrs)], default: Attrs) {
        if self.last == key {
            return;
        }
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
            top: rect.text_top(theme::UI_LINE_HEIGHT, self.align),
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
    cursor: CursorIcon,
    pub align: VAlign,
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
            cursor: CursorIcon::Text,
            align: VAlign::Center,
            caret: 0,
            anchor: 0,
        }
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
        if self.shown == content {
            return;
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

    /// Byte index nearest the local x (relative to the text start).
    fn byte_at_local_x(&self, local_x: f32) -> usize {
        if self.text.is_empty() {
            return 0;
        }
        if let Some(run) = self.buffer.layout_runs().next() {
            for g in run.glyphs.iter() {
                if local_x < g.x + g.w * 0.5 {
                    return g.start;
                }
            }
        }
        self.text.len()
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
        let x = rect.x + pad_x + self.x_for_byte(self.caret);
        let h = theme::UI_LINE_HEIGHT - 6.0;
        let y = rect.text_top(h, self.align);
        Quad::new(x, y, 1.5, h, theme::CURSOR())
    }

    /// Selection highlight rect(s) — one quad for the single line.
    pub fn selection_quads(&self, rect: Rect, pad_x: f32, out: &mut Vec<Quad>) {
        if !self.has_selection() {
            return;
        }
        let (a, b) = self.sel_range();
        let x0 = rect.x + pad_x + self.x_for_byte(a);
        let x1 = rect.x + pad_x + self.x_for_byte(b);
        let h = theme::UI_LINE_HEIGHT - 2.0;
        let y = rect.text_top(h, self.align);
        out.push(Quad::new(x0, y, (x1 - x0).max(1.0), h, theme::SELECTION()));
    }

    pub fn draw<'a>(&'a self, rect: Rect, pad_x: f32, color: glyphon::Color, areas: &mut Vec<TextArea<'a>>) {
        areas.push(TextArea {
            buffer: &self.buffer,
            left: rect.x + pad_x,
            top: rect.text_top(theme::UI_LINE_HEIGHT, self.align),
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
            label: TextInput::new(fs, 700.0, theme::TITLE_BAR_H),
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
        bg_quads.push(rect.quad(fill));
        bg_quads.push(Quad::new(rect.x, rect.y, rect.w, 1.0, theme::SEARCH_BORDER()));
        bg_quads.push(Quad::new(rect.x, rect.y + rect.h - 1.0, rect.w, 1.0, theme::SEARCH_BORDER()));
        bg_quads.push(Quad::new(rect.x, rect.y, 1.0, rect.h, theme::SEARCH_BORDER()));
        bg_quads.push(Quad::new(rect.x + rect.w - 1.0, rect.y, 1.0, rect.h, theme::SEARCH_BORDER()));
    }

    pub fn draw<'a>(&'a self, rect: Rect, areas: &mut Vec<TextArea<'a>>) {
        // Leading search glyph at the left edge.
        let icon_rect = Rect { x: rect.x + 4.0, y: rect.y, w: 22.0, h: rect.h };
        self.icon.draw(icon_rect, theme::TITLE_FG(), areas);
        // Centered label, kept clear of the icon.
        let pad = ((rect.w - self.label.width()) * 0.5).max(28.0);
        self.label.draw(rect, pad, theme::FG_TEXT(), areas);
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
                let mut l = TextLabel::new(fs, 240.0, theme::TITLE_BAR_H);
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

    /// Per-item rects laid out from the left edge of `bar`.
    fn item_rects(&self, bar: Rect) -> Vec<Rect> {
        let pad = 9.0;
        let mut x = bar.x + 6.0;
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
}

impl Gutter {
    pub fn new(fs: &mut FontSystem, w: f32) -> Self {
        Self {
            buffer: make_ui_buffer_mono(fs, w, 4000.0),
            last_lines: usize::MAX,
            last_rows: usize::MAX,
        }
    }

    /// Force a rebuild on next `set_from_buffer` (e.g. after the editor font changed).
    pub fn invalidate(&mut self) {
        self.last_rows = usize::MAX;
    }

    /// Build line numbers aligned to the editor buffer's VISUAL rows: the number
    /// on each logical line's first row, blanks on wrap-continuation rows. This
    /// keeps numbers aligned whether or not word-wrap is on (with wrap off there's
    /// one visual row per logical line, so it's just 1..n).
    pub fn set_from_buffer(&mut self, fs: &mut FontSystem, src: &Buffer) {
        // Cheap change-detection: (logical line count, total visual rows).
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
        if rows == self.last_rows && lines == self.last_lines {
            return;
        }

        // NOTE: line 1's "1" doesn't render on real GPUs (glyphon drops this
        // buffer's first laid-out line) — known minor issue.
        let mut s = String::with_capacity(rows * 6);
        let mut prev = usize::MAX;
        for run in src.layout_runs() {
            if run.line_i != prev {
                s.push_str(&format!("{:>4} \n", run.line_i + 1));
                prev = run.line_i;
            } else {
                s.push('\n'); // wrap-continuation row → blank
            }
        }
        self.buffer.set_metrics(fs, Metrics::new(theme::FONT_SIZE(), theme::LINE_HEIGHT()));
        self.buffer
            .set_size(fs, None, Some(rows as f32 * theme::LINE_HEIGHT() + 200.0));
        self.buffer.set_text(
            fs,
            &s,
            Attrs::new().family(Family::Name(theme::MONO_FAMILY())),
            Shaping::Advanced,
        );
        self.buffer.shape_until_scroll(fs, false);
        self.last_lines = lines;
        self.last_rows = rows;
    }

    pub fn draw<'a>(&'a self, region: Rect, scroll_y: f32, color: glyphon::Color, areas: &mut Vec<TextArea<'a>>) {
        areas.push(TextArea {
            buffer: &self.buffer,
            left: region.x,
            top: region.y + theme::EDITOR_PAD - scroll_y,
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
}

/// Reusable vertical list of fixed-height rows backed by one shared multi-line
/// buffer. Provides the single source of truth for row geometry (`row_rect` /
/// `row_at`) shared by hover/selection backgrounds, hit-testing, and the text
/// draw. Used for the file tree and the command palette list.
pub struct ListView {
    buffer: Buffer,
    last_key: String,
    row_h: f32,
    pad_x: f32,
    cursor: CursorIcon,
}

impl ListView {
    pub fn new(fs: &mut FontSystem, w: f32, h: f32, row_h: f32, pad_x: f32) -> Self {
        Self {
            buffer: make_ui_buffer(fs, w, h),
            last_key: String::new(),
            row_h,
            pad_x,
            cursor: CursorIcon::Pointer,
        }
    }

    pub fn cursor(&self) -> CursorIcon {
        self.cursor
    }

    pub fn set_text(&mut self, fs: &mut FontSystem, key: &str, w: f32, h: f32) {
        if self.last_key == key {
            return;
        }
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
        if self.last_key == key {
            return;
        }
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
        Rect {
            x: region.x,
            y: region.y + i as f32 * self.row_h,
            w: region.w,
            h: self.row_h,
        }
    }

    /// Row index under `p` within `region`, bounded to `count` rows.
    pub fn row_at(&self, region: Rect, p: (f32, f32), count: usize) -> Option<usize> {
        if !region.contains(p) {
            return None;
        }
        let idx = ((p.1 - region.y) / self.row_h) as usize;
        (idx < count).then_some(idx)
    }

    pub fn draw<'a>(&'a self, region: Rect, color: glyphon::Color, areas: &mut Vec<TextArea<'a>>) {
        self.draw_at(region, region.y, color, areas);
    }

    /// Draw the buffer with row 0 placed at `top`, clipped to `clip`. Lets a
    /// caller render a vertical slice (e.g. rows above/below an inline insert).
    pub fn draw_at<'a>(&'a self, clip: Rect, top: f32, color: glyphon::Color, areas: &mut Vec<TextArea<'a>>) {
        areas.push(TextArea {
            buffer: &self.buffer,
            left: clip.x + self.pad_x,
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
    width: f32,
}

impl Menu {
    pub fn new(fs: &mut FontSystem, width: f32) -> Self {
        Self {
            list: ListView::new(fs, width, 400.0, theme::MENU_ITEM_H, 12.0),
            count: 0,
            width,
        }
    }

    pub fn set_items(&mut self, fs: &mut FontSystem, labels: &[&str]) {
        let mut t = String::new();
        for l in labels {
            t.push(' ');
            t.push_str(l);
            t.push('\n');
        }
        self.list.set_text(fs, &t, self.width, 400.0);
        self.count = labels.len();
    }

    /// The popup box rect for `anchor`, clamped within the window `win`.
    pub fn rect(&self, anchor: (f32, f32), win: (f32, f32)) -> Rect {
        let h = self.count as f32 * theme::MENU_ITEM_H + 8.0;
        Rect {
            x: anchor.0.min(win.0 - self.width - 4.0).max(0.0),
            y: anchor.1.min(win.1 - h - 4.0).max(0.0),
            w: self.width,
            h,
        }
    }

    fn inner(&self, menu: Rect) -> Rect {
        Rect { x: menu.x, y: menu.y + 4.0, w: menu.w, h: menu.h - 8.0 }
    }

    pub fn item_at(&self, menu: Rect, p: (f32, f32)) -> Option<usize> {
        self.list.row_at(self.inner(menu), p, self.count)
    }

    pub fn draw_bg(&self, menu: Rect, hovered: Option<usize>, quads: &mut Vec<Quad>) {
        quads.push(
            Rect { x: menu.x - 1.0, y: menu.y - 1.0, w: menu.w + 2.0, h: menu.h + 2.0 }
                .quad(theme::CONTEXT_BORDER()),
        );
        quads.push(menu.quad(theme::CONTEXT_BG()));
        if let Some(i) = hovered {
            quads.push(self.list.row_rect(self.inner(menu), i).quad(theme::CONTEXT_SEL()));
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
            message: TextLabel::new(fs, 600.0, 60.0),
            buttons: Vec::new(),
            check: TextLabel::new(fs, 320.0, 24.0),
            width: 460.0,
        }
    }

    pub fn set(&mut self, fs: &mut FontSystem, message: &str, buttons: &[&str], check: Option<&str>) {
        self.message.set(fs, message, theme::UI_FAMILY());
        if self.buttons.len() != buttons.len() {
            self.buttons = buttons.iter().map(|_| TextLabel::new(fs, 200.0, theme::DIALOG_BTN_H)).collect();
        }
        for (b, l) in self.buttons.iter_mut().zip(buttons) {
            b.set(fs, l, theme::UI_FAMILY());
        }
        if let Some(c) = check {
            self.check.set(fs, c, theme::UI_FAMILY());
        }
    }

    pub fn box_rect(&self, win: (f32, f32), has_check: bool) -> Rect {
        let h = if has_check { 176.0 } else { 148.0 };
        Rect { x: (win.0 - self.width) * 0.5, y: (win.1 - h) * 0.5, w: self.width, h }
    }

    pub fn button_rects(&self, b: Rect) -> Vec<Rect> {
        let bw = 100.0;
        let bh = theme::DIALOG_BTN_H;
        let gap = 10.0;
        let n = self.buttons.len();
        let y = b.y + b.h - bh - 16.0;
        (0..n)
            .map(|i| Rect {
                x: b.x + b.w - 16.0 - (n - i) as f32 * (bw + gap) + gap,
                y,
                w: bw,
                h: bh,
            })
            .collect()
    }

    fn check_box(&self, b: Rect) -> Rect {
        Rect { x: b.x + 18.0, y: b.y + b.h - 30.0 - 17.0, w: 18.0, h: 18.0 }
    }

    pub fn button_at(&self, b: Rect, p: (f32, f32)) -> Option<usize> {
        self.button_rects(b).iter().position(|r| r.contains(p))
    }

    /// True if `p` hit the checkbox or its label.
    pub fn check_hit(&self, b: Rect, p: (f32, f32)) -> bool {
        let cb = self.check_box(b);
        Rect { x: cb.x, y: cb.y - 2.0, w: 220.0, h: cb.h + 4.0 }.contains(p)
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
        quads.push(Rect { x: b.x - 1.0, y: b.y - 1.0, w: b.w + 2.0, h: b.h + 2.0 }.quad(theme::PALETTE_BORDER()));
        quads.push(b.quad(theme::PALETTE_BG()));
        for (i, r) in self.button_rects(b).iter().enumerate() {
            let c = if hovered == Some(i) { theme::DIALOG_BTN_HOVER() } else { theme::DIALOG_BTN() };
            quads.push(r.quad(c));
        }
        if has_check {
            let cb = self.check_box(b);
            quads.push(Rect { x: cb.x - 1.0, y: cb.y - 1.0, w: cb.w + 2.0, h: cb.h + 2.0 }.quad(theme::PALETTE_BORDER()));
            quads.push(cb.quad(if checked { theme::DIALOG_BTN_HOVER() } else { theme::PALETTE_INPUT_BG() }));
        }
    }

    pub fn draw<'a>(&'a self, b: Rect, has_check: bool, areas: &mut Vec<TextArea<'a>>) {
        let msg = Rect { x: b.x + 18.0, y: b.y + 14.0, w: b.w - 36.0, h: theme::UI_LINE_HEIGHT };
        self.message.draw_left(msg, 0.0, theme::FG_TEXT(), areas);
        for (lab, r) in self.buttons.iter().zip(self.button_rects(b)) {
            lab.draw_center(r, theme::FG_TEXT(), areas);
        }
        if has_check {
            let cb = self.check_box(b);
            let lr = Rect { x: cb.x + cb.w + 8.0, y: cb.y - 2.0, w: 220.0, h: theme::UI_LINE_HEIGHT };
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

/// A draggable divider that owns a resizable dimension (the sidebar width) plus
/// its clamp range and drag state. Self-contained: the rest of the app just asks
/// for `size()` and forwards mouse events; the handle geometry, hit-test, and
/// clamping all live here.
pub struct Splitter {
    size: f32,
    min: f32,
    max: f32,
    dragging: bool,
    cursor: CursorIcon,
}

impl Splitter {
    pub fn new(size: f32, min: f32, max: f32) -> Self {
        Self {
            size,
            min,
            max,
            dragging: false,
            cursor: CursorIcon::ColResize,
        }
    }

    pub fn size(&self) -> f32 {
        self.size
    }

    pub fn cursor(&self) -> CursorIcon {
        self.cursor
    }

    pub fn is_dragging(&self) -> bool {
        self.dragging
    }

    /// Thin hit strip straddling the right edge of `region`.
    pub fn handle_rect(&self, region: Rect) -> Rect {
        let half = theme::SIDEBAR_RESIZE_HANDLE * 0.5;
        Rect {
            x: region.x + region.w - half,
            y: region.y,
            w: theme::SIDEBAR_RESIZE_HANDLE,
            h: region.h,
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

    /// While dragging, set the size from the cursor; `origin` is the edge the
    /// size is measured from. Returns true if the size changed.
    pub fn drag(&mut self, cursor: f32, origin: f32) -> bool {
        if !self.dragging {
            return false;
        }
        let new = (cursor - origin).clamp(self.min, self.max);
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
    pub const HEIGHT: f32 = 84.0;
    const PAD_X: f32 = 12.0;
    const ICON: f32 = 42.0;
    const GAP: f32 = 12.0;
    const TOP: f32 = 9.0;

    pub fn new(fs: &mut FontSystem, name: &str, meta: &str, desc: &str, icon_uv: Option<[f32; 4]>) -> Self {
        let mut nl = TextLabel::new(fs, theme::SIDEBAR_WIDTH, theme::UI_LINE_HEIGHT);
        nl.align = VAlign::Center;
        nl.set(fs, name, theme::UI_FAMILY());
        let mut ml = TextLabel::new(fs, theme::SIDEBAR_WIDTH, theme::UI_LINE_HEIGHT);
        ml.align = VAlign::Center;
        ml.set(fs, meta, theme::UI_FAMILY());
        let mut dl = TextLabel::new(fs, 800.0, theme::UI_LINE_HEIGHT);
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
        Rect { x: b.x + Self::PAD_X, y: b.y + (b.h - Self::ICON) * 0.5, w: Self::ICON, h: Self::ICON }
    }
    fn text_x(b: Rect) -> f32 {
        b.x + Self::PAD_X + Self::ICON + Self::GAP
    }
    fn line_rect(b: Rect, n: f32) -> Rect {
        let x = Self::text_x(b);
        Rect { x, y: b.y + Self::TOP + n * theme::UI_LINE_HEIGHT, w: b.x + b.w - Self::PAD_X - x, h: theme::UI_LINE_HEIGHT }
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
        self.rows.len() as f32 * ExtensionRow::HEIGHT
    }

    fn row_bounds(&self, region: Rect, scroll: f32, i: usize) -> Rect {
        Rect {
            x: region.x,
            y: region.y - scroll + i as f32 * ExtensionRow::HEIGHT,
            w: region.w,
            h: ExtensionRow::HEIGHT,
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
