// Pure geometry: computes every region rect from the window size + UI state.
// Every widget and the renderer read their rects from here, so positions have a
// single source of truth.

use std::sync::atomic::{AtomicBool, Ordering};

use crate::theme;
use crate::widgets::Rect;
use crate::SidebarView;

// View-mode flags (View > Zen Mode / Centered Layout). Globals like the settings
// store: layout geometry is computed from many call sites, and these are pure
// presentation toggles with no per-window state.
static ZEN: AtomicBool = AtomicBool::new(false);
static CENTERED: AtomicBool = AtomicBool::new(false);

pub fn zen() -> bool {
    ZEN.load(Ordering::Relaxed)
}
pub fn set_zen(on: bool) {
    ZEN.store(on, Ordering::Relaxed);
}
pub fn centered() -> bool {
    CENTERED.load(Ordering::Relaxed)
}
pub fn set_centered(on: bool) {
    CENTERED.store(on, Ordering::Relaxed);
}

pub struct Layout {
    pub title_bar: Rect,
    pub activity_bar: Rect,
    pub sidebar: Rect,
    pub tab_strip: Rect,
    pub breadcrumbs: Rect, // path bar below the tab strip (h == 0 when hidden)
    pub gutter: Rect,
    pub editor_text: Rect,
    pub status_bar: Rect,
    pub terminal_panel: Option<Rect>,
    pub palette: Option<PaletteLayout>,
    /// Secondary (right) sidebar — the AI chat panel. Zero-width when hidden.
    pub right_sidebar: Rect,
    /// OUTLINE section body height in the explorer sidebar: None = section
    /// hidden (other view / sidebar closed), Some(0.0) = collapsed to header.
    outline_body_h: Option<f32>,
}

pub struct PaletteLayout {
    pub box_: Rect,
    pub input: Rect,
    pub list: Rect,
}

impl Layout {
    /// Whether the breadcrumb path bar should show for `doc`: a regular file tab
    /// (has a path, not a diff/image/info/graph/binary view). Single source of
    /// truth shared by both `Layout::compute` call sites.
    pub fn breadcrumbs_visible(doc: Option<&crate::document::Document>) -> bool {
        doc.map_or(false, |d| {
            d.path.is_some()
                && d.diff.is_none()
                && d.image.is_none()
                && d.info.is_none()
                && d.graph.is_none()
                && !d.binary
        })
    }

