// Secondary (right) sidebar: the Outline view — the active document's symbols
// with kind icons, click to jump. Self-contained panel: owns its list buffer,
// scroll state, hover, and hit-testing; `App` only feeds it the active doc and
// applies the returned jump line.

use glyphon::{Attrs, Family, FontSystem, TextArea};

use crate::document::Document;
use crate::quad::Quad;
use crate::theme;
use crate::widgets::{ListView, Rect, ScrollOpts, ScrollView};

pub struct OutlinePanel {
    pub scroll: ScrollView,
    list: ListView,
    header: crate::widgets::TextLabel,
    lines: Vec<usize>, // 1-based jump target per row
    hover: Option<usize>,
}

fn header_h() -> f32 {
    theme::zpx(26.0) // matches Layout::outline_header_rect
}

impl OutlinePanel {
    pub fn new(fs: &mut FontSystem) -> Self {
        Self {
            scroll: ScrollView::new(ScrollOpts::vertical()),
            list: ListView::new(fs, 200.0, 100_000.0, 22.0, 10.0),
            header: crate::widgets::TextLabel::new(fs, 200.0, header_h()),
            lines: Vec::new(),
            hover: None,
        }
    }

    fn list_region(region: Rect) -> Rect {
        Rect {
            x: region.x,
            y: region.y + header_h(),
            w: region.w,
            h: (region.h - header_h()).max(0.0),
        }
    }

    /// Rebuild the rows from the active document's symbols (cheap when the doc
    /// version / zoom / width are unchanged — the ListView keys its reshape).
    /// `open` drives the section chevron and skips symbol extraction when
    /// collapsed.
    pub fn update(&mut self, fs: &mut FontSystem, region: Rect, doc: Option<&Document>, open: bool) {
        // Collapsible-section header: chevron + title (VSCode explorer style).
        let chev = if open { theme::ICON_CHEVRON_DOWN } else { theme::ICON_CHEVRON_RIGHT };
        let dim = Attrs::new().family(Family::Name(theme::UI_FAMILY())).color(theme::FG_DIM());
        self.header.set_rich(
            fs,
            &format!("hdr{open}{}", theme::ui_zoom()),
            &[
                (format!("{chev} "), Attrs::new().family(Family::Name(theme::ICON_FAMILY)).color(theme::FG_DIM())),
                ("OUTLINE".to_string(), dim),
            ],
            dim,
        );
        if !open {
            return; // body hidden — keep the previous rows for re-expand
        }
        let lr = Self::list_region(region);
        let ui_attrs = Attrs::new().family(Family::Name(theme::UI_FAMILY())).color(theme::FG_TEXT());
        let (key, spans, lines) = match doc {
            Some(d) => {
                let symbols = crate::extract_symbols(&d.text(), d.ext());
                let mut key = format!("OUTLINE {} v{}\n", d.name, d.version);
                let mut spans: Vec<(String, Attrs)> = Vec::new();
                let mut lines = Vec::new();
                if symbols.is_empty() {
                    spans.push(("  No symbols in this file".into(), ui_attrs.color(theme::FG_DIM())));
                    key.push_str("(empty)");
                }
                for (name, kind, line) in symbols {
                    let (g, col) = theme::symbol_icon(&kind);
                    spans.push((format!(" {}  ", g), Attrs::new().family(Family::Name(theme::ICON_FAMILY)).color(col)));
                    spans.push((format!("{name}\n"), ui_attrs));
                    lines.push(line);
                }
                (key, spans, lines)
            }
            None => (
                "OUTLINE (no doc)".to_string(),
                vec![("  No editor open".to_string(), ui_attrs.color(theme::FG_DIM()))],
                Vec::new(),
            ),
        };
        let content_h = (lines.len().max(1) as f32) * theme::TREE_ROW_HEIGHT();
        self.list.set_rich(fs, &key, &spans, lr.w, content_h.max(lr.h));
        self.lines = lines;
        self.scroll.set_metrics(lr, (lr.w, content_h + theme::zpx(8.0)));
    }

    /// Background quads: section divider, hover row, scrollbar. (No bg fill —
    /// the section sits inside the sidebar, which paints its own.)
    pub fn draw_quads(&self, region: Rect, now: std::time::Instant, bg: &mut Vec<Quad>, fg: &mut Vec<Quad>) {
        // Hairline above the section header (separates it from the file tree).
        bg.push(Quad::new(region.x, region.y, region.w, 1.0, [1.0, 1.0, 1.0, 0.08]));
        if let Some(i) = self.hover {
            let lr = Self::list_region(region);
            let y = lr.y + i as f32 * theme::TREE_ROW_HEIGHT() - self.scroll.offset().1;
            if y + theme::TREE_ROW_HEIGHT() > lr.y && y < lr.y + lr.h {
                bg.push(Quad::new(lr.x, y, lr.w, theme::TREE_ROW_HEIGHT(), theme::TREE_HOVER()));
            }
        }
        self.scroll.draw(now, fg);
    }

    /// Header label + symbol rows (clipped to the list region).
    pub fn draw_text<'a>(&'a self, region: Rect, areas: &mut Vec<TextArea<'a>>) {
        self.header.push(
            region.x + theme::zpx(12.0),
            Rect { x: region.x, y: region.y, w: region.w, h: header_h() },
            theme::FG_DIM(),
            areas,
        );
        let lr = Self::list_region(region);
        self.list.draw_at(lr, lr.y - self.scroll.offset().1, theme::FG_TEXT(), areas);
    }

    /// Row index under `p`, if it maps to a symbol.
    pub fn row_at(&self, p: (f32, f32), region: Rect) -> Option<usize> {
        let lr = Self::list_region(region);
        if !lr.contains(p) {
            return None;
        }
        let i = ((p.1 - lr.y + self.scroll.offset().1) / theme::TREE_ROW_HEIGHT()).floor();
        let i = (i.max(0.0)) as usize;
        (i < self.lines.len()).then_some(i)
    }

    /// Mouse-move hover; returns true when the highlight changed (needs redraw).
    pub fn on_move(&mut self, p: (f32, f32), region: Rect) -> bool {
        let new = self.row_at(p, region);
        if new != self.hover {
            self.hover = new;
            true
        } else {
            false
        }
    }

    pub fn on_wheel(&mut self, p: (f32, f32), region: Rect, dy: f32) -> bool {
        if !region.contains(p) {
            return false;
        }
        self.scroll.on_wheel(0.0, dy);
        true
    }

    /// Click: scrollbar press wins; otherwise a row returns its jump line.
    pub fn on_press(&mut self, p: (f32, f32), region: Rect) -> Option<usize> {
        if self.scroll.press(p) {
            return None;
        }
        self.row_at(p, region).map(|i| self.lines[i])
    }
}
