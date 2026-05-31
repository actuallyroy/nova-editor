// Feedback / "Report Issue" modal form, modeled on VSCode's Issue Reporter.
// Collects a type (Bug / Feature / Other), a title, multi-line details, and an
// optional system-info block, then `App` files it as a GitHub issue via `gh`.
//
// Self-contained: owns its inputs + labels, computes its own rects (single source
// of truth for draw + hit-test), and reports a `FormAction` from input.

use glyphon::{FontSystem, TextArea};

use crate::quad::Quad;
use crate::theme;
use crate::widgets::{Rect, TextInput, TextLabel};

#[derive(Clone, Copy, PartialEq)]
pub enum FeedbackType {
    Bug,
    Feature,
    Other,
}
const TYPES: [(FeedbackType, &str); 3] =
    [(FeedbackType::Bug, "Bug"), (FeedbackType::Feature, "Feature"), (FeedbackType::Other, "Other")];

#[derive(Clone, Copy, PartialEq)]
enum Field {
    Title,
    Details,
}

/// What the form wants `App` to do after an input event.
pub enum FormAction {
    None,
    Close,
    Submit,
}

/// Rects for every interactive region (computed once, used by draw + hit-test).
struct Rects {
    box_: Rect,
    chips: [Rect; 3],
    title: Rect,
    details: Rect,
    sysinfo: Rect, // checkbox row (toggles on click anywhere on it)
    submit: Rect,
    cancel: Rect,
}

pub struct FeedbackForm {
    pub ftype: FeedbackType,
    title: TextInput,
    details: TextInput,
    focus: Field,
    selecting: Option<Field>, // field currently being drag-selected
    include_sysinfo: bool,
    l_header: TextLabel,
    l_type: TextLabel,
    l_title: TextLabel,
    l_details: TextLabel,
    l_sysinfo: TextLabel,
    l_submit: TextLabel,
    l_cancel: TextLabel,
    l_check: TextLabel,
    chips: [TextLabel; 3],
}

impl FeedbackForm {
    pub fn new(fs: &mut FontSystem) -> Self {
        let mk = |fs: &mut FontSystem, s: &str| {
            let mut l = TextLabel::new(fs, 640.0, theme::UI_LINE_HEIGHT());
            l.set(fs, s, theme::UI_FAMILY());
            l
        };
        let mut title = TextInput::new(fs, 560.0, 34.0);
        title.set_placeholder(fs, "Please enter a title");
        title.focus(true);
        let mut details = TextInput::new(fs, 560.0, 200.0).multiline(true);
        details.set_placeholder(fs, "Describe the bug, steps to reproduce, or your idea…");
        Self {
            ftype: FeedbackType::Bug,
            title,
            details,
            focus: Field::Title,
            selecting: None,
            include_sysinfo: true,
            l_header: mk(fs, "Report an issue or send feedback. Submitted to GitHub via your gh login."),
            l_type: mk(fs, "This is a"),
            l_title: mk(fs, "Title"),
            l_details: mk(fs, "Details"),
            l_sysinfo: mk(fs, "Include system information (Nova version, OS)"),
            l_submit: mk(fs, "Create on GitHub"),
            l_cancel: mk(fs, "Cancel"),
            l_check: {
                let mut l = TextLabel::new(fs, 24.0, theme::UI_LINE_HEIGHT());
                l.set(fs, "\u{eab2}", theme::ICON_FAMILY); // codicon "check"
                l
            },
            chips: [mk(fs, "Bug"), mk(fs, "Feature"), mk(fs, "Other")],
        }
    }

