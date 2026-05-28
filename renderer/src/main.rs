// Hide the console window — without this the binary uses the console subsystem
// and Windows spawns a terminal alongside the GUI. We still capture stderr when
// we launch nova via a redirected pipe, so no debug visibility is lost.
#![windows_subsystem = "windows"]

// Nova — Phase 1 vertical slice with VSCode-shaped UI shell.
// Activity bar, sidebar file tree, tab strip, editor (gutter + text),
// status bar, command palette (Ctrl+Shift+P), find bar (Ctrl+F).

mod document;
mod quad;
mod theme;
mod workspace;

use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use arboard::Clipboard;
use glyphon::{
    Attrs, Buffer, Cache, Family, FontSystem, Metrics, Resolution, Shaping, SwashCache, TextArea,
    TextAtlas, TextBounds, TextRenderer, Viewport,
};
use wgpu::{
    Backends, CommandEncoderDescriptor, CompositeAlphaMode, DeviceDescriptor, Instance,
    InstanceDescriptor, LoadOp, MultisampleState, Operations, PresentMode,
    RenderPassColorAttachment, RenderPassDescriptor, RequestAdapterOptions, StoreOp,
    SurfaceConfiguration, TextureFormat, TextureUsages, TextureViewDescriptor,
};
use winit::{
    application::ApplicationHandler,
    dpi::{LogicalSize, PhysicalPosition},
    event::{ElementState, MouseButton, MouseScrollDelta, WindowEvent},
    event_loop::{ActiveEventLoop, ControlFlow, EventLoop},
    keyboard::{Key, ModifiersState, NamedKey},
    window::{CursorIcon, Window, WindowId},
};

use document::Document;
use quad::{Quad, QuadRenderer};
use workspace::Workspace;

// ---------- Layout primitives ----------

#[derive(Clone, Copy, Default)]
struct Rect {
    x: f32,
    y: f32,
    w: f32,
    h: f32,
}

impl Rect {
    fn contains(&self, p: (f32, f32)) -> bool {
        p.0 >= self.x && p.0 < self.x + self.w && p.1 >= self.y && p.1 < self.y + self.h
    }
    fn quad(&self, color: [f32; 4]) -> Quad {
        Quad::new(self.x, self.y, self.w, self.h, color)
    }
}

/// Reusable icon button. A single `rect` (supplied at draw time from the layout)
/// is the single source of truth for the hit region, the hover background, and
/// the centered glyph — so they can never drift apart. Backed by a one-glyph
/// buffer to sidestep glyphon's multi-line layout quirks.
struct IconButton {
    buffer: Buffer,
    size: f32,
    cursor: CursorIcon,
}

