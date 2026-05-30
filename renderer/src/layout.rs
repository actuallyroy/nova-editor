// Pure geometry: computes every region rect from the window size + UI state.
// Every widget and the renderer read their rects from here, so positions have a
// single source of truth.

use crate::theme;
use crate::widgets::Rect;

pub struct Layout {
    pub title_bar: Rect,
    pub activity_bar: Rect,
    pub sidebar: Rect,
    pub tab_strip: Rect,
    pub gutter: Rect,
    pub editor_text: Rect,
    pub status_bar: Rect,
    pub find_bar: Option<Rect>,
    pub palette: Option<PaletteLayout>,
}

pub struct PaletteLayout {
    pub box_: Rect,
    pub input: Rect,
    pub list: Rect,
}

impl Layout {
    pub fn compute(
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
        // workbench.activityBar.visible — collapse to 0 width when hidden.
        let activity_w = if crate::settings::activitybar_visible() { theme::ACTIVITY_BAR_WIDTH } else { 0.0 };
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
        // editor.lineNumbers — collapse the gutter to 0 width when off.
        let gutter_w = if crate::settings::line_numbers() { theme::GUTTER_WIDTH } else { 0.0 };
        let gutter = Rect {
            x: editor_left,
            y: editor_y,
            w: gutter_w,
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
    pub fn activity_rects(&self) -> Vec<Rect> {
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
    pub fn title_btn_rects(&self) -> Vec<Rect> {
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
    pub fn tab_rects(&self, n: usize) -> Vec<Rect> {
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
    pub fn layout_btn_rects(&self) -> Vec<Rect> {
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
    pub fn menu_bar_rect(&self) -> Rect {
        Rect {
            x: 0.0,
            y: self.title_bar.y,
            w: self.title_bar.w,
            h: theme::TITLE_BAR_H,
        }
    }

    /// The centered command-center search box in the title bar.
    pub fn header_search_rect(&self) -> Rect {
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
    pub fn root_row_rect(&self) -> Rect {
        Rect {
            x: self.sidebar.x,
            y: self.sidebar.y + theme::SIDEBAR_HEADER_H,
            w: self.sidebar.w,
            h: theme::TREE_ROW_HEIGHT,
        }
    }

    /// The file-tree list region: the sidebar below the header + root row.
    pub fn tree_region(&self) -> Rect {
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
    pub fn explorer_action_rects(&self) -> Vec<Rect> {
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
    pub fn sidebar_header_rect(&self) -> Rect {
        Rect {
            x: self.sidebar.x,
            y: self.sidebar.y,
            w: self.sidebar.w,
            h: theme::SIDEBAR_HEADER_H,
        }
    }

    /// The close-button cell within a tab — a square icon-button rect pinned to
    /// the tab's right edge. Drives both the × glyph and its hit region.
    pub fn tab_close_rect(tab: Rect) -> Rect {
        let s = 20.0;
        Rect {
            x: tab.x + tab.w - s - 6.0,
            y: tab.y + (tab.h - s) * 0.5,
            w: s,
            h: s,
        }
    }
}