    fn rects(&self, win: (f32, f32)) -> Rects {
        let z = theme::ui_zoom();
        let bw = (640.0 * z).min(win.0 - 60.0);
        let bh = (560.0 * z).min(win.1 - 60.0);
        let bx = (win.0 - bw) * 0.5;
        let by = (win.1 - bh) * 0.5;
        let pad = 24.0 * z;
        let lx = bx + pad;
        let fw = bw - pad * 2.0;
        let row = 34.0 * z;
        let label_h = 22.0 * z;
        let gap = 14.0 * z;
        let mut y = by + pad + 28.0 * z; // below header

        // Type chips
        let chip_w = 90.0 * z;
        let chips = std::array::from_fn(|i| Rect { x: lx + i as f32 * (chip_w + 8.0 * z), y, w: chip_w, h: row });
        y += row + gap;
        // Title
        y += label_h;
        let title = Rect { x: lx, y, w: fw, h: row };
        y += row + gap;
        // Details
        y += label_h;
        let details = Rect { x: lx, y, w: fw, h: (by + bh - pad - row - 36.0 * z) - y };
        let after_details = details.y + details.h + gap;
        let sysinfo = Rect { x: lx, y: after_details, w: fw, h: label_h };
        // Buttons (bottom-right)
        let btn_h = 30.0 * z;
        let by2 = by + bh - pad - btn_h;
        let submit_w = 150.0 * z;
        let cancel_w = 90.0 * z;
        let submit = Rect { x: bx + bw - pad - submit_w, y: by2, w: submit_w, h: btn_h };
        let cancel = Rect { x: submit.x - 10.0 * z - cancel_w, y: by2, w: cancel_w, h: btn_h };
        Rects { box_: Rect { x: bx, y: by, w: bw, h: bh }, chips, title, details, sysinfo, submit, cancel }
    }

    pub fn draw_quads(&self, win: (f32, f32), blink: bool, bg: &mut Vec<Quad>, fg: &mut Vec<Quad>) {
        let r = self.rects(win);
        bg.push(Quad::new(0.0, 0.0, win.0, win.1, theme::DIALOG_OVERLAY()));
        bg.push(r.box_.rounded_quad(theme::CONTEXT_BG(), 8.0));
        bg.push(Quad::new(r.box_.x, r.box_.y, r.box_.w, 1.0, theme::CONTEXT_BORDER()));
        // Type chips: selected = accent fill.
        for (i, c) in r.chips.iter().enumerate() {
            let sel = TYPES[i].0 == self.ftype;
            bg.push(c.rounded_quad(if sel { theme::PALETTE_SELECTED() } else { theme::SEARCH_BG() }, 4.0));
        }
        // Field boxes: a darker fill (clearly distinct from the modal surface) with
        // a full 1px border, so each input reads as an input and the placeholder
        // text stays legible inside it.
        let field_fill = [0.10, 0.10, 0.12, 1.0];
        let border = theme::SEARCH_BORDER();
        for f in [r.title, r.details] {
            bg.push(f.rounded_quad(field_fill, 4.0));
            bg.push(Quad::new(f.x, f.y, f.w, 1.0, border));
            bg.push(Quad::new(f.x, f.y + f.h - 1.0, f.w, 1.0, border));
            bg.push(Quad::new(f.x, f.y, 1.0, f.h, border));
            bg.push(Quad::new(f.x + f.w - 1.0, f.y, 1.0, f.h, border));
        }
        // Checkbox square: blue when checked, dark with a visible border when not.
        let sz = 16.0 * theme::ui_zoom();
        let cb = Rect { x: r.sysinfo.x, y: r.sysinfo.y + 2.0, w: sz, h: sz };
        bg.push(cb.rounded_quad(if self.include_sysinfo { theme::BADGE_BG() } else { [0.10, 0.10, 0.12, 1.0] }, 3.0));
        if !self.include_sysinfo {
            let bd = theme::FG_DIM();
            let bdc = [bd.r() as f32 / 255.0, bd.g() as f32 / 255.0, bd.b() as f32 / 255.0, 1.0];
            bg.push(Quad::new(cb.x, cb.y, cb.w, 1.0, bdc));
            bg.push(Quad::new(cb.x, cb.y + cb.h - 1.0, cb.w, 1.0, bdc));
            bg.push(Quad::new(cb.x, cb.y, 1.0, cb.h, bdc));
            bg.push(Quad::new(cb.x + cb.w - 1.0, cb.y, 1.0, cb.h, bdc));
        }
        // Buttons.
        bg.push(r.submit.rounded_quad(theme::DIALOG_BTN_HOVER(), 4.0));
        bg.push(r.cancel.rounded_quad(theme::DIALOG_BTN(), 4.0));
        // Selection highlight (under text) + caret (over text) of the focused field.
        let pad = 8.0 * theme::ui_zoom();
        let (fld, frect) = if self.focus == Field::Title { (&self.title, r.title) } else { (&self.details, r.details) };
        fld.selection_quads(frect, pad, bg);
        if blink {
            fg.push(fld.caret_quad(frect, pad));
        }
    }

