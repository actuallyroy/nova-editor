// Markdown rendering for the README pane. Parses CommonMark/GFM with
// pulldown-cmark into a sequence of *blocks* — runs of inline text (each its own
// shaped, wrapping glyphon buffer) interleaved with block-level images. The block
// model lets images reserve real vertical space and report their on-screen rect so
// the media layer can draw the actual picture/GIF at that position. Inline styles
// (headings, code, links, quotes, lists) map to fonts + theme colors.

use glyphon::{Attrs, Buffer, Color, Family, FontSystem, Metrics, Shaping, TextArea, TextBounds};
use pulldown_cmark::{Event, HeadingLevel, Options, Parser, Tag, TagEnd};

use crate::theme;
use crate::widgets::Rect;

fn attrs(family: &'static str, color: Color) -> Attrs<'static> {
    Attrs::new().family(Family::Name(family)).color(color)
}

fn heading_metrics(level: HeadingLevel) -> Metrics {
    match level {
        HeadingLevel::H1 => Metrics::new(22.0, 30.0),
        HeadingLevel::H2 => Metrics::new(18.0, 26.0),
        HeadingLevel::H3 => Metrics::new(16.0, 24.0),
        _ => Metrics::new(14.5, 22.0),
    }
}

struct Style {
    heading: Option<HeadingLevel>,
    code: bool,
    link: bool,
    quote: bool,
}

impl Style {
    fn new() -> Self {
        Self { heading: None, code: false, link: false, quote: false }
    }
    fn text_attrs(&self) -> Attrs<'static> {
        if self.heading.is_some() {
            // Heading size comes from the block's base metrics (uniform line
            // advance); here we only set the heading color.
            return attrs(theme::UI_FAMILY(), theme::MD_HEADING());
        }
        if self.code {
            return attrs(theme::MONO_FAMILY(), theme::MD_CODE());
        }
        if self.link {
            return attrs(theme::UI_FAMILY(), theme::FG_ACTIVE());
        }
        if self.quote {
            return attrs(theme::UI_FAMILY(), theme::MD_QUOTE());
        }
        attrs(theme::UI_FAMILY(), theme::FG_TEXT())
    }
}

/// A hyperlink within a text block: byte range [start, end) into the block's text
/// and the destination URL.
type LinkRun = (usize, usize, String);

enum Block {
    Text { buffer: Buffer, height: f32, links: Vec<LinkRun> },
    Image { url: String },
}

pub struct Markdown {
    blocks: Vec<Block>,
    image_urls: Vec<String>,
    last_key: String,
    width: f32,
}

const IMG_GAP: f32 = 10.0;
const IMG_PLACEHOLDER_H: f32 = 160.0;
const IMG_MAX_H: f32 = 420.0;
const BLOCK_GAP: f32 = 4.0; // vertical space between stacked text blocks

impl Markdown {
    pub fn new(_fs: &mut FontSystem) -> Self {
        Self { blocks: Vec::new(), image_urls: Vec::new(), last_key: String::new(), width: 0.0 }
    }

