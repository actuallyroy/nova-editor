// Frame rendering: builds the quad + text-area lists from App state and submits
// the wgpu passes (main pass, clipped extensions pass, context-menu + dialog
// overlays). Extracted from main.rs so the entrypoint stays thin; reads App
// fields directly (they're pub(crate)).

use anyhow::Result;
use glyphon::{Attrs, Family, Resolution, Shaping, TextArea, TextBounds};
use wgpu::{
    CommandEncoderDescriptor, LoadOp, Operations, RenderPassColorAttachment,
    RenderPassDescriptor, StoreOp, TextureViewDescriptor,
};

use crate::commands::COMMANDS;
use crate::extensions::open_ext_view;
use crate::layout::Layout;
use crate::quad::Quad;
use crate::widgets::{Rect, VAlign};
use crate::{icon, marketplace, theme};
use crate::{
    active_activity_idx, create_row_geometry, ext_filter_rect, ext_list_region, x_range_in_run,
    App, SidebarView, MENU_ACTIONS,
};

/// The full editor area (gutter + text columns) — where the document or the
/// extension detail page is drawn.
pub(crate) fn editor_region(layout: &Layout) -> Rect {
    Rect {
        x: layout.gutter.x,
        y: layout.gutter.y,
        w: layout.gutter.w + layout.editor_text.w,
        h: layout.gutter.h,
    }
}

