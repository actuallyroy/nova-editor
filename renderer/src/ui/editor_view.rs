// Editor view interaction: mouse hit-testing → caret placement, multi-click
// word/line/document selection, drag-select, and keeping the caret in view. The
// heavy editing lives on `Document` (in `document.rs`); this owns only the
// view-interaction state and translates pointer input into `Document` calls.

use crate::document::Document;
use crate::layout::Layout;
use crate::theme;

/// Drag-move of the selected text: armed on a fresh press inside the selection,
/// active once the pointer travels past a small threshold; `drop` is the byte the
/// text will move to on release.
pub struct TextMove {
    pub armed_at: (f32, f32),
    pub active: bool,
    pub drop: Option<usize>,
}

#[derive(Default)]
pub struct EditorView {
    /// A mouse drag-select is in progress.
    pub dragging: bool,
    /// Consecutive-click count: 1 = place, 2 = word, 3 = line, 4 = document (cycles).
    pub click_count: u32,
    /// In-flight drag-move of the current selection (None = not dragging text).
    pub text_move: Option<TextMove>,
}

impl EditorView {
    pub fn new() -> Self {
        Self::default()
    }

    /// Hit-test `(x, y)` against the document's shaped buffer and move the caret
    /// there, extending the selection when `extend`.
    pub fn place_caret(doc: &mut Document, layout: &Layout, x: f32, y: f32, extend: bool) {
        if let Some(b) = Self::byte_at(doc, layout, x, y) {
            doc.place(b, extend);
        }
    }

    /// The document byte under `(x, y)`, if it hits the shaped buffer.
    pub fn byte_at(doc: &Document, layout: &Layout, x: f32, y: f32) -> Option<usize> {
        let buf_x = x - (layout.editor_text.x + theme::EDITOR_PAD()) + doc.scroll_x();
        // Large docs shape a sliding window: hit-test in window coordinates and
        // translate the hit line back to a document line.
        let buf_y = doc.expand_visual_y(y - (layout.editor_text.y + theme::EDITOR_PAD()) + doc.scroll_y())
            - doc.buf_offset_px();
        let hit = doc.buffer.hit(buf_x, buf_y.max(0.0))?;
        let line = hit.line + doc.buf_first_line();
        if line >= doc.rope.len_lines() {
            return None;
        }
        let line_start = doc.rope.line_to_byte(line);
        let line_len = doc.rope.line(line).len_bytes();
        Some(line_start + hit.index.min(line_len))
    }

    /// The buffer line under `(x, y)`, if any — used to hit-test diff file headers.
    /// Line is driven by `y`, so a click anywhere across the row resolves correctly.
    pub fn line_at(doc: &Document, layout: &Layout, x: f32, y: f32) -> Option<usize> {
        let buf_x = x - (layout.editor_text.x + theme::EDITOR_PAD()) + doc.scroll_x();
        let buf_y = y - (layout.editor_text.y + theme::EDITOR_PAD()) + doc.scroll_y();
        doc.buffer.hit(buf_x, buf_y).map(|h| h.line)
    }

    /// Editor mouse-press: place the caret, then word/line/document-select on
    /// consecutive clicks (cycling). `consecutive` = within the double-click window.
    pub fn on_press(&mut self, doc: &mut Document, layout: &Layout, x: f32, y: f32, extend: bool, consecutive: bool) {
        // A fresh single click INSIDE the current selection arms a drag-move of that
        // text. Caret placement defers to release so the selection isn't destroyed
        // before the user can drag it.
        if !extend && !consecutive && !doc.sel.is_empty() && !doc.read_only {
            if let Some(b) = Self::byte_at(doc, layout, x, y) {
                let (lo, hi) = doc.sel.range();
                if b > lo && b < hi {
                    self.text_move = Some(TextMove { armed_at: (x, y), active: false, drop: None });
                    self.dragging = false;
                    self.click_count = 1;
                    return;
                }
            }
        }
        self.click_count = if consecutive { (self.click_count % 4) + 1 } else { 1 };
        Self::place_caret(doc, layout, x, y, extend);
        if self.click_count >= 2 {
            let b = doc.sel.head;
            match self.click_count {
                2 => doc.select_word(b),
                3 => doc.select_line(b),
                _ => doc.select_all(),
            }
            self.dragging = false;
        } else {
            self.dragging = true;
        }
    }

    /// Drag-extend the selection while the mouse is held. Returns true if a drag
    /// was active (and thus the caret moved).
    pub fn on_drag(&mut self, doc: &mut Document, layout: &Layout, x: f32, y: f32) -> bool {
        // Text drag-move: activate past a small threshold, then track the drop byte.
        if let Some(tm) = self.text_move.as_mut() {
            let (dx, dy) = (x - tm.armed_at.0, y - tm.armed_at.1);
            if !tm.active && (dx * dx + dy * dy).sqrt() > 4.0 * theme::ui_zoom() {
                tm.active = true;
            }
            if tm.active {
                tm.drop = Self::byte_at(doc, layout, x, y);
                return true;
            }
            return false;
        }
        if !self.dragging {
            return false;
        }
        Self::place_caret(doc, layout, x, y, true);
        true
    }

    pub fn on_release(&mut self) {
        self.dragging = false;
    }

    /// Scroll the document so the caret's line stays within the editor viewport.
    pub fn ensure_cursor_visible(doc: &mut Document, layout: &Layout) {
        let editor_inner_h = layout.editor_text.h - theme::EDITOR_PAD() * 2.0;
        if editor_inner_h <= 0.0 {
            return;
        }
        let (line, _) = doc.head_line_col();
        let cursor_top = line as f32 * theme::LINE_HEIGHT();
        let cursor_bottom = cursor_top + theme::LINE_HEIGHT();
        let scroll_y = doc.scroll_y();
        if cursor_top < scroll_y {
            doc.scroll.scroll_to_y(cursor_top.max(0.0));
        } else if cursor_bottom > scroll_y + editor_inner_h {
            doc.scroll.scroll_to_y(cursor_bottom - editor_inner_h);
        }
    }
}
