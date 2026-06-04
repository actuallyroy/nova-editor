// Secondary (right) sidebar: the AI chat panel. Full conversation UI — message
// history with role tags, a multiline input, Enter-to-send — with a stubbed
// backend for now: `respond()` is the single seam where a real model hooks in.

use glyphon::{Attrs, Buffer, Family, FontSystem, Metrics, Shaping, TextArea, TextBounds};

use crate::quad::Quad;
use crate::theme;
use crate::widgets::{Rect, ScrollOpts, ScrollView, TextInput};

#[derive(Clone, Copy, PartialEq)]
pub enum Role {
    User,
    Assistant,
}

struct Msg {
    role: Role,
    text: String,
}

struct ShapedMsg {
    role: Role,
    buf: Buffer,
    h: f32,
}

pub struct ChatPanel {
    pub scroll: ScrollView,
    input: TextInput,
    msgs: Vec<Msg>,
    shaped: Vec<ShapedMsg>,
    shape_key: String,
    header: crate::widgets::TextLabel,
    role_you: crate::widgets::TextLabel,
    role_ai: crate::widgets::TextLabel,
}

fn header_h() -> f32 {
    theme::zpx(30.0)
}
fn input_h() -> f32 {
    theme::zpx(64.0)
}
fn pad() -> f32 {
    theme::zpx(12.0)
}
fn role_h() -> f32 {
    theme::zpx(20.0)
}
fn msg_gap() -> f32 {
    theme::zpx(14.0)
}

impl ChatPanel {
    pub fn new(fs: &mut FontSystem) -> Self {
        let mut header = crate::widgets::TextLabel::new(fs, 200.0, header_h());
        header.set(fs, "CHAT", theme::UI_FAMILY());
        let mut role_you = crate::widgets::TextLabel::new(fs, 100.0, role_h());
        role_you.set(fs, "You", theme::UI_FAMILY());
        let mut role_ai = crate::widgets::TextLabel::new(fs, 100.0, role_h());
        role_ai.set(fs, "Assistant", theme::UI_FAMILY());
        let mut input = TextInput::new(fs, 200.0, input_h()).multiline(true);
        input.set_placeholder(fs, "Ask anything… (Enter to send)");
        Self {
            scroll: ScrollView::new(ScrollOpts { vertical: true, horizontal: false, stick_to_end: true }),
            input,
            msgs: Vec::new(),
            shaped: Vec::new(),
            shape_key: String::new(),
            header,
            role_you,
            role_ai,
        }
    }

    fn messages_region(region: Rect) -> Rect {
        Rect {
            x: region.x,
            y: region.y + header_h(),
            w: region.w,
            h: (region.h - header_h() - input_h() - pad()).max(0.0),
        }
    }

    fn input_rect(region: Rect) -> Rect {
        Rect {
            x: region.x + pad() * 0.75,
            y: region.y + region.h - input_h() - pad() * 0.5,
            w: region.w - pad() * 1.5,
            h: input_h(),
        }
    }

    pub fn focused(&self) -> bool {
        self.input.focused()
    }

    pub fn set_unfocused(&mut self) {
        self.input.focus(false);
    }

    /// Reshape message bubbles when the conversation / width / zoom changed.
    pub fn update(&mut self, fs: &mut FontSystem, region: Rect) {
        let mr = Self::messages_region(region);
        let text_w = (mr.w - pad() * 2.0).max(50.0);
        let key = format!("{} {:.0} {:.2} {}", self.msgs.len(), text_w, theme::ui_zoom(), theme::shape_epoch());
        if self.shape_key != key {
            self.shape_key = key;
            let m = Metrics::new(theme::UI_FONT_SIZE(), theme::UI_LINE_HEIGHT());
            self.shaped = self
                .msgs
                .iter()
                .map(|msg| {
                    let mut buf = Buffer::new(fs, m);
                    buf.set_size(fs, Some(text_w), Some(100_000.0));
                    let attrs = Attrs::new().family(Family::Name(theme::UI_FAMILY())).color(theme::FG_TEXT());
                    buf.set_rich_text(fs, [(msg.text.as_str(), attrs)], attrs, Shaping::Advanced);
                    buf.shape_until_scroll(fs, false);
                    let mut lines = 0usize;
                    for i in 0..buf.lines.len() {
                        if let Some(layout) = buf.line_layout(fs, i) {
                            lines += layout.len();
                        }
                    }
                    let h = lines as f32 * m.line_height;
                    ShapedMsg { role: msg.role, buf, h }
                })
                .collect();
        }
        let content_h: f32 = self.shaped.iter().map(|s| role_h() + s.h + msg_gap()).sum::<f32>() + pad();
        self.scroll.set_metrics(mr, (mr.w, content_h));
        self.input.rezoom(fs);
    }