impl IconButton {
    fn new(fs: &mut FontSystem, glyph: char, family: &str, size: f32) -> Self {
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
        }
    }

    fn cursor(&self) -> CursorIcon {
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
    fn draw<'a>(&'a self, rect: Rect, color: glyphon::Color, areas: &mut Vec<TextArea<'a>>) {
        let gw = self.glyph_w();
        areas.push(TextArea {
            buffer: &self.buffer,
            left: rect.x + (rect.w - gw) * 0.5,
            top: rect.y + (rect.h - self.size) * 0.5 - 1.0,
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
struct TextLabel {
    buffer: Buffer,
    last: String,
}

impl TextLabel {
    fn new(fs: &mut FontSystem, w: f32, h: f32) -> Self {
        Self {
            buffer: make_ui_buffer(fs, w, h),
            last: String::new(),
        }
    }

    fn set(&mut self, fs: &mut FontSystem, text: &str, family: &str) {
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
    fn set_rich(&mut self, fs: &mut FontSystem, key: &str, spans: &[(String, Attrs)], default: Attrs) {
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

    fn width(&self) -> f32 {
        self.buffer
            .layout_runs()
            .next()
            .map(|r| r.line_w)
            .unwrap_or(0.0)
    }

    fn push<'a>(&'a self, left: f32, rect: Rect, color: glyphon::Color, areas: &mut Vec<TextArea<'a>>) {
        areas.push(TextArea {
            buffer: &self.buffer,
            left,
            top: rect.y + (rect.h - theme::UI_LINE_HEIGHT) * 0.5,
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

    fn draw_left<'a>(&'a self, rect: Rect, pad: f32, color: glyphon::Color, areas: &mut Vec<TextArea<'a>>) {
        self.push(rect.x + pad, rect, color, areas);
    }

    fn draw_center<'a>(&'a self, rect: Rect, color: glyphon::Color, areas: &mut Vec<TextArea<'a>>) {
        self.push(rect.x + (rect.w - self.width()) * 0.5, rect, color, areas);
    }

    fn draw_right<'a>(&'a self, rect: Rect, pad: f32, color: glyphon::Color, areas: &mut Vec<TextArea<'a>>) {
        self.push(rect.x + rect.w - self.width() - pad, rect, color, areas);
    }
}

/// A real editable single-line text input. It OWNS its text and edit operations
/// (`insert` / `backspace` / `clear` / `set_text`), tracks focus, and renders a
/// blinking caret at the end of the text — so callers route keystrokes into it
/// instead of keeping their own parallel String. A leading space pads the text
/// off the left edge; when empty it shows `placeholder`.
struct TextInput {
    buffer: Buffer,
    text: String,
    placeholder: String,
    focused: bool,
    shown: String, // last shaped content (change detection)
    cursor: CursorIcon,
}

impl TextInput {
    fn new(fs: &mut FontSystem, w: f32, h: f32) -> Self {
        Self {
            buffer: make_ui_buffer(fs, w, h),
            text: String::new(),
            placeholder: String::new(),
            focused: false,
            shown: String::new(),
            cursor: CursorIcon::Text,
        }
    }

    fn cursor(&self) -> CursorIcon {
        self.cursor
    }

    fn text(&self) -> &str {
        &self.text
    }

    fn focused(&self) -> bool {
        self.focused
    }

    fn focus(&mut self, on: bool) {
        self.focused = on;
    }

    fn reshape(&mut self, fs: &mut FontSystem) {
        let content = if self.text.is_empty() {
            self.placeholder.clone()
        } else {
            format!(" {}", self.text)
        };
        if self.shown == content {
            return;
        }
        self.buffer.set_text(
            fs,
            &content,
            Attrs::new().family(Family::Name(theme::UI_FAMILY)),
            Shaping::Advanced,
        );
        self.buffer.shape_until_scroll(fs, false);
        self.shown = content;
    }

    fn set_placeholder(&mut self, fs: &mut FontSystem, s: &str) {
        if self.placeholder != s {
            self.placeholder = s.to_string();
            if self.text.is_empty() {
                self.reshape(fs);
            }
        }
    }

    fn set_text(&mut self, fs: &mut FontSystem, s: &str) {
        self.text = s.to_string();
        self.reshape(fs);
    }

    fn clear(&mut self, fs: &mut FontSystem) {
        self.text.clear();
        self.reshape(fs);
    }

    fn insert(&mut self, fs: &mut FontSystem, s: &str) {
        self.text.push_str(s);
        self.reshape(fs);
    }

    fn backspace(&mut self, fs: &mut FontSystem) {
        self.text.pop();
        self.reshape(fs);
    }

    /// Width of the shaped content (used for caret position / label centering).
    fn width(&self) -> f32 {
        self.buffer
            .layout_runs()
            .next()
            .map(|r| r.line_w)
            .unwrap_or(0.0)
    }

    /// A thin caret bar at the end of the text (start when empty).
    fn caret_quad(&self, rect: Rect, pad_x: f32) -> Quad {
        let x = if self.text.is_empty() {
            rect.x + pad_x + 1.0
        } else {
            rect.x + pad_x + self.width()
        };
        let h = theme::UI_LINE_HEIGHT - 6.0;
        let y = rect.y + (rect.h - h) * 0.5;
        Quad::new(x, y, 1.5, h, theme::CURSOR)
    }

    fn draw<'a>(&'a self, rect: Rect, pad_x: f32, color: glyphon::Color, areas: &mut Vec<TextArea<'a>>) {
        areas.push(TextArea {
            buffer: &self.buffer,
            left: rect.x + pad_x,
            top: rect.y + (rect.h - theme::UI_LINE_HEIGHT) * 0.5,
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
struct SearchField {
    icon: IconButton,
    label: TextInput,
    cursor: CursorIcon,
}

impl SearchField {
    fn new(fs: &mut FontSystem) -> Self {
        let mut icon = IconButton::new(fs, theme::ICON_SEARCH, theme::ICON_FAMILY, 12.0);
        icon.cursor = CursorIcon::Pointer;
        Self {
            icon,
            label: TextInput::new(fs, 700.0, theme::TITLE_BAR_H),
            cursor: CursorIcon::Pointer,
        }
    }

    fn cursor(&self) -> CursorIcon {
        self.cursor
    }

    fn set(&mut self, fs: &mut FontSystem, label: &str) {
        self.label.set_placeholder(fs, label);
    }

    /// Box fill + 1px border, drawn from `rect`.
    fn draw_bg(&self, rect: Rect, hovered: bool, bg_quads: &mut Vec<Quad>) {
        let fill = if hovered {
            theme::SEARCH_BG_HOVER
        } else {
            theme::SEARCH_BG
        };
        bg_quads.push(rect.quad(fill));
        bg_quads.push(Quad::new(rect.x, rect.y, rect.w, 1.0, theme::SEARCH_BORDER));
        bg_quads.push(Quad::new(rect.x, rect.y + rect.h - 1.0, rect.w, 1.0, theme::SEARCH_BORDER));
        bg_quads.push(Quad::new(rect.x, rect.y, 1.0, rect.h, theme::SEARCH_BORDER));
        bg_quads.push(Quad::new(rect.x + rect.w - 1.0, rect.y, 1.0, rect.h, theme::SEARCH_BORDER));
    }

    fn draw<'a>(&'a self, rect: Rect, areas: &mut Vec<TextArea<'a>>) {
        // Leading search glyph at the left edge.
        let icon_rect = Rect { x: rect.x + 4.0, y: rect.y, w: 22.0, h: rect.h };
        self.icon.draw(icon_rect, theme::TITLE_FG, areas);
        // Centered label, kept clear of the icon.
        let pad = ((rect.w - self.label.width()) * 0.5).max(28.0);
        self.label.draw(rect, pad, theme::FG_TEXT, areas);
    }
}

/// Top-left menu bar (File, Edit, ...). Each item is a TextLabel laid out
/// left-to-right; the per-item rects are the single source of truth for hover
/// backgrounds and hit-testing.
const MENU_ITEMS: [&str; 8] = [
    "File", "Edit", "Selection", "View", "Go", "Run", "Terminal", "Help",
];

struct MenuBar {
    labels: Vec<TextLabel>,
    cursor: CursorIcon,
}

impl MenuBar {
    fn new(fs: &mut FontSystem) -> Self {
        let labels = MENU_ITEMS
            .iter()
            .map(|t| {
                let mut l = TextLabel::new(fs, 240.0, theme::TITLE_BAR_H);
                l.set(fs, t, theme::UI_FAMILY);
                l
            })
            .collect();
        Self {
            labels,
            cursor: CursorIcon::Pointer,
        }
    }

    fn cursor(&self) -> CursorIcon {
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

    fn item_at(&self, bar: Rect, p: (f32, f32)) -> Option<usize> {
        self.item_rects(bar).iter().position(|r| r.contains(p))
    }

    fn draw_bg(&self, bar: Rect, hovered: Option<usize>, bg_quads: &mut Vec<Quad>) {
        if let Some(i) = hovered {
            if let Some(r) = self.item_rects(bar).get(i) {
                bg_quads.push(r.quad(theme::MENU_HOVER));
            }
        }
    }

    fn draw<'a>(&'a self, bar: Rect, areas: &mut Vec<TextArea<'a>>) {
        let rects = self.item_rects(bar);
        for (l, r) in self.labels.iter().zip(rects) {
            l.draw_center(r, theme::TITLE_FG, areas);
        }
    }
}

/// Reusable line-number gutter. Owns its buffer and rebuilds only when the line
/// count changes. Encapsulates the glyphon "first laid-out line is dropped"
/// quirk in one place (see the known-issue note in `set`).
struct Gutter {
    buffer: Buffer,
    last_count: usize,
}

impl Gutter {
    fn new(fs: &mut FontSystem, w: f32) -> Self {
        Self {
            buffer: make_ui_buffer_mono(fs, w, 4000.0),
            last_count: usize::MAX,
        }
    }

    fn set(&mut self, fs: &mut FontSystem, count: usize) {
        if self.last_count == count {
            return;
        }
        // NOTE: line 1's "1" doesn't render on real GPUs (glyphon drops this
        // buffer's first laid-out line). A spacer workaround caused bleed over the
        // tab strip when scrolled, so it's left as a known minor issue.
        let mut s = String::with_capacity(count * 6);
        for i in 1..=count {
            s.push_str(&format!("{:>4} \n", i));
        }
        self.buffer
            .set_size(fs, None, Some(count as f32 * theme::LINE_HEIGHT + 200.0));
        self.buffer.set_text(
            fs,
            &s,
            Attrs::new().family(Family::Name(theme::MONO_FAMILY)),
            Shaping::Advanced,
        );
        self.buffer.shape_until_scroll(fs, false);
        self.last_count = count;
    }

    fn draw<'a>(&'a self, region: Rect, scroll_y: f32, color: glyphon::Color, areas: &mut Vec<TextArea<'a>>) {
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
struct ListView {
    buffer: Buffer,
    last_key: String,
    row_h: f32,
    pad_x: f32,
    cursor: CursorIcon,
}

impl ListView {
    fn new(fs: &mut FontSystem, w: f32, h: f32, row_h: f32, pad_x: f32) -> Self {
        Self {
            buffer: make_ui_buffer(fs, w, h),
            last_key: String::new(),
            row_h,
            pad_x,
            cursor: CursorIcon::Pointer,
        }
    }

    fn cursor(&self) -> CursorIcon {
        self.cursor
    }

    fn set_text(&mut self, fs: &mut FontSystem, key: &str, w: f32, h: f32) {
        if self.last_key == key {
            return;
        }
        self.buffer.set_size(fs, Some(w), Some(h));
        self.buffer.set_text(
            fs,
            key,
            Attrs::new().family(Family::Name(theme::UI_FAMILY)),
            Shaping::Advanced,
        );
        self.buffer.shape_until_scroll(fs, false);
        self.last_key = key.to_string();
    }

    fn set_rich(&mut self, fs: &mut FontSystem, key: &str, spans: &[(String, Attrs)], w: f32, h: f32) {
        if self.last_key == key {
            return;
        }
        self.buffer.set_size(fs, Some(w), Some(h));
        let default = Attrs::new()
            .family(Family::Name(theme::UI_FAMILY))
            .color(theme::FG_TEXT);
        self.buffer.set_rich_text(
            fs,
            spans.iter().map(|(s, a)| (s.as_str(), *a)),
            default,
            Shaping::Advanced,
        );
        self.buffer.shape_until_scroll(fs, false);
        self.last_key = key.to_string();
    }

    fn row_rect(&self, region: Rect, i: usize) -> Rect {
        Rect {
            x: region.x,
            y: region.y + i as f32 * self.row_h,
            w: region.w,
            h: self.row_h,
        }
    }

    /// Row index under `p` within `region`, bounded to `count` rows.
    fn row_at(&self, region: Rect, p: (f32, f32), count: usize) -> Option<usize> {
        if !region.contains(p) {
            return None;
        }
        let idx = ((p.1 - region.y) / self.row_h) as usize;
        (idx < count).then_some(idx)
    }

    fn draw<'a>(&'a self, region: Rect, color: glyphon::Color, areas: &mut Vec<TextArea<'a>>) {
        self.draw_at(region, region.y, color, areas);
    }

    /// Draw the buffer with row 0 placed at `top`, clipped to `clip`. Lets a
    /// caller render a vertical slice (e.g. rows above/below an inline insert).
    fn draw_at<'a>(&'a self, clip: Rect, top: f32, color: glyphon::Color, areas: &mut Vec<TextArea<'a>>) {
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

/// A draggable divider that owns a resizable dimension (the sidebar width) plus
/// its clamp range and drag state. Self-contained: the rest of the app just asks
/// for `size()` and forwards mouse events; the handle geometry, hit-test, and
/// clamping all live here.
struct Splitter {
    size: f32,
    min: f32,
    max: f32,
    dragging: bool,
    cursor: CursorIcon,
}

impl Splitter {
    fn new(size: f32, min: f32, max: f32) -> Self {
        Self {
            size,
            min,
            max,
            dragging: false,
            cursor: CursorIcon::ColResize,
        }
    }

    fn size(&self) -> f32 {
        self.size
    }

    fn cursor(&self) -> CursorIcon {
        self.cursor
    }

    fn is_dragging(&self) -> bool {
        self.dragging
    }

    /// Thin hit strip straddling the right edge of `region`.
    fn handle_rect(&self, region: Rect) -> Rect {
        let half = theme::SIDEBAR_RESIZE_HANDLE * 0.5;
        Rect {
            x: region.x + region.w - half,
            y: region.y,
            w: theme::SIDEBAR_RESIZE_HANDLE,
            h: region.h,
        }
    }

    /// Begin a drag if `p` lands on the handle. Returns true if a drag started.
    fn press(&mut self, p: (f32, f32), region: Rect) -> bool {
        if self.handle_rect(region).contains(p) {
            self.dragging = true;
            true
        } else {
            false
        }
    }

    /// While dragging, set the size from the cursor; `origin` is the edge the
    /// size is measured from. Returns true if the size changed.
    fn drag(&mut self, cursor: f32, origin: f32) -> bool {
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

    fn release(&mut self) {
        self.dragging = false;
    }
}

struct Layout {
    title_bar: Rect,
    activity_bar: Rect,
    sidebar: Rect,
    tab_strip: Rect,
    gutter: Rect,
    editor_text: Rect,
    status_bar: Rect,
    find_bar: Option<Rect>,
    palette: Option<PaletteLayout>,
}

struct PaletteLayout {
    box_: Rect,
    input: Rect,
    list: Rect,
}

impl Layout {
    fn compute(
        w: f32,
        h: f32,
        sidebar_visible: bool,
        sidebar_width: f32,
        find_active: bool,
        palette_active: bool,
    ) -> Self {
        let tb = theme::TITLE_BAR_H;
        let title_bar = Rect { x: 0.0, y: 0.0, w, h: tb };
        let panel_h = h - theme::STATUS_BAR_HEIGHT - tb;
        let activity_bar = Rect {
            x: 0.0,
            y: tb,
            w: theme::ACTIVITY_BAR_WIDTH,
            h: panel_h,
        };
        let sidebar = Rect {
            x: activity_bar.w,
            y: tb,
            w: if sidebar_visible { sidebar_width } else { 0.0 },
            h: panel_h,
        };
        let editor_left = sidebar.x + sidebar.w;
        let tab_strip = Rect {
            x: editor_left,
            y: tb,
            w: (w - editor_left).max(0.0),
            h: theme::TAB_HEIGHT,
        };
        let find_bar = if find_active {
            Some(Rect {
                x: editor_left,
                y: tb + tab_strip.h,
                w: tab_strip.w,
                h: theme::FIND_BAR_HEIGHT,
            })
        } else {
            None
        };
        let editor_y = tb + tab_strip.h + if find_active { theme::FIND_BAR_HEIGHT } else { 0.0 };
        let editor_h = (h - editor_y - theme::STATUS_BAR_HEIGHT).max(0.0);
        let gutter = Rect {
            x: editor_left,
            y: editor_y,
            w: theme::GUTTER_WIDTH,
            h: editor_h,
        };
        let editor_text = Rect {
            x: gutter.x + gutter.w,
            y: editor_y,
            w: (w - gutter.x - gutter.w).max(0.0),
            h: editor_h,
        };
        let status_bar = Rect {
            x: 0.0,
            y: h - theme::STATUS_BAR_HEIGHT,
            w,
            h: theme::STATUS_BAR_HEIGHT,
        };
        let palette = if palette_active {
            let pw = theme::PALETTE_WIDTH.min(w - 40.0);
            let visible = 8usize;
            let ph = theme::PALETTE_INPUT_HEIGHT
                + theme::PALETTE_ROW_HEIGHT * visible as f32
                + 8.0;
            let bx = (w - pw) * 0.5;
            let by = 80.0;
            let box_ = Rect {
                x: bx,
                y: by,
                w: pw,
                h: ph,
            };
            let input = Rect {
                x: box_.x + 4.0,
                y: box_.y + 4.0,
                w: box_.w - 8.0,
                h: theme::PALETTE_INPUT_HEIGHT,
            };
            let list = Rect {
                x: box_.x + 4.0,
                y: input.y + input.h + 4.0,
                w: box_.w - 8.0,
                h: theme::PALETTE_ROW_HEIGHT * visible as f32,
            };
            Some(PaletteLayout { box_, input, list })
        } else {
            None
        };
        Self {
            title_bar,
            activity_bar,
            sidebar,
            tab_strip,
            gutter,
            editor_text,
            status_bar,
            find_bar,
            palette,
        }
    }

    /// Single source of truth for activity-bar button rects: 5 at the top,
    /// 2 (account, settings) pinned to the bottom. Index matches the icon order.
    fn activity_rects(&self) -> Vec<Rect> {
        let ab = self.activity_bar;
        (0..7)
            .map(|i| {
                let y = if i < 5 {
                    ab.y + i as f32 * theme::ACTIVITY_CELL
                } else {
                    ab.y + ab.h - (7 - i) as f32 * theme::ACTIVITY_CELL
                };
                Rect {
                    x: ab.x,
                    y,
                    w: ab.w,
                    h: theme::ACTIVITY_CELL,
                }
            })
            .collect()
    }

    /// Single source of truth for the window-control button rects (min, max,
    /// close), left-to-right at the right edge of the title bar.
    fn title_btn_rects(&self) -> Vec<Rect> {
        (0..3)
            .map(|b| Rect {
                x: self.title_bar.w - (3 - b) as f32 * theme::TITLE_BTN_W,
                y: self.title_bar.y,
                w: theme::TITLE_BTN_W,
                h: theme::TITLE_BAR_H,
            })
            .collect()
    }

    /// Single source of truth for tab rects: equal-width columns clamped to
    /// [TAB_MIN_WIDTH, TAB_MAX_WIDTH], left-to-right across the tab strip.
    fn tab_rects(&self, n: usize) -> Vec<Rect> {
        if n == 0 {
            return Vec::new();
        }
        let ideal = theme::TAB_MAX_WIDTH.min(self.tab_strip.w / n as f32);
        let tab_w = ideal.max(theme::TAB_MIN_WIDTH).min(theme::TAB_MAX_WIDTH);
        (0..n)
            .map(|i| Rect {
                x: self.tab_strip.x + i as f32 * tab_w,
                y: self.tab_strip.y,
                w: tab_w,
                h: self.tab_strip.h,
            })
            .collect()
    }

    /// The layout-toggle buttons (left of the window controls).
    fn layout_btn_rects(&self) -> Vec<Rect> {
        let cw = 36.0;
        let right = self.title_bar.w - 3.0 * theme::TITLE_BTN_W;
        (0..3)
            .map(|i| Rect {
                x: right - (3 - i) as f32 * cw,
                y: self.title_bar.y,
                w: cw,
                h: theme::TITLE_BAR_H,
            })
            .collect()
    }

    /// The menu-bar region (left portion of the title bar).
    fn menu_bar_rect(&self) -> Rect {
        Rect {
            x: 0.0,
            y: self.title_bar.y,
            w: self.title_bar.w,
            h: theme::TITLE_BAR_H,
        }
    }

    /// The centered command-center search box in the title bar.
    fn header_search_rect(&self) -> Rect {
        let w = (self.title_bar.w * 0.34).clamp(280.0, 560.0);
        let h = 22.0;
        Rect {
            x: (self.title_bar.w - w) * 0.5,
            y: self.title_bar.y + (theme::TITLE_BAR_H - h) * 0.5,
            w,
            h,
        }
    }

    /// The root-folder row (below the EXPLORER header, above the tree).
    fn root_row_rect(&self) -> Rect {
        Rect {
            x: self.sidebar.x,
            y: self.sidebar.y + theme::SIDEBAR_HEADER_H,
            w: self.sidebar.w,
            h: theme::TREE_ROW_HEIGHT,
        }
    }

    /// The file-tree list region: the sidebar below the header + root row.
    fn tree_region(&self) -> Rect {
        let top = self.sidebar.y + theme::SIDEBAR_HEADER_H + theme::TREE_ROW_HEIGHT;
        Rect {
            x: self.sidebar.x,
            y: top,
            w: self.sidebar.w,
            h: (self.sidebar.y + self.sidebar.h - top).max(0.0),
        }
    }

    /// The Explorer header action buttons (New File / New Folder / Refresh /
    /// Collapse All), right-aligned in the sidebar header.
    fn explorer_action_rects(&self) -> Vec<Rect> {
        let cw = 26.0;
        let n = 4;
        let right = self.sidebar.x + self.sidebar.w - 6.0;
        let y = self.sidebar.y + (theme::SIDEBAR_HEADER_H - cw) * 0.5;
        (0..n)
            .map(|i| Rect {
                x: right - (n - i) as f32 * cw,
                y,
                w: cw,
                h: cw,
            })
            .collect()
    }

    /// The sidebar header region ("EXPLORER" + workspace name).
    fn sidebar_header_rect(&self) -> Rect {
        Rect {
            x: self.sidebar.x,
            y: self.sidebar.y,
            w: self.sidebar.w,
            h: theme::SIDEBAR_HEADER_H,
        }
    }

    /// The close-button cell within a tab — a square icon-button rect pinned to
    /// the tab's right edge. Drives both the × glyph and its hit region.
    fn tab_close_rect(tab: Rect) -> Rect {
        let s = 20.0;
        Rect {
            x: tab.x + tab.w - s - 6.0,
            y: tab.y + (tab.h - s) * 0.5,
            w: s,
            h: s,
        }
    }
}

// ---------- Commands & palette ----------

#[derive(Clone, Copy)]
enum Command {
    Save,
    Close,
    Find,
    Undo,
    Redo,
    SelectAll,
    ToggleSidebar,
    NewFile,
}

const COMMANDS: &[(Command, &str, &str)] = &[
    (Command::Save, "File: Save", "Ctrl+S"),
    (Command::NewFile, "File: New Untitled", ""),
    (Command::Close, "File: Close Tab", "Ctrl+W"),
    (Command::Find, "Edit: Find", "Ctrl+F"),
    (Command::Undo, "Edit: Undo", "Ctrl+Z"),
    (Command::Redo, "Edit: Redo", "Ctrl+Y"),
    (Command::SelectAll, "Edit: Select All", "Ctrl+A"),
    (Command::ToggleSidebar, "View: Toggle Sidebar", ""),
];

struct PaletteState {
    active: bool,
    selected: usize,
    filtered: Vec<usize>,
}

impl PaletteState {
    fn new() -> Self {
        let filtered: Vec<usize> = (0..COMMANDS.len()).collect();
        Self {
            active: false,
            selected: 0,
            filtered,
        }
    }
    fn refilter(&mut self, query: &str) {
        let q = query.to_lowercase();
        self.filtered = (0..COMMANDS.len())
            .filter(|&i| q.is_empty() || COMMANDS[i].1.to_lowercase().contains(&q))
            .collect();
        if self.selected >= self.filtered.len() {
            self.selected = self.filtered.len().saturating_sub(1);
        }
    }
    fn open(&mut self) {
        self.active = true;
        self.selected = 0;
        self.refilter("");
    }
    fn close(&mut self) {
        self.active = false;
    }
}

struct FindBarState {
    active: bool,
    last_match: Option<usize>,
}

impl FindBarState {
    fn new() -> Self {
        Self {
            active: false,
            last_match: None,
        }
    }
}

// ---------- GpuState ----------

struct UiBuffers {
    sidebar_header: TextLabel,
    root_label: TextLabel,
    sidebar: ListView,
    tabs: Buffer,
    status: TextLabel,
    status_right: TextLabel,
    line_numbers: Gutter,
    palette_input: TextInput,
    palette_list: ListView,
    find_input: TextInput,
}

struct GpuState {
    window: Arc<Window>,
    surface: wgpu::Surface<'static>,
    device: wgpu::Device,
    queue: wgpu::Queue,
    config: SurfaceConfiguration,
    font_system: FontSystem,
    swash_cache: SwashCache,
    viewport: Viewport,
    atlas: TextAtlas,
    text_renderer: TextRenderer,
    quad_renderer: QuadRenderer,
    ui: UiBuffers,
    activity_btns: Vec<IconButton>,
    titlebar_btns: Vec<IconButton>,
    tab_close_btn: IconButton,
    search: SearchField,
    menubar: MenuBar,
    layout_btns: Vec<IconButton>,
    explorer_btns: Vec<IconButton>,
    create_icons: [IconButton; 2],
    create_input: TextInput,
}

impl GpuState {
    async fn new(window: Arc<Window>) -> Result<Self> {
        let size = window.inner_size();
        let instance = Instance::new(InstanceDescriptor {
            backends: Backends::PRIMARY,
            ..Default::default()
        });
        let surface = instance.create_surface(window.clone())?;
        let adapter = instance
            .request_adapter(&RequestAdapterOptions {
                power_preference: wgpu::PowerPreference::HighPerformance,
                compatible_surface: Some(&surface),
                force_fallback_adapter: false,
            })
            .await
            .context("no compatible GPU adapter")?;
        let (device, queue) = adapter
            .request_device(
                &DeviceDescriptor {
                    label: Some("nova-device"),
                    ..Default::default()
                },
                None,
            )
            .await
            .context("failed to acquire wgpu device")?;

        // Render to a non-sRGB surface so our sRGB-authored palette (and glyphon's
        // sRGB glyph colors) display at their true values. An sRGB surface would
        // re-encode the colors we write, washing every dark gray toward mid-gray.
        let caps = surface.get_capabilities(&adapter);
        let format = caps
            .formats
            .iter()
            .copied()
            .find(|f| *f == TextureFormat::Bgra8Unorm)
            .or_else(|| caps.formats.iter().copied().find(|f| !f.is_srgb()))
            .unwrap_or(caps.formats[0]);
        let config = SurfaceConfiguration {
            usage: TextureUsages::RENDER_ATTACHMENT,
            format,
            width: size.width.max(1),
            height: size.height.max(1),
            present_mode: PresentMode::Fifo,
            alpha_mode: CompositeAlphaMode::Auto,
            view_formats: vec![],
            desired_maximum_frame_latency: 2,
        };
        surface.configure(&device, &config);

        let mut font_system = FontSystem::new();
        // Load VSCode's Codicon icon font so our glyphs match VSCode exactly.
        font_system
            .db_mut()
            .load_font_data(include_bytes!("../assets/codicon.ttf").to_vec());
        let swash_cache = SwashCache::new();
        let cache = Cache::new(&device);
        let viewport = Viewport::new(&device, &cache);
        let mut atlas = TextAtlas::new(&device, &queue, &cache, config.format);
        let text_renderer =
            TextRenderer::new(&mut atlas, &device, MultisampleState::default(), None);
        let quad_renderer = QuadRenderer::new(&device, config.format);

        let ic = theme::ICON_FAMILY;
        let activity_btns = vec![
            IconButton::new(&mut font_system, theme::ICON_FILES, ic, 20.0),
            IconButton::new(&mut font_system, theme::ICON_SEARCH, ic, 20.0),
            IconButton::new(&mut font_system, theme::ICON_SOURCE_CONTROL, ic, 20.0),
            IconButton::new(&mut font_system, theme::ICON_RUN, ic, 20.0),
            IconButton::new(&mut font_system, theme::ICON_EXTENSIONS, ic, 20.0),
            IconButton::new(&mut font_system, theme::ICON_ACCOUNT, ic, 20.0),
            IconButton::new(&mut font_system, theme::ICON_SETTINGS, ic, 20.0),
        ];
        let titlebar_btns = vec![
            IconButton::new(&mut font_system, theme::ICON_MIN, ic, 12.0),
            IconButton::new(&mut font_system, theme::ICON_MAX, ic, 12.0),
            IconButton::new(&mut font_system, theme::ICON_WIN_CLOSE, ic, 12.0),
        ];
        let tab_close_btn = IconButton::new(&mut font_system, theme::ICON_CLOSE, ic, 12.0);
        let search = SearchField::new(&mut font_system);
        let menubar = MenuBar::new(&mut font_system);
        let layout_btns = vec![
            IconButton::new(&mut font_system, theme::ICON_LAYOUT_SIDEBAR_LEFT, ic, 16.0),
            IconButton::new(&mut font_system, theme::ICON_LAYOUT_PANEL, ic, 16.0),
            IconButton::new(&mut font_system, theme::ICON_LAYOUT_SIDEBAR_RIGHT, ic, 16.0),
        ];
        let explorer_btns = vec![
            IconButton::new(&mut font_system, theme::ICON_NEW_FILE, ic, 16.0),
            IconButton::new(&mut font_system, theme::ICON_NEW_FOLDER, ic, 16.0),
            IconButton::new(&mut font_system, theme::ICON_REFRESH, ic, 16.0),
            IconButton::new(&mut font_system, theme::ICON_COLLAPSE_ALL, ic, 16.0),
        ];
        let create_icons = [
            IconButton::new(&mut font_system, theme::ICON_FILE, ic, 16.0),
            IconButton::new(&mut font_system, theme::ICON_FOLDER_CLOSED, ic, 16.0),
        ];
        let create_input = TextInput::new(&mut font_system, theme::SIDEBAR_WIDTH, theme::TREE_ROW_HEIGHT);

        let ui = UiBuffers {
            sidebar_header: TextLabel::new(&mut font_system, theme::SIDEBAR_WIDTH, 60.0),
            root_label: TextLabel::new(&mut font_system, theme::SIDEBAR_WIDTH, theme::TREE_ROW_HEIGHT),
            sidebar: ListView::new(
                &mut font_system,
                theme::SIDEBAR_WIDTH,
                800.0,
                theme::TREE_ROW_HEIGHT,
                12.0,
            ),
            tabs: make_ui_buffer(&mut font_system, 4000.0, theme::TAB_HEIGHT),
            status: TextLabel::new(&mut font_system, 4000.0, theme::STATUS_BAR_HEIGHT),
            status_right: TextLabel::new(&mut font_system, 4000.0, theme::STATUS_BAR_HEIGHT),
            line_numbers: Gutter::new(&mut font_system, theme::GUTTER_WIDTH),
            palette_input: TextInput::new(&mut font_system, 600.0, theme::PALETTE_INPUT_HEIGHT),
            palette_list: ListView::new(
                &mut font_system,
                600.0,
                800.0,
                theme::PALETTE_ROW_HEIGHT,
                6.0,
            ),
            find_input: TextInput::new(&mut font_system, 800.0, theme::FIND_BAR_HEIGHT),
        };

        Ok(Self {
            window,
            surface,
            device,
            queue,
            config,
            font_system,
            swash_cache,
            viewport,
            atlas,
            text_renderer,
            quad_renderer,
            ui,
            activity_btns,
            titlebar_btns,
            tab_close_btn,
            search,
            menubar,
            layout_btns,
            explorer_btns,
            create_icons,
            create_input,
        })
    }

    fn resize(&mut self, w: u32, h: u32) {
        if w == 0 || h == 0 {
            return;
        }
        self.config.width = w;
        self.config.height = h;
        self.surface.configure(&self.device, &self.config);
    }
}

fn make_ui_buffer(fs: &mut FontSystem, w: f32, h: f32) -> Buffer {
    let mut b = Buffer::new(fs, Metrics::new(theme::UI_FONT_SIZE, theme::UI_LINE_HEIGHT));
    b.set_size(fs, Some(w), Some(h));
    b
}

fn make_ui_buffer_mono(fs: &mut FontSystem, w: f32, h: f32) -> Buffer {
    let mut b = Buffer::new(fs, Metrics::new(theme::FONT_SIZE, theme::LINE_HEIGHT));
    b.set_size(fs, Some(w), Some(h));
    b
}

// ---------- App ----------

struct UiCache {
    tabs: String,
}

impl UiCache {
    fn new() -> Self {
        Self {
            tabs: String::new(),
        }
    }
}

/// In-progress New File / New Folder creation (inline name entry in the tree).
struct PendingCreate {
    is_dir: bool,
    parent: PathBuf,
    row: usize,   // insertion index in the visible tree
    depth: usize, // indent level of the inline field
}

struct App {
    cwd: PathBuf,
    initial_file: Option<PathBuf>,
    workspace: Workspace,
    gpu: Option<GpuState>,
    mouse_pos: PhysicalPosition<f64>,
    mouse_pressed: bool,
    dragging_editor: bool,
    mods: ModifiersState,
    clipboard: Option<Clipboard>,
    sidebar_visible: bool,
    sidebar_split: Splitter,
    palette: PaletteState,
    find: FindBarState,
    ui_cache: UiCache,
    hovered_tab: Option<usize>,
    hovered_tab_close: Option<usize>,
    hovered_tree: Option<usize>,
    hovered_activity: Option<usize>,
    hovered_titlebtn: Option<usize>,
    hovered_search: bool,
    hovered_menu: Option<usize>,
    hovered_layout: Option<usize>,
    hovered_explorer: Option<usize>,
    selected_tree: Option<usize>,
    creating: Option<PendingCreate>,
    pending_close: bool,
    cursor_blink_on: bool,
    last_blink: Instant,
    cursor_icon: CursorIcon,
}

impl App {
    fn new(root: PathBuf, initial_file: Option<PathBuf>) -> Self {
        Self {
            cwd: root.clone(),
            initial_file,
            workspace: Workspace::new(root),
            gpu: None,
            mouse_pos: PhysicalPosition::new(0.0, 0.0),
            mouse_pressed: false,
            dragging_editor: false,
            mods: ModifiersState::empty(),
            clipboard: Clipboard::new().ok(),
            sidebar_visible: true,
            sidebar_split: Splitter::new(
                theme::SIDEBAR_WIDTH,
                theme::SIDEBAR_MIN_WIDTH,
                theme::SIDEBAR_MAX_WIDTH,
            ),
            palette: PaletteState::new(),
            find: FindBarState::new(),
            ui_cache: UiCache::new(),
            hovered_tab: None,
            hovered_tab_close: None,
            hovered_tree: None,
            hovered_activity: None,
            hovered_titlebtn: None,
            hovered_search: false,
            hovered_menu: None,
            hovered_layout: None,
            hovered_explorer: None,
            selected_tree: None,
            creating: None,
            pending_close: false,
            cursor_blink_on: true,
            last_blink: Instant::now(),
            cursor_icon: CursorIcon::Default,
        }
    }

    fn reset_blink(&mut self) {
        self.cursor_blink_on = true;
        self.last_blink = Instant::now();
    }

    fn recompute_hover(&mut self) {
        let layout = self.layout();
        let p = (self.mouse_pos.x as f32, self.mouse_pos.y as f32);
        let mut changed = false;

        let new_titlebtn = self.title_btn_at(p.0, p.1, &layout);
        if new_titlebtn != self.hovered_titlebtn {
            self.hovered_titlebtn = new_titlebtn;
            changed = true;
        }

        let new_activity = layout.activity_rects().iter().position(|r| r.contains(p));
        if new_activity != self.hovered_activity {
            self.hovered_activity = new_activity;
            changed = true;
        }

        let new_tree = if self.sidebar_visible {
            self.gpu.as_ref().and_then(|gpu| {
                gpu.ui
                    .sidebar
                    .row_at(layout.tree_region(), p, self.workspace.tree.nodes.len())
            })
        } else {
            None
        };
        if new_tree != self.hovered_tree {
            self.hovered_tree = new_tree;
            changed = true;
        }

        let tab_rects = layout.tab_rects(self.workspace.documents.len());
        let new_tab = tab_rects.iter().position(|r| r.contains(p));
        let new_close =
            new_tab.filter(|&i| Layout::tab_close_rect(tab_rects[i]).contains(p));
        if new_tab != self.hovered_tab {
            self.hovered_tab = new_tab;
            changed = true;
        }
        if new_close != self.hovered_tab_close {
            self.hovered_tab_close = new_close;
            changed = true;
        }

        let new_search = layout.palette.is_none() && layout.header_search_rect().contains(p);
        if new_search != self.hovered_search {
            self.hovered_search = new_search;
            changed = true;
        }

        let new_menu = if layout.palette.is_none() {
            self.gpu
                .as_ref()
                .and_then(|g| g.menubar.item_at(layout.menu_bar_rect(), p))
        } else {
            None
        };
        if new_menu != self.hovered_menu {
            self.hovered_menu = new_menu;
            changed = true;
        }

        let new_layout = if layout.palette.is_none() {
            layout.layout_btn_rects().iter().position(|r| r.contains(p))
        } else {
            None
        };
        if new_layout != self.hovered_layout {
            self.hovered_layout = new_layout;
            changed = true;
        }

        let new_explorer = if self.sidebar_visible && layout.palette.is_none() {
            layout.explorer_action_rects().iter().position(|r| r.contains(p))
        } else {
            None
        };
        if new_explorer != self.hovered_explorer {
            self.hovered_explorer = new_explorer;
            changed = true;
        }

        // Resolve the cursor by asking whichever widget is under the pointer for
        // its own cursor; regions without a widget (editor, empty chrome) fall
        // back to explicit defaults.
        let over_handle = self.sidebar_visible
            && layout.palette.is_none()
            && self.sidebar_split.handle_rect(layout.sidebar).contains(p);
        let new_cursor = if self.sidebar_split.is_dragging() || over_handle {
            self.sidebar_split.cursor()
        } else if new_search {
            self.gpu
                .as_ref()
                .map(|g| g.search.cursor())
                .unwrap_or(CursorIcon::Default)
        } else if new_menu.is_some() {
            self.gpu
                .as_ref()
                .map(|g| g.menubar.cursor())
                .unwrap_or(CursorIcon::Default)
        } else if let Some(i) = new_layout {
            self.gpu
                .as_ref()
                .map(|g| g.layout_btns[i].cursor())
                .unwrap_or(CursorIcon::Default)
        } else if let Some(i) = new_explorer {
            self.gpu
                .as_ref()
                .map(|g| g.explorer_btns[i].cursor())
                .unwrap_or(CursorIcon::Default)
        } else if let Some(pal) = layout.palette.as_ref() {
            // Palette is modal: pointer over a row, arrow elsewhere.
            self.gpu
                .as_ref()
                .and_then(|g| {
                    g.ui
                        .palette_list
                        .row_at(pal.list, p, self.palette.filtered.len())
                        .map(|_| g.ui.palette_list.cursor())
                })
                .unwrap_or(CursorIcon::Default)
        } else if let Some(g) = self.gpu.as_ref() {
            if let Some(i) = new_titlebtn {
                g.titlebar_btns[i].cursor()
            } else if let Some(i) = new_activity {
                g.activity_btns[i].cursor()
            } else if new_close.is_some() {
                g.tab_close_btn.cursor()
            } else if new_tab.is_some() {
                // Tab body is clickable but has no dedicated widget.
                CursorIcon::Pointer
            } else if new_tree.is_some() {
                g.ui.sidebar.cursor()
            } else if layout.editor_text.contains(p) {
                // Editor text area: I-beam (not a component).
                CursorIcon::Text
            } else {
                CursorIcon::Default
            }
        } else {
            CursorIcon::Default
        };
        if new_cursor != self.cursor_icon {
            self.cursor_icon = new_cursor;
            if let Some(g) = self.gpu.as_ref() {
                g.window.set_cursor(new_cursor);
            }
        }

        if changed {
            self.redraw();
        }
    }

    fn open_initial(&mut self) {
        let Some(gpu) = self.gpu.as_mut() else {
            return;
        };
        // Open the file passed on the command line, else PRD.md if present.
        if let Some(f) = self.initial_file.clone() {
            let _ = self.workspace.open_file(&f, &mut gpu.font_system);
        } else {
            let prd = self.cwd.join("PRD.md");
            if prd.exists() {
                let _ = self.workspace.open_file(&prd, &mut gpu.font_system);
            }
        }
        if self.workspace.documents.is_empty() {
            let doc = Document::new(
                None,
                "Welcome to Nova\n\nUse the sidebar to open files.\nCtrl+Shift+P for command palette.\n"
                    .to_string(),
                &mut gpu.font_system,
            );
            self.workspace.documents.push(doc);
            self.workspace.active = Some(0);
        }
    }

    fn layout(&self) -> Layout {
        let (w, h) = match self.gpu.as_ref() {
            Some(g) => (g.config.width as f32, g.config.height as f32),
            None => (1280.0, 800.0),
        };
        Layout::compute(
            w,
            h,
            self.sidebar_visible,
            self.sidebar_split.size(),
            self.find.active,
            self.palette.active,
        )
    }

    fn ensure_cursor_visible(&mut self) {
        let layout = self.layout();
        let editor_inner_h = layout.editor_text.h - theme::EDITOR_PAD * 2.0;
        if editor_inner_h <= 0.0 {
            return;
        }
        let Some(doc) = self.workspace.active_doc_mut() else {
            return;
        };
        let (line, _) = doc.head_line_col();
        let cursor_top = line as f32 * theme::LINE_HEIGHT;
        let cursor_bottom = cursor_top + theme::LINE_HEIGHT;
        if cursor_top < doc.scroll_y {
            doc.scroll_y = cursor_top.max(0.0);
        } else if cursor_bottom > doc.scroll_y + editor_inner_h {
            doc.scroll_y = cursor_bottom - editor_inner_h;
        }
    }

    fn redraw(&self) {
        if let Some(g) = self.gpu.as_ref() {
            g.window.request_redraw();
        }
    }

    /// Begin an inline New File / New Folder, scoped to the selected folder (or
    /// the parent of a selected file, or the workspace root). Expands the target
    /// folder so the field appears at the top of its contents.
    fn begin_create(&mut self, is_dir: bool) {
        let nodes = &self.workspace.tree.nodes;
        let (parent, row, depth) = match self.selected_tree.and_then(|i| nodes.get(i).map(|n| (i, n))) {
            Some((i, n)) if n.is_dir => {
                let path = n.path.clone();
                let depth = n.depth + 1;
                self.workspace.tree.expand(&path);
                (path, i + 1, depth)
            }
            Some((i, n)) => {
                let parent = n
                    .path
                    .parent()
                    .map(|p| p.to_path_buf())
                    .unwrap_or_else(|| self.workspace.tree.root.clone());
                (parent, i, n.depth)
            }
            None => (self.workspace.tree.root.clone(), 0, 0),
        };
        self.creating = Some(PendingCreate {
            is_dir,
            parent,
            row,
            depth,
        });
        if let Some(g) = self.gpu.as_mut() {
            g.create_input.clear(&mut g.font_system);
            g.create_input
                .set_placeholder(&mut g.font_system, if is_dir { " folder name" } else { " file name" });
            g.create_input.focus(true);
        }
        self.redraw();
    }

    /// Finish an inline creation: create the entry if a non-empty name was typed
    /// (opening it when it's a file), otherwise just dismiss the field.
    fn commit_create(&mut self) {
        let Some(pc) = self.creating.take() else {
            return;
        };
        let name = self
            .gpu
            .as_ref()
            .map(|g| g.create_input.text().trim().to_string())
            .unwrap_or_default();
        if let Some(g) = self.gpu.as_mut() {
            g.create_input.focus(false);
        }
        if !name.is_empty() {
            if let Ok(path) = self.workspace.create_entry(&pc.parent, &name, pc.is_dir) {
                if !pc.is_dir {
                    if let Some(g) = self.gpu.as_mut() {
                        let _ = self.workspace.open_file(&path, &mut g.font_system);
                    }
                }
            }
        }
        self.redraw();
    }

    fn cancel_create(&mut self) {
        self.creating = None;
        if let Some(g) = self.gpu.as_mut() {
            g.create_input.focus(false);
        }
        self.redraw();
    }

    /// Open the command palette and focus its input.
    fn open_palette(&mut self) {
        self.palette.open();
        if let Some(g) = self.gpu.as_mut() {
            g.ui.palette_input.clear(&mut g.font_system);
            g.ui.palette_input.focus(true);
        }
        self.redraw();
    }

    /// Re-filter the palette from its input's current text.
    fn refilter_palette(&mut self) {
        let q = self
            .gpu
            .as_ref()
            .map(|g| g.ui.palette_input.text().to_string())
            .unwrap_or_default();
        self.palette.refilter(&q);
    }

    fn exec_command(&mut self, cmd: Command) {
        match cmd {
            Command::Save => {
                if let Some(d) = self.workspace.active_doc_mut() {
                    let _ = d.save();
                }
            }
            Command::Close => {
                self.workspace.close_active();
            }
            Command::Find => {
                self.find.active = true;
                if let Some(g) = self.gpu.as_mut() {
                    g.ui.find_input.clear(&mut g.font_system);
                    g.ui.find_input.set_placeholder(&mut g.font_system, " Find...");
                    g.ui.find_input.focus(true);
                }
            }
            Command::Undo => {
                if let Some(gpu) = self.gpu.as_mut() {
                    if let Some(d) = self.workspace.active_doc_mut() {
                        d.undo(&mut gpu.font_system);
                    }
                }
                self.ensure_cursor_visible();
            }
            Command::Redo => {
                if let Some(gpu) = self.gpu.as_mut() {
                    if let Some(d) = self.workspace.active_doc_mut() {
                        d.redo(&mut gpu.font_system);
                    }
                }
                self.ensure_cursor_visible();
            }
            Command::SelectAll => {
                if let Some(d) = self.workspace.active_doc_mut() {
                    d.select_all();
                }
            }
            Command::ToggleSidebar => {
                self.sidebar_visible = !self.sidebar_visible;
            }
            Command::NewFile => {
                if let Some(gpu) = self.gpu.as_mut() {
                    let d = Document::new(None, String::new(), &mut gpu.font_system);
                    self.workspace.documents.push(d);
                    self.workspace.active = Some(self.workspace.documents.len() - 1);
                }
            }
        }
        self.redraw();
    }

    fn copy(&mut self) {
        let Some(text) = self.workspace.active_doc().and_then(|d| d.selected_text()) else {
            return;
        };
        if let Some(cb) = self.clipboard.as_mut() {
            let _ = cb.set_text(text);
        }
    }

    fn paste(&mut self) {
        let text = match self.clipboard.as_mut().and_then(|cb| cb.get_text().ok()) {
            Some(t) => t,
            None => return,
        };
        if let Some(gpu) = self.gpu.as_mut() {
            if let Some(d) = self.workspace.active_doc_mut() {
                d.insert_str(&text, &mut gpu.font_system);
            }
        }
        self.ensure_cursor_visible();
    }

    fn cut(&mut self) {
        self.copy();
        if let Some(gpu) = self.gpu.as_mut() {
            if let Some(d) = self.workspace.active_doc_mut() {
                d.delete_selection(&mut gpu.font_system);
            }
        }
        self.ensure_cursor_visible();
    }

    fn find_step(&mut self, forward: bool) {
        let needle = self
            .gpu
            .as_ref()
            .map(|g| g.ui.find_input.text().to_string())
            .unwrap_or_default();
        if needle.is_empty() {
            return;
        }
        let Some(gpu) = self.gpu.as_mut() else {
            return;
        };
        let Some(d) = self.workspace.active_doc_mut() else {
            return;
        };
        let from = if forward {
            d.sel.head
        } else {
            let (lo, _) = d.sel.range();
            lo
        };
        let result = if forward {
            d.find_next(&needle, from + if d.sel.is_empty() { 0 } else { needle.len() })
        } else {
            d.find_prev(&needle, from)
        };
        if let Some(pos) = result {
            d.sel.anchor = pos;
            d.sel.head = pos + needle.len();
            d.sel.desired_col = None;
            self.find.last_match = Some(pos);
            let _ = gpu;
        }
        self.ensure_cursor_visible();
    }

    // ---- Input dispatch ----

    fn title_btn_at(&self, x: f32, y: f32, layout: &Layout) -> Option<usize> {
        layout.title_btn_rects().iter().position(|r| r.contains((x, y)))
    }

    fn on_mouse_press(&mut self, x: f32, y: f32) {
        let layout = self.layout();

        // A click anywhere while an inline create field is open commits it
        // (creates if a name was typed, discards if empty), then consumes the click.
        if self.creating.is_some() {
            self.commit_create();
            return;
        }

        // Sidebar resize handle — let the Splitter claim the press.
        if self.sidebar_visible
            && layout.palette.is_none()
            && self.sidebar_split.press((x, y), layout.sidebar)
        {
            return;
        }

        // Title bar: window controls or drag.
        if layout.palette.is_none() && layout.title_bar.contains((x, y)) {
            // Command-center search box opens the palette (VSCode quick open).
            if layout.header_search_rect().contains((x, y)) {
                self.open_palette();
                return;
            }
            // Layout toggles: primary sidebar is wired; panel/secondary are
            // placeholders until those regions exist.
            if let Some(i) = layout.layout_btn_rects().iter().position(|r| r.contains((x, y))) {
                if i == 0 {
                    self.sidebar_visible = !self.sidebar_visible;
                    self.redraw();
                }
                return;
            }
            // Menu items open the command palette for now (dropdown menus TBD).
            let on_menu = self
                .gpu
                .as_ref()
                .map(|g| g.menubar.item_at(layout.menu_bar_rect(), (x, y)).is_some())
                .unwrap_or(false);
            if on_menu {
                self.open_palette();
                return;
            }
            match self.title_btn_at(x, y, &layout) {
                Some(0) => {
                    if let Some(g) = self.gpu.as_ref() {
                        g.window.set_minimized(true);
                    }
                }
                Some(1) => {
                    if let Some(g) = self.gpu.as_ref() {
                        let m = g.window.is_maximized();
                        g.window.set_maximized(!m);
                    }
                }
                Some(2) => {
                    self.pending_close = true;
                }
                _ => {
                    if let Some(g) = self.gpu.as_ref() {
                        let _ = g.window.drag_window();
                    }
                }
            }
            return;
        }

        // Palette
        if let Some(pal) = layout.palette.as_ref() {
            if !pal.box_.contains((x, y)) {
                self.palette.close();
                self.redraw();
                return;
            }
            let row = self
                .gpu
                .as_ref()
                .and_then(|gpu| gpu.ui.palette_list.row_at(pal.list, (x, y), self.palette.filtered.len()));
            if let Some(idx) = row {
                self.palette.selected = idx;
                let cmd = COMMANDS[self.palette.filtered[idx]].0;
                self.palette.close();
                self.exec_command(cmd);
            }
            return;
        }

        if layout.status_bar.contains((x, y)) {
            return;
        }

        if let Some(idx) = layout.activity_rects().iter().position(|r| r.contains((x, y))) {
            if idx == 0 {
                self.sidebar_visible = !self.sidebar_visible;
                self.redraw();
            }
            return;
        }

        // Explorer header action buttons (New File / New Folder / Refresh / Collapse).
        if self.sidebar_visible {
            if let Some(i) = layout
                .explorer_action_rects()
                .iter()
                .position(|r| r.contains((x, y)))
            {
                match i {
                    0 => self.begin_create(false),
                    1 => self.begin_create(true),
                    2 => self.workspace.tree.refresh(),
                    3 => self.workspace.tree.collapse_all(),
                    _ => {}
                }
                self.redraw();
                return;
            }
        }

        if self.sidebar_visible && layout.sidebar.contains((x, y)) {
            let row = self.gpu.as_ref().and_then(|gpu| {
                gpu.ui
                    .sidebar
                    .row_at(layout.tree_region(), (x, y), self.workspace.tree.nodes.len())
            });
            if let Some(idx) = row {
                self.selected_tree = Some(idx);
                let is_dir = self.workspace.tree.nodes[idx].is_dir;
                if is_dir {
                    self.workspace.tree.toggle(idx);
                } else {
                    let path = self.workspace.tree.nodes[idx].path.clone();
                    if let Some(gpu) = self.gpu.as_mut() {
                        let _ = self.workspace.open_file(&path, &mut gpu.font_system);
                    }
                }
                self.redraw();
            }
            return;
        }

        if layout.tab_strip.contains((x, y)) {
            let tab_rects = layout.tab_rects(self.workspace.documents.len());
            if let Some(idx) = tab_rects.iter().position(|r| r.contains((x, y))) {
                if Layout::tab_close_rect(tab_rects[idx]).contains((x, y)) {
                    self.workspace.close_idx(idx);
                } else {
                    self.workspace.switch_to(idx);
                }
                self.redraw();
            }
            return;
        }

        if let Some(fb) = layout.find_bar.as_ref() {
            if fb.contains((x, y)) {
                return;
            }
        }

        if layout.editor_text.contains((x, y)) {
            self.dragging_editor = true;
            self.editor_click(x, y, self.mods.shift_key(), layout);
            return;
        }
    }

    fn on_mouse_move(&mut self, x: f32, y: f32) {
        if self.sidebar_split.is_dragging() && self.mouse_pressed {
            if self.sidebar_split.drag(x, theme::ACTIVITY_BAR_WIDTH) {
                self.redraw();
            }
            return;
        }
        if self.dragging_editor && self.mouse_pressed {
            let layout = self.layout();
            self.editor_click(x, y, true, layout);
        }
    }

    fn on_mouse_release(&mut self) {
        self.dragging_editor = false;
        self.sidebar_split.release();
    }

    fn editor_click(&mut self, x: f32, y: f32, extend: bool, layout: Layout) {
        let Some(gpu) = self.gpu.as_mut() else {
            return;
        };
        let Some(d) = self.workspace.active_doc_mut() else {
            return;
        };
        let buf_x = x - (layout.editor_text.x + theme::EDITOR_PAD);
        let buf_y = y - (layout.editor_text.y + theme::EDITOR_PAD) + d.scroll_y;
        if let Some(hit) = d.buffer.hit(buf_x, buf_y) {
            let line = hit.line;
            if line < d.rope.len_lines() {
                let line_start = d.rope.line_to_byte(line);
                let line_len = d.rope.line(line).len_bytes();
                let col = hit.index.min(line_len);
                d.place(line_start + col, extend);
            }
        }
        let _ = gpu;
        self.redraw();
    }

    fn on_scroll(&mut self, dy: f32) {
        let layout = self.layout();
        if !layout.editor_text.contains((self.mouse_pos.x as f32, self.mouse_pos.y as f32)) {
            // Could route to sidebar tree, but flat list fits fine for now.
            return;
        }
        let Some(d) = self.workspace.active_doc_mut() else {
            return;
        };
        let total_lines = d.rope.len_lines() as f32;
        let max = (total_lines * theme::LINE_HEIGHT - (layout.editor_text.h - theme::EDITOR_PAD * 2.0)).max(0.0);
        d.scroll_y = (d.scroll_y - dy).clamp(0.0, max);
        self.redraw();
    }

    fn on_key(&mut self, event: winit::event::KeyEvent) {
        if event.state != ElementState::Pressed {
            return;
        }
        let extend = self.mods.shift_key();
        let ctrl = self.mods.control_key();

        // Inline New File / New Folder name entry captures everything; the text
        // lives in the create_input TextInput component.
        if self.creating.is_some() {
            match event.logical_key.as_ref() {
                Key::Named(NamedKey::Escape) => {
                    self.cancel_create();
                    return;
                }
                Key::Named(NamedKey::Backspace) => {
                    if let Some(g) = self.gpu.as_mut() {
                        g.create_input.backspace(&mut g.font_system);
                    }
                    self.redraw();
                    return;
                }
                Key::Named(NamedKey::Enter) => {
                    self.commit_create();
                    return;
                }
                _ => {}
            }
            if let Some(t) = event.text.as_ref() {
                let s: &str = t;
                if !s.chars().any(|c| c.is_control()) && !s.contains('/') && !s.contains('\\') {
                    if let Some(g) = self.gpu.as_mut() {
                        g.create_input.insert(&mut g.font_system, s);
                    }
                    self.redraw();
                }
            }
            return;
        }

        // Palette captures everything when active.
        if self.palette.active {
            match event.logical_key.as_ref() {
                Key::Named(NamedKey::Escape) => {
                    self.palette.close();
                    self.redraw();
                    return;
                }
                Key::Named(NamedKey::ArrowDown) => {
                    if !self.palette.filtered.is_empty() {
                        self.palette.selected =
                            (self.palette.selected + 1) % self.palette.filtered.len();
                    }
                    self.redraw();
                    return;
                }
                Key::Named(NamedKey::ArrowUp) => {
                    if !self.palette.filtered.is_empty() {
                        if self.palette.selected == 0 {
                            self.palette.selected = self.palette.filtered.len() - 1;
                        } else {
                            self.palette.selected -= 1;
                        }
                    }
                    self.redraw();
                    return;
                }
                Key::Named(NamedKey::Enter) => {
                    if let Some(&i) = self.palette.filtered.get(self.palette.selected) {
                        let cmd = COMMANDS[i].0;
                        self.palette.close();
                        self.exec_command(cmd);
                    }
                    return;
                }
                Key::Named(NamedKey::Backspace) => {
                    if let Some(g) = self.gpu.as_mut() {
                        g.ui.palette_input.backspace(&mut g.font_system);
                    }
                    self.refilter_palette();
                    self.redraw();
                    return;
                }
                _ => {}
            }
            if let Some(t) = event.text.as_ref() {
                let s: &str = t;
                if !s.chars().any(|c| c.is_control()) {
                    if let Some(g) = self.gpu.as_mut() {
                        g.ui.palette_input.insert(&mut g.font_system, s);
                    }
                    self.refilter_palette();
                    self.redraw();
                }
            }
            return;
        }

        // Find bar captures when active.
        if self.find.active {
            match event.logical_key.as_ref() {
                Key::Named(NamedKey::Escape) => {
                    self.find.active = false;
                    if let Some(g) = self.gpu.as_mut() {
                        g.ui.find_input.focus(false);
                    }
                    self.redraw();
                    return;
                }
                Key::Named(NamedKey::Enter) => {
                    self.find_step(!extend);
                    self.redraw();
                    return;
                }
                Key::Named(NamedKey::Backspace) => {
                    if let Some(g) = self.gpu.as_mut() {
                        g.ui.find_input.backspace(&mut g.font_system);
                    }
                    self.redraw();
                    return;
                }
                _ => {}
            }
            if let Some(t) = event.text.as_ref() {
                let s: &str = t;
                if !s.chars().any(|c| c.is_control()) {
                    if let Some(g) = self.gpu.as_mut() {
                        g.ui.find_input.insert(&mut g.font_system, s);
                    }
                    self.redraw();
                }
            }
            return;
        }

        // Ctrl+Shift+P opens palette.
        if ctrl && self.mods.shift_key() {
            if let Key::Character(c) = event.logical_key.as_ref() {
                if c == "p" || c == "P" {
                    self.open_palette();
                    return;
                }
            }
        }

        if ctrl {
            if let Key::Character(c) = event.logical_key.as_ref() {
                match c {
                    "a" | "A" => {
                        self.exec_command(Command::SelectAll);
                        return;
                    }
                    "c" | "C" => {
                        self.copy();
                        return;
                    }
                    "x" | "X" => {
                        self.cut();
                        return;
                    }
                    "v" | "V" => {
                        self.paste();
                        return;
                    }
                    "s" | "S" => {
                        self.exec_command(Command::Save);
                        return;
                    }
                    "w" | "W" => {
                        self.exec_command(Command::Close);
                        return;
                    }
                    "z" | "Z" => {
                        self.exec_command(Command::Undo);
                        return;
                    }
                    "y" | "Y" => {
                        self.exec_command(Command::Redo);
                        return;
                    }
                    "f" | "F" => {
                        self.exec_command(Command::Find);
                        return;
                    }
                    "b" | "B" => {
                        self.exec_command(Command::ToggleSidebar);
                        return;
                    }
                    "n" | "N" => {
                        self.exec_command(Command::NewFile);
                        return;
                    }
                    _ => {}
                }
            }
        }

        // Editor-targeted keys.
        let Some(gpu) = self.gpu.as_mut() else {
            return;
        };
        let Some(d) = self.workspace.active_doc_mut() else {
            return;
        };

        match event.logical_key.as_ref() {
            Key::Named(NamedKey::ArrowLeft) => {
                d.move_left(extend);
            }
            Key::Named(NamedKey::ArrowRight) => {
                d.move_right(extend);
            }
            Key::Named(NamedKey::ArrowUp) => {
                d.move_up(extend);
            }
            Key::Named(NamedKey::ArrowDown) => {
                d.move_down(extend);
            }
            Key::Named(NamedKey::Home) => {
                d.move_home(extend);
            }
            Key::Named(NamedKey::End) => {
                d.move_end(extend);
            }
            Key::Named(NamedKey::Backspace) => {
                d.backspace(&mut gpu.font_system);
            }
            Key::Named(NamedKey::Delete) => {
                d.delete_forward(&mut gpu.font_system);
            }
            Key::Named(NamedKey::Enter) => {
                d.insert_str("\n", &mut gpu.font_system);
            }
            Key::Named(NamedKey::Tab) => {
                d.insert_str("    ", &mut gpu.font_system);
            }
            Key::Named(NamedKey::PageUp) => {
                let (line, _) = d.head_line_col();
                let lines_per_page = 20;
                d.move_to_line(line.saturating_sub(lines_per_page), extend);
            }
            Key::Named(NamedKey::PageDown) => {
                let (line, _) = d.head_line_col();
                d.move_to_line(line + 20, extend);
            }
            _ => {
                if ctrl {
                    return;
                }
                if let Some(t) = event.text.as_ref() {
                    let s: &str = t;
                    if !s.is_empty() && !s.chars().any(|c| c.is_control()) {
                        d.insert_str(s, &mut gpu.font_system);
                    }
                }
            }
        }
        let _ = (d, gpu);
        self.ensure_cursor_visible();
        self.redraw();
    }
}

// ---------- Rendering ----------

/// Geometry of the inline New File/Folder row within tree region `tr`:
/// returns (row rect, icon rect, text-field rect) for the given insert row/depth.
fn create_row_geometry(tr: Rect, row: usize, depth: usize) -> (Rect, Rect, Rect) {
    let row_y = tr.y + row as f32 * theme::TREE_ROW_HEIGHT;
    // Match the file tree: 12px left pad + ~8px per depth, left-aligned icon.
    let indent = 12.0 + depth as f32 * 8.0;
    let icon_w = 16.0;
    let row_rect = Rect { x: tr.x, y: row_y, w: tr.w, h: theme::TREE_ROW_HEIGHT };
    let icon_rect = Rect { x: tr.x + indent, y: row_y, w: icon_w, h: theme::TREE_ROW_HEIGHT };
    let field = Rect {
        x: tr.x + indent + icon_w + 4.0,
        y: row_y,
        w: (tr.w - indent - icon_w - 4.0).max(0.0),
        h: theme::TREE_ROW_HEIGHT,
    };
    (row_rect, icon_rect, field)
}

fn cursor_pos_in_buffer(buffer: &Buffer, line: usize, col_byte: usize) -> (f32, f32, f32) {
    let mut x = 0.0f32;
    let mut y = line as f32 * theme::LINE_HEIGHT;
    let mut h = theme::LINE_HEIGHT;
    for run in buffer.layout_runs() {
        if run.line_i != line {
            continue;
        }
        y = run.line_top;
        h = run.line_height;
        let mut last_end = 0.0f32;
        let mut placed = false;
        for glyph in run.glyphs.iter() {
            if (glyph.start as usize) >= col_byte {
                x = glyph.x;
                placed = true;
                break;
            }
            last_end = glyph.x + glyph.w;
        }
        if !placed {
            x = last_end;
        }
        break;
    }
    (x, y, h)
}

fn x_range_in_run(
    run: &glyphon::cosmic_text::LayoutRun,
    col_start: usize,
    col_end: usize,
) -> (f32, f32) {
    let mut x_start: Option<f32> = if col_start == 0 { Some(0.0) } else { None };
    let mut x_end: Option<f32> = None;
    let mut last_end = 0.0f32;
    for glyph in run.glyphs.iter() {
        let g_start = glyph.start as usize;
        if x_start.is_none() && g_start >= col_start {
            x_start = Some(glyph.x);
        }
        if x_end.is_none() && g_start >= col_end {
            x_end = Some(glyph.x);
        }
        last_end = glyph.x + glyph.w;
    }
    (x_start.unwrap_or(last_end), x_end.unwrap_or(last_end))
}

fn render(app: &mut App) -> Result<()> {
    let Some(gpu) = app.gpu.as_mut() else {
        return Ok(());
    };
    let layout = Layout::compute(
        gpu.config.width as f32,
        gpu.config.height as f32,
        app.sidebar_visible,
        app.sidebar_split.size(),
        app.find.active,
        app.palette.active,
    );

    // ---- Update UI buffer texts (only on cache miss) ----
    {
        let fs = &mut gpu.font_system;
        let cache = &mut app.ui_cache;

        // Header command-center label — active file, or the project name.
        let header_label = match app.workspace.active_doc() {
            Some(d) => d.name.clone(),
            None => app
                .cwd
                .file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_else(|| "Search".into()),
        };
        gpu.search.set(fs, &header_label);
        // Activity-bar icons and window controls are IconButton widgets now
        // (rendered below from layout rects) — no per-glyph buffer juggling here.

        // Sidebar header ("EXPLORER" title) + root folder row (chevron + name).
        gpu.ui.sidebar_header.set(fs, "EXPLORER", theme::UI_FAMILY);
        let ws_name = app
            .cwd
            .file_name()
            .map(|n| n.to_string_lossy().to_uppercase())
            .unwrap_or_else(|| "WORKSPACE".into());
        let root_spans = [
            (
                format!("{}  ", theme::ICON_CHEVRON_DOWN),
                Attrs::new().family(Family::Name(theme::ICON_FAMILY)).color(theme::FG_TEXT),
            ),
            (
                ws_name.clone(),
                Attrs::new().family(Family::Name(theme::UI_FAMILY)).color(theme::FG_TEXT),
            ),
        ];
        gpu.ui
            .root_label
            .set_rich(fs, &ws_name, &root_spans, Attrs::new().family(Family::Name(theme::UI_FAMILY)));

        // Sidebar — file tree with monochrome MDL2 folder/file icons (rich text:
        // icon glyphs in the icon font, names in the UI font).
        let mut sidebar_key = String::new();
        for node in app.workspace.tree.nodes.iter() {
            sidebar_key.push_str(&node.depth.to_string());
            sidebar_key.push(if node.is_dir {
                if node.expanded {
                    'v'
                } else {
                    '>'
                }
            } else {
                '.'
            });
            sidebar_key.push_str(&node.name);
            sidebar_key.push('\n');
        }
        {
            let ui_attrs = Attrs::new()
                .family(Family::Name(theme::UI_FAMILY))
                .color(theme::FG_TEXT);
            let folder_attrs = Attrs::new()
                .family(Family::Name(theme::ICON_FAMILY))
                .color(theme::ICON_FOLDER_COLOR);
            let mut spans: Vec<(String, Attrs)> = Vec::new();
            for node in app.workspace.tree.nodes.iter() {
                spans.push(("  ".repeat(node.depth), ui_attrs));
                if node.is_dir {
                    let g = if node.expanded {
                        theme::ICON_FOLDER_OPEN
                    } else {
                        theme::ICON_FOLDER_CLOSED
                    };
                    spans.push((format!("{}  ", g), folder_attrs));
                } else {
                    let fc = Attrs::new()
                        .family(Family::Name(theme::ICON_FAMILY))
                        .color(theme::file_icon_color(&node.name));
                    spans.push((format!("{}  ", theme::ICON_FILE), fc));
                }
                spans.push((format!("{}\n", node.name), ui_attrs));
            }
            gpu.ui.sidebar.set_rich(
                fs,
                &sidebar_key,
                &spans,
                layout.sidebar.w,
                layout.sidebar.h.max(800.0),
            );
        }

        // Tab strip.
        let mut tab_text = String::new();
        for (i, d) in app.workspace.documents.iter().enumerate() {
            if i > 0 {
                tab_text.push('\n');
            }
            tab_text.push_str(&d.name);
            if d.dirty {
                tab_text.push_str(" •");
            }
        }
        if cache.tabs != tab_text {
            // Wide (no wrap) + tall so every tab's label line is shaped on its own
            // line; per-tab bounds clip horizontally & vertically.
            gpu.ui.tabs.set_size(fs, Some(4000.0), Some(4000.0));
            gpu.ui.tabs.set_text(
                fs,
                &tab_text,
                Attrs::new().family(Family::Name(theme::UI_FAMILY)),
                Shaping::Advanced,
            );
            gpu.ui.tabs.shape_until_scroll(fs, false);
            cache.tabs = tab_text;
        }


        // Status bar — left: path; right: position/indent/encoding/EOL/language.
        let (status_text, status_right_text) = if let Some(d) = app.workspace.active_doc() {
            let (line, col) = d.head_line_col();
            let path = d
                .path
                .as_ref()
                .map(|p| p.display().to_string())
                .unwrap_or_else(|| "Untitled".into());
            let dirty = if d.dirty { " ●" } else { "" };
            let lang: String = d
                .path
                .as_ref()
                .and_then(|p| p.extension())
                .map(|e| match e.to_string_lossy().as_ref() {
                    "rs" => "Rust".to_string(),
                    "md" => "Markdown".to_string(),
                    "toml" | "lock" => "TOML".to_string(),
                    "json" => "JSON".to_string(),
                    "wgsl" => "WGSL".to_string(),
                    other => other.to_uppercase(),
                })
                .unwrap_or_else(|| "Plain Text".to_string());
            (
                format!(" {}{}", path, dirty),
                format!("Ln {}, Col {}    Spaces: 4    UTF-8    LF    {}    ", line + 1, col + 1, lang),
            )
        } else {
            ("Nova".to_string(), String::new())
        };
        gpu.ui.status.set(fs, &status_text, theme::UI_FAMILY);
        gpu.ui.status_right.set(fs, &status_right_text, theme::UI_FAMILY);

        // Line numbers.
        let line_count = app
            .workspace
            .active
            .and_then(|i| app.workspace.documents.get(i))
            .map(|d| d.rope.len_lines().max(1))
            .unwrap_or(0);
        gpu.ui.line_numbers.set(fs, line_count);

        // Palette list (the input owns its own text now).
        if let Some(pal) = layout.palette.as_ref() {
            let mut list_text = String::new();
            for &i in app.palette.filtered.iter() {
                let (_, label, shortcut) = COMMANDS[i];
                if shortcut.is_empty() {
                    list_text.push_str(&format!(" {}\n", label));
                } else {
                    list_text.push_str(&format!(" {}   [{}]\n", label, shortcut));
                }
            }
            gpu.ui
                .palette_list
                .set_text(fs, &list_text, pal.list.w, pal.list.h);
        }
    }

    // ---- Build quad lists ----
    let mut bg_quads: Vec<Quad> = Vec::new();
    let mut fg_quads: Vec<Quad> = Vec::new();

    // Title bar bg + window-control hover (hover rect == the button rect).
    bg_quads.push(layout.title_bar.quad(theme::TITLE_BAR_BG));
    // Header command-center search box.
    gpu.search
        .draw_bg(layout.header_search_rect(), app.hovered_search, &mut bg_quads);
    // Menu-bar hover + layout-toggle hover.
    gpu.menubar
        .draw_bg(layout.menu_bar_rect(), app.hovered_menu, &mut bg_quads);
    if let Some(i) = app.hovered_layout {
        bg_quads.push(layout.layout_btn_rects()[i].quad(theme::TITLE_BTN_HOVER));
    }
    if let Some(b) = app.hovered_titlebtn {
        let color = if b == 2 {
            theme::TITLE_CLOSE_HOVER
        } else {
            theme::TITLE_BTN_HOVER
        };
        bg_quads.push(layout.title_btn_rects()[b].quad(color));
    }

    // Activity bar bg + hover (hover rect == the button rect).
    bg_quads.push(layout.activity_bar.quad(theme::ACTIVITY_BAR_BG));
    let act_rects = layout.activity_rects();
    if let Some(idx) = app.hovered_activity {
        bg_quads.push(act_rects[idx].quad(theme::ACTIVITY_BAR_ACTIVE));
    }
    // Active-section accent stripe (Files = idx 0 when sidebar visible).
    if app.sidebar_visible {
        let r = act_rects[0];
        bg_quads.push(Quad::new(r.x, r.y, 2.0, r.h, [1.0, 1.0, 1.0, 0.85]));
    }
    // Sidebar bg
    if app.sidebar_visible {
        bg_quads.push(layout.sidebar.quad(theme::SIDEBAR_BG));
        // Explorer header action hover.
        if let Some(i) = app.hovered_explorer {
            bg_quads.push(layout.explorer_action_rects()[i].quad(theme::MENU_HOVER));
        }
        // Inline-create row highlight (at the insert position).
        if let Some(pc) = app.creating.as_ref() {
            let (row_rect, _, _) = create_row_geometry(layout.tree_region(), pc.row, pc.depth);
            bg_quads.push(row_rect.quad(theme::TREE_SELECTED));
        }
        // Tree row hover (below the header) — row rect from the ListView.
        if let Some(idx) = app.hovered_tree {
            bg_quads.push(
                gpu.ui
                    .sidebar
                    .row_rect(layout.tree_region(), idx)
                    .quad(theme::TREE_HOVER),
            );
        }
        // Subtle right border.
        bg_quads.push(Quad::new(
            layout.sidebar.x + layout.sidebar.w - 1.0,
            layout.sidebar.y,
            1.0,
            layout.sidebar.h,
            [0.10, 0.10, 0.10, 1.0],
        ));
    }
    // Tab strip bg
    bg_quads.push(layout.tab_strip.quad(theme::TAB_BAR_BG));
    // Per-tab styling — geometry from the single-source tab rects.
    let n_tabs = app.workspace.documents.len();
    let tab_rects = layout.tab_rects(n_tabs);
    for (i, tab) in tab_rects.iter().enumerate() {
        let active = app.workspace.active == Some(i);
        let hover = app.hovered_tab == Some(i);
        let fill = if active {
            theme::TAB_ACTIVE
        } else if hover {
            theme::TAB_HOVER
        } else {
            theme::TAB_INACTIVE
        };
        bg_quads.push(tab.quad(fill));
        // Top accent stripe for active tab.
        if active {
            bg_quads.push(Quad::new(tab.x, tab.y, tab.w, 2.0, [0.0, 0.475, 0.78, 1.0]));
        }
        // Subtle vertical divider between tabs.
        if i + 1 < n_tabs {
            bg_quads.push(Quad::new(
                tab.x + tab.w - 1.0,
                tab.y + 4.0,
                1.0,
                tab.h - 8.0,
                [0.30, 0.30, 0.30, 0.6],
            ));
        }
        // Close button hover background — same rect the × glyph uses.
        if app.hovered_tab_close == Some(i) {
            bg_quads.push(Layout::tab_close_rect(*tab).quad([1.0, 1.0, 1.0, 0.10]));
        }
    }
    // Bottom border of tab strip.
    bg_quads.push(Quad::new(
        layout.tab_strip.x,
        layout.tab_strip.y + layout.tab_strip.h - 1.0,
        layout.tab_strip.w,
        1.0,
        [0.10, 0.10, 0.10, 1.0],
    ));

    // Editor bg
    let editor_full = Rect {
        x: layout.gutter.x,
        y: layout.gutter.y,
        w: layout.gutter.w + layout.editor_text.w,
        h: layout.gutter.h,
    };
    bg_quads.push(editor_full.quad([
        theme::BG_EDITOR.r as f32,
        theme::BG_EDITOR.g as f32,
        theme::BG_EDITOR.b as f32,
        theme::BG_EDITOR.a as f32,
    ]));

    // Current-line highlight + selection.
    if let Some(d) = app.workspace.active_doc() {
        let (cur_line, _) = d.head_line_col();
        // Current line highlight across full editor width.
        let line_y = layout.editor_text.y + theme::EDITOR_PAD
            + cur_line as f32 * theme::LINE_HEIGHT
            - d.scroll_y;
        if line_y + theme::LINE_HEIGHT > layout.editor_text.y
            && line_y < layout.editor_text.y + layout.editor_text.h
        {
            bg_quads.push(Quad::new(
                editor_full.x,
                line_y,
                editor_full.w,
                theme::LINE_HEIGHT,
                theme::LINE_HIGHLIGHT,
            ));
        }

        // Selection quads.
        if !d.sel.is_empty() {
            let (lo, hi) = d.sel.range();
            let lo_line = d.rope.byte_to_line(lo);
            let hi_line = d.rope.byte_to_line(hi);
            let lo_col = lo - d.rope.line_to_byte(lo_line);
            let hi_col = hi - d.rope.line_to_byte(hi_line);
            for run in d.buffer.layout_runs() {
                let line = run.line_i;
                if line < lo_line || line > hi_line {
                    continue;
                }
                let (col_start, col_end) = if lo_line == hi_line {
                    (lo_col, hi_col)
                } else if line == lo_line {
                    (lo_col, usize::MAX)
                } else if line == hi_line {
                    (0, hi_col)
                } else {
                    (0, usize::MAX)
                };
                let (xs, xe) = x_range_in_run(&run, col_start, col_end);
                let w = (xe - xs).max(2.0);
                bg_quads.push(Quad::new(
                    layout.editor_text.x + theme::EDITOR_PAD + xs,
                    layout.editor_text.y + theme::EDITOR_PAD + run.line_top - d.scroll_y,
                    w,
                    run.line_height,
                    theme::SELECTION,
                ));
            }
        }

        // Cursor (foreground so it sits over glyphs) — gated by blink.
        if app.cursor_blink_on {
            let (cur_line2, cur_col_byte) = d.head_line_col();
            let (cx, cy, ch) = cursor_pos_in_buffer(&d.buffer, cur_line2, cur_col_byte);
            fg_quads.push(Quad::new(
                layout.editor_text.x + theme::EDITOR_PAD + cx,
                layout.editor_text.y + theme::EDITOR_PAD + cy - d.scroll_y,
                theme::CURSOR_WIDTH,
                ch,
                theme::CURSOR,
            ));
        }
    }

    // Status bar
    bg_quads.push(layout.status_bar.quad(theme::STATUS_BAR_BG));

    // Find bar
    if let Some(fb) = layout.find_bar.as_ref() {
        bg_quads.push(fb.quad(theme::TAB_BAR_BG));
        bg_quads.push(Quad::new(
            fb.x,
            fb.y + fb.h - 1.0,
            fb.w,
            1.0,
            theme::BORDER,
        ));
    }

    // Palette dim overlay + box
    if let Some(pal) = layout.palette.as_ref() {
        bg_quads.push(Quad::new(
            0.0,
            0.0,
            gpu.config.width as f32,
            gpu.config.height as f32,
            [0.0, 0.0, 0.0, 0.6],
        ));
        bg_quads.push(pal.box_.quad(theme::PALETTE_BG));
        bg_quads.push(Quad::new(
            pal.box_.x - 1.0,
            pal.box_.y - 1.0,
            pal.box_.w + 2.0,
            pal.box_.h + 2.0,
            theme::PALETTE_BORDER,
        ));
        bg_quads.push(pal.input.quad(theme::PALETTE_INPUT_BG));
        // Selected row highlight — row rect from the ListView.
        if !app.palette.filtered.is_empty() {
            bg_quads.push(
                gpu.ui
                    .palette_list
                    .row_rect(pal.list, app.palette.selected)
                    .quad(theme::PALETTE_SELECTED),
            );
        }
    }

    // Text-input carets (blink-gated, drawn on top via fg_quads).
    if app.cursor_blink_on {
        if let Some(pal) = layout.palette.as_ref() {
            fg_quads.push(gpu.ui.palette_input.caret_quad(pal.input, 6.0));
        } else if let Some(fb) = layout.find_bar.as_ref() {
            fg_quads.push(gpu.ui.find_input.caret_quad(*fb, 8.0));
        }
        if let Some(pc) = app.creating.as_ref() {
            let (_, _, field) = create_row_geometry(layout.tree_region(), pc.row, pc.depth);
            fg_quads.push(gpu.create_input.caret_quad(field, 0.0));
        }
    }

    // ---- Build text areas ----
    let active_idx = app.workspace.active;

    let (cfg_w, cfg_h) = (gpu.config.width, gpu.config.height);
    gpu.quad_renderer
        .prepare(&gpu.device, &gpu.queue, &bg_quads, &fg_quads, (cfg_w, cfg_h));
    gpu.viewport.update(
        &gpu.queue,
        Resolution {
            width: cfg_w,
            height: cfg_h,
        },
    );

    let ui = &gpu.ui;
    let mut areas: Vec<TextArea> = Vec::new();

    // When the palette (modal) is open, suppress all underlying text so it can't
    // bleed through — text renders in one pass after the bg quads, so the dim
    // overlay alone can't occlude it. Only the palette text is drawn below.
    if layout.palette.is_none() {
    // Title bar: menu bar (left) + centered search box + layout toggles and
    // window controls (right).
    gpu.menubar.draw(layout.menu_bar_rect(), &mut areas);
    gpu.search.draw(layout.header_search_rect(), &mut areas);
    let layout_rects = layout.layout_btn_rects();
    for (i, btn) in gpu.layout_btns.iter().enumerate() {
        btn.draw(layout_rects[i], theme::TITLE_FG, &mut areas);
    }
    // Window controls — IconButton widgets at their layout rects (the same
    // rects the hover bg used above; glyph is centered in each).
    let tb_rects = layout.title_btn_rects();
    for (b, btn) in gpu.titlebar_btns.iter().enumerate() {
        let color = if app.hovered_titlebtn == Some(b) {
            theme::FG_ACTIVE
        } else {
            theme::TITLE_FG
        };
        btn.draw(tb_rects[b], color, &mut areas);
    }

    // Activity-bar icons — IconButton widgets at their cell rects.
    let act_rects = layout.activity_rects();
    for (i, btn) in gpu.activity_btns.iter().enumerate() {
        let color = if i == 0 && app.sidebar_visible {
            theme::ACTIVITY_ICON_ACTIVE
        } else {
            theme::ACTIVITY_ICON_FG
        };
        btn.draw(act_rects[i], color, &mut areas);
    }

    // Sidebar header + action buttons + root folder row + tree
    if app.sidebar_visible {
        ui.sidebar_header
            .push(layout.sidebar.x + 12.0, layout.sidebar_header_rect(), theme::FG_DIM, &mut areas);
        let er = layout.explorer_action_rects();
        for (i, btn) in gpu.explorer_btns.iter().enumerate() {
            btn.draw(er[i], theme::TITLE_FG, &mut areas);
        }
        // Root folder row (chevron + workspace name).
        ui.root_label
            .draw_left(layout.root_row_rect(), 10.0, theme::FG_TEXT, &mut areas);
        let tr = layout.tree_region();
        if let Some(pc) = app.creating.as_ref() {
            // Insert an inline field at row `pc.row`: draw the rows above it, the
            // field, then the rows from `pc.row` on shifted down by one row.
            let rowh = theme::TREE_ROW_HEIGHT;
            let (_, icon_rect, field) = create_row_geometry(tr, pc.row, pc.depth);
            if pc.row > 0 {
                let clip_a = Rect { x: tr.x, y: tr.y, w: tr.w, h: pc.row as f32 * rowh };
                ui.sidebar.draw_at(clip_a, tr.y, theme::FG_TEXT, &mut areas);
            }
            gpu.create_icons[pc.is_dir as usize].draw(icon_rect, theme::ICON_FILE_COLOR, &mut areas);
            gpu.create_input.draw(field, 0.0, theme::FG_TEXT, &mut areas);
            let below_y = tr.y + (pc.row as f32 + 1.0) * rowh;
            let clip_b = Rect {
                x: tr.x,
                y: below_y,
                w: tr.w,
                h: (tr.y + tr.h - below_y).max(0.0),
            };
            ui.sidebar.draw_at(clip_b, tr.y + rowh, theme::FG_TEXT, &mut areas);
        } else {
            ui.sidebar.draw(tr, theme::FG_TEXT, &mut areas);
        }
    }

    // Tab labels — the shared `tabs` buffer holds one label per line; we render
    // it once per tab, shifted up by one line and clipped to that tab's column,
    // so each tab shows only its own label. Geometry comes from `tab_rects`.
    let tab_rects = layout.tab_rects(n_tabs);
    for (i, tab) in tab_rects.iter().enumerate() {
        let active = app.workspace.active == Some(i);
        let line_top = i as f32 * theme::UI_LINE_HEIGHT;
        let color = if active {
            theme::TAB_FG_ACTIVE
        } else {
            theme::TAB_FG_INACTIVE
        };
        areas.push(TextArea {
            buffer: &ui.tabs,
            left: tab.x + 12.0,
            top: tab.y + 9.0 - line_top,
            scale: 1.0,
            bounds: TextBounds {
                left: tab.x as i32 + 6,
                top: (tab.y + 7.0) as i32,
                right: (tab.x + tab.w - 26.0) as i32,
                bottom: (tab.y + tab.h - 5.0) as i32,
            },
            default_color: color,
            custom_glyphs: &[],
        });

        // Close × — reusable IconButton at the close-button rect (same rect as
        // its hover bg + hit region). Hidden unless the tab is active/hovered.
        let close_color = if app.hovered_tab_close == Some(i) {
            theme::CLOSE_FG_HOVER
        } else if active || app.hovered_tab == Some(i) {
            theme::CLOSE_FG
        } else {
            glyphon::Color::rgba(0xD4, 0xD4, 0xD4, 0)
        };
        gpu.tab_close_btn
            .draw(Layout::tab_close_rect(*tab), close_color, &mut areas);
    }

    // Editor: gutter line numbers + doc text.
    if let Some(i) = active_idx {
        let d = &app.workspace.documents[i];

        // Line numbers — clipped to the gutter region so they never bleed over
        // the tab strip when scrolled.
        ui.line_numbers
            .draw(layout.gutter, d.scroll_y, theme::FG_GUTTER, &mut areas);

        // Document text
        areas.push(TextArea {
            buffer: &d.buffer,
            left: layout.editor_text.x + theme::EDITOR_PAD,
            top: layout.editor_text.y + theme::EDITOR_PAD - d.scroll_y,
            scale: 1.0,
            bounds: TextBounds {
                left: layout.editor_text.x as i32,
                top: layout.editor_text.y as i32,
                right: (layout.editor_text.x + layout.editor_text.w) as i32,
                bottom: (layout.editor_text.y + layout.editor_text.h) as i32,
            },
            default_color: theme::FG_TEXT,
            custom_glyphs: &[],
        });
    }

    // Status bar — left: path; right: position/encoding/etc. Both via the
    // reusable TextLabel (left-padded and right-padded alignment helpers).
    ui.status
        .draw_left(layout.status_bar, 12.0, theme::STATUS_BAR_FG, &mut areas);
    ui.status_right
        .draw_right(layout.status_bar, 8.0, theme::STATUS_BAR_FG, &mut areas);

    // Find bar
    if let Some(fb) = layout.find_bar.as_ref() {
        ui.find_input.draw(*fb, 8.0, theme::FG_TEXT, &mut areas);
    }
    } // end: palette closed

    // Palette text
    if let Some(pal) = layout.palette.as_ref() {
        ui.palette_input
            .draw(pal.input, 6.0, theme::FG_TEXT, &mut areas);
        ui.palette_list
            .draw(pal.list, theme::FG_TEXT, &mut areas);
    }

    gpu.text_renderer.prepare(
        &gpu.device,
        &gpu.queue,
        &mut gpu.font_system,
        &mut gpu.atlas,
        &gpu.viewport,
        areas,
        &mut gpu.swash_cache,
    )?;

    // ---- Submit ----
    let frame = gpu.surface.get_current_texture()?;
    let view = frame.texture.create_view(&TextureViewDescriptor::default());
    let mut encoder = gpu.device.create_command_encoder(&CommandEncoderDescriptor {
        label: Some("nova-encoder"),
    });
    {
        let mut pass = encoder.begin_render_pass(&RenderPassDescriptor {
            label: Some("nova-pass"),
            color_attachments: &[Some(RenderPassColorAttachment {
                view: &view,
                resolve_target: None,
                ops: Operations {
                    load: LoadOp::Clear(theme::BG_EDITOR),
                    store: StoreOp::Store,
                },
            })],
            depth_stencil_attachment: None,
            timestamp_writes: None,
            occlusion_query_set: None,
        });
        gpu.quad_renderer.render_bg(&mut pass);
        gpu.text_renderer
            .render(&gpu.atlas, &gpu.viewport, &mut pass)?;
        gpu.quad_renderer.render_fg(&mut pass);
    }
    gpu.queue.submit(Some(encoder.finish()));
    frame.present();
    gpu.atlas.trim();
    Ok(())
}

// ---------- winit glue ----------

impl ApplicationHandler for App {
    fn about_to_wait(&mut self, el: &ActiveEventLoop) {
        let interval = Duration::from_millis(theme::BLINK_MS);
        let now = Instant::now();
        if now.duration_since(self.last_blink) >= interval {
            self.cursor_blink_on = !self.cursor_blink_on;
            self.last_blink = now;
            self.redraw();
        }
        el.set_control_flow(ControlFlow::WaitUntil(self.last_blink + interval));
    }

    fn resumed(&mut self, el: &ActiveEventLoop) {
        if self.gpu.is_some() {
            return;
        }
        let attrs = Window::default_attributes()
            .with_title("Nova")
            .with_decorations(false)
            .with_inner_size(LogicalSize::new(1400.0, 900.0));
        let window = Arc::new(el.create_window(attrs).expect("create window"));
        match pollster::block_on(GpuState::new(window)) {
            Ok(gpu) => {
                self.gpu = Some(gpu);
                self.open_initial();
            }
            Err(e) => {
                eprintln!("init failed: {e:?}");
                el.exit();
            }
        }
    }

    fn window_event(&mut self, el: &ActiveEventLoop, _id: WindowId, event: WindowEvent) {
        match event {
            WindowEvent::CloseRequested => el.exit(),
            WindowEvent::ModifiersChanged(m) => {
                self.mods = m.state();
            }
            WindowEvent::Resized(size) => {
                if let Some(g) = self.gpu.as_mut() {
                    g.resize(size.width, size.height);
                }
                self.redraw();
            }
            WindowEvent::CursorMoved { position, .. } => {
                self.mouse_pos = position;
                self.on_mouse_move(position.x as f32, position.y as f32);
                self.recompute_hover();
            }
            WindowEvent::CursorLeft { .. } => {
                self.hovered_tab = None;
                self.hovered_tab_close = None;
                self.hovered_tree = None;
                self.hovered_activity = None;
                self.redraw();
            }
            WindowEvent::MouseInput {
                state,
                button: MouseButton::Left,
                ..
            } => match state {
                ElementState::Pressed => {
                    self.mouse_pressed = true;
                    self.reset_blink();
                    self.on_mouse_press(self.mouse_pos.x as f32, self.mouse_pos.y as f32);
                    if self.pending_close {
                        el.exit();
                    }
                }
                ElementState::Released => {
                    self.mouse_pressed = false;
                    self.on_mouse_release();
                }
            },
            WindowEvent::MouseWheel { delta, .. } => {
                let dy = match delta {
                    MouseScrollDelta::LineDelta(_, y) => y * theme::LINE_HEIGHT * 3.0,
                    MouseScrollDelta::PixelDelta(p) => p.y as f32,
                };
                self.on_scroll(dy);
            }
            WindowEvent::KeyboardInput { event, .. } => {
                self.reset_blink();
                self.on_key(event);
            }
            WindowEvent::RedrawRequested => {
                if let Err(e) = render(self) {
                    eprintln!("render: {e}");
                }
            }
            _ => {}
        }
    }
}

fn main() -> Result<()> {
    env_logger::init();
    // Optional path arg: a directory becomes the workspace root; a file is opened
    // (and its parent becomes the root). Falls back to the current directory.
    let arg = std::env::args().nth(1).map(PathBuf::from);
    let (root, initial_file) = match arg {
        Some(p) if p.is_dir() => (p, None),
        Some(p) if p.is_file() => {
            let parent = p
                .parent()
                .map(|x| x.to_path_buf())
                .unwrap_or_else(|| PathBuf::from("."));
            (parent, Some(p))
        }
        _ => (
            std::env::current_dir().unwrap_or_else(|_| PathBuf::from(".")),
            None,
        ),
    };
    let event_loop = EventLoop::new()?;
    let mut app = App::new(root, initial_file);
    event_loop.run_app(&mut app)?;
    Ok(())
}
