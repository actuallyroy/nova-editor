// Markdown rendering for the extension README pane. Parses CommonMark/GFM with
// pulldown-cmark into a flat list of styled text spans, then shapes them in one
// wrapping glyphon buffer. We don't render to HTML or an AST — we walk the event
// stream and map inline/block styles to fonts + theme colors (headings larger,
// code monospace, links/quotes colored, lists bulleted). Images become inline
// placeholders here; the actual picture/GIF drawing is layered on separately.

use glyphon::{Attrs, Buffer, Color, Family, FontSystem, Metrics, Shaping, TextArea, TextBounds};
use pulldown_cmark::{Event, HeadingLevel, Options, Parser, Tag, TagEnd};

use crate::theme;
use crate::widgets::Rect;

fn attrs(family: &'static str, color: Color) -> Attrs<'static> {
    Attrs::new().family(Family::Name(family)).color(color)
}

/// Heading metrics (font size, line height) by level — larger for h1/h2.
fn heading_metrics(level: HeadingLevel) -> Metrics {
    match level {
        HeadingLevel::H1 => Metrics::new(22.0, 30.0),
        HeadingLevel::H2 => Metrics::new(18.0, 26.0),
        HeadingLevel::H3 => Metrics::new(16.0, 24.0),
        _ => Metrics::new(14.5, 22.0),
    }
}

/// Active inline/block style while walking the event stream.
struct Style {
    heading: Option<HeadingLevel>,
    code: bool,  // inside a fenced/indented code block
    link: bool,
    quote: bool,
    skip_text: bool, // inside an image (alt text handled as placeholder)
}

impl Style {
    fn new() -> Self {
        Self { heading: None, code: false, link: false, quote: false, skip_text: false }
    }

    /// The attrs for normal text under the current style.
    fn text_attrs(&self) -> Attrs<'static> {
        if let Some(level) = self.heading {
            return attrs(theme::UI_FAMILY, theme::MD_HEADING()).metrics(heading_metrics(level));
        }
        if self.code {
            return attrs(theme::MONO_FAMILY, theme::MD_CODE());
        }
        if self.link {
            return attrs(theme::UI_FAMILY, theme::FG_ACTIVE());
        }
        if self.quote {
            return attrs(theme::UI_FAMILY, theme::MD_QUOTE());
        }
        attrs(theme::UI_FAMILY, theme::FG_TEXT())
    }
}

