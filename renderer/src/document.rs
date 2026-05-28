// One edited file: rope text, selection, scroll, undo/redo, glyphon Buffer.
// Edits go through push_and_apply so undo/redo stays consistent.

use std::path::PathBuf;

use glyphon::{Attrs, Buffer, Family, FontSystem, Metrics, Shaping};
use ropey::Rope;

use crate::theme;

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
    pub scroll_y: f32,
    pub dirty: bool,
    history: Vec<Edit>,
    future: Vec<Edit>,
    pub buffer: Buffer,
    is_md: bool,
}

fn apply_buffer_text(buffer: &mut Buffer, fs: &mut FontSystem, text: &str, lines: usize, is_md: bool) {
    let h = (lines as f32 + 2.0) * theme::LINE_HEIGHT + 200.0;
    buffer.set_size(fs, None, Some(h));
    let mono = Attrs::new().family(Family::Name(theme::MONO_FAMILY));
    if is_md {
        let spans = md_spans(text);
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

/// Line-level markdown highlighting (headings, quotes, rules, list markers,
/// fenced code). Returns (text, attrs) spans for set_rich_text.
fn md_spans(text: &str) -> Vec<(String, Attrs<'static>)> {
    let mono = |c| Attrs::new().family(Family::Name(theme::MONO_FAMILY)).color(c);
    let mut out: Vec<(String, Attrs)> = Vec::new();
    let mut in_fence = false;
    for line in text.split_inclusive('\n') {
        let body = line.trim_end_matches('\n');
        let trimmed = body.trim_start();
        if trimmed.starts_with("```") {
            in_fence = !in_fence;
            out.push((line.to_string(), mono(theme::MD_CODE)));
            continue;
        }
        if in_fence {
            out.push((line.to_string(), mono(theme::MD_CODE)));
        } else if trimmed.starts_with('#') {
            out.push((line.to_string(), mono(theme::MD_HEADING)));
        } else if trimmed.starts_with('>') {
            out.push((line.to_string(), mono(theme::MD_QUOTE)));
        } else if !trimmed.is_empty()
            && trimmed.chars().all(|c| c == '-' || c == '*' || c == '_' || c == ' ')
            && trimmed.chars().filter(|&c| c == '-' || c == '*' || c == '_').count() >= 3
        {
            out.push((line.to_string(), mono(theme::MD_RULE)));
        } else if trimmed.starts_with("* ") || trimmed.starts_with("- ") || trimmed.starts_with("+ ") {
            let indent = body.len() - trimmed.len();
            out.push((body[..indent + 1].to_string(), mono(theme::MD_LIST)));
            out.push((format!("{}\n", &body[indent + 1..]), mono(theme::FG_TEXT)));
        } else {
            out.push((line.to_string(), mono(theme::FG_TEXT)));
        }
    }
    out
}

impl Document {
    pub fn new(path: Option<PathBuf>, contents: String, fs: &mut FontSystem) -> Self {
        let is_md = path
            .as_ref()
            .and_then(|p| p.extension())
            .map(|e| e.eq_ignore_ascii_case("md"))
            .unwrap_or(false);
        let mut buffer = Buffer::new(fs, Metrics::new(theme::FONT_SIZE, theme::LINE_HEIGHT));
        // Strip CR for display: cosmic-text renders a stray \r (from CRLF files)
        // as an extra line break, double-spacing the whole document. The rope
        // keeps the original \r\n so saving preserves line endings.
        let display = contents.replace('\r', "");
        apply_buffer_text(&mut buffer, fs, &display, display.matches('\n').count(), is_md);
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
            scroll_y: 0.0,
            dirty: false,
            history: Vec::new(),
            future: Vec::new(),
            buffer,
            is_md,
        }
    }

    pub fn reshape(&mut self, fs: &mut FontSystem) {
        let text = self.rope.to_string().replace('\r', "");
        let lines = self.rope.len_lines();
        apply_buffer_text(&mut self.buffer, fs, &text, lines, self.is_md);
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
    }

    pub fn insert_str(&mut self, s: &str, fs: &mut FontSystem) {
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
        if self.sel.is_empty() {
            return;
        }
        self.delete_selection_no_reshape();
        self.reshape(fs);
    }

    pub fn backspace(&mut self, fs: &mut FontSystem) {
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
        std::fs::write(&path, self.rope.to_string())?;
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
