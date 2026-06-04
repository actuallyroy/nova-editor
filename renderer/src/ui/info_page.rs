// Hand-designed informational pages (Help > Welcome / Keyboard Shortcuts /
// Tips and Tricks) — a real layout, not rendered markdown: section headings with
// rules, aligned shortcut tables with keycap chips, zebra rows, and clickable
// link rows. The component owns all of its geometry (shape → draw/quads/links);
// callers only supply the body rect and scroll offset.

use std::path::PathBuf;

use glyphon::{Attrs, Buffer, Color, Family, FontSystem, Metrics, Shaping, TextArea, TextBounds};

use crate::quad::Quad;
use crate::theme;
use crate::widgets::Rect;

/// What clicking a link row does.
#[derive(Clone, Debug)]
pub enum Action {
    Url(String),
    OpenFolder(PathBuf),
}

/// Inline fragment of a bullet/paragraph row. `Key` renders in the mono font
/// with the keycap color (no pill — pills are for the table column).
pub enum Seg {
    Text(String),
    Key(String),
    Em(String), // accent-colored emphasis (feature names, menu paths)
}

pub enum Row {
    /// Aligned table row: keycap chips in a shared-width left column + description.
    KeyDesc { keys: Vec<String>, desc: String },
    /// Wrapping bullet line built from inline segments.
    Bullet(Vec<Seg>),
    /// Wrapping paragraph (no bullet).
    Para(Vec<Seg>),
    /// Clickable row: accent label + dim sub-text, fires `Action` on click.
    LinkRow { label: String, sub: String, action: Action },
}

pub struct Section {
    pub heading: String,
    pub rows: Vec<Row>,
}

pub struct InfoPage {
    pub title: String,
    pub subtitle: String,
    pub sections: Vec<Section>,
    shaped: Option<Shaped>,
    shape_key: String,
}

// ---- Shaped geometry (relative to the content's top-left) ----

struct Chip {
    buf: Buffer,
    rect: Rect, // pill rect, relative
}

struct SRow {
    y: f32,
    h: f32,
    zebra: bool,
    chips: Vec<Chip>,
    text: Option<(Buffer, f32, f32)>, // (buffer, x, y) relative
    link: Option<(Rect, Action)>,    // clickable rect, relative
}

struct Shaped {
    rows: Vec<SRow>,
    height: f32,
}

fn ui(c: Color) -> Attrs<'static> {
    Attrs::new().family(Family::Name(theme::UI_FAMILY())).color(c)
}
fn mono(c: Color) -> Attrs<'static> {
    Attrs::new().family(Family::Name(theme::MONO_FAMILY())).color(c)
}