    pub fn compute(
        w: f32,
        h: f32,
        sidebar_visible: bool,
        sidebar_width: f32,
        find_active: bool,
        palette_active: bool,
        // The terminal panel's requested height when open, or None when hidden.
        // The actual height is clamped to leave room for the editor.
        terminal_height: Option<f32>,
        // True when the active tab is a diff view — the gutter widens to fit the
        // dual old │ new line-number columns.
        diff_gutter: bool,
        // Secondary (right) sidebar width; 0.0 = hidden. Collapses in Zen Mode.
        right_width: f32,
        // Explorer OUTLINE section: None = not showing (other sidebar view),
        // Some(open) = header visible, body expanded when `open`.
        explorer_outline: Option<bool>,
        // True when the active tab is a regular file → show the breadcrumb path bar.
        show_breadcrumbs: bool,
    ) -> Self {
        let tb = theme::TITLE_BAR_H();
        let title_bar = Rect { x: 0.0, y: 0.0, w, h: tb };
        // Zen Mode strips the chrome: status + activity bars collapse to nothing.
        let sb_h = if zen() { 0.0 } else { theme::STATUS_BAR_HEIGHT() };
        let panel_h = h - sb_h - tb;
        // workbench.activityBar.visible — collapse to 0 width when hidden.
        let activity_w = if !zen() && crate::settings::activitybar_visible() { theme::ACTIVITY_BAR_WIDTH() } else { 0.0 };
        let activity_bar = Rect {
            x: 0.0,
            y: tb,
            w: activity_w,
            h: panel_h,
        };
        let sidebar = Rect {
            x: activity_bar.w,
            y: tb,
            w: if sidebar_visible { sidebar_width } else { 0.0 },
            h: panel_h,
        };
        let editor_left = sidebar.x + sidebar.w;
        // Secondary (right) sidebar: full content height on the right edge; the
        // tab strip, editor, and terminal all stop at its left edge.
        let rw = if zen() { 0.0 } else { right_width.max(0.0) };
        let right_sidebar = Rect {
            x: w - rw,
            y: tb,
            w: rw,
            h: panel_h,
        };
        let content_right = w - rw;
        let tab_strip = Rect {
            x: editor_left,
            y: tb,
            w: (content_right - editor_left).max(0.0),
            h: theme::TAB_HEIGHT(),
        };
        // The find/replace widget floats over the editor's top-right (it doesn't
        // push the editor down), so no vertical reservation here.
        let _ = find_active;
        // Breadcrumb path bar sits between the tab strip and the editor; it pushes
        // the editor down by its height (0 when hidden).
        let breadcrumbs = Rect {
            x: editor_left,
            y: tb + tab_strip.h,
            w: (content_right - editor_left).max(0.0),
            h: if show_breadcrumbs { theme::BREADCRUMB_HEIGHT() } else { 0.0 },
        };
        let editor_y = tb + tab_strip.h + breadcrumbs.h;
        // Terminal panel sits above the status bar; the editor shrinks to fit. A
        // maximize request (huge height) is clamped here to fill the whole content
        // area (editor_h → 0); normal drag is bounded by the splitter's own max.
        let term_h = match terminal_height {
            Some(req) => req.min((h - editor_y - sb_h).max(0.0)),
            None => 0.0,
        };
        let editor_h = (h - editor_y - sb_h - term_h).max(0.0);
        let terminal_panel = if term_h > 0.0 {
            Some(Rect {
                x: editor_left,
                y: editor_y + editor_h,
                w: (content_right - editor_left).max(0.0),
                h: term_h,
            })
        } else {
            None
        };
        // editor.lineNumbers — collapse the gutter to 0 width when off. Diff views
        // also collapse it: the side-by-side renderer draws its own per-pane gutters
        // across the full editor region.
        let gutter_w = if !crate::settings::line_numbers() || diff_gutter {
            0.0
        } else {
            theme::GUTTER_WIDTH()
        };
        let mut gutter = Rect {
            x: editor_left,
            y: editor_y,
            w: gutter_w,
            h: editor_h,
        };
        let mut editor_text = Rect {
            x: gutter.x + gutter.w,
            y: editor_y,
            w: (content_right - gutter.x - gutter.w).max(0.0),
            h: editor_h,
        };
        // Centered Layout: inset the editor column symmetrically (the tab strip and
        // panels keep their full width, like VSCode).
        if centered() {
            let region_w = content_right - gutter.x;
            let target = (region_w * 0.66).max((theme::zpx(700.0)).min(region_w));
            let pad = ((region_w - target) * 0.5).max(0.0);
            gutter.x += pad;
            editor_text.x += pad;
            editor_text.w = (editor_text.w - pad * 2.0).max(0.0);
        }
        let status_bar = Rect {
            x: 0.0,
            y: h - sb_h,
            w,
            h: sb_h,
        };
        let palette = if palette_active {
            let pw = theme::PALETTE_WIDTH().min(w - theme::zpx(40.0));
            let visible = 9usize;
            let pad = theme::zpx(10.0); // inner card padding
            // The title-bar search pill IS the palette input (VSCode command-center
            // style) — no second input field inside the card; the card is just the
            // results list dropping down beneath the bar.
            let z = theme::ui_zoom();
            let iw = (w * 0.34).clamp(280.0 * z, 560.0 * z);
            let ih = 22.0 * z;
            let input = Rect { x: (w - iw) * 0.5, y: (theme::TITLE_BAR_H() - ih) * 0.5, w: iw, h: ih };
            let ph = pad + theme::PALETTE_ROW_HEIGHT() * visible as f32 + pad;
            let bx = (w - pw) * 0.5;
            let by = theme::TITLE_BAR_H() + theme::zpx(6.0);
            let box_ = Rect { x: bx, y: by, w: pw, h: ph };
            let list = Rect {
                x: box_.x + pad,
                y: box_.y + pad,
                w: box_.w - pad * 2.0,
                h: theme::PALETTE_ROW_HEIGHT() * visible as f32,
            };
            Some(PaletteLayout { box_, input, list })
        } else {
            None
        };
        // Explorer OUTLINE section body: ~35% of the sidebar when expanded.
        let outline_body_h = match explorer_outline {
            Some(_) if sidebar.w <= 0.0 => None,
            Some(true) => Some((panel_h * 0.35).clamp(theme::zpx(120.0), theme::zpx(420.0))),
            Some(false) => Some(0.0),
            None => None,
        };
        Self {
            title_bar,
            activity_bar,
            sidebar,
            tab_strip,
            breadcrumbs,
            gutter,
            editor_text,
            status_bar,
            terminal_panel,
            palette,
            right_sidebar,
            outline_body_h,
        }
    }