    /// Re-parse + reshape only when content (`key`) or wrap `width` changes.
    pub fn set(&mut self, fs: &mut FontSystem, key: &str, src: &str, width: f32) {
        if self.last_key == key && (self.width - width).abs() < 0.5 {
            return;
        }
        self.blocks.clear();
        self.image_urls.clear();

        let mut opts = Options::empty();
        opts.insert(Options::ENABLE_STRIKETHROUGH);
        opts.insert(Options::ENABLE_TABLES);
        opts.insert(Options::ENABLE_TASKLISTS);
        let parser = Parser::new_ext(src, opts);

        let base = attrs(theme::UI_FAMILY(), theme::FG_TEXT());
        let mut spans: Vec<(String, Attrs<'static>)> = Vec::new();
        let mut st = Style::new();
        let mut list_stack: Vec<Option<u64>> = Vec::new();
        let mut cell_idx = 0u32; // column position within the current table row
        let mut image_depth = 0u32; // >0 while inside an image (skip its alt text)

        // Flush accumulated inline spans into a Text block shaped at `metrics`
        // (the block's uniform line advance — headings use a larger metrics so
        // consecutive heading lines don't overlap, which a per-span override can't
        // fix since cosmic-text advances by the buffer's base line height).
        let mut flush = |spans: &mut Vec<(String, Attrs<'static>)>, blocks: &mut Vec<Block>, fs: &mut FontSystem, metrics: Metrics, links: &mut Vec<LinkRun>| {
            if spans.iter().all(|(s, _)| s.trim().is_empty()) {
                spans.clear();
                links.clear();
                return;
            }
            let mut buffer = Buffer::new(fs, metrics);
            buffer.set_size(fs, Some(width), Some(100_000.0));
            buffer.set_rich_text(fs, spans.iter().map(|(s, a)| (s.as_str(), *a)), base, Shaping::Advanced);
            buffer.shape_until_scroll(fs, false);
            // Force layout of EVERY logical line and count the resulting visual
            // (wrapped) lines. shape_until_scroll only lays out the first screenful,
            // so measuring via layout_runs under-reports tall blocks' height — which
            // made later blocks stack on top of them. line_layout forces full layout.
            let mut visual_lines = 0usize;
            for i in 0..buffer.lines.len() {
                if let Some(layout) = buffer.line_layout(fs, i) {
                    visual_lines += layout.len();
                }
            }
            let height = visual_lines as f32 * metrics.line_height;
            blocks.push(Block::Text { buffer, height, links: std::mem::take(links) });
            spans.clear();
        };
        let base_m = Metrics::new(theme::UI_FONT_SIZE, theme::UI_LINE_HEIGHT);
        // Link tracking: byte offset within the current block's accumulated text.
        let byte_len = |spans: &[(String, Attrs<'static>)]| spans.iter().map(|(s, _)| s.len()).sum::<usize>();
        let mut cur_links: Vec<LinkRun> = Vec::new();
        let mut link_open: Option<(usize, String)> = None;

        for ev in parser {
            match ev {
                Event::Start(tag) => match tag {
                    Tag::Heading { level, .. } => {
                        // Headings get their own uniform-metrics block.
                        flush(&mut spans, &mut self.blocks, fs, base_m, &mut cur_links);
                        st.heading = Some(level);
                    }
                    Tag::CodeBlock(_) => {
                        st.code = true;
                        spans.push(("\n".into(), base));
                    }
                    Tag::Link { dest_url, .. } => {
                        st.link = true;
                        link_open = Some((byte_len(&spans), dest_url.to_string()));
                    }
                    Tag::Image { dest_url, .. } => {
                        image_depth += 1;
                        // Block-level image. Skip SVGs (no rasterizer) so we don't
                        // reserve empty placeholder gaps for shields-style badges.
                        let url = dest_url.to_string();
                        let is_svg = url.split('?').next().unwrap_or(&url).to_lowercase().ends_with(".svg");
                        if !url.is_empty() && !is_svg {
                            flush(&mut spans, &mut self.blocks, fs, base_m, &mut cur_links);
                            self.image_urls.push(url.clone());
                            self.blocks.push(Block::Image { url });
                        }
                    }
                    Tag::List(start) => list_stack.push(start),
                    Tag::Item => {
                        let indent = "    ".repeat(list_stack.len().saturating_sub(1));
                        let marker = match list_stack.last_mut() {
                            Some(Some(n)) => { let s = format!("{n}. "); *n += 1; s }
                            _ => "•  ".to_string(),
                        };
                        spans.push((format!("{indent}{marker}"), attrs(theme::UI_FAMILY(), theme::MD_LIST())));
                    }
                    Tag::BlockQuote(_) => st.quote = true,
                    // Tables: render each row on its own line with separated cells
                    // (no column alignment yet, but readable instead of run-together).
                    Tag::Table(_) => flush(&mut spans, &mut self.blocks, fs, base_m, &mut cur_links),
                    Tag::TableHead | Tag::TableRow => cell_idx = 0,
                    Tag::TableCell => {
                        if cell_idx > 0 {
                            spans.push((" │ ".into(), attrs(theme::UI_FAMILY(), theme::FG_DIM())));
                        }
                        cell_idx += 1;
                    }
                    _ => {}
                },
                Event::End(tag) => match tag {
                    TagEnd::Heading(_) => {
                        let m = st.heading.map(heading_metrics).unwrap_or(base_m);
                        flush(&mut spans, &mut self.blocks, fs, m, &mut cur_links);
                        st.heading = None;
                    }
                    TagEnd::CodeBlock => { st.code = false; spans.push(("\n".into(), base)); }
                    TagEnd::Link => {
                        st.link = false;
                        if let Some((s, url)) = link_open.take() {
                            let e = byte_len(&spans);
                            if e > s {
                                cur_links.push((s, e, url));
                            }
                        }
                    }
                    TagEnd::Image => image_depth = image_depth.saturating_sub(1),
                    TagEnd::List(_) => { list_stack.pop(); if list_stack.is_empty() { spans.push(("\n".into(), base)); } }
                    TagEnd::Item => spans.push(("\n".into(), base)),
                    TagEnd::BlockQuote(_) => { st.quote = false; spans.push(("\n".into(), base)); }
                    TagEnd::Paragraph => spans.push(("\n\n".into(), base)),
                    TagEnd::TableHead | TagEnd::TableRow => spans.push(("\n".into(), base)),
                    TagEnd::Table => spans.push(("\n".into(), base)),
                    _ => {}
                },
                Event::Text(t) => {
                    if image_depth == 0 {
                        spans.push((t.to_string(), st.text_attrs()));
                    }
                }
                Event::Code(t) => spans.push((t.to_string(), attrs(theme::MONO_FAMILY(), theme::MD_CODE()))),
                Event::SoftBreak => spans.push((" ".into(), base)),
                Event::HardBreak => spans.push(("\n".into(), base)),
                Event::Rule => spans.push(("\n────────────────\n\n".into(), attrs(theme::UI_FAMILY(), theme::MD_RULE()))),
                Event::TaskListMarker(done) => spans.push(((if done { "[x] " } else { "[ ] " }).into(), st.text_attrs())),
                _ => {}
            }
        }
        flush(&mut spans, &mut self.blocks, fs, base_m, &mut cur_links);

        self.last_key = key.to_string();
        self.width = width;
    }

    /// All image URLs referenced (for prefetching).
    pub fn image_urls(&self) -> &[String] {
        &self.image_urls
    }

    /// Display height of an image given its natural size and the column width.
    fn image_height(natural: Option<(f32, f32)>, width: f32) -> f32 {
        match natural {
            Some((w, h)) if w > 0.0 => {
                let dw = w.min(width);
                (dw * h / w).min(IMG_MAX_H)
            }
            _ => IMG_PLACEHOLDER_H,
        }
    }

    /// Total laid-out height (depends on loaded image sizes via `size_of`).
    pub fn content_height(&self, size_of: &dyn Fn(&str) -> Option<(f32, f32)>) -> f32 {
        let mut y = 0.0;
        for b in &self.blocks {
            match b {
                Block::Text { height, .. } => y += height + BLOCK_GAP,
                // Only loaded images take space — unloaded/failed ones collapse to
                // nothing (no empty gap) until their pixels arrive.
                Block::Image { url } => {
                    if let Some(nat) = size_of(url) {
                        y += Self::image_height(Some(nat), self.width) + IMG_GAP * 2.0;
                    }
                }
            }
        }
        y
    }

    /// Draw text blocks (clipped + scrolled) and collect image draw rects for the
    /// media layer. `size_of` supplies loaded image natural sizes.
    pub fn draw<'a>(
        &'a self,
        rect: Rect,
        scroll: f32,
        size_of: &dyn Fn(&str) -> Option<(f32, f32)>,
        areas: &mut Vec<TextArea<'a>>,
        img_rects: &mut Vec<(String, Rect)>,
    ) {
        let clip = TextBounds {
            left: rect.x as i32,
            top: rect.y as i32,
            right: (rect.x + rect.w) as i32,
            bottom: (rect.y + rect.h) as i32,
        };
        let mut y = rect.y - scroll;
        for b in &self.blocks {
            match b {
                Block::Text { buffer, height, .. } => {
                    // Only emit if any part is within the viewport.
                    if y + height > rect.y && y < rect.y + rect.h {
                        areas.push(TextArea {
                            buffer,
                            left: rect.x,
                            top: y,
                            scale: 1.0,
                            bounds: clip,
                            default_color: theme::FG_TEXT(),
                            custom_glyphs: &[],
                        });
                    }
                    y += height + BLOCK_GAP;
                }
                Block::Image { url } => {
                    // Only loaded images occupy space + draw; unloaded ones collapse.
                    if let Some((nw, nh)) = size_of(url) {
                        let dh = Self::image_height(Some((nw, nh)), self.width);
                        let dw = if nw > 0.0 { nw.min(self.width) } else { self.width };
                        y += IMG_GAP;
                        if y + dh > rect.y && y < rect.y + rect.h {
                            img_rects.push((url.clone(), Rect { x: rect.x, y, w: dw, h: dh }));
                        }
                        y += dh + IMG_GAP;
                    }
                }
            }
        }
    }

    /// Screen-space rects for every visible link fragment (a link may span several
    /// runs/lines → several rects), each paired with its URL. Used both to draw
    /// underlines and to hit-test clicks — single source of truth for link geometry.
    pub fn link_geometry(&self, rect: Rect, scroll: f32, size_of: &dyn Fn(&str) -> Option<(f32, f32)>) -> Vec<(Rect, String)> {
        let mut out = Vec::new();
        let mut y = rect.y - scroll;
        for b in &self.blocks {
            match b {
                Block::Text { buffer, height, links } => {
                    if !links.is_empty() && y + height > rect.y && y < rect.y + rect.h {
                        // Glyph `start` offsets are local to each logical line; build
                        // each line's start offset in the block's global text so we
                        // can match against the (global) link byte ranges.
                        let mut line_start: Vec<usize> = Vec::with_capacity(buffer.lines.len());
                        let mut acc = 0usize;
                        for bl in buffer.lines.iter() {
                            line_start.push(acc);
                            acc += bl.text().len() + 1; // +1 for the '\n' separator
                        }
                        for run in buffer.layout_runs() {
                            let base = line_start.get(run.line_i).copied().unwrap_or(0);
                            let line_y = y + run.line_top;
                            for (s, e, url) in links {
                                let mut lo = f32::INFINITY;
                                let mut hi = f32::NEG_INFINITY;
                                for g in run.glyphs.iter() {
                                    let gs = base + g.start;
                                    if gs >= *s && gs < *e {
                                        lo = lo.min(g.x);
                                        hi = hi.max(g.x + g.w);
                                    }
                                }
                                if hi > lo {
                                    out.push((
                                        Rect { x: rect.x + lo, y: line_y, w: hi - lo, h: run.line_height },
                                        url.clone(),
                                    ));
                                }
                            }
                        }
                    }
                    y += height + BLOCK_GAP;
                }
                Block::Image { url } => {
                    if let Some(nat) = size_of(url) {
                        y += Self::image_height(Some(nat), self.width) + IMG_GAP * 2.0;
                    }
                }
            }
        }
        out
    }
}