/// Shape `spans` into a buffer wrapped at `width`; returns its visual height.
fn shaped_buf(
    fs: &mut FontSystem,
    spans: &[(String, Attrs<'static>)],
    metrics: Metrics,
    width: f32,
) -> (Buffer, f32, f32) {
    let mut b = Buffer::new(fs, metrics);
    b.set_size(fs, Some(width), Some(100_000.0));
    let base = ui(theme::FG_TEXT());
    b.set_rich_text(fs, spans.iter().map(|(s, a)| (s.as_str(), *a)), base, Shaping::Advanced);
    b.shape_until_scroll(fs, false);
    let mut lines = 0usize;
    let mut max_w = 0f32;
    for i in 0..b.lines.len() {
        if let Some(layout) = b.line_layout(fs, i) {
            lines += layout.len();
            for l in layout {
                max_w = max_w.max(l.w);
            }
        }
    }
    let h = lines as f32 * metrics.line_height;
    (b, max_w, h)
}

impl InfoPage {
    pub fn new(title: impl Into<String>, subtitle: impl Into<String>, sections: Vec<Section>) -> Self {
        Self {
            title: title.into(),
            subtitle: subtitle.into(),
            sections,
            shaped: None,
            shape_key: String::new(),
        }
    }

    /// Reading-column rect inside the editor region: centered, capped width.
    pub fn body(region: Rect) -> Rect {
        let pad = theme::zpx(32.0);
        let w = (region.w - pad * 2.0).min(theme::zpx(760.0)).max(0.0);
        Rect {
            x: region.x + ((region.w - w) * 0.5).max(pad),
            y: region.y + pad,
            w,
            h: (region.h - pad).max(0.0),
        }
    }

    /// (Re)shape all rows for `width`, cached per zoom + width.
    pub fn shape(&mut self, fs: &mut FontSystem, width: f32) {
        let key = format!("{:.2}x{:.0}", theme::ui_zoom(), width);
        if self.shaped.is_some() && self.shape_key == key {
            return;
        }
        let z = theme::ui_zoom();
        let chip_m = Metrics::new(11.0 * z, 16.0 * z);
        let body_m = Metrics::new(13.0 * z, 20.0 * z);
        let head_m = Metrics::new(16.0 * z, 24.0 * z);
        let title_m = Metrics::new(26.0 * z, 34.0 * z);
        let sub_m = Metrics::new(12.5 * z, 18.0 * z);
        let chip_pad_x = theme::zpx(8.0);
        let chip_h = theme::zpx(20.0);
        let chip_gap = theme::zpx(6.0);
        let row_h = theme::zpx(30.0);
        let desc_gap = theme::zpx(20.0);

        let mut rows: Vec<SRow> = Vec::new();
        let mut y = 0.0f32;

        // Title + subtitle.
        let (tb, _, th) = shaped_buf(fs, &[(self.title.clone(), ui(theme::FG_TEXT()))], title_m, width);
        rows.push(SRow { y, h: th, zebra: false, chips: Vec::new(), text: Some((tb, 0.0, 0.0)), link: None });
        y += th + theme::zpx(2.0);
        if !self.subtitle.is_empty() {
            let (sb, _, sh) = shaped_buf(fs, &[(self.subtitle.clone(), ui(theme::FG_DIM()))], sub_m, width);
            rows.push(SRow { y, h: sh, zebra: false, chips: Vec::new(), text: Some((sb, 0.0, 0.0)), link: None });
            y += sh;
        }
        y += theme::zpx(18.0);

        for sec in &self.sections {
            // Section heading + rule (the rule quad is derived from the row in quads()).
            let (hb, _, hh) = shaped_buf(fs, &[(sec.heading.clone(), ui(theme::MD_HEADING()))], head_m, width);
            rows.push(SRow { y, h: hh, zebra: true, chips: Vec::new(), text: Some((hb, 0.0, 0.0)), link: None });
            y += hh + theme::zpx(12.0);

            // Shared chip-column width across this section's KeyDesc rows.
            let mut col_w = 0.0f32;
            let mut measured: Vec<(usize, Vec<(Buffer, f32)>)> = Vec::new(); // row idx in sec → chips
            for (ri, row) in sec.rows.iter().enumerate() {
                if let Row::KeyDesc { keys, .. } = row {
                    let mut chips = Vec::new();
                    let mut w_sum = 0.0;
                    for k in keys {
                        let (cb, cw, _) = shaped_buf(fs, &[(k.clone(), mono(theme::MD_CODE()))], chip_m, 10_000.0);
                        let pill_w = cw + chip_pad_x * 2.0;
                        w_sum += pill_w + if w_sum > 0.0 { chip_gap } else { 0.0 };
                        chips.push((cb, pill_w));
                    }
                    col_w = col_w.max(w_sum);
                    measured.push((ri, chips));
                }
            }
            let mut measured = measured.into_iter();
            let mut next_measured = measured.next();
            let mut zebra = false;

            for (ri, row) in sec.rows.iter().enumerate() {
                match row {
                    Row::KeyDesc { desc, .. } => {
                        let chips_meas = match &mut next_measured {
                            Some((mi, _)) if *mi == ri => {
                                let (_, chips) = next_measured.take().unwrap();
                                next_measured = measured.next();
                                chips
                            }
                            _ => Vec::new(),
                        };
                        let text_x = col_w + desc_gap;
                        let (db, _, dh) =
                            shaped_buf(fs, &[(desc.clone(), ui(theme::FG_TEXT()))], body_m, (width - text_x).max(50.0));
                        let h = row_h.max(dh + theme::zpx(8.0));
                        let mut chips = Vec::new();
                        let mut cx = 0.0;
                        for (cb, pw) in chips_meas {
                            chips.push(Chip {
                                buf: cb,
                                rect: Rect { x: cx, y: (h - chip_h) * 0.5, w: pw, h: chip_h },
                            });
                            cx += pw + chip_gap;
                        }
                        rows.push(SRow {
                            y,
                            h,
                            zebra,
                            chips,
                            text: Some((db, text_x, (h - dh) * 0.5)),
                            link: None,
                        });
                        zebra = !zebra;
                        y += h;
                    }
                    Row::Bullet(segs) | Row::Para(segs) => {
                        let mut spans: Vec<(String, Attrs<'static>)> = Vec::new();
                        if matches!(row, Row::Bullet(_)) {
                            spans.push(("•  ".into(), ui(theme::MD_LIST())));
                        }
                        for seg in segs {
                            spans.push(match seg {
                                Seg::Text(t) => (t.clone(), ui(theme::FG_TEXT())),
                                Seg::Key(t) => (t.clone(), mono(theme::MD_CODE())),
                                Seg::Em(t) => (t.clone(), ui(theme::FG_ACTIVE())),
                            });
                        }
                        let (bb, _, bh) = shaped_buf(fs, &spans, body_m, width);
                        rows.push(SRow { y, h: bh, zebra: false, chips: Vec::new(), text: Some((bb, 0.0, 0.0)), link: None });
                        zebra = false;
                        y += bh + theme::zpx(6.0);
                    }
                    Row::LinkRow { label, sub, action } => {
                        let spans = vec![
                            (label.clone(), ui(theme::FG_ACTIVE())),
                            (if sub.is_empty() { String::new() } else { format!("   {sub}") }, ui(theme::FG_DIM())),
                        ];
                        let (lb, lw, lh) = shaped_buf(fs, &spans, body_m, width);
                        let h = lh + theme::zpx(6.0);
                        rows.push(SRow {
                            y,
                            h,
                            zebra: false,
                            chips: Vec::new(),
                            text: Some((lb, 0.0, theme::zpx(3.0))),
                            link: Some((Rect { x: 0.0, y: 0.0, w: lw, h }, action.clone())),
                        });
                        zebra = false;
                        y += h;
                    }
                }
            }
            y += theme::zpx(26.0); // gap after section
        }

        self.shaped = Some(Shaped { rows, height: y });
        self.shape_key = key;
    }

    pub fn content_height(&self) -> f32 {
        self.shaped.as_ref().map_or(0.0, |s| s.height)
    }

    /// Background quads: zebra row stripes, keycap pills, section rules.
    pub fn quads(&self, body: Rect, scroll: f32, out: &mut Vec<Quad>) {
        let Some(s) = self.shaped.as_ref() else { return };
        let (top, bot) = (body.y, body.y + body.h);
        for r in &s.rows {
            let ry = body.y + r.y - scroll;
            if ry + r.h < top || ry > bot {
                continue;
            }
            // `zebra` on a heading row marks the rule line under the heading.
            if r.chips.is_empty() && r.zebra {
                out.push(Quad::new(body.x, ry + r.h + theme::zpx(5.0), body.w, 1.0, [1.0, 1.0, 1.0, 0.08]));
                continue;
            }
            if r.zebra {
                out.push(Quad::rounded(body.x - theme::zpx(8.0), ry, body.w + theme::zpx(16.0), r.h, [1.0, 1.0, 1.0, 0.03], theme::zpx(5.0)));
            }
            for c in &r.chips {
                let cr = Rect { x: body.x + c.rect.x, y: ry + c.rect.y, w: c.rect.w, h: c.rect.h };
                // Border pill slightly larger under the fill pill.
                out.push(Quad::rounded(cr.x - 0.5, cr.y - 0.5, cr.w + 1.0, cr.h + 1.0, [1.0, 1.0, 1.0, 0.14], theme::zpx(5.0)));
                out.push(Quad::rounded(cr.x, cr.y, cr.w, cr.h, [0.16, 0.18, 0.24, 1.0], theme::zpx(5.0)));
            }
            // Link rows: underline on the label.
            if let Some((lr, _)) = &r.link {
                let a = theme::FG_ACTIVE();
                let c = [a.r() as f32 / 255.0, a.g() as f32 / 255.0, a.b() as f32 / 255.0, 0.55];
                out.push(Quad::new(body.x + lr.x, ry + r.h - theme::zpx(5.0), lr.w, 1.0, c));
            }
        }
    }

    /// Text areas (clipped to `body`).
    pub fn draw<'a>(&'a self, body: Rect, scroll: f32, areas: &mut Vec<TextArea<'a>>) {
        let Some(s) = self.shaped.as_ref() else { return };
        let clip = TextBounds {
            left: body.x as i32,
            top: body.y as i32,
            right: (body.x + body.w) as i32,
            bottom: (body.y + body.h) as i32,
        };
        let (top, bot) = (body.y, body.y + body.h);
        for r in &s.rows {
            let ry = body.y + r.y - scroll;
            if ry + r.h < top || ry > bot {
                continue;
            }
            if let Some((buf, x, y)) = &r.text {
                areas.push(TextArea {
                    buffer: buf,
                    left: body.x + x,
                    top: ry + y,
                    scale: 1.0,
                    bounds: clip,
                    default_color: theme::FG_TEXT(),
                    custom_glyphs: &[],
                });
            }
            for c in &r.chips {
                areas.push(TextArea {
                    buffer: &c.buf,
                    left: body.x + c.rect.x + theme::zpx(8.0),
                    top: ry + c.rect.y + (c.rect.h - theme::zpx(16.0)) * 0.5,
                    scale: 1.0,
                    bounds: clip,
                    default_color: theme::MD_CODE(),
                    custom_glyphs: &[],
                });
            }
        }
    }

    /// Screen-space clickable rects (link rows) with their actions.
    pub fn links(&self, body: Rect, scroll: f32) -> Vec<(Rect, Action)> {
        let Some(s) = self.shaped.as_ref() else { return Vec::new() };
        let mut out = Vec::new();
        for r in &s.rows {
            if let Some((lr, action)) = &r.link {
                let ry = body.y + r.y - scroll;
                if ry + r.h >= body.y && ry <= body.y + body.h {
                    out.push((Rect { x: body.x + lr.x, y: ry, w: lr.w, h: r.h }, action.clone()));
                }
            }
        }
        out
    }
}

// ---- Page contents ----

fn kd(keys: &[&str], desc: &str) -> Row {
    Row::KeyDesc { keys: keys.iter().map(|k| k.to_string()).collect(), desc: desc.into() }
}

/// Help > Welcome.
pub fn welcome(recent: &[PathBuf]) -> InfoPage {
    let mut recent_rows: Vec<Row> = recent
        .iter()
        .take(5)
        .map(|p| Row::LinkRow {
            label: p.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default(),
            sub: p.display().to_string(),
            action: Action::OpenFolder(p.clone()),
        })
        .collect();
    if recent_rows.is_empty() {
        recent_rows.push(Row::Para(vec![Seg::Text("None yet — ".into()), Seg::Em("File > Open Folder".into()), Seg::Text(" to get started.".into())]));
    }
    InfoPage::new(
        "Welcome to Aether",
        format!("v{} — a GPU-native code editor", env!("CARGO_PKG_VERSION")),
        vec![
            Section {
                heading: "Start".into(),
                rows: vec![
                    kd(&["Ctrl+P"], "Open a file by name (fuzzy)"),
                    kd(&["Ctrl+Shift+P"], "Command palette — or double-tap Shift"),
                    kd(&["Ctrl+O"], "Open a folder"),
                    kd(&["Ctrl+`"], "Toggle the integrated terminal"),
                ],
            },
            Section { heading: "Recent folders".into(), rows: recent_rows },
            Section {
                heading: "Palette prefixes".into(),
                rows: vec![
                    kd(&[">"], "Run a command"),
                    kd(&["@"], "Symbols in this file"),
                    kd(&["#"], "Symbols in the workspace"),
                    kd(&[":"], "Go to line"),
                    kd(&["%"], "Text in all files (live grep)"),
                ],
            },
            Section {
                heading: "Learn more".into(),
                rows: vec![
                    Row::Para(vec![
                        Seg::Em("Help > Keyboard Shortcuts Reference".into()),
                        Seg::Text(" lists every binding; ".into()),
                        Seg::Em("Help > Tips and Tricks".into()),
                        Seg::Text(" covers the features that are easy to miss.".into()),
                    ]),
                    Row::LinkRow {
                        label: "Documentation".into(),
                        sub: "github.com/actuallyroy/aether-editor".into(),
                        action: Action::Url("https://github.com/actuallyroy/aether-editor#readme".into()),
                    },
                    Row::LinkRow {
                        label: "Release Notes".into(),
                        sub: "what changed in each version".into(),
                        action: Action::Url("https://github.com/actuallyroy/aether-editor/releases".into()),
                    },
                ],
            },
        ],
    )
}

/// Help > Keyboard Shortcuts Reference — generated from the command table so it
/// can't drift, plus the non-command bindings.
pub fn shortcuts() -> InfoPage {
    let mut cmd_rows: Vec<(String, Vec<String>)> = crate::commands::COMMANDS
        .iter()
        .filter(|(_, _, hint)| !hint.is_empty())
        .map(|(_, label, hint)| (label.to_string(), vec![hint.to_string()]))
        .collect();
    cmd_rows.sort();
    let commands = cmd_rows
        .into_iter()
        .map(|(label, keys)| Row::KeyDesc { keys, desc: label })
        .collect();
    InfoPage::new(
        "Keyboard Shortcuts",
        "the editor follows VS Code's keymap",
        vec![
            Section { heading: "Commands".into(), rows: commands },
            Section {
                heading: "Editor".into(),
                rows: vec![
                    kd(&["Alt+Up", "Alt+Down"], "Move line up / down"),
                    kd(&["Shift+Alt+Up", "Shift+Alt+Down"], "Copy line up / down"),
                    kd(&["Shift+Alt+Left", "Shift+Alt+Right"], "Shrink / expand selection"),
                    kd(&["Shift+Alt+A"], "Toggle block comment"),
                    kd(&["Alt+Left", "Alt+Right"], "Navigate back / forward"),
                    kd(&["F8", "Shift+F8"], "Next / previous problem"),
                    kd(&["F12", "Shift+F12"], "Go to definition / references"),
                    kd(&["Ctrl+Shift+M"], "Problems list"),
                    kd(&["Ctrl+T"], "Workspace symbols"),
                    kd(&["Ctrl+Shift+\\"], "Jump to matching bracket"),
                ],
            },
            Section {
                heading: "Terminal".into(),
                rows: vec![
                    kd(&["Ctrl+Shift+C", "Ctrl+Shift+V"], "Copy / paste"),
                    kd(&["Ctrl+Shift+A"], "Select all"),
                    kd(&["Esc"], "Clear the prompt when idle · sent to the running app when busy"),
                ],
            },
        ],
    )
}

/// Help > Tips and Tricks.
pub fn tips() -> InfoPage {
    InfoPage::new(
        "Tips and Tricks",
        "the features that are easy to miss",
        vec![
            Section {
                heading: "Palette".into(),
                rows: vec![
                    Row::Bullet(vec![Seg::Text("Double-tap ".into()), Seg::Key("Shift".into()), Seg::Text(" to open the command palette from anywhere.".into())]),
                    Row::Bullet(vec![Seg::Text("Type ".into()), Seg::Key("%text%".into()), Seg::Text(" to live-grep the workspace — results stream into the list.".into())]),
                    Row::Bullet(vec![Seg::Key("@".into()), Seg::Text(" jumps to a symbol and tints its whole block while you arrow through.".into())]),
                    Row::Bullet(vec![Seg::Key("#".into()), Seg::Text(" searches symbols across the entire workspace via the language server.".into())]),
                ],
            },
            Section {
                heading: "Editor".into(),
                rows: vec![
                    Row::Bullet(vec![Seg::Em("Expand Selection".into()), Seg::Text(" (".into()), Seg::Key("Shift+Alt+Right".into()), Seg::Text(") grows word → brackets → lines → all; ".into()), Seg::Key("Shift+Alt+Left".into()), Seg::Text(" shrinks it back step by step.".into())]),
                    Row::Bullet(vec![Seg::Text("Drag selected text to move it — one undo restores the whole move.".into())]),
                    Row::Bullet(vec![Seg::Key("Ctrl+/".into()), Seg::Text(" understands the language: ".into()), Seg::Key("//".into()), Seg::Text(" in Rust, ".into()), Seg::Key("#".into()), Seg::Text(" in Python, ".into()), Seg::Key("<!-- -->".into()), Seg::Text(" in HTML.".into())]),
                    Row::Bullet(vec![Seg::Em("File > Auto Save".into()), Seg::Text(" writes changes about a second after you stop typing.".into())]),
                    Row::Bullet(vec![Seg::Text("Diagnostics move with your edits — squiggles stay glued to the code.".into())]),
                ],
            },
            Section {
                heading: "Terminal".into(),
                rows: vec![
                    Row::Bullet(vec![Seg::Text("Terminals ".into()), Seg::Em("survive a full editor restart".into()), Seg::Text(" — shells keep running in a background host and re-attach on launch.".into())]),
                    Row::Bullet(vec![Seg::Text("Click anywhere in the prompt line to move the shell cursor there.".into())]),
                    Row::Bullet(vec![Seg::Text("Drag a file from the explorer into the terminal to paste its quoted path.".into())]),
                    Row::Bullet(vec![Seg::Em("Terminal > Run Selected Text".into()), Seg::Text(" sends the editor selection to the shell.".into())]),
                ],
            },
            Section {
                heading: "Workbench".into(),
                rows: vec![
                    Row::Bullet(vec![Seg::Text("Right-click everything: tabs, the editor, SCM rows, the file tree.".into())]),
                    Row::Bullet(vec![Seg::Em("View > Zen Mode".into()), Seg::Text(" strips all chrome; ".into()), Seg::Em("Centered Layout".into()), Seg::Text(" narrows the column.".into())]),
                    Row::Bullet(vec![Seg::Key("Alt+Left".into()), Seg::Text(" / ".into()), Seg::Key("Alt+Right".into()), Seg::Text(" walk your navigation history like a browser.".into())]),
                ],
            },
        ],
    )
}