    /// Single source of truth for activity-bar button rects: 5 at the top,
    /// 2 (account, settings) pinned to the bottom. Index matches the icon order.
    pub fn activity_rects(&self) -> Vec<Rect> {
        let ab = self.activity_bar;
        (0..7)
            .map(|i| {
                let y = if i < 5 {
                    ab.y + i as f32 * theme::ACTIVITY_CELL()
                } else {
                    ab.y + ab.h - (7 - i) as f32 * theme::ACTIVITY_CELL()
                };
                Rect {
                    x: ab.x,
                    y,
                    w: ab.w,
                    h: theme::ACTIVITY_CELL(),
                }
            })
            .collect()
    }

    /// Single source of truth for the window-control button rects (min, max,
    /// close), left-to-right at the right edge of the title bar.
    pub fn title_btn_rects(&self) -> Vec<Rect> {
        (0..3)
            .map(|b| Rect {
                x: self.title_bar.w - (3 - b) as f32 * theme::TITLE_BTN_W(),
                y: self.title_bar.y,
                w: theme::TITLE_BTN_W(),
                h: theme::TITLE_BAR_H(),
            })
            .collect()
    }

    /// Single source of truth for tab rects: equal-width columns clamped to
    /// [TAB_MIN_WIDTH, TAB_MAX_WIDTH], left-to-right across the tab strip.
    pub fn tab_rects(&self, n: usize) -> Vec<Rect> {
        if n == 0 {
            return Vec::new();
        }
        // Always fit every tab in the strip: equal columns of (strip / n), capped at
        // MAX (so few tabs don't stretch huge) and floored only at a tiny width that
        // still shows the file icon — full name shows on hover.
        let tab_w = (self.tab_strip.w / n as f32).min(theme::TAB_MAX_WIDTH()).max(theme::zpx(30.0));
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
    pub fn layout_btn_rects(&self) -> Vec<Rect> {
        let cw = theme::zpx(28.0);
        // The buttons hug the right edge. Windows/Linux reserve space for the
        // drawn window controls (min/max/close); macOS traffic lights are native
        // on the LEFT, so only a small pad remains.
        let right = if cfg!(target_os = "macos") {
            self.title_bar.w - theme::zpx(8.0)
        } else {
            self.title_bar.w - 3.0 * theme::TITLE_BTN_W()
        };
        (0..3)
            .map(|i| Rect {
                x: right - (3 - i) as f32 * cw,
                y: self.title_bar.y,
                w: cw,
                h: theme::TITLE_BAR_H(),
            })
            .collect()
    }

    /// The menu-bar region (left portion of the title bar).
    pub fn menu_bar_rect(&self) -> Rect {
        Rect {
            x: 0.0,
            y: self.title_bar.y,
            w: self.title_bar.w,
            h: theme::TITLE_BAR_H(),
        }
    }

    /// The centered command-center search box in the title bar.
    pub fn header_search_rect(&self) -> Rect {
        let z = theme::ui_zoom();
        let w = (self.title_bar.w * 0.34).clamp(280.0 * z, 560.0 * z);
        let h = 22.0 * z;
        Rect {
            x: (self.title_bar.w - w) * 0.5,
            y: self.title_bar.y + (theme::TITLE_BAR_H() - h) * 0.5,
            w,
            h,
        }
    }

    /// The root-folder row (below the EXPLORER header, above the tree).
    pub fn root_row_rect(&self) -> Rect {
        Rect {
            x: self.sidebar.x,
            y: self.sidebar.y + theme::SIDEBAR_HEADER_H(),
            w: self.sidebar.w,
            h: theme::TREE_ROW_HEIGHT(),
        }
    }

    /// The file-tree list region: the sidebar below the header + root row.
    pub fn tree_region(&self) -> Rect {
        // The Explorer reserves a row below the header for its root ("AETHER-EDITOR") row.
        let top = self.sidebar.y + theme::SIDEBAR_HEADER_H() + theme::TREE_ROW_HEIGHT();
        // The OUTLINE section (header + optional body) docks at the sidebar bottom.
        let bottom = self.outline_header_rect().map_or(self.sidebar.y + self.sidebar.h, |h| h.y);
        Rect {
            x: self.sidebar.x,
            y: top,
            w: self.sidebar.w,
            h: (bottom - top).max(0.0),
        }
    }

    /// The OUTLINE section's collapsible header bar at the explorer's bottom
    /// (None when the explorer view isn't showing).
    pub fn outline_header_rect(&self) -> Option<Rect> {
        let body_h = self.outline_body_h?;
        let h = theme::zpx(26.0);
        Some(Rect {
            x: self.sidebar.x,
            y: self.sidebar.y + self.sidebar.h - body_h - h,
            w: self.sidebar.w,
            h,
        })
    }

    /// The OUTLINE section's body (symbol list) under its header; zero-height
    /// when collapsed.
    pub fn outline_body_rect(&self) -> Option<Rect> {
        let body_h = self.outline_body_h?;
        Some(Rect {
            x: self.sidebar.x,
            y: self.sidebar.y + self.sidebar.h - body_h,
            w: self.sidebar.w,
            h: body_h,
        })
    }

    /// Content region for sidebar panels WITHOUT a root row (Source Control, Search,
    /// Extensions): starts just below the header with a small pad, so there's no empty
    /// row of gap between the header (e.g. "SOURCE CONTROL") and the first item.
    pub fn panel_region(&self) -> Rect {
        let top = self.sidebar.y + theme::SIDEBAR_HEADER_H() + theme::zpx(6.0);
        Rect {
            x: self.sidebar.x,
            y: top,
            w: self.sidebar.w,
            h: (self.sidebar.y + self.sidebar.h - top).max(0.0),
        }
    }

    /// The Explorer header action buttons (New File / New Folder / Refresh /
    /// Collapse All), right-aligned in the sidebar header.
    pub fn explorer_action_rects(&self) -> Vec<Rect> {
        let cw = theme::zpx(26.0);
        let n = 4;
        let right = self.sidebar.x + self.sidebar.w - theme::zpx(6.0);
        let y = self.sidebar.y + (theme::SIDEBAR_HEADER_H() - cw) * 0.5;
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
    pub fn sidebar_header_rect(&self) -> Rect {
        Rect {
            x: self.sidebar.x,
            y: self.sidebar.y,
            w: self.sidebar.w,
            h: theme::SIDEBAR_HEADER_H(),
        }
    }

    /// The close-button cell within a tab — a square icon-button rect pinned to
    /// the tab's right edge. Drives both the × glyph and its hit region.
    pub fn tab_close_rect(tab: Rect) -> Rect {
        let s = theme::zpx(20.0);
        Rect {
            x: tab.x + tab.w - s - theme::zpx(6.0),
            y: tab.y + (tab.h - s) * 0.5,
            w: s,
            h: s,
        }
    }
}

// ===== Free-standing region geometry (moved out of main.rs) =====
// These derive sub-region rects from a parent rect; re-exported at the crate root
// so existing `crate::<fn>` references keep working.

/// `icon_x` is the ABSOLUTE x of the icon column for this row's depth — the caller
/// sources it from the tree (`IconList::icon_x_for_depth`) so the inline input lines
/// up with sibling rows exactly, instead of re-deriving the indent math (which drifts
/// from the tree's character-based indentation at non-100% zoom).
pub(crate) fn create_row_geometry(tr: Rect, row: usize, icon_x: f32) -> (Rect, Rect, Rect) {
    let row_y = tr.y + row as f32 * theme::TREE_ROW_HEIGHT();
    let icon_w = theme::zpx(16.0);
    let field_x = icon_x + icon_w + theme::zpx(4.0);
    let row_rect = Rect { x: tr.x, y: row_y, w: tr.w, h: theme::TREE_ROW_HEIGHT() };
    let icon_rect = Rect { x: icon_x, y: row_y, w: icon_w, h: theme::TREE_ROW_HEIGHT() };
    let field = Rect {
        x: field_x,
        y: row_y,
        w: (tr.x + tr.w - field_x).max(0.0),
        h: theme::TREE_ROW_HEIGHT(),
    };
    (row_rect, icon_rect, field)
}

/// The activity-bar icon index that's currently "active" (highlighted).
pub(crate) fn active_activity_idx(sidebar_visible: bool, view: SidebarView) -> Option<usize> {
    if !sidebar_visible {
        return None;
    }
    match view {
        SidebarView::Explorer => Some(0),
        SidebarView::Search => Some(1),
        SidebarView::SourceControl => Some(2),
        SidebarView::Debug => Some(3),
        SidebarView::Extensions => Some(4),
    }
}

/// The search/filter box rect at the top of the Extensions sidebar.
pub(crate) fn ext_filter_rect(tree: Rect) -> Rect {
    Rect { x: tree.x + theme::zpx(10.0), y: tree.y + theme::zpx(8.0), w: tree.w - theme::zpx(20.0), h: theme::zpx(30.0) }
}

/// The scrollable extension-row list region (below the filter box).
pub(crate) fn ext_list_region(tree: Rect) -> Rect {
    let strip = theme::zpx(46.0); // filter box + padding
    Rect { x: tree.x, y: tree.y + strip, w: tree.w, h: (tree.h - strip).max(0.0) }
}

/// Right-aligned icon-button rects in the terminal panel header, drawn left→right
/// as: +, split, trash, …, maximize, close (6 buttons — matches `GpuState::terminal_btns`).
pub(crate) const TERMINAL_HEADER_BTNS: usize = 6;
pub(crate) fn terminal_header_button_rects(panel: Rect) -> Vec<Rect> {
    let bw = theme::zpx(28.0);
    let right = panel.x + panel.w - theme::zpx(8.0);
    let start_x = right - TERMINAL_HEADER_BTNS as f32 * bw;
    (0..TERMINAL_HEADER_BTNS)
        .map(|i| Rect { x: start_x + i as f32 * bw, y: panel.y, w: bw, h: theme::TERMINAL_HEADER_H() })
        .collect()
}

pub(crate) const TERMINAL_TABLIST_W: f32 = 160.0;

/// The right-side terminal-tab list rect — shown (VSCode-style) only when there's
/// more than one tab, so a single terminal still uses the full width.
pub(crate) fn terminal_tablist_rect(content: Rect, group_count: usize) -> Option<Rect> {
    if group_count <= 1 {
        return None;
    }
    let w = theme::zpx(TERMINAL_TABLIST_W).min(content.w * 0.4);
    Some(Rect { x: content.x + content.w - w, y: content.y, w, h: content.h })
}

/// The pane area: the content minus the tab list (when the list is shown).
pub(crate) fn terminal_pane_area(content: Rect, group_count: usize) -> Rect {
    match terminal_tablist_rect(content, group_count) {
        Some(tl) => Rect { w: (content.w - tl.w).max(1.0), ..content },
        None => content,
    }
}

/// The close (×) button rect for terminal tab-list row `row`.
pub(crate) fn terminal_tab_close_rect(tl: Rect, row: usize) -> Rect {
    let s = theme::zpx(18.0);
    Rect {
        x: tl.x + tl.w - s - theme::zpx(6.0),
        y: tl.y + row as f32 * theme::TREE_ROW_HEIGHT() + (theme::TREE_ROW_HEIGHT() - s) * 0.5,
        w: s,
        h: s,
    }
}

/// Split the terminal pane area into `n` side-by-side pane rects (1px gaps).
pub(crate) fn terminal_pane_rects(content: Rect, n: usize) -> Vec<Rect> {
    if n == 0 {
        return Vec::new();
    }
    let gap = 1.0;
    let w = ((content.w - gap * (n - 1) as f32) / n as f32).max(1.0);
    (0..n)
        .map(|i| Rect { x: content.x + i as f32 * (w + gap), y: content.y, w, h: content.h })
        .collect()
}

/// The terminal text/grid area: the panel minus the header strip at its top.
pub(crate) fn terminal_content(panel: Rect) -> Rect {
    let h = theme::TERMINAL_HEADER_H();
    Rect {
        x: panel.x,
        y: panel.y + h,
        w: panel.w,
        h: (panel.h - h).max(0.0),
    }
}

/// Rows/cols that fit `panel` for a monospace cell of `char_w` px wide. Using the
/// real measured advance keeps the PTY's column count matched to what's rendered.
pub(crate) fn terminal_grid_size(panel: Rect, char_w: f32) -> (usize, usize) {
    let char_w = char_w.max(1.0);
    let cols = (((panel.w - theme::zpx(16.0)) / char_w) as usize).clamp(8, 400);
    let rows = (((panel.h - theme::zpx(8.0)) / theme::LINE_HEIGHT()) as usize).clamp(2, 200);
    (rows, cols)
}

/// The x-pixel range (start, end) within a shaped layout run for byte columns
/// `[col_start, col_end)` — used for selection/match highlight rects.
pub(crate) fn x_range_in_run(
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