/// Parse markdown `src` into styled spans for a single shaped buffer.
fn to_spans(src: &str) -> Vec<(String, Attrs<'static>)> {
    let mut opts = Options::empty();
    opts.insert(Options::ENABLE_STRIKETHROUGH);
    opts.insert(Options::ENABLE_TABLES);
    opts.insert(Options::ENABLE_TASKLISTS);
    let parser = Parser::new_ext(src, opts);

    let base = attrs(theme::UI_FAMILY, theme::FG_TEXT());
    let mut out: Vec<(String, Attrs<'static>)> = Vec::new();
    let mut st = Style::new();
    let mut list_stack: Vec<Option<u64>> = Vec::new();

    for ev in parser {
        match ev {
            Event::Start(tag) => match tag {
                Tag::Heading { level, .. } => st.heading = Some(level),
                Tag::CodeBlock(_) => {
                    st.code = true;
                    out.push(("\n".into(), base));
                }
                Tag::Link { .. } => st.link = true,
                Tag::Image { dest_url, title, .. } => {
                    st.skip_text = true;
                    let label = if !title.is_empty() { title.to_string() } else { dest_url.to_string() };
                    out.push((format!("🖼 {label}\n"), attrs(theme::UI_FAMILY, theme::FG_DIM())));
                }
                Tag::List(start) => list_stack.push(start),
                Tag::Item => {
                    let indent = "    ".repeat(list_stack.len().saturating_sub(1));
                    let marker = match list_stack.last_mut() {
                        Some(Some(n)) => {
                            let s = format!("{n}. ");
                            *n += 1;
                            s
                        }
                        _ => "•  ".to_string(),
                    };
                    out.push((format!("{indent}{marker}"), attrs(theme::UI_FAMILY, theme::MD_LIST())));
                }
                Tag::BlockQuote(_) => st.quote = true,
                _ => {}
            },
            Event::End(tag) => match tag {
                TagEnd::Heading(_) => {
                    st.heading = None;
                    out.push(("\n\n".into(), base));
                }
                TagEnd::CodeBlock => {
                    st.code = false;
                    out.push(("\n".into(), base));
                }
                TagEnd::Link => st.link = false,
                TagEnd::Image => st.skip_text = false,
                TagEnd::List(_) => {
                    list_stack.pop();
                    if list_stack.is_empty() {
                        out.push(("\n".into(), base));
                    }
                }
                TagEnd::Item => out.push(("\n".into(), base)),
                TagEnd::BlockQuote(_) => {
                    st.quote = false;
                    out.push(("\n".into(), base));
                }
                TagEnd::Paragraph => out.push(("\n\n".into(), base)),
                _ => {}
            },
            Event::Text(t) => {
                if !st.skip_text {
                    out.push((t.to_string(), st.text_attrs()));
                }
            }
            Event::Code(t) => {
                out.push((t.to_string(), attrs(theme::MONO_FAMILY, theme::MD_CODE())));
            }
            Event::SoftBreak => out.push((" ".into(), base)),
            Event::HardBreak => out.push(("\n".into(), base)),
            Event::Rule => out.push(("\n────────────────\n\n".into(), attrs(theme::UI_FAMILY, theme::MD_RULE()))),
            Event::TaskListMarker(done) => {
                out.push(((if done { "[x] " } else { "[ ] " }).into(), st.text_attrs()));
            }
            _ => {} // raw HTML, footnotes, etc. — skipped
        }
    }
    out
}

/// A scrollable markdown view backed by one wrapping glyphon buffer. Owns its
/// buffer + change-detection key + measured content height (for scroll clamping).
pub struct Markdown {
    buffer: Buffer,
    last_key: String,
    width: f32,
    content_height: f32,
}

impl Markdown {
    pub fn new(fs: &mut FontSystem) -> Self {
        let buffer = Buffer::new(fs, Metrics::new(theme::UI_FONT_SIZE, theme::UI_LINE_HEIGHT));
        Self { buffer, last_key: String::new(), width: 0.0, content_height: 0.0 }
    }

    /// Re-parse + reshape only when the content (`key`) or wrap `width` changes.
    pub fn set(&mut self, fs: &mut FontSystem, key: &str, src: &str, width: f32) {
        if self.last_key == key && (self.width - width).abs() < 0.5 {
            return;
        }
        let spans = to_spans(src);
        self.buffer.set_size(fs, Some(width), None);
        self.buffer.set_rich_text(
            fs,
            spans.iter().map(|(s, a)| (s.as_str(), *a)),
            attrs(theme::UI_FAMILY, theme::FG_TEXT()),
            Shaping::Advanced,
        );
        self.buffer.shape_until_scroll(fs, false);
        self.content_height = self
            .buffer
            .layout_runs()
            .map(|r| r.line_top + r.line_height)
            .fold(0.0_f32, f32::max);
        self.last_key = key.to_string();
        self.width = width;
    }

    pub fn content_height(&self) -> f32 {
        self.content_height
    }

    /// Draw at `rect`, scrolled up by `scroll` and clipped to `rect`.
    pub fn draw<'a>(&'a self, rect: Rect, scroll: f32, areas: &mut Vec<TextArea<'a>>) {
        areas.push(TextArea {
            buffer: &self.buffer,
            left: rect.x,
            top: rect.y - scroll,
            scale: 1.0,
            bounds: TextBounds {
                left: rect.x as i32,
                top: rect.y as i32,
                right: (rect.x + rect.w) as i32,
                bottom: (rect.y + rect.h) as i32,
            },
            default_color: theme::FG_TEXT(),
            custom_glyphs: &[],
        });
    }
}