    /// Panel chrome: bg, hairlines, input box, message rails.
    pub fn draw_quads(&self, region: Rect, now: std::time::Instant, bg: &mut Vec<Quad>, fg: &mut Vec<Quad>) {
        bg.push(region.quad(theme::SIDEBAR_BG()));
        bg.push(Quad::new(region.x, region.y, 1.0, region.h, [1.0, 1.0, 1.0, 0.08]));
        bg.push(Quad::new(region.x, region.y + header_h() - 1.0, region.w, 1.0, [1.0, 1.0, 1.0, 0.06]));
        // A thin accent rail beside each user message (visual role separation).
        let mr = Self::messages_region(region);
        let mut y = mr.y + pad() * 0.5 - self.scroll.offset().1;
        for s in &self.shaped {
            let block_h = role_h() + s.h;
            if s.role == Role::User && y + block_h > mr.y && y < mr.y + mr.h {
                let a = theme::ACCENT();
                bg.push(Quad::new(mr.x + pad() * 0.5, y.max(mr.y), 2.0, block_h.min(mr.y + mr.h - y.max(mr.y)), [a[0], a[1], a[2], 0.7]));
            }
            y += block_h + msg_gap();
        }
        // Input box: filled pill with a focus ring.
        let ir = Self::input_rect(region);
        let ring = if self.input.focused() { theme::ACCENT() } else { [1.0, 1.0, 1.0, 0.14] };
        bg.push(Quad::rounded(ir.x - 1.0, ir.y - 1.0, ir.w + 2.0, ir.h + 2.0, ring, theme::zpx(7.0)));
        bg.push(Quad::rounded(ir.x, ir.y, ir.w, ir.h, [0.10, 0.11, 0.15, 1.0], theme::zpx(6.0)));
        self.scroll.draw(now, fg);
    }

    pub fn draw_text<'a>(&'a self, region: Rect, areas: &mut Vec<TextArea<'a>>) {
        self.header.push(
            region.x + theme::zpx(12.0),
            Rect { x: region.x, y: region.y, w: region.w, h: header_h() },
            theme::FG_DIM(),
            areas,
        );
        let mr = Self::messages_region(region);
        let clip = TextBounds {
            left: mr.x as i32,
            top: mr.y as i32,
            right: (mr.x + mr.w) as i32,
            bottom: (mr.y + mr.h) as i32,
        };
        let mut y = mr.y + pad() * 0.5 - self.scroll.offset().1;
        for s in &self.shaped {
            let role_rect = Rect { x: mr.x + pad(), y, w: mr.w - pad() * 2.0, h: role_h() };
            if y + role_h() > mr.y && y < mr.y + mr.h {
                let (label, color) = match s.role {
                    Role::User => (&self.role_you, theme::FG_ACTIVE()),
                    Role::Assistant => (&self.role_ai, theme::FG_DIM()),
                };
                label.push_in(role_rect.x, role_rect, mr, color, areas);
            }
            y += role_h();
            if y + s.h > mr.y && y < mr.y + mr.h {
                areas.push(TextArea {
                    buffer: &s.buf,
                    left: mr.x + pad(),
                    top: y,
                    scale: 1.0,
                    bounds: clip,
                    default_color: theme::FG_TEXT(),
                    custom_glyphs: &[],
                });
            }
            y += s.h + msg_gap();
        }
        // The input draws its own text/caret/placeholder.
        let ir = Self::input_rect(region);
        self.input.draw(ir, theme::zpx(10.0), theme::FG_TEXT(), areas);
    }

    pub fn on_wheel(&mut self, p: (f32, f32), region: Rect, dy: f32) -> bool {
        if !region.contains(p) {
            return false;
        }
        self.scroll.on_wheel(0.0, dy);
        true
    }

    /// Click: focus the input, grab the scrollbar, or just consume in-panel clicks.
    pub fn on_press(&mut self, p: (f32, f32), region: Rect) -> bool {
        if !region.contains(p) {
            if self.input.focused() {
                self.input.focus(false);
            }
            return false;
        }
        if self.scroll.press(p) {
            return true;
        }
        self.input.focus(Self::input_rect(region).contains(p));
        true
    }

    pub fn cursor(&self, p: (f32, f32), region: Rect) -> Option<winit::window::CursorIcon> {
        if Self::input_rect(region).contains(p) {
            Some(winit::window::CursorIcon::Text)
        } else if region.contains(p) {
            Some(winit::window::CursorIcon::Default)
        } else {
            None
        }
    }

    /// Keyboard while the input is focused. Enter sends; Shift+Enter inserts a
    /// newline (handled by `edit_input`); Esc unfocuses.
    pub fn on_key(
        &mut self,
        event: &winit::event::KeyEvent,
        ctrl: bool,
        shift: bool,
        fs: &mut FontSystem,
        clip: Option<&mut arboard::Clipboard>,
    ) -> bool {
        use winit::keyboard::{Key, NamedKey};
        if !self.input.focused() {
            return false;
        }
        match event.logical_key.as_ref() {
            Key::Named(NamedKey::Escape) => {
                self.input.focus(false);
                return true;
            }
            Key::Named(NamedKey::Enter) if !shift => {
                self.send(fs);
                return true;
            }
            _ => {}
        }
        match crate::edit_input(&mut self.input, fs, clip, event, ctrl, shift) {
            Some(_) => true,
            None => !ctrl,
        }
    }

    fn send(&mut self, fs: &mut FontSystem) {
        let text = self.input.text().trim().to_string();
        if text.is_empty() {
            return;
        }
        self.input.clear(fs);
        self.msgs.push(Msg { role: Role::User, text });
        self.respond();
        self.scroll.scroll_to_end();
    }

    /// Backend seam: produce the assistant's reply to the latest user message.
    /// Stubbed for now — a real model (CLI or API) plugs in here.
    fn respond(&mut self) {
        self.msgs.push(Msg {
            role: Role::Assistant,
            text: "The AI backend isn't connected yet — this panel is the interface only. \
                   Hook a model into ChatPanel::respond() to bring it to life."
                .to_string(),
        });
    }
}