    pub fn draw_text<'a>(&'a self, win: (f32, f32), areas: &mut Vec<TextArea<'a>>) {
        let r = self.rects(win);
        let pad = 8.0 * theme::ui_zoom();
        let z2 = theme::ui_zoom();
        self.l_header.push(
            r.box_.x + 24.0 * z2,
            Rect { x: r.box_.x, y: r.box_.y + 14.0 * z2, w: r.box_.w, h: 22.0 * z2 },
            theme::FG_DIM(),
            areas,
        );
        for (i, c) in r.chips.iter().enumerate() {
            let col = if TYPES[i].0 == self.ftype { theme::FG_ACTIVE() } else { theme::FG_TEXT() };
            self.chips[i].push(c.x + (c.w - self.chips[i].width()) * 0.5, *c, col, areas);
        }
        // Field labels sit just above each field.
        let z = theme::ui_zoom();
        self.l_title.push(r.title.x, Rect { y: r.title.y - 24.0 * z, h: 22.0 * z, ..r.title }, theme::FG_TEXT(), areas);
        self.l_details.push(r.details.x, Rect { y: r.details.y - 24.0 * z, h: 22.0 * z, ..r.details }, theme::FG_TEXT(), areas);
        // Field contents.
        let tc = if self.title.text().is_empty() { theme::FG_DIM() } else { theme::FG_TEXT() };
        self.title.draw(r.title, pad, tc, areas);
        let dc = if self.details.text().is_empty() { theme::FG_DIM() } else { theme::FG_TEXT() };
        self.details.draw(r.details, pad, dc, areas);
        // Checkbox tick + label.
        if self.include_sysinfo {
            self.l_check.push(r.sysinfo.x + 1.0, Rect { w: 16.0 * theme::ui_zoom(), ..r.sysinfo }, theme::BADGE_FG(), areas);
        }
        self.l_sysinfo.push(r.sysinfo.x + 24.0 * theme::ui_zoom(), r.sysinfo, theme::FG_TEXT(), areas);
        // Buttons.
        self.l_submit.push(r.submit.x + (r.submit.w - self.l_submit.width()) * 0.5, r.submit, theme::FG_ACTIVE(), areas);
        self.l_cancel.push(r.cancel.x + (r.cancel.w - self.l_cancel.width()) * 0.5, r.cancel, theme::FG_TEXT(), areas);
    }

    pub fn on_press(&mut self, pt: (f32, f32), win: (f32, f32), clicks: u32) -> FormAction {
        let r = self.rects(win);
        self.selecting = None;
        if !r.box_.contains(pt) {
            return FormAction::Close; // click outside dismisses
        }
        let pad = 8.0 * theme::ui_zoom();
        for (i, c) in r.chips.iter().enumerate() {
            if c.contains(pt) {
                self.ftype = TYPES[i].0;
                return FormAction::None;
            }
        }
        if r.title.contains(pt) {
            self.focus = Field::Title;
            self.details.focus(false);
            self.title.on_click(r.title, pad, pt.0, pt.1, clicks);
            self.selecting = Some(Field::Title);
            return FormAction::None;
        }
        if r.details.contains(pt) {
            self.focus = Field::Details;
            self.title.focus(false);
            self.details.on_click(r.details, pad, pt.0, pt.1, clicks);
            self.selecting = Some(Field::Details);
            return FormAction::None;
        }
        if r.sysinfo.contains(pt) {
            self.include_sysinfo = !self.include_sysinfo;
            return FormAction::None;
        }
        if r.submit.contains(pt) {
            return FormAction::Submit;
        }
        if r.cancel.contains(pt) {
            return FormAction::Close;
        }
        FormAction::None
    }

    /// Extend the selection in the field being drag-selected (mouse move while held).
    pub fn on_drag(&mut self, pt: (f32, f32), win: (f32, f32)) {
        let r = self.rects(win);
        let pad = 8.0 * theme::ui_zoom();
        match self.selecting {
            Some(Field::Title) => self.title.on_drag(r.title, pad, pt.0, pt.1),
            Some(Field::Details) => self.details.on_drag(r.details, pad, pt.0, pt.1),
            None => {}
        }
    }

    pub fn end_drag(&mut self) {
        self.selecting = None;
    }

    /// Cursor over the form: text I-beam in the fields, pointer over actionable
    /// controls, arrow elsewhere on the modal.
    pub fn cursor(&self, pt: (f32, f32), win: (f32, f32)) -> winit::window::CursorIcon {
        use winit::window::CursorIcon;
        let r = self.rects(win);
        if r.title.contains(pt) || r.details.contains(pt) {
            CursorIcon::Text
        } else if r.chips.iter().any(|c| c.contains(pt))
            || r.sysinfo.contains(pt)
            || r.submit.contains(pt)
            || r.cancel.contains(pt)
        {
            CursorIcon::Pointer
        } else {
            CursorIcon::Default
        }
    }

    /// Route a key to the focused field. Returns the resulting action.
    pub fn on_key(
        &mut self,
        event: &winit::event::KeyEvent,
        ctrl: bool,
        extend: bool,
        fs: &mut FontSystem,
        clip: Option<&mut arboard::Clipboard>,
    ) -> FormAction {
        use winit::keyboard::{Key, NamedKey};
        match event.logical_key.as_ref() {
            Key::Named(NamedKey::Escape) => return FormAction::Close,
            Key::Named(NamedKey::Tab) => {
                self.focus = if self.focus == Field::Title { Field::Details } else { Field::Title };
                self.title.focus(self.focus == Field::Title);
                self.details.focus(self.focus == Field::Details);
                return FormAction::None;
            }
            Key::Named(NamedKey::Enter) => {
                if ctrl {
                    return FormAction::Submit;
                }
                if self.focus == Field::Details {
                    self.details.insert(fs, "\n");
                } else {
                    // Enter in the title moves to details.
                    self.focus = Field::Details;
                    self.title.focus(false);
                    self.details.focus(true);
                }
                return FormAction::None;
            }
            _ => {}
        }
        let f = if self.focus == Field::Title { &mut self.title } else { &mut self.details };
        crate::edit_input(f, fs, clip, event, ctrl, extend);
        FormAction::None
    }

    /// (title, body) for the issue, or None if the title is empty.
    pub fn issue(&self) -> Option<(String, String)> {
        let title = self.title.text().trim();
        if title.is_empty() {
            return None;
        }
        let prefix = match self.ftype {
            FeedbackType::Bug => "[Bug] ",
            FeedbackType::Feature => "[Feature] ",
            FeedbackType::Other => "",
        };
        let mut body = self.details.text().trim().to_string();
        if self.include_sysinfo {
            body.push_str(&format!(
                "\n\n---\nNova {} · {} ({})",
                env!("CARGO_PKG_VERSION"),
                std::env::consts::OS,
                std::env::consts::ARCH
            ));
        }
        Some((format!("{prefix}{title}"), body))
    }
}