pub(crate) fn render(app: &mut App) -> Result<()> {
    let Some(gpu) = app.gpu.as_mut() else {
        return Ok(());
    };
    let layout = Layout::compute(
        gpu.config.width as f32,
        gpu.config.height as f32,
        app.sidebar_visible,
        app.sidebar_split.size(),
        app.find.active,
        app.palette.active,
        if app.terminal_visible { Some(app.terminal_split.size()) } else { None },
    );

    // editor.wordWrap — wrap the active document to the editor width (or disable).
    {
        let wrap = if crate::settings::word_wrap() {
            Some((layout.editor_text.w - theme::EDITOR_PAD * 2.0).max(50.0))
        } else {
            None
        };
        if let Some(d) = app.workspace.active_doc_mut() {
            d.set_wrap(&mut gpu.font_system, wrap);
        }
    }

    // Keep the terminal grid sized to its panel.
    if let Some(panel) = layout.terminal_panel {
        if let Some(t) = app.terminal.as_mut() {
            let (rows, cols) = crate::terminal_grid_size(panel);
            let (dc, dr) = t.dims();
            if dc != cols || dr != rows {
                t.resize(rows, cols);
            }
        }
    }

    // ---- Update UI buffer texts (only on cache miss) ----
    {
        let fs = &mut gpu.font_system;
        let cache = &mut app.ui_cache;

        // Header command-center label — active file, or the project name.
        let header_label = match app.workspace.active_doc() {
            Some(d) => d.name.clone(),
            None => app
                .cwd
                .file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_else(|| "Search".into()),
        };
        gpu.search.set(fs, &header_label);
        // Activity-bar icons and window controls are IconButton widgets now
        // (rendered below from layout rects) — no per-glyph buffer juggling here.

        // Sidebar header — title depends on the active view.
        let header = if app.sidebar_view == SidebarView::Extensions {
            "EXTENSIONS"
        } else {
            "EXPLORER"
        };
        gpu.ui.sidebar_header.set(fs, header, theme::UI_FAMILY());

        // Extension detail page text (works for local + marketplace extensions).
        if let Some(v) = open_ext_view(app.open_extension, &app.extensions, &app.ext_remote) {
            let uv = gpu.icon_atlas.get(&v.key);
            let region = editor_region(&layout);
            gpu.ui.ext_detail.set(
                fs, region, &v.name, &v.publisher, &v.category, &v.description, &v.version,
                v.downloads, v.rating, v.supported, v.installed, uv, app.ext_readme.as_deref(),
                app.ext_changelog.as_deref(), &app.ext_features,
            );
        }
        let ws_name = app
            .cwd
            .file_name()
            .map(|n| n.to_string_lossy().to_uppercase())
            .unwrap_or_else(|| "WORKSPACE".into());
        let root_spans = [
            (
                format!("{}  ", theme::ICON_CHEVRON_DOWN),
                Attrs::new().family(Family::Name(theme::ICON_FAMILY)).color(theme::FG_TEXT()),
            ),
            (
                ws_name.clone(),
                Attrs::new().family(Family::Name(theme::UI_FAMILY())).color(theme::FG_TEXT()),
            ),
        ];
        gpu.ui
            .root_label
            .set_rich(fs, &ws_name, &root_spans, Attrs::new().family(Family::Name(theme::UI_FAMILY())));

        // Sidebar — file tree with monochrome MDL2 folder/file icons (rich text:
        // icon glyphs in the icon font, names in the UI font).
        let mut sidebar_key = String::new();
        for node in app.workspace.tree.nodes.iter() {
            sidebar_key.push_str(&node.depth.to_string());
            sidebar_key.push(if node.is_dir {
                if node.expanded {
                    'v'
                } else {
                    '>'
                }
            } else {
                '.'
            });
            sidebar_key.push_str(&node.name);
            sidebar_key.push('\n');
        }
        {
            let ui_attrs = Attrs::new()
                .family(Family::Name(theme::UI_FAMILY()))
                .color(theme::FG_TEXT());
            let folder_attrs = Attrs::new()
                .family(Family::Name(theme::ICON_FAMILY))
                .color(theme::ICON_FOLDER_COLOR());
            let mut spans: Vec<(String, Attrs)> = Vec::new();
            for node in app.workspace.tree.nodes.iter() {
                spans.push(("  ".repeat(node.depth), ui_attrs));
                if node.is_dir {
                    let g = if node.expanded {
                        theme::ICON_FOLDER_OPEN
                    } else {
                        theme::ICON_FOLDER_CLOSED
                    };
                    spans.push((format!("{}  ", g), folder_attrs));
                } else {
                    let fc = Attrs::new()
                        .family(Family::Name(theme::ICON_FAMILY))
                        .color(theme::file_icon_color(&node.name));
                    spans.push((format!("{}  ", theme::ICON_FILE), fc));
                }
                spans.push((format!("{}\n", node.name), ui_attrs));
            }
            gpu.ui.sidebar.set_rich(
                fs,
                &sidebar_key,
                &spans,
                layout.sidebar.w,
                layout.sidebar.h.max(800.0),
            );
        }

        // Tab strip — one label per document, plus the open extension page as its
        // own trailing tab ("Extension: <name>").
        let mut tab_text = String::new();
        for (i, d) in app.workspace.documents.iter().enumerate() {
            if i > 0 {
                tab_text.push('\n');
            }
            tab_text.push_str(&d.name);
            if d.dirty {
                tab_text.push_str(" •");
            }
        }
        if let Some(v) = open_ext_view(app.open_extension, &app.extensions, &app.ext_remote) {
            if !app.workspace.documents.is_empty() {
                tab_text.push('\n');
            }
            tab_text.push_str(&format!("Extension: {}", v.name));
        }
        if cache.tabs != tab_text {
            // Wide (no wrap) + tall so every tab's label line is shaped on its own
            // line; per-tab bounds clip horizontally & vertically.
            gpu.ui.tabs.set_size(fs, Some(4000.0), Some(4000.0));
            gpu.ui.tabs.set_text(
                fs,
                &tab_text,
                Attrs::new().family(Family::Name(theme::UI_FAMILY())),
                Shaping::Advanced,
            );
            gpu.ui.tabs.shape_until_scroll(fs, false);
            cache.tabs = tab_text;
        }


        // Status bar — left: path; right: position/indent/encoding/EOL/language.
        let (status_text, status_right_text) = if let Some(d) = app.workspace.active_doc() {
            let (line, col) = d.head_line_col();
            let path = d
                .path
                .as_ref()
                .map(|p| p.display().to_string())
                .unwrap_or_else(|| "Untitled".into());
            let dirty = if d.dirty { " ●" } else { "" };
            let lang: String = d
                .path
                .as_ref()
                .and_then(|p| p.extension())
                .map(|e| match e.to_string_lossy().as_ref() {
                    "rs" => "Rust".to_string(),
                    "md" => "Markdown".to_string(),
                    "toml" | "lock" => "TOML".to_string(),
                    "json" => "JSON".to_string(),
                    "wgsl" => "WGSL".to_string(),
                    other => other.to_uppercase(),
                })
                .unwrap_or_else(|| "Plain Text".to_string());
            // Reflect the live settings so the status bar updates when they change.
            let s = crate::settings::current();
            let indent = if s.editor_insert_spaces {
                format!("Spaces: {}", s.editor_tab_size)
            } else {
                format!("Tab Size: {}", s.editor_tab_size)
            };
            // EOL reflects the FILE's actual line ending, not the global setting.
            let eol = if d.eol() == "\r\n" { "CRLF" } else { "LF" };
            let autosave = if s.files_auto_save { "    Auto Save" } else { "" };
            (
                format!(" {}{}", path, dirty),
                format!(
                    "Ln {}, Col {}    {}    UTF-8    {}    {}{}    ",
                    line + 1,
                    col + 1,
                    indent,
                    eol,
                    lang,
                    autosave,
                ),
            )
        } else {
            ("Nova".to_string(), String::new())
        };
        gpu.ui.status.set(fs, &status_text, theme::UI_FAMILY());
        gpu.ui.status_right.set(fs, &status_right_text, theme::UI_FAMILY());

        // Line numbers — aligned to the active document's visual rows (wrap-aware).
        if let Some(d) = app.workspace.active.and_then(|i| app.workspace.documents.get(i)) {
            gpu.ui.line_numbers.set_from_buffer(fs, &d.buffer);
        }

        // Integrated terminal grid text (rich, monospace, per-cell colors).
        if app.terminal_visible {
            if let (Some(t), Some(panel)) = (app.terminal.as_ref(), layout.terminal_panel) {
                let to_attr = |c: [f32; 4]| {
                    Attrs::new().family(Family::Name(theme::MONO_FAMILY())).color(glyphon::Color::rgba(
                        (c[0] * 255.0) as u8,
                        (c[1] * 255.0) as u8,
                        (c[2] * 255.0) as u8,
                        255,
                    ))
                };
                let owned: Vec<(String, Attrs)> =
                    t.visual_spans().into_iter().map(|(s, c)| (s, to_attr(c))).collect();
                gpu.ui.terminal.set_size(fs, None, Some(panel.h + 200.0));
                gpu.ui.terminal.set_rich_text(
                    fs,
                    owned.iter().map(|(s, a)| (s.as_str(), *a)),
                    to_attr([0.83, 0.83, 0.83, 1.0]),
                    Shaping::Advanced,
                );
                gpu.ui.terminal.shape_until_scroll(fs, false);
            }
        }

        // Palette list (the input owns its own text now).
        if let Some(pal) = layout.palette.as_ref() {
            let mut list_text = String::new();
            for &i in app.palette.filtered.iter() {
                let (_, label, shortcut) = COMMANDS[i];
                if shortcut.is_empty() {
                    list_text.push_str(&format!(" {}\n", label));
                } else {
                    list_text.push_str(&format!(" {}   [{}]\n", label, shortcut));
                }
            }
            gpu.ui
                .palette_list
                .set_text(fs, &list_text, pal.list.w, pal.list.h);
        }

        // Context menu items.
        if app.context_menu.is_some() {
            let labels: Vec<&str> = MENU_ACTIONS.iter().map(|(_, l)| *l).collect();
            gpu.ui.menu.set_items(fs, &labels);
        }
    }

    // ---- Build quad lists ----
    let mut bg_quads: Vec<Quad> = Vec::new();
    let mut fg_quads: Vec<Quad> = Vec::new();

    // Title bar bg + window-control hover (hover rect == the button rect).
    bg_quads.push(layout.title_bar.quad(theme::TITLE_BAR_BG()));
    // Header command-center search box.
    gpu.search
        .draw_bg(layout.header_search_rect(), app.hovered_search, &mut bg_quads);
    // Menu-bar hover + layout-toggle hover.
    gpu.menubar
        .draw_bg(layout.menu_bar_rect(), app.hovered_menu, &mut bg_quads);
    if let Some(i) = app.hovered_layout {
        bg_quads.push(layout.layout_btn_rects()[i].quad(theme::TITLE_BTN_HOVER()));
    }
    if let Some(b) = app.hovered_titlebtn {
        let color = if b == 2 {
            theme::TITLE_CLOSE_HOVER()
        } else {
            theme::TITLE_BTN_HOVER()
        };
        bg_quads.push(layout.title_btn_rects()[b].quad(color));
    }

    // Activity bar bg + hover (hover rect == the button rect).
    bg_quads.push(layout.activity_bar.quad(theme::ACTIVITY_BAR_BG()));
    let act_rects = layout.activity_rects();
    if let Some(idx) = app.hovered_activity {
        bg_quads.push(act_rects[idx].quad(theme::ACTIVITY_BAR_ACTIVE()));
    }
    // Active-section accent stripe on the active view's icon.
    if let Some(ai) = active_activity_idx(app.sidebar_visible, app.sidebar_view) {
        let r = act_rects[ai];
        bg_quads.push(Quad::new(r.x, r.y, 2.0, r.h, [1.0, 1.0, 1.0, 0.85]));
    }
    // Sidebar bg
    if app.sidebar_visible {
        bg_quads.push(layout.sidebar.quad(theme::SIDEBAR_BG()));
        if app.sidebar_view == SidebarView::Explorer {
            // Explorer header action hover.
            if let Some(i) = app.hovered_explorer {
                bg_quads.push(layout.explorer_action_rects()[i].quad(theme::MENU_HOVER()));
            }
            // Inline-create row highlight (at the insert position).
            if let Some(pc) = app.creating.as_ref() {
                let (row_rect, _, _) = create_row_geometry(layout.tree_region(), pc.row, pc.depth);
                bg_quads.push(row_rect.quad(theme::TREE_SELECTED()));
            }
            // Active-file highlight: the tree row matching the open document.
            if app.creating.is_none() {
                if let Some(path) = app.workspace.active_doc().and_then(|d| d.path.clone()) {
                    if let Some(idx) = app.workspace.tree.nodes.iter().position(|n| n.path == path) {
                        bg_quads.push(
                            gpu.ui
                                .sidebar
                                .row_rect(layout.tree_region(), idx)
                                .quad(theme::TREE_ACTIVE_FILE()),
                        );
                    }
                }
            }
            // Tree row hover (below the header) — row rect from the ListView.
            if let Some(idx) = app.hovered_tree {
                bg_quads.push(
                    gpu.ui
                        .sidebar
                        .row_rect(layout.tree_region(), idx)
                        .quad(theme::TREE_HOVER()),
                );
            }
        } else {
            // Extensions view: filter box chrome (fixed at top). The scrollable rows
            // are drawn in their own clipped pass after the main pass.
            let fr = ext_filter_rect(layout.tree_region());
            let border = Rect { x: fr.x - 1.0, y: fr.y - 1.0, w: fr.w + 2.0, h: fr.h + 2.0 };
            bg_quads.push(border.quad(theme::SEARCH_BORDER()));
            bg_quads.push(fr.quad(theme::SEARCH_BG()));
        }
        // Subtle right border.
        bg_quads.push(Quad::new(
            layout.sidebar.x + layout.sidebar.w - 1.0,
            layout.sidebar.y,
            1.0,
            layout.sidebar.h,
            [0.10, 0.10, 0.10, 1.0],
        ));
    }
    // Tab strip bg
    bg_quads.push(layout.tab_strip.quad(theme::TAB_BAR_BG()));
    // Per-tab styling — geometry from the single-source tab rects. (Inline rather
    // than App::tab_count/active_tab so we don't borrow all of `app` while `gpu` is
    // mutably held; the fields here are disjoint from `app.gpu`.)
    let n_tabs = app.workspace.documents.len() + app.open_extension.is_some() as usize;
    let active_tab = if app.open_extension.is_some() {
        Some(app.workspace.documents.len())
    } else {
        app.workspace.active
    };
    let tab_rects = layout.tab_rects(n_tabs);
    for (i, tab) in tab_rects.iter().enumerate() {
        let active = active_tab == Some(i);
        let hover = app.hovered_tab == Some(i);
        let fill = if active {
            theme::TAB_ACTIVE()
        } else if hover {
            theme::TAB_HOVER()
        } else {
            theme::TAB_INACTIVE()
        };
        bg_quads.push(tab.quad(fill));
        // Top accent stripe for active tab.
        if active {
            bg_quads.push(Quad::new(tab.x, tab.y, tab.w, 2.0, [0.0, 0.475, 0.78, 1.0]));
        }
        // Subtle vertical divider between tabs.
        if i + 1 < n_tabs {
            bg_quads.push(Quad::new(
                tab.x + tab.w - 1.0,
                tab.y + 4.0,
                1.0,
                tab.h - 8.0,
                [0.30, 0.30, 0.30, 0.6],
            ));
        }
        // Close button hover background — same rect the × glyph uses.
        if app.hovered_tab_close == Some(i) {
            bg_quads.push(Layout::tab_close_rect(*tab).quad([1.0, 1.0, 1.0, 0.10]));
        }
    }
    // Bottom border of tab strip.
    bg_quads.push(Quad::new(
        layout.tab_strip.x,
        layout.tab_strip.y + layout.tab_strip.h - 1.0,
        layout.tab_strip.w,
        1.0,
        [0.10, 0.10, 0.10, 1.0],
    ));

    // Editor bg
    let editor_full = editor_region(&layout);
    bg_quads.push(editor_full.quad([
        theme::BG_EDITOR().r as f32,
        theme::BG_EDITOR().g as f32,
        theme::BG_EDITOR().b as f32,
        theme::BG_EDITOR().a as f32,
    ]));

    // Extension detail page chrome (icon tile, Install, tabs, sidebar separator).
    if app.open_extension.is_some() {
        gpu.ui.ext_detail.draw_quads(
            editor_full,
            app.hovered_page_install,
            app.hovered_detail_tab,
            &mut bg_quads,
        );
    }

    // Current-line highlight + selection. All editor quads must be clipped to
    // the editor's vertical band so scrolled-off rows don't bleed into the tab
    // strip / title bar above (text is clipped via its TextArea bounds; quads
    // have no implicit clip, so we clamp them here). Skipped while the extension
    // page occupies the editor area.
    if let Some(d) = app.open_extension.is_none().then(|| app.workspace.active_doc()).flatten() {
        let etop = layout.editor_text.y;
        let ebot = layout.editor_text.y + layout.editor_text.h;
        let clip_v = |y: f32, h: f32| -> Option<(f32, f32)> {
            let top = y.max(etop);
            let bot = (y + h).min(ebot);
            (bot > top).then_some((top, bot - top))
        };

        let (cur_line, _) = d.head_line_col();
        // Current line highlight across full editor width (editor.renderLineHighlight).
        // Wrap-aware via the document's visual bounds (covers all wrapped rows).
        if crate::settings::render_line_highlight() {
            let (ltop, lh) = d.line_visual_bounds(cur_line);
            let line_y = layout.editor_text.y + theme::EDITOR_PAD + ltop - d.scroll_y;
            if let Some((qy, qh)) = clip_v(line_y, lh) {
                bg_quads.push(Quad::new(editor_full.x, qy, editor_full.w, qh, theme::LINE_HIGHLIGHT()));
            }
        }

        // editor.rulers — vertical guide line(s) at N monospace columns.
        let rulers = crate::settings::rulers();
        if rulers > 0 {
            let char_w = theme::FONT_SIZE() * 0.6; // monospace advance approximation
            let rx = layout.editor_text.x + theme::EDITOR_PAD + rulers as f32 * char_w - d.scroll_x;
            if rx > layout.editor_text.x && rx < layout.editor_text.x + layout.editor_text.w {
                bg_quads.push(Quad::new(rx, layout.editor_text.y, 1.0, layout.editor_text.h, theme::BORDER()));
            }
        }

        // Selection quads.
        if !d.sel.is_empty() {
            let (lo, hi) = d.sel.range();
            let lo_line = d.rope.byte_to_line(lo);
            let hi_line = d.rope.byte_to_line(hi);
            let lo_col = lo - d.rope.line_to_byte(lo_line);
            let hi_col = hi - d.rope.line_to_byte(hi_line);
            for run in d.buffer.layout_runs() {
                let line = run.line_i;
                if line < lo_line || line > hi_line {
                    continue;
                }
                let (col_start, col_end) = if lo_line == hi_line {
                    (lo_col, hi_col)
                } else if line == lo_line {
                    (lo_col, usize::MAX)
                } else if line == hi_line {
                    (0, hi_col)
                } else {
                    (0, usize::MAX)
                };
                let (xs, xe) = x_range_in_run(&run, col_start, col_end);
                let w = (xe - xs).max(2.0);
                let sel_y = layout.editor_text.y + theme::EDITOR_PAD + run.line_top - d.scroll_y;
                if let Some((qy, qh)) = clip_v(sel_y, run.line_height) {
                    bg_quads.push(Quad::new(
                        layout.editor_text.x + theme::EDITOR_PAD + xs - d.scroll_x,
                        qy,
                        w,
                        qh,
                        theme::SELECTION(),
                    ));
                }
            }
        }

        // Cursor (foreground so it sits over glyphs) — gated by blink.
        if app.cursor_blink_on {
            let (cx, cy, ch) = d.caret_visual();
            let cursor_y = layout.editor_text.y + theme::EDITOR_PAD + cy - d.scroll_y;
            if let Some((qy, qh)) = clip_v(cursor_y, ch) {
                fg_quads.push(Quad::new(
                    layout.editor_text.x + theme::EDITOR_PAD + cx - d.scroll_x,
                    qy,
                    theme::CURSOR_WIDTH,
                    qh,
                    theme::CURSOR(),
                ));
            }
        }

        // Editor scrollbar thumb (over text).
        let view = layout.editor_text.h;
        let content = d.rope.len_lines() as f32 * theme::LINE_HEIGHT() + theme::EDITOR_PAD * 2.0;
        let track = Rect {
            x: layout.editor_text.x + layout.editor_text.w - theme::SCROLLBAR_WIDTH,
            y: layout.editor_text.y,
            w: theme::SCROLLBAR_WIDTH,
            h: layout.editor_text.h,
        };
        if let Some(th) = app.editor_scroll.thumb(track, content, view, d.scroll_y) {
            let color = if app.hovered_scrollbar || app.editor_scroll.is_dragging() {
                theme::SCROLLBAR_THUMB_HOVER()
            } else {
                theme::SCROLLBAR_THUMB()
            };
            fg_quads.push(th.quad(color));
        }

        // Horizontal scrollbar thumb.
        let hview = layout.editor_text.w;
        let hcontent = d.max_line_width() + theme::EDITOR_PAD * 2.0;
        if hcontent > hview {
            let htrack = Rect {
                x: layout.editor_text.x,
                y: layout.editor_text.y + layout.editor_text.h - theme::SCROLLBAR_WIDTH,
                w: layout.editor_text.w - theme::SCROLLBAR_WIDTH,
                h: theme::SCROLLBAR_WIDTH,
            };
            if let Some(th) = app.editor_hscroll.thumb(htrack, hcontent, hview, d.scroll_x) {
                let color = if app.editor_hscroll.is_dragging() {
                    theme::SCROLLBAR_THUMB_HOVER()
                } else {
                    theme::SCROLLBAR_THUMB()
                };
                fg_quads.push(th.quad(color));
            }
        }
    }

    // Terminal panel — background, top divider, and the block cursor (when focused).
    if let Some(panel) = layout.terminal_panel {
        bg_quads.push(panel.quad(theme::PANEL_BG()));
        // Low-contrast divider on the editor/panel seam.
        bg_quads.push(Quad::new(panel.x, panel.y, panel.w, 1.0, theme::PANEL_BORDER()));
        if app.terminal_focused {
            if let Some(t) = app.terminal.as_ref() {
                let (cc, cr) = t.cursor();
                let char_w = theme::FONT_SIZE() * 0.6;
                let cx = panel.x + 8.0 + cc as f32 * char_w;
                let cy = panel.y + 4.0 + cr as f32 * theme::LINE_HEIGHT();
                if cy + theme::LINE_HEIGHT() <= panel.y + panel.h {
                    bg_quads.push(Quad::new(cx, cy, char_w.max(2.0), theme::LINE_HEIGHT(), [0.6, 0.6, 0.6, 0.6]));
                }
            }
        }
    }

    // Status bar
    bg_quads.push(layout.status_bar.quad(theme::STATUS_BAR_BG()));

    // Find bar
    if let Some(fb) = layout.find_bar.as_ref() {
        bg_quads.push(fb.quad(theme::TAB_BAR_BG()));
        bg_quads.push(Quad::new(
            fb.x,
            fb.y + fb.h - 1.0,
            fb.w,
            1.0,
            theme::BORDER(),
        ));
    }

    // Palette dim overlay + box
    if let Some(pal) = layout.palette.as_ref() {
        bg_quads.push(Quad::new(
            0.0,
            0.0,
            gpu.config.width as f32,
            gpu.config.height as f32,
            [0.0, 0.0, 0.0, 0.6],
        ));
        bg_quads.push(pal.box_.quad(theme::PALETTE_BG()));
        bg_quads.push(Quad::new(
            pal.box_.x - 1.0,
            pal.box_.y - 1.0,
            pal.box_.w + 2.0,
            pal.box_.h + 2.0,
            theme::PALETTE_BORDER(),
        ));
        bg_quads.push(pal.input.quad(theme::PALETTE_INPUT_BG()));
        // Selected row highlight — row rect from the ListView.
        if !app.palette.filtered.is_empty() {
            bg_quads.push(
                gpu.ui
                    .palette_list
                    .row_rect(pal.list, app.palette.selected)
                    .quad(theme::PALETTE_SELECTED()),
            );
        }
    }

    // Text-input carets (blink-gated, drawn on top via fg_quads).
    if app.cursor_blink_on {
        if let Some(pal) = layout.palette.as_ref() {
            fg_quads.push(gpu.ui.palette_input.caret_quad(pal.input, 6.0));
        } else if let Some(fb) = layout.find_bar.as_ref() {
            fg_quads.push(gpu.ui.find_input.caret_quad(*fb, 8.0));
        }
        if let Some(pc) = app.creating.as_ref() {
            let (_, _, field) = create_row_geometry(layout.tree_region(), pc.row, pc.depth);
            fg_quads.push(gpu.create_input.caret_quad(field, 0.0));
        }
        if app.ext_filter_active && app.sidebar_visible && app.sidebar_view == SidebarView::Extensions {
            let fr = ext_filter_rect(layout.tree_region());
            fg_quads.push(gpu.ui.ext_filter.caret_quad(fr, 6.0));
        }
    }

    // Text-input selection highlights — drawn into bg_quads (under glyphs, over
    // the input box). Not blink-gated.
    if let Some(pal) = layout.palette.as_ref() {
        gpu.ui.palette_input.selection_quads(pal.input, 6.0, &mut bg_quads);
    }
    if let Some(fb) = layout.find_bar.as_ref() {
        gpu.ui.find_input.selection_quads(*fb, 8.0, &mut bg_quads);
    }
    if app.ext_filter_active && app.sidebar_visible && app.sidebar_view == SidebarView::Extensions {
        let fr = ext_filter_rect(layout.tree_region());
        gpu.ui.ext_filter.selection_quads(fr, 6.0, &mut bg_quads);
    }

    // ---- Build text areas ----
    let active_idx = app.workspace.active;

    let (cfg_w, cfg_h) = (gpu.config.width, gpu.config.height);
    gpu.quad_renderer
        .prepare(&gpu.device, &gpu.queue, &bg_quads, &fg_quads, (cfg_w, cfg_h));
    // The detail-page header icon is drawn via the atlas in the main pass.
    let mut detail_icons: Vec<icon::IconInstance> = Vec::new();
    if app.open_extension.is_some() {
        if let Some(inst) = gpu.ui.ext_detail.icon_instance(editor_full) {
            detail_icons.push(inst);
        }
    }
    gpu.icon_atlas
        .prepare(&gpu.device, &gpu.queue, &detail_icons, (cfg_w, cfg_h));
    gpu.viewport.update(
        &gpu.queue,
        Resolution {
            width: cfg_w,
            height: cfg_h,
        },
    );

    let ui = &gpu.ui;
    let mut areas: Vec<TextArea> = Vec::new();
    // README image draw rects (collected during the detail-page text draw, drawn in
    // a clipped media pass after the main pass).
    let mut detail_img_rects: Vec<(String, Rect)> = Vec::new();

    // When the palette (modal) is open, suppress all underlying text so it can't
    // bleed through — text renders in one pass after the bg quads, so the dim
    // overlay alone can't occlude it. Only the palette text is drawn below.
    if layout.palette.is_none() {
    // Title bar: menu bar (left) + centered search box + layout toggles and
    // window controls (right).
    gpu.menubar.draw(layout.menu_bar_rect(), &mut areas);
    gpu.search.draw(layout.header_search_rect(), &mut areas);
    let layout_rects = layout.layout_btn_rects();
    for (i, btn) in gpu.layout_btns.iter().enumerate() {
        btn.draw(layout_rects[i], theme::TITLE_FG(), &mut areas);
    }
    // Window controls — IconButton widgets at their layout rects (the same
    // rects the hover bg used above; glyph is centered in each).
    let tb_rects = layout.title_btn_rects();
    for (b, btn) in gpu.titlebar_btns.iter().enumerate() {
        let color = if app.hovered_titlebtn == Some(b) {
            theme::FG_ACTIVE()
        } else {
            theme::TITLE_FG()
        };
        btn.draw(tb_rects[b], color, &mut areas);
    }

    // Activity-bar icons — IconButton widgets at their cell rects.
    let act_rects = layout.activity_rects();
    let active_act = active_activity_idx(app.sidebar_visible, app.sidebar_view);
    for (i, btn) in gpu.activity_btns.iter().enumerate() {
        let color = if Some(i) == active_act {
            theme::ACTIVITY_ICON_ACTIVE()
        } else {
            theme::ACTIVITY_ICON_FG()
        };
        btn.draw(act_rects[i], color, &mut areas);
    }

    // Sidebar header + (Explorer tree | Extensions list)
    if app.sidebar_visible {
        ui.sidebar_header
            .push(layout.sidebar.x + 12.0, layout.sidebar_header_rect(), theme::FG_DIM(), &mut areas);
        let tr = layout.tree_region();
        if app.sidebar_view == SidebarView::Explorer {
            let er = layout.explorer_action_rects();
            for (i, btn) in gpu.explorer_btns.iter().enumerate() {
                btn.draw(er[i], theme::TITLE_FG(), &mut areas);
            }
            // Root folder row (chevron + workspace name).
            ui.root_label
                .draw_left(layout.root_row_rect(), 10.0, theme::FG_TEXT(), &mut areas);
            if let Some(pc) = app.creating.as_ref() {
                let rowh = theme::TREE_ROW_HEIGHT;
                let (_, icon_rect, field) = create_row_geometry(tr, pc.row, pc.depth);
                if pc.row > 0 {
                    let clip_a = Rect { x: tr.x, y: tr.y, w: tr.w, h: pc.row as f32 * rowh };
                    ui.sidebar.draw_at(clip_a, tr.y, theme::FG_TEXT(), &mut areas);
                }
                gpu.create_icons[pc.is_dir as usize].draw(icon_rect, theme::ICON_FILE_COLOR(), &mut areas);
                gpu.create_input.draw(field, 0.0, theme::FG_TEXT(), &mut areas);
                let below_y = tr.y + (pc.row as f32 + 1.0) * rowh;
                let clip_b = Rect {
                    x: tr.x,
                    y: below_y,
                    w: tr.w,
                    h: (tr.y + tr.h - below_y).max(0.0),
                };
                ui.sidebar.draw_at(clip_b, tr.y + rowh, theme::FG_TEXT(), &mut areas);
            } else {
                ui.sidebar.draw(tr, theme::FG_TEXT(), &mut areas);
            }
        } else {
            // Extensions filter box text (fixed). The scrollable row text is drawn
            // in the dedicated clipped pass after the main pass.
            let fr = ext_filter_rect(tr);
            let fc = if ui.ext_filter.text().is_empty() { theme::FG_DIM() } else { theme::FG_TEXT() };
            ui.ext_filter.draw(fr, 6.0, fc, &mut areas);
        }
    }

    // Tab labels — the shared `tabs` buffer holds one label per line; we render
    // it once per tab, shifted up by one line and clipped to that tab's column,
    // so each tab shows only its own label. Geometry comes from `tab_rects`.
    let tab_rects = layout.tab_rects(n_tabs);
    for (i, tab) in tab_rects.iter().enumerate() {
        let active = active_tab == Some(i);
        let line_top = i as f32 * theme::UI_LINE_HEIGHT;
        let color = if active {
            theme::TAB_FG_ACTIVE()
        } else {
            theme::TAB_FG_INACTIVE()
        };
        let label_top = tab.text_top(theme::UI_LINE_HEIGHT, VAlign::Center);
        areas.push(TextArea {
            buffer: &ui.tabs,
            left: tab.x + 12.0,
            top: label_top - line_top,
            scale: 1.0,
            // Clip to just this label's line band (the buffer holds every tab's
            // label, one per line) so neighbours don't bleed in.
            bounds: TextBounds {
                left: tab.x as i32 + 6,
                top: (label_top - 2.0) as i32,
                right: (tab.x + tab.w - 26.0) as i32,
                bottom: (label_top + theme::UI_LINE_HEIGHT) as i32,
            },
            default_color: color,
            custom_glyphs: &[],
        });

        // Close × — reusable IconButton at the close-button rect (same rect as
        // its hover bg + hit region). Hidden unless the tab is active/hovered.
        let close_color = if app.hovered_tab_close == Some(i) {
            theme::CLOSE_FG_HOVER()
        } else if active || app.hovered_tab == Some(i) {
            theme::CLOSE_FG()
        } else {
            glyphon::Color::rgba(0xD4, 0xD4, 0xD4, 0)
        };
        gpu.tab_close_btn
            .draw(Layout::tab_close_rect(*tab), close_color, &mut areas);
    }

    // Editor area: either the extension detail page or the document.
    if app.open_extension.is_some() {
        let size_of = |k: &str| gpu.media.size(k);
        ui.ext_detail.draw_text(
            editor_region(&layout),
            app.ext_detail_scroll,
            &size_of,
            &mut areas,
            &mut detail_img_rects,
        );
    } else if let Some(i) = active_idx {
        let d = &app.workspace.documents[i];

        // Line numbers — clipped to the gutter region so they never bleed over
        // the tab strip when scrolled.
        ui.line_numbers
            .draw(layout.gutter, d.scroll_y, theme::FG_GUTTER(), &mut areas);

        // Document text
        areas.push(TextArea {
            buffer: &d.buffer,
            left: layout.editor_text.x + theme::EDITOR_PAD - d.scroll_x,
            top: layout.editor_text.y + theme::EDITOR_PAD - d.scroll_y,
            scale: 1.0,
            bounds: TextBounds {
                left: layout.editor_text.x as i32,
                top: layout.editor_text.y as i32,
                right: (layout.editor_text.x + layout.editor_text.w) as i32,
                bottom: (layout.editor_text.y + layout.editor_text.h) as i32,
            },
            default_color: theme::FG_TEXT(),
            custom_glyphs: &[],
        });
    }

    // Status bar — left: path; right: position/encoding/etc. Both via the
    // reusable TextLabel (left-padded and right-padded alignment helpers).
    ui.status
        .draw_left(layout.status_bar, 12.0, theme::STATUS_BAR_FG(), &mut areas);
    ui.status_right
        .draw_right(layout.status_bar, 8.0, theme::STATUS_BAR_FG(), &mut areas);

    // Find bar
    if let Some(fb) = layout.find_bar.as_ref() {
        ui.find_input.draw(*fb, 8.0, theme::FG_TEXT(), &mut areas);
    }

    // Terminal grid text, clipped to its panel.
    if app.terminal_visible {
        if let Some(panel) = layout.terminal_panel {
            areas.push(TextArea {
                buffer: &ui.terminal,
                left: panel.x + 8.0,
                top: panel.y + 4.0,
                scale: 1.0,
                bounds: TextBounds {
                    left: panel.x as i32,
                    top: panel.y as i32,
                    right: (panel.x + panel.w) as i32,
                    bottom: (panel.y + panel.h) as i32,
                },
                default_color: theme::FG_TEXT(),
                custom_glyphs: &[],
            });
        }
    }
    } // end: palette closed

    // Palette text
    if let Some(pal) = layout.palette.as_ref() {
        ui.palette_input
            .draw(pal.input, 6.0, theme::FG_TEXT(), &mut areas);
        ui.palette_list
            .draw(pal.list, theme::FG_TEXT(), &mut areas);
    }

    gpu.text_renderer.prepare(
        &gpu.device,
        &gpu.queue,
        &mut gpu.font_system,
        &mut gpu.atlas,
        &gpu.viewport,
        areas,
        &mut gpu.swash_cache,
    )?;

    // ---- Submit ----
    let frame = gpu.surface.get_current_texture()?;
    let view = frame.texture.create_view(&TextureViewDescriptor::default());
    let mut encoder = gpu.device.create_command_encoder(&CommandEncoderDescriptor {
        label: Some("nova-encoder"),
    });
    {
        let mut pass = encoder.begin_render_pass(&RenderPassDescriptor {
            label: Some("nova-pass"),
            color_attachments: &[Some(RenderPassColorAttachment {
                view: &view,
                resolve_target: None,
                ops: Operations {
                    load: LoadOp::Clear(theme::BG_EDITOR()),
                    store: StoreOp::Store,
                },
            })],
            depth_stencil_attachment: None,
            timestamp_writes: None,
            occlusion_query_set: None,
        });
        gpu.quad_renderer.render_bg(&mut pass);
        gpu.icon_atlas.render(&mut pass); // detail-page header icon
        gpu.text_renderer
            .render(&gpu.atlas, &gpu.viewport, &mut pass)?;
        gpu.quad_renderer.render_fg(&mut pass);
    }
    gpu.queue.submit(Some(encoder.finish()));

    // ---- README images: request any referenced by the active tab, then draw ----
    if app.open_extension.is_some() {
        // Drive fetching from ALL image URLs in the active body (not just loaded
        // ones) — otherwise nothing would ever load (the draw list only holds
        // already-loaded images). Clone to drop the gpu.ui borrow before fetching.
        use marketplace::ImgSource;
        let urls: Vec<String> = gpu.ui.ext_detail.image_urls().to_vec();
        for key in &urls {
            if gpu.media.has(key) || app.requested_images.contains(key) {
                continue;
            }
            app.requested_images.insert(key.clone());
            // Resolve to a source; fetch+decode happens off-thread either way.
            let src = if key.starts_with("http://") || key.starts_with("https://") {
                Some(ImgSource::Http(key.clone()))
            } else if let Some(dir) = &app.ext_img_dir {
                Some(ImgSource::File(dir.join(key.trim_start_matches("./"))))
            } else if let Some(base) = &app.ext_img_base {
                marketplace::join_url(base, key).map(ImgSource::Http)
            } else {
                None
            };
            if let Some(src) = src {
                marketplace::image_async(app.worker_tx.clone(), key.clone(), src);
            }
        }
    }

    // Draw loaded images + link underlines in a clipped pass over the README body.
    if app.open_extension.is_some() {
        let region = editor_region(&layout);
        let clip = crate::ext_detail::ExtensionDetail::body_viewport(region);
        let scroll = app.ext_detail_scroll;
        // Link underlines (thin lines under each link fragment, in the link color).
        let lc = theme::FG_ACTIVE();
        let ul_color = [lc.r() as f32 / 255.0, lc.g() as f32 / 255.0, lc.b() as f32 / 255.0, 1.0];
        let mut underlines: Vec<Quad> = Vec::new();
        for (r, _url) in gpu.ui.ext_detail.link_rects(region, scroll, &|k| gpu.media.size(k)) {
            underlines.push(Quad::new(r.x, r.y + r.h - 2.0, r.w, 1.0, ul_color));
        }
        if !detail_img_rects.is_empty() || !underlines.is_empty() {
            let now_ms = app.anim_start.elapsed().as_millis() as u64;
            gpu.media.prepare(&gpu.device, &gpu.queue, &detail_img_rects, (cfg_w, cfg_h), now_ms);
            gpu.quad_renderer.prepare(&gpu.device, &gpu.queue, &underlines, &[], (cfg_w, cfg_h));
            let mut enc = gpu.device.create_command_encoder(&CommandEncoderDescriptor {
                label: Some("nova-media-pass"),
            });
            {
                let mut pass = enc.begin_render_pass(&RenderPassDescriptor {
                    label: Some("nova-media"),
                    color_attachments: &[Some(RenderPassColorAttachment {
                        view: &view,
                        resolve_target: None,
                        ops: Operations { load: LoadOp::Load, store: StoreOp::Store },
                    })],
                    depth_stencil_attachment: None,
                    timestamp_writes: None,
                    occlusion_query_set: None,
                });
                let sx = clip.x.max(0.0) as u32;
                let sy = clip.y.max(0.0) as u32;
                let sw = clip.w.min(cfg_w as f32 - clip.x).max(0.0) as u32;
                let sh = clip.h.min(cfg_h as f32 - clip.y).max(0.0) as u32;
                if sw > 0 && sh > 0 {
                    pass.set_scissor_rect(sx, sy, sw, sh);
                    gpu.media.render(&mut pass);
                    gpu.quad_renderer.render_bg(&mut pass);
                }
            }
            gpu.queue.submit(Some(enc.finish()));
        }
    }

    // ---- Extensions list: clipped, scrollable pass over the sidebar ----
    if app.sidebar_visible
        && layout.palette.is_none()
        && app.sidebar_view == SidebarView::Extensions
    {
        let region = ext_list_region(layout.tree_region());
        let scroll = app.ext_scroll;
        let mut eq: Vec<Quad> = Vec::new();
        gpu.ui.ext_rows.draw_quads(region, scroll, app.hovered_ext, &mut eq);
        let mut einst: Vec<icon::IconInstance> = Vec::new();
        gpu.ui.ext_rows.icon_instances(region, scroll, &mut einst);
        gpu.quad_renderer.prepare(&gpu.device, &gpu.queue, &eq, &[], (cfg_w, cfg_h));
        gpu.icon_atlas.prepare(&gpu.device, &gpu.queue, &einst, (cfg_w, cfg_h));
        let mut eareas: Vec<TextArea> = Vec::new();
        gpu.ui.ext_rows.draw_text(region, scroll, &mut eareas);
        gpu.text_renderer.prepare(
            &gpu.device,
            &gpu.queue,
            &mut gpu.font_system,
            &mut gpu.atlas,
            &gpu.viewport,
            eareas,
            &mut gpu.swash_cache,
        )?;
        let mut enc = gpu.device.create_command_encoder(&CommandEncoderDescriptor {
            label: Some("nova-ext-pass"),
        });
        {
            let mut pass = enc.begin_render_pass(&RenderPassDescriptor {
                label: Some("nova-ext"),
                color_attachments: &[Some(RenderPassColorAttachment {
                    view: &view,
                    resolve_target: None,
                    ops: Operations { load: LoadOp::Load, store: StoreOp::Store },
                })],
                depth_stencil_attachment: None,
                timestamp_writes: None,
                occlusion_query_set: None,
            });
            // Clip to the list region so scrolled rows can't bleed over the filter
            // box above or the status bar below.
            let sx = region.x.max(0.0) as u32;
            let sy = region.y.max(0.0) as u32;
            let sw = (region.w.min(cfg_w as f32 - region.x)).max(0.0) as u32;
            let sh = (region.h.min(cfg_h as f32 - region.y)).max(0.0) as u32;
            if sw > 0 && sh > 0 {
                pass.set_scissor_rect(sx, sy, sw, sh);
                gpu.quad_renderer.render_bg(&mut pass);
                gpu.icon_atlas.render(&mut pass);
                gpu.text_renderer.render(&gpu.atlas, &gpu.viewport, &mut pass)?;
            }
        }
        gpu.queue.submit(Some(enc.finish()));
    }

    // ---- Context menu overlay (second pass, drawn over everything) ----
    if let Some(cm) = app.context_menu.as_ref() {
        let menu = gpu.ui.menu.rect(cm.anchor, (cfg_w as f32, cfg_h as f32));
        let mut mq: Vec<Quad> = Vec::new();
        gpu.ui.menu.draw_bg(menu, app.hovered_menu_item, &mut mq);
        gpu.quad_renderer
            .prepare(&gpu.device, &gpu.queue, &mq, &[], (cfg_w, cfg_h));
        let mut mareas: Vec<TextArea> = Vec::new();
        gpu.ui.menu.draw(menu, &mut mareas);
        gpu.text_renderer.prepare(
            &gpu.device,
            &gpu.queue,
            &mut gpu.font_system,
            &mut gpu.atlas,
            &gpu.viewport,
            mareas,
            &mut gpu.swash_cache,
        )?;
        let mut enc2 = gpu.device.create_command_encoder(&CommandEncoderDescriptor {
            label: Some("nova-menu-pass"),
        });
        {
            let mut pass = enc2.begin_render_pass(&RenderPassDescriptor {
                label: Some("nova-menu"),
                color_attachments: &[Some(RenderPassColorAttachment {
                    view: &view,
                    resolve_target: None,
                    ops: Operations {
                        load: LoadOp::Load,
                        store: StoreOp::Store,
                    },
                })],
                depth_stencil_attachment: None,
                timestamp_writes: None,
                occlusion_query_set: None,
            });
            gpu.quad_renderer.render_bg(&mut pass);
            gpu.text_renderer
                .render(&gpu.atlas, &gpu.viewport, &mut pass)?;
        }
        gpu.queue.submit(Some(enc2.finish()));
    }

    // ---- Modal dialog overlay (third pass) ----
    if let Some(ds) = app.dialog.as_ref() {
        let win = (cfg_w as f32, cfg_h as f32);
        let box_ = gpu.ui.dialog.box_rect(win, ds.has_check);
        let mut dq: Vec<Quad> = Vec::new();
        gpu.ui
            .dialog
            .draw_bg(box_, win, ds.hovered, ds.checked, ds.has_check, &mut dq);
        gpu.quad_renderer
            .prepare(&gpu.device, &gpu.queue, &dq, &[], (cfg_w, cfg_h));
        let mut dareas: Vec<TextArea> = Vec::new();
        gpu.ui.dialog.draw(box_, ds.has_check, &mut dareas);
        gpu.text_renderer.prepare(
            &gpu.device,
            &gpu.queue,
            &mut gpu.font_system,
            &mut gpu.atlas,
            &gpu.viewport,
            dareas,
            &mut gpu.swash_cache,
        )?;
        let mut enc3 = gpu.device.create_command_encoder(&CommandEncoderDescriptor {
            label: Some("nova-dialog-pass"),
        });
        {
            let mut pass = enc3.begin_render_pass(&RenderPassDescriptor {
                label: Some("nova-dialog"),
                color_attachments: &[Some(RenderPassColorAttachment {
                    view: &view,
                    resolve_target: None,
                    ops: Operations {
                        load: LoadOp::Load,
                        store: StoreOp::Store,
                    },
                })],
                depth_stencil_attachment: None,
                timestamp_writes: None,
                occlusion_query_set: None,
            });
            gpu.quad_renderer.render_bg(&mut pass);
            gpu.text_renderer
                .render(&gpu.atlas, &gpu.viewport, &mut pass)?;
        }
        gpu.queue.submit(Some(enc3.finish()));
    }

    frame.present();
    gpu.atlas.trim();
    Ok(())
}
