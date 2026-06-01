// Frame rendering: builds the quad + text-area lists from App state and submits
// the wgpu passes (main pass, clipped extensions pass, context-menu + dialog
// overlays). Extracted from main.rs so the entrypoint stays thin; reads App
// fields directly (they're pub(crate)).

use std::time::Instant;

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
    active_activity_idx, create_row_geometry, ext_list_region, x_range_in_run,
    App, SidebarView, MENU_ACTIONS,
};

/// Left-to-right rects for the panel-header tab labels, sized to each label's
/// measured width. Shared by the underline (quad phase) and the text (areas phase).
fn panel_tab_rects(header: Rect, tabs: &[crate::widgets::TextLabel]) -> Vec<Rect> {
    let pad_l = theme::zpx(12.0);
    let gap = theme::zpx(18.0);
    let mut x = header.x + pad_l;
    let mut out = Vec::with_capacity(tabs.len());
    for t in tabs {
        let w = t.width();
        out.push(Rect { x, y: header.y, w, h: header.h });
        x += w + gap;
    }
    out
}

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

/// Scale that fits a `iw`x`ih` image inside `region` (16px padding), never upscaling.
pub(crate) fn image_fit_scale(iw: f32, ih: f32, region: Rect) -> f32 {
    let pad = 16.0;
    let aw = (region.w - pad * 2.0).max(1.0);
    let ah = (region.h - pad * 2.0).max(1.0);
    (aw / iw).min(ah / ih).min(1.0)
}

/// Displayed image rect for the given `scale` + `pan`, centred in `region`.
pub(crate) fn image_rect(iw: f32, ih: f32, region: Rect, scale: f32, pan: (f32, f32)) -> Rect {
    let (w, h) = (iw * scale, ih * scale);
    let cx = region.x + region.w * 0.5;
    let cy = region.y + region.h * 0.5;
    Rect { x: cx - w * 0.5 + pan.0, y: cy - h * 0.5 + pan.1, w, h }
}

/// Status-bar window-zoom control cells [minus, percent, plus], at the right edge.
pub(crate) fn zoom_ctrl_cells(status: Rect) -> [Rect; 3] {
    let z = theme::ui_zoom();
    let h = status.h;
    let (cw, pw) = (22.0 * z, 52.0 * z);
    let right = status.x + status.w - 8.0 * z;
    let plus = Rect { x: right - cw, y: status.y, w: cw, h };
    let pct = Rect { x: plus.x - pw, y: status.y, w: pw, h };
    let minus = Rect { x: pct.x - cw, y: status.y, w: cw, h };
    [minus, pct, plus]
}

/// Zoom-control overlay cells [zoom_out, percent, zoom_in, fit], a pill at the
/// bottom-centre of the image region.
pub(crate) fn image_ctrl_cells(region: Rect) -> [Rect; 4] {
    let z = theme::ui_zoom();
    let h = 30.0 * z;
    let widths = [38.0 * z, 64.0 * z, 38.0 * z, 50.0 * z];
    let total: f32 = widths.iter().sum();
    let x0 = region.x + (region.w - total) * 0.5;
    let y = region.y + region.h - h - 16.0 * z;
    let mut x = x0;
    let mut out = [Rect { x: 0.0, y: 0.0, w: 0.0, h: 0.0 }; 4];
    for (i, w) in widths.iter().enumerate() {
        out[i] = Rect { x, y, w: *w, h };
        x += *w;
    }
    out
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
        // Inlined (not the App method) so these stay disjoint-field reads while
        // `gpu` holds a mutable borrow of app.gpu.
        if app.terminal.visible {
            Some(if app.terminal.maximized { 100_000.0 } else { app.terminal.split.size() })
        } else {
            None
        },
        app.workspace.active_doc().map_or(false, |d| d.diff.is_some()),
    );

    // editor.wordWrap — wrap the active document to the editor width (or disable).
    {
        let wrap = if crate::settings::word_wrap() {
            Some((layout.editor_text.w - theme::EDITOR_PAD() * 2.0).max(50.0))
        } else {
            None
        };
        if let Some(d) = app.workspace.active_doc_mut() {
            d.set_wrap(&mut gpu.font_system, wrap);
        }
    }

    let now = Instant::now();

    // Size the active tab's split panes' grids + scroll viewports to their columns.
    if let Some(panel) = layout.terminal_panel {
        let area = crate::terminal_pane_area(crate::terminal_content(panel), app.terminal.groups.len());
        let cell_w = app.terminal_cell_w;
        if let Some(g) = app.terminal.groups.get_mut(app.terminal.active) {
            let rects = crate::terminal_pane_rects(area, g.panes.len());
            for (i, pane) in g.panes.iter_mut().enumerate() {
                let rect = rects[i];
                let (rows, cols) = crate::terminal_grid_size(rect, cell_w);
                let (dc, dr) = pane.term.dims();
                if dc != cols || dr != rows {
                    pane.term.resize(rows, cols);
                    pane.dirty = true;
                }
                let content_h = pane.term.total_lines() as f32 * theme::LINE_HEIGHT();
                pane.scroll.set_metrics(rect, (rect.w, content_h));
            }
        }
    }

    // Size the scroll viewports for the extensions list + README detail page so
    // their offsets are clamped and their thumbs are positioned this frame.
    if app.sidebar_visible && app.sidebar_view == SidebarView::Extensions {
        if let Some(ep) = app.extensions_panel.as_mut() {
            ep.update(layout.tree_region());
        }
    }
    // File-tree scroll viewport (content = one row per tree node).
    if app.sidebar_visible && app.sidebar_view == SidebarView::Explorer {
        let tr = layout.tree_region();
        let content_h = app.workspace.tree.nodes.len() as f32 * theme::TREE_ROW_HEIGHT();
        app.explorer.scroll.set_metrics(tr, (tr.w, content_h));
    }
    // Keep the terminal panel's resize bounds tied to the window height (and zoom),
    // so it can grow to most of the window instead of a fixed zoom-1 cap.
    {
        let max_h = (gpu.config.height as f32 - theme::zpx(200.0)).max(theme::TERMINAL_MIN_HEIGHT());
        app.terminal.split.set_bounds(theme::TERMINAL_MIN_HEIGHT(), max_h);
    }
    if app.sidebar_visible && app.sidebar_view == SidebarView::SourceControl {
        if let Some(scp) = app.source_control.as_mut() {
            scp.update(&mut gpu.font_system, layout.tree_region());
        }
    }
    if app.detail.open_extension.is_some() {
        let vp = crate::ext_detail::ExtensionDetail::body_viewport(editor_region(&layout));
        let content_h = gpu.ui.ext_detail.body_content_height(&|k| gpu.media.size(k));
        app.detail.ext_detail_scroll.set_metrics(vp, (vp.w, content_h));
    } else if let Some(d) = app.workspace.active_doc_mut() {
        // Editor: size the document's scroll viewport (offset clamps here, thumbs
        // position from these metrics). Content height uses logical lines + padding.
        let content_h = d.rope.len_lines() as f32 * theme::LINE_HEIGHT() + theme::EDITOR_PAD() * 2.0;
        let content_w = d.max_line_width() + theme::EDITOR_PAD() * 2.0;
        d.scroll.set_metrics(layout.editor_text, (content_w, content_h));
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
        let header = match app.sidebar_view {
            SidebarView::Extensions => "EXTENSIONS",
            SidebarView::Search => "SEARCH",
            SidebarView::SourceControl => "SOURCE CONTROL",
            SidebarView::Explorer => "EXPLORER",
        };
        gpu.ui.sidebar_header.set(fs, header, theme::UI_FAMILY());

        // Extension detail page text (works for local + marketplace extensions).
        if let Some(v) = open_ext_view(app.detail.open_extension, &app.extensions, &app.ext_remote) {
            let uv = gpu.icon_atlas.get(&v.key);
            let region = editor_region(&layout);
            gpu.ui.ext_detail.set(
                fs, region, &v.name, &v.publisher, &v.category, &v.description, &v.version,
                v.downloads, v.rating, v.supported, v.installed, uv, app.detail.ext_readme.as_deref(),
                app.detail.ext_changelog.as_deref(), &app.detail.ext_features,
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
            // Lay the buffer out tall enough for every row (not just the visible
            // viewport) so scrolling past the first screenful isn't clipped.
            let full_h = app.workspace.tree.nodes.len() as f32 * theme::TREE_ROW_HEIGHT() + 200.0;
            gpu.ui.sidebar.set_rich(
                fs,
                &sidebar_key,
                &spans,
                layout.sidebar.w,
                full_h.max(layout.sidebar.h),
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
        if let Some(v) = open_ext_view(app.detail.open_extension, &app.extensions, &app.ext_remote) {
            if !app.workspace.documents.is_empty() {
                tab_text.push('\n');
            }
            tab_text.push_str(&format!("Extension: {}", v.name));
        }
        if cache.tabs != tab_text {
            // Wide (no wrap) + tall so every tab's label line is shaped on its own
            // line; per-tab bounds clip horizontally & vertically.
            gpu.ui.tabs.set_metrics(fs, glyphon::Metrics::new(theme::UI_FONT_SIZE(), theme::UI_LINE_HEIGHT()));
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
        gpu.ui.zoom_pct.set(fs, &format!("{}%", (theme::ui_zoom() * 100.0).round() as i32), theme::UI_FAMILY());

        // Source Control change-count badge text (capped at 99+).
        let scm_count = app.source_control.as_ref().map_or(0, |s| s.change_count());
        if scm_count > 0 {
            let s = if scm_count > 99 { "99+".to_string() } else { scm_count.to_string() };
            gpu.ui.scm_badge.set(fs, &s, theme::UI_FAMILY());
        }

        // Image zoom percentage label (for the zoom-control overlay).
        if let Some((key, scale_opt)) = app
            .workspace
            .active
            .and_then(|i| app.workspace.documents.get(i))
            .and_then(|d| d.image.as_ref().map(|k| (k.clone(), d.image_scale)))
        {
            if let Some((iw, ih)) = gpu.media.size(&key) {
                let region = editor_region(&layout);
                let eff = scale_opt.unwrap_or_else(|| image_fit_scale(iw, ih, region));
                gpu.ui.img_pct.set(fs, &format!("{}%", (eff * 100.0).round() as i32), theme::UI_FAMILY());
            }
        }

        // Line numbers — aligned to the active document's visual rows (wrap-aware).
        if let Some(d) = app.workspace.active.and_then(|i| app.workspace.documents.get(i)) {
            match d.diff.as_ref() {
                Some(diff) => {
                    gpu.ui.line_numbers.set_from_diff_side(fs, &diff.rows, true);
                    gpu.ui.line_numbers2.set_from_diff_side(fs, &diff.rows, false);
                }
                None => gpu.ui.line_numbers.set_from_buffer(fs, &d.buffer),
            }
        }

        // Integrated terminal: one buffer per split pane, reshaped only when that
        // pane's grid changed (pane.dirty). Advanced shaping keeps the monospace grid
        // (Basic mis-advances glyphs and drops box-drawing/powerline fallback).
        if app.terminal.visible {
            if let Some(panel) = layout.terminal_panel {
                let area = crate::terminal_pane_area(crate::terminal_content(panel), app.terminal.groups.len());
                let n = app.terminal.groups.get(app.terminal.active).map_or(0, |g| g.panes.len());
                while gpu.ui.terminal_panes.len() < n {
                    let b = crate::widgets::make_ui_buffer_mono(fs, 4000.0, 4000.0);
                    gpu.ui.terminal_panes.push(b);
                }
                let to_attr = |c: [f32; 4]| {
                    Attrs::new().family(Family::Name(theme::MONO_FAMILY())).color(glyphon::Color::rgba(
                        (c[0] * 255.0) as u8,
                        (c[1] * 255.0) as u8,
                        (c[2] * 255.0) as u8,
                        255,
                    ))
                };
                let rects = crate::terminal_pane_rects(area, n);
                let panes = app.terminal.groups.get_mut(app.terminal.active).map(|g| &mut g.panes);
                for (i, pane) in panes.into_iter().flatten().enumerate() {
                    if !pane.dirty {
                        continue;
                    }
                    let rect = rects[i];
                    let top_line = (pane.scroll.offset().1 / theme::LINE_HEIGHT()).round() as usize;
                    let owned: Vec<(String, Attrs)> = pane
                        .term
                        .visual_spans(top_line)
                        .into_iter()
                        .map(|(s, c)| (s, to_attr(c)))
                        .collect();
                    let buf = &mut gpu.ui.terminal_panes[i];
                    // Re-apply metrics each reshape so the grid font tracks UI zoom.
                    buf.set_metrics(fs, glyphon::Metrics::new(theme::FONT_SIZE(), theme::LINE_HEIGHT()));
                    buf.set_size(fs, None, Some(rect.h + 200.0));
                    buf.set_rich_text(
                        fs,
                        owned.iter().map(|(s, a)| (s.as_str(), *a)),
                        to_attr([0.83, 0.83, 0.83, 1.0]),
                        Shaping::Advanced,
                    );
                    buf.shape_until_scroll(fs, false);
                    // Capture the real monospace advance so cursors map exactly. Use the
                    // MOST COMMON advance (the ASCII majority), not the first glyph: TUIs
                    // like Claude Code mix in box-drawing/powerline glyphs whose fallback
                    // font is wider, so picking the first glyph makes cell_w flip-flop
                    // frame-to-frame — which storms PTY resizes (SIGWINCH) and floods
                    // scrollback with redraws. The mode is stable regardless of content.
                    let mut adv_counts: std::collections::HashMap<u32, (f32, u32)> =
                        std::collections::HashMap::new();
                    for w in buf
                        .layout_runs()
                        .flat_map(|r| r.glyphs.iter())
                        .map(|g| g.w)
                        .filter(|w| *w > 0.5)
                    {
                        let e = adv_counts.entry(w.to_bits()).or_insert((w, 0));
                        e.1 += 1;
                    }
                    if let Some((adv, _)) = adv_counts.values().copied().max_by_key(|(_, c)| *c) {
                        app.terminal_cell_w = adv;
                    }
                    pane.dirty = false;
                }
            }
        }

        // Terminal tab-list labels (only meaningful with more than one tab).
        if app.terminal.visible && app.terminal.groups.len() > 1 {
            let key: String = app
                .terminal
                .groups
                .iter()
                .enumerate()
                .map(|(i, g)| format!("{}: {}", i + 1, g.title()))
                .collect::<Vec<_>>()
                .join("\n");
            gpu.ui.term_tablist.set_text(fs, &key, theme::zpx(crate::TERMINAL_TABLIST_W), 800.0);
        }

        // Find-in-files panel shapes its own buffers (results list, inputs, labels).
        if app.sidebar_view == SidebarView::Search {
            if let Some(sp) = app.search.as_mut() {
                sp.update(fs, layout.tree_region());
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
        if app.explorer.context_menu.is_some() {
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
    // Menu-bar hover + layout-toggle hover. The open dropdown's title stays lit.
    gpu.menubar
        .draw_bg(layout.menu_bar_rect(), app.open_menu.or(app.hovered_menu), &mut bg_quads);
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
    // Source Control change-count badge (blue pill, bottom-right of the SCM icon).
    let scm_count = app.source_control.as_ref().map_or(0, |s| s.change_count());
    if scm_count > 0 {
        if let Some(r) = act_rects.get(2) {
            let z = theme::ui_zoom();
            let bw = gpu.ui.scm_badge.width() + 8.0 * z;
            let bh = 16.0 * z;
            let bx = r.x + r.w - bw - 2.0 * z;
            let by = r.y + r.h - bh - 8.0 * z;
            bg_quads.push(Rect { x: bx, y: by, w: bw, h: bh }.rounded_quad(theme::BADGE_BG(), bh * 0.5));
        }
    }
    // Sidebar bg
    if app.sidebar_visible {
        bg_quads.push(layout.sidebar.quad(theme::SIDEBAR_BG()));
        if app.sidebar_view == SidebarView::Explorer {
            let tr = layout.tree_region();
            let sy = app.explorer.scroll.offset().1;
            // Shift a row rect by the scroll offset and clip it to the tree viewport
            // (so highlights for off-screen rows don't bleed into the header).
            let clip_row = |mut r: Rect| -> Option<Rect> {
                r.y -= sy;
                let top = r.y.max(tr.y);
                let bot = (r.y + r.h).min(tr.y + tr.h);
                (bot > top).then_some(Rect { y: top, h: bot - top, ..r })
            };
            // Explorer header action hover.
            if let Some(i) = app.hovered_explorer {
                bg_quads.push(layout.explorer_action_rects()[i].quad(theme::MENU_HOVER()));
            }
            // Inline-create row highlight (at the insert position).
            if let Some(pc) = app.explorer.creating.as_ref() {
                let (row_rect, _, _) = create_row_geometry(tr, pc.row, pc.depth);
                if let Some(rr) = clip_row(row_rect) {
                    bg_quads.push(rr.quad(theme::TREE_SELECTED()));
                }
            }
            // Active-file highlight: the tree row matching the open document.
            if app.explorer.creating.is_none() {
                if let Some(path) = app.workspace.active_doc().and_then(|d| d.path.clone()) {
                    if let Some(idx) = app.workspace.tree.nodes.iter().position(|n| n.path == path) {
                        if let Some(rr) = clip_row(gpu.ui.sidebar.row_rect(tr, idx)) {
                            bg_quads.push(rr.quad(theme::TREE_ACTIVE_FILE()));
                        }
                    }
                }
            }
            // Tree row hover (below the header) — row rect from the ListView.
            if let Some(idx) = app.hovered_tree {
                if let Some(rr) = clip_row(gpu.ui.sidebar.row_rect(tr, idx)) {
                    bg_quads.push(rr.quad(theme::TREE_HOVER()));
                }
            }
            // Auto-hiding file-tree scrollbar.
            app.explorer.scroll.draw(now, &mut fg_quads);
        } else if app.sidebar_view == SidebarView::Extensions {
            // Extensions panel: filter box chrome + selection/caret (fixed at top).
            // The scrollable rows draw in their own clipped pass after the main pass.
            if let Some(ep) = app.extensions_panel.as_ref() {
                ep.draw_quads(layout.tree_region(), app.cursor_blink_on, &mut bg_quads, &mut fg_quads);
            }
        } else if app.sidebar_view == SidebarView::Search {
            // Search (find-in-files) panel paints its own chrome, selection, caret,
            // match highlights, and scrollbar overlay.
            if let Some(sp) = app.search.as_ref() {
                sp.draw_quads(
                    layout.tree_region(),
                    app.cursor_blink_on,
                    std::time::Instant::now(),
                    &mut bg_quads,
                    &mut fg_quads,
                );
            }
        } else if app.sidebar_view == SidebarView::SourceControl {
            if let Some(scp) = app.source_control.as_ref() {
                scp.draw_quads(layout.tree_region(), app.cursor_blink_on, &mut bg_quads, &mut fg_quads);
            }
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
    let n_tabs = app.workspace.documents.len() + app.detail.open_extension.is_some() as usize;
    let active_tab = if app.detail.open_extension.is_some() {
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
    if app.detail.open_extension.is_some() {
        gpu.ui.ext_detail.draw_quads(
            editor_full,
            app.detail.hovered_page_install,
            app.detail.hovered_detail_tab,
            &mut bg_quads,
        );
    }

    // Current-line highlight + selection. All editor quads must be clipped to
    // the editor's vertical band so scrolled-off rows don't bleed into the tab
    // strip / title bar above (text is clipped via its TextArea bounds; quads
    // have no implicit clip, so we clamp them here). Skipped while the extension
    // page occupies the editor area.
    if let Some(d) = app.detail.open_extension.is_none().then(|| app.workspace.active_doc()).flatten() {
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
        // Skipped in diff views (the per-line add/del backgrounds carry the meaning).
        if crate::settings::render_line_highlight() && d.diff.is_none() {
            let (ltop, lh) = d.line_visual_bounds(cur_line);
            let line_y = layout.editor_text.y + theme::EDITOR_PAD() + ltop - d.scroll_y();
            if let Some((qy, qh)) = clip_v(line_y, lh) {
                bg_quads.push(Quad::new(editor_full.x, qy, editor_full.w, qh, theme::LINE_HIGHLIGHT()));
            }
        }

        // Side-by-side diff: two panes over the full editor region — old (left) /
        // new (right). Per-row backgrounds: del=red on left, add=green on right, the
        // opposite side filled "no line" grey; hunk headers span both. Colours go on
        // the text sub-rects only so the per-pane gutter numbers stay readable.
        if let Some(diff) = d.diff.as_ref() {
            use crate::diff::RowKind;
            let half = (editor_full.w * 0.5).floor();
            let (lt_x, lt_w) = (editor_full.x + theme::GUTTER_WIDTH(), (half - theme::GUTTER_WIDTH()).max(0.0));
            let (rt_x, rt_w) = (editor_full.x + half + theme::GUTTER_WIDTH(), (editor_full.w - half - theme::GUTTER_WIDTH()).max(0.0));
            for run in d.buffer.layout_runs() {
                let Some(row) = diff.rows.get(run.line_i) else { continue };
                let y = editor_full.y + theme::EDITOR_PAD() + run.line_top - d.scroll_y();
                let Some((qy, qh)) = clip_v(y, run.line_height) else { continue };
                match row.kind {
                    RowKind::Hunk => bg_quads.push(Quad::new(editor_full.x, qy, editor_full.w, qh, theme::DIFF_HUNK_BG())),
                    RowKind::Del => {
                        bg_quads.push(Quad::new(lt_x, qy, lt_w, qh, theme::DIFF_DEL_BG()));
                        bg_quads.push(Quad::new(rt_x, qy, rt_w, qh, theme::DIFF_FILLER_BG()));
                    }
                    RowKind::Add => {
                        bg_quads.push(Quad::new(lt_x, qy, lt_w, qh, theme::DIFF_FILLER_BG()));
                        bg_quads.push(Quad::new(rt_x, qy, rt_w, qh, theme::DIFF_ADD_BG()));
                    }
                    RowKind::Context => {}
                }
            }
            // Vertical divider between the two panes.
            bg_quads.push(Quad::new(editor_full.x + half, editor_full.y, 1.0, editor_full.h, theme::BORDER()));
        }

        // editor.rulers — vertical guide line(s) at N monospace columns.
        let rulers = crate::settings::rulers();
        if rulers > 0 {
            let char_w = theme::FONT_SIZE() * 0.6; // monospace advance approximation
            let rx = layout.editor_text.x + theme::EDITOR_PAD() + rulers as f32 * char_w - d.scroll_x();
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
                let sel_y = layout.editor_text.y + theme::EDITOR_PAD() + run.line_top - d.scroll_y();
                if let Some((qy, qh)) = clip_v(sel_y, run.line_height) {
                    bg_quads.push(Quad::new(
                        layout.editor_text.x + theme::EDITOR_PAD() + xs - d.scroll_x(),
                        qy,
                        w,
                        qh,
                        theme::SELECTION(),
                    ));
                }
            }
        }

        // Cursor (foreground so it sits over glyphs) — gated by blink. Read-only
        // tabs (images, diffs) have nothing to edit, so they show no caret.
        if app.cursor_blink_on && !d.read_only {
            let (cx, cy, ch) = d.caret_visual();
            let cursor_y = layout.editor_text.y + theme::EDITOR_PAD() + cy - d.scroll_y();
            if let Some((qy, qh)) = clip_v(cursor_y, ch) {
                fg_quads.push(Quad::new(
                    layout.editor_text.x + theme::EDITOR_PAD() + cx - d.scroll_x(),
                    qy,
                    theme::CURSOR_WIDTH(),
                    qh,
                    theme::CURSOR(),
                ));
            }
        }

        // Editor scrollbars (auto-hide overlay; vertical + horizontal).
        d.scroll.draw(now, &mut fg_quads);
    }

    // Terminal panel — background, top divider, and the block cursor (when focused).
    if let Some(panel) = layout.terminal_panel {
        let content = crate::terminal_content(panel);
        bg_quads.push(panel.quad(theme::PANEL_BG()));
        // Editor/panel seam divider (panel top) + header/content divider.
        bg_quads.push(Quad::new(panel.x, panel.y, panel.w, 1.0, theme::PANEL_BORDER()));
        bg_quads.push(Quad::new(content.x, content.y - 1.0, content.w, 1.0, theme::PANEL_BORDER()));
        // Active panel tab underline (text + buttons are drawn in the areas phase).
        let header = Rect { x: panel.x, y: panel.y, w: panel.w, h: theme::TERMINAL_HEADER_H() };
        if let Some(r) = panel_tab_rects(header, &gpu.terminal_tabs).get(theme::PANEL_ACTIVE_TAB) {
            bg_quads.push(Quad::new(r.x, header.y + header.h - 2.0, r.w, 2.0, [0.9, 0.9, 0.9, 1.0]));
        }
        let char_w = app.terminal_cell_w;
        let line_h = theme::LINE_HEIGHT();
        let area = crate::terminal_pane_area(content, app.terminal.groups.len());
        if let Some(g) = app.terminal.groups.get(app.terminal.active) {
            let rects = crate::terminal_pane_rects(area, g.panes.len());
            for (i, pane) in g.panes.iter().enumerate() {
                let rect = rects[i];
                // Divider between adjacent split panes.
                if i > 0 {
                    bg_quads.push(Quad::new(rect.x - 1.0, rect.y, 1.0, rect.h, theme::PANEL_BORDER()));
                }
                let right = rect.x + rect.w;
                let top_line = (pane.scroll.offset().1 / line_h).round() as usize;
                let at_bottom = pane.scroll.at_end();
                // Per-cell background fills (reverse-video cursor, colored TUIs), clipped to the pane.
                for (row, c0, c1, bg) in pane.term.bg_cells(top_line) {
                    let x = rect.x + theme::zpx(8.0) + c0 as f32 * char_w;
                    let w = ((c1 - c0) as f32 * char_w).min((right - x).max(0.0));
                    let y = rect.y + theme::zpx(4.0) + row as f32 * line_h;
                    if w > 0.0 && y + line_h <= rect.y + rect.h {
                        bg_quads.push(Quad::new(x, y, w, line_h, bg));
                    }
                }
                // Block cursor only in the focused pane, when the shell shows it
                // (DECTCEM) and we're at the live bottom (not scrolled into history).
                let focused = app.terminal.focused && i == g.focused;
                if focused && pane.term.cursor_visible() && at_bottom {
                    let (cc, cr) = pane.term.cursor();
                    let cx = rect.x + theme::zpx(8.0) + cc as f32 * char_w;
                    let cy = rect.y + theme::zpx(4.0) + cr as f32 * line_h;
                    if cx < right && cy + line_h <= rect.y + rect.h {
                        bg_quads.push(Quad::new(cx, cy, char_w.max(theme::zpx(2.0)), line_h, [0.6, 0.6, 0.6, 0.6]));
                    }
                }
                // Auto-hiding scrollback scrollbar (overlay) for this pane.
                pane.scroll.draw(now, &mut fg_quads);
            }
        }
        // Terminal tab list (right side, shown when there's more than one tab).
        if let Some(tl) = crate::terminal_tablist_rect(content, app.terminal.groups.len()) {
            bg_quads.push(tl.quad(theme::SIDEBAR_BG()));
            bg_quads.push(Quad::new(tl.x, tl.y, 1.0, tl.h, theme::PANEL_BORDER()));
            let ry = tl.y + app.terminal.active as f32 * theme::TREE_ROW_HEIGHT();
            bg_quads.push(Quad::new(tl.x, ry, tl.w, theme::TREE_ROW_HEIGHT(), theme::TREE_ACTIVE_FILE()));
        }
    }

    // README / extension detail page scrollbar (overlay over the editor area).
    if app.detail.open_extension.is_some() {
        app.detail.ext_detail_scroll.draw(now, &mut fg_quads);
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
            fg_quads.push(gpu.ui.palette_input.caret_quad(pal.input, theme::zpx(6.0)));
        } else if let Some(fb) = layout.find_bar.as_ref() {
            fg_quads.push(gpu.ui.find_input.caret_quad(*fb, theme::zpx(8.0)));
        }
        if let Some(pc) = app.explorer.creating.as_ref() {
            let (_, _, field) = create_row_geometry(layout.tree_region(), pc.row, pc.depth);
            fg_quads.push(gpu.create_input.caret_quad(field, 0.0));
        }
        // (The Extensions and Search panels draw their carets in their own draw_quads.)
    }

    // Text-input selection highlights — drawn into bg_quads (under glyphs, over
    // the input box). Not blink-gated.
    if let Some(pal) = layout.palette.as_ref() {
        gpu.ui.palette_input.selection_quads(pal.input, theme::zpx(6.0), &mut bg_quads);
    }
    if let Some(fb) = layout.find_bar.as_ref() {
        gpu.ui.find_input.selection_quads(*fb, theme::zpx(8.0), &mut bg_quads);
    }
    // (The Extensions and Search panels draw their selection highlights in their own draw_quads.)

    // ---- Build text areas ----
    let active_idx = app.workspace.active;

    let (cfg_w, cfg_h) = (gpu.config.width, gpu.config.height);
    gpu.quad_renderer
        .prepare(&gpu.device, &gpu.queue, &bg_quads, &fg_quads, (cfg_w, cfg_h));
    // The detail-page header icon is drawn via the atlas in the main pass.
    let mut detail_icons: Vec<icon::IconInstance> = Vec::new();
    if app.detail.open_extension.is_some() {
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
    // Source Control badge number, centered in the pill drawn in the quad phase.
    let scm_count = app.source_control.as_ref().map_or(0, |s| s.change_count());
    if scm_count > 0 {
        if let Some(r) = act_rects.get(2) {
            let z = theme::ui_zoom();
            let bw = ui.scm_badge.width() + 8.0 * z;
            let bh = 16.0 * z;
            let bx = r.x + r.w - bw - 2.0 * z;
            let by = r.y + r.h - bh - 8.0 * z;
            let badge = Rect { x: bx, y: by, w: bw, h: bh };
            ui.scm_badge
                .push(bx + (bw - ui.scm_badge.width()) * 0.5, badge, theme::BADGE_FG(), &mut areas);
        }
    }

    // Sidebar header + (Explorer tree | Extensions list)
    if app.sidebar_visible {
        ui.sidebar_header
            .push(layout.sidebar.x + theme::zpx(12.0), layout.sidebar_header_rect(), theme::FG_DIM(), &mut areas);
        let tr = layout.tree_region();
        if app.sidebar_view == SidebarView::Explorer {
            let er = layout.explorer_action_rects();
            for (i, btn) in gpu.explorer_btns.iter().enumerate() {
                btn.draw(er[i], theme::TITLE_FG(), &mut areas);
            }
            // Root folder row (chevron + workspace name).
            ui.root_label
                .draw_left(layout.root_row_rect(), theme::zpx(10.0), theme::FG_TEXT(), &mut areas);
            let sy = app.explorer.scroll.offset().1;
            if let Some(pc) = app.explorer.creating.as_ref() {
                let rowh = theme::TREE_ROW_HEIGHT();
                // Scrolled tree origin: rows (and the inline create field) shift up by `sy`.
                let stop = tr.y - sy;
                let (_, icon_rect, field) = create_row_geometry(Rect { y: stop, ..tr }, pc.row, pc.depth);
                let split = stop + pc.row as f32 * rowh; // top of the create row
                if split > tr.y {
                    let clip_a = Rect { x: tr.x, y: tr.y, w: tr.w, h: (split - tr.y).min(tr.h) };
                    ui.sidebar.draw_at(clip_a, stop, theme::FG_TEXT(), &mut areas);
                }
                gpu.create_icons[pc.is_dir as usize].draw(icon_rect, theme::ICON_FILE_COLOR(), &mut areas);
                gpu.create_input.draw(field, 0.0, theme::FG_TEXT(), &mut areas);
                let below_y = (split + rowh).max(tr.y);
                let clip_b = Rect {
                    x: tr.x,
                    y: below_y,
                    w: tr.w,
                    h: (tr.y + tr.h - below_y).max(0.0),
                };
                // The rows after the create row keep their natural positions; draw the
                // buffer shifted so row (pc.row+1) lands at `below_y`'s logical slot.
                ui.sidebar.draw_at(clip_b, stop + rowh, theme::FG_TEXT(), &mut areas);
            } else {
                ui.sidebar.draw_at(tr, tr.y - sy, theme::FG_TEXT(), &mut areas);
            }
        } else if let Some(ep) = (app.sidebar_view == SidebarView::Extensions)
            .then(|| app.extensions_panel.as_ref())
            .flatten()
        {
            // Extensions filter box text (fixed). The scrollable row text is drawn
            // in the dedicated clipped pass after the main pass.
            ep.draw_text(tr, &mut areas);
        } else if app.sidebar_view == SidebarView::Search {
            // Search panel renders its own text (inputs, toggle captions, results
            // list with bright file headers + chevrons).
            if let Some(sp) = app.search.as_ref() {
                sp.draw_text(tr, &mut areas);
            }
        } else if app.sidebar_view == SidebarView::SourceControl {
            if let Some(scp) = app.source_control.as_ref() {
                scp.draw_text(tr, &mut areas);
            }
        }
    }

    // Tab labels — the shared `tabs` buffer holds one label per line; we render
    // it once per tab, shifted up by one line and clipped to that tab's column,
    // so each tab shows only its own label. Geometry comes from `tab_rects`.
    let tab_rects = layout.tab_rects(n_tabs);
    for (i, tab) in tab_rects.iter().enumerate() {
        let active = active_tab == Some(i);
        let line_top = i as f32 * theme::UI_LINE_HEIGHT();
        let color = if active {
            theme::TAB_FG_ACTIVE()
        } else {
            theme::TAB_FG_INACTIVE()
        };
        let label_top = tab.text_top(theme::UI_LINE_HEIGHT(), VAlign::Center);
        areas.push(TextArea {
            buffer: &ui.tabs,
            left: tab.x + theme::zpx(12.0),
            top: label_top - line_top,
            scale: 1.0,
            // Clip to just this label's line band (the buffer holds every tab's
            // label, one per line) so neighbours don't bleed in.
            bounds: TextBounds {
                left: tab.x as i32 + theme::zpx(6.0) as i32,
                top: (label_top - 2.0) as i32,
                right: (tab.x + tab.w - theme::zpx(26.0)) as i32,
                bottom: (label_top + theme::UI_LINE_HEIGHT()) as i32,
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
    if app.detail.open_extension.is_some() {
        let size_of = |k: &str| gpu.media.size(k);
        ui.ext_detail.draw_text(
            editor_region(&layout),
            app.detail.ext_detail_scroll.offset().1,
            &size_of,
            &mut areas,
            &mut detail_img_rects,
        );
    } else if let Some(i) = active_idx {
        let d = &app.workspace.documents[i];

        if d.image.is_some() {
            // Image tab: the picture is drawn in the media pass below; no text/gutter.
        } else if let Some(right) = d.diff_right.as_ref() {
            // Side-by-side diff: two gutters + two text panes over the full region.
            let full = editor_region(&layout);
            let half = (full.w * 0.5).floor();
            let g = theme::GUTTER_WIDTH();
            let lg = Rect { x: full.x, y: full.y, w: g, h: full.h };
            let lt = Rect { x: full.x + g, y: full.y, w: (half - g).max(0.0), h: full.h };
            let rg = Rect { x: full.x + half, y: full.y, w: g, h: full.h };
            let rt = Rect { x: full.x + half + g, y: full.y, w: (full.w - half - g).max(0.0), h: full.h };
            ui.line_numbers.draw(lg, d.scroll_y(), theme::FG_GUTTER(), &mut areas);
            ui.line_numbers2.draw(rg, d.scroll_y(), theme::FG_GUTTER(), &mut areas);
            for (buf, r) in [(&d.buffer, lt), (right, rt)] {
                areas.push(TextArea {
                    buffer: buf,
                    left: r.x + theme::EDITOR_PAD() - d.scroll_x(),
                    top: r.y + theme::EDITOR_PAD() - d.scroll_y(),
                    scale: 1.0,
                    bounds: TextBounds { left: r.x as i32, top: r.y as i32, right: (r.x + r.w) as i32, bottom: (r.y + r.h) as i32 },
                    default_color: theme::FG_TEXT(),
                    custom_glyphs: &[],
                });
            }
        } else {
            // Line numbers — clipped to the gutter region so they never bleed over
            // the tab strip when scrolled.
            ui.line_numbers
                .draw(layout.gutter, d.scroll_y(), theme::FG_GUTTER(), &mut areas);

            // Document text
            areas.push(TextArea {
                buffer: &d.buffer,
                left: layout.editor_text.x + theme::EDITOR_PAD() - d.scroll_x(),
                top: layout.editor_text.y + theme::EDITOR_PAD() - d.scroll_y(),
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
    }

    // Status bar — left: path; right: position/encoding/etc. Both via the
    // reusable TextLabel (left-padded and right-padded alignment helpers).
    ui.status
        .draw_left(layout.status_bar, theme::zpx(12.0), theme::STATUS_BAR_FG(), &mut areas);
    // Window-zoom control (− % +) pinned to the right; status info is padded left of it.
    let zoom_cells = zoom_ctrl_cells(layout.status_bar);
    ui.status_right
        .draw_right(layout.status_bar, 8.0 + (layout.status_bar.x + layout.status_bar.w - zoom_cells[0].x), theme::STATUS_BAR_FG(), &mut areas);
    let sfg = theme::STATUS_BAR_FG();
    for (lbl, c) in [
        (&ui.zoom_minus, zoom_cells[0]),
        (&ui.zoom_pct, zoom_cells[1]),
        (&ui.zoom_plus, zoom_cells[2]),
    ] {
        lbl.push(c.x + (c.w - lbl.width()) * 0.5, c, sfg, &mut areas);
    }

    // Find bar
    if let Some(fb) = layout.find_bar.as_ref() {
        ui.find_input.draw(*fb, theme::zpx(8.0), theme::FG_TEXT(), &mut areas);
    }

    // Panel header (VSCode-style tabs + stub icon buttons) and terminal grid text.
    if app.terminal.visible {
        if let Some(panel) = layout.terminal_panel {
            let content = crate::terminal_content(panel);
            let header = Rect { x: panel.x, y: panel.y, w: panel.w, h: theme::TERMINAL_HEADER_H() };
            // Tab labels (active = bright, others dimmed).
            for (i, r) in panel_tab_rects(header, &gpu.terminal_tabs).into_iter().enumerate() {
                let color = if i == theme::PANEL_ACTIVE_TAB {
                    theme::FG_ACTIVE()
                } else {
                    theme::FG_DIM()
                };
                gpu.terminal_tabs[i].push(r.x, r, color, &mut areas);
            }
            // Right-side icon buttons.
            for (i, r) in crate::terminal_header_button_rects(panel).into_iter().enumerate() {
                if let Some(b) = gpu.terminal_btns.get(i) {
                    b.draw(r, theme::FG_TEXT(), &mut areas);
                }
            }
            // Each split pane's grid text in the active tab, clipped to its column.
            let area = crate::terminal_pane_area(content, app.terminal.groups.len());
            let n = app.terminal.groups.get(app.terminal.active).map_or(0, |g| g.panes.len());
            for (i, r) in crate::terminal_pane_rects(area, n).into_iter().enumerate() {
                if let Some(buf) = ui.terminal_panes.get(i) {
                    areas.push(TextArea {
                        buffer: buf,
                        left: r.x + theme::zpx(8.0),
                        top: r.y + theme::zpx(4.0),
                        scale: 1.0,
                        bounds: TextBounds {
                            left: r.x as i32,
                            top: r.y as i32,
                            right: (r.x + r.w) as i32,
                            bottom: (r.y + r.h) as i32,
                        },
                        default_color: theme::FG_TEXT(),
                        custom_glyphs: &[],
                    });
                }
            }
            // Tab-list labels + per-tab close (×) buttons (right-side switcher).
            if let Some(tl) = crate::terminal_tablist_rect(content, app.terminal.groups.len()) {
                ui.term_tablist.draw(tl, theme::FG_TEXT(), &mut areas);
                for row in 0..app.terminal.groups.len() {
                    gpu.tab_close_btn.draw(
                        crate::terminal_tab_close_rect(tl, row),
                        theme::FG_TEXT(),
                        &mut areas,
                    );
                }
            }
        }
    }
    } // end: palette closed

    // Palette text
    if let Some(pal) = layout.palette.as_ref() {
        ui.palette_input
            .draw(pal.input, theme::zpx(6.0), theme::FG_TEXT(), &mut areas);
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
    // A pending feedback screenshot redirects this frame into an offscreen
    // COPY_SRC texture (the surface texture can't be read back), so we can grab it
    // as PNG after the passes run.
    let capture = app.pending_capture.take();
    let cap_tex = capture.is_some().then(|| {
        gpu.device.create_texture(&wgpu::TextureDescriptor {
            label: Some("nova-capture"),
            size: wgpu::Extent3d {
                width: gpu.config.width,
                height: gpu.config.height,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: gpu.config.format,
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::COPY_SRC,
            view_formats: &[],
        })
    });
    let view = match &cap_tex {
        Some(t) => t.create_view(&TextureViewDescriptor::default()),
        None => frame.texture.create_view(&TextureViewDescriptor::default()),
    };
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
    if app.detail.open_extension.is_some() {
        // Drive fetching from ALL image URLs in the active body (not just loaded
        // ones) — otherwise nothing would ever load (the draw list only holds
        // already-loaded images). Clone to drop the gpu.ui borrow before fetching.
        use marketplace::ImgSource;
        let urls: Vec<String> = gpu.ui.ext_detail.image_urls().to_vec();
        for key in &urls {
            if gpu.media.has(key) || app.detail.requested_images.contains(key) {
                continue;
            }
            app.detail.requested_images.insert(key.clone());
            // Resolve to a source; fetch+decode happens off-thread either way.
            let src = if key.starts_with("http://") || key.starts_with("https://") {
                Some(ImgSource::Http(key.clone()))
            } else if let Some(dir) = &app.detail.ext_img_dir {
                Some(ImgSource::File(dir.join(key.trim_start_matches("./"))))
            } else if let Some(base) = &app.detail.ext_img_base {
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
    if app.detail.open_extension.is_some() {
        let region = editor_region(&layout);
        let clip = crate::ext_detail::ExtensionDetail::body_viewport(region);
        let scroll = app.detail.ext_detail_scroll.offset().1;
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

    // ---- Image tab: draw the active image (scaled/panned) + zoom controls ----
    if let Some((key, scale_opt, pan)) = app
        .workspace
        .active_doc()
        .and_then(|d| d.image.as_ref().map(|k| (k.clone(), d.image_scale, d.image_pan)))
    {
        if let Some((iw, ih)) = gpu.media.size(&key) {
            let region = editor_region(&layout);
            let scale = scale_opt.unwrap_or_else(|| image_fit_scale(iw, ih, region));
            let rect = image_rect(iw, ih, region, scale, pan);
            let now_ms = app.anim_start.elapsed().as_millis() as u64;
            let items = vec![(key.clone(), rect)];
            gpu.media.prepare(&gpu.device, &gpu.queue, &items, (cfg_w, cfg_h), now_ms);

            // Zoom-control overlay (pill + − / % / + / Fit).
            let cells = image_ctrl_cells(region);
            let pill = Rect {
                x: cells[0].x - 6.0,
                y: cells[0].y,
                w: (cells[3].x + cells[3].w) - cells[0].x + 12.0,
                h: cells[0].h,
            };
            let ctrl_quads = vec![pill.rounded_quad([0.16, 0.16, 0.18, 0.95], pill.h * 0.5)];
            gpu.quad_renderer.prepare(&gpu.device, &gpu.queue, &ctrl_quads, &[], (cfg_w, cfg_h));
            let mut careas: Vec<TextArea> = Vec::new();
            let fg = theme::FG_TEXT();
            for (lbl, c) in [
                (&gpu.ui.img_minus, cells[0]),
                (&gpu.ui.img_pct, cells[1]),
                (&gpu.ui.img_plus, cells[2]),
                (&gpu.ui.img_fit, cells[3]),
            ] {
                lbl.push(c.x + (c.w - lbl.width()) * 0.5, c, fg, &mut careas);
            }
            gpu.text_renderer.prepare(
                &gpu.device,
                &gpu.queue,
                &mut gpu.font_system,
                &mut gpu.atlas,
                &gpu.viewport,
                careas,
                &mut gpu.swash_cache,
            )?;

            let mut enc = gpu.device.create_command_encoder(&CommandEncoderDescriptor {
                label: Some("nova-image-pass"),
            });
            {
                let mut pass = enc.begin_render_pass(&RenderPassDescriptor {
                    label: Some("nova-image"),
                    color_attachments: &[Some(RenderPassColorAttachment {
                        view: &view,
                        resolve_target: None,
                        ops: Operations { load: LoadOp::Load, store: StoreOp::Store },
                    })],
                    depth_stencil_attachment: None,
                    timestamp_writes: None,
                    occlusion_query_set: None,
                });
                let sx = region.x.max(0.0) as u32;
                let sy = region.y.max(0.0) as u32;
                let sw = region.w.min(cfg_w as f32 - region.x).max(0.0) as u32;
                let sh = region.h.min(cfg_h as f32 - region.y).max(0.0) as u32;
                if sw > 0 && sh > 0 {
                    pass.set_scissor_rect(sx, sy, sw, sh);
                    gpu.media.render(&mut pass);
                    gpu.quad_renderer.render_bg(&mut pass);
                    gpu.text_renderer.render(&gpu.atlas, &gpu.viewport, &mut pass)?;
                }
            }
            gpu.queue.submit(Some(enc.finish()));
            if gpu.media.is_animated(&key) {
                gpu.window.request_redraw();
            }
        }
    }

    // ---- Extensions list: clipped, scrollable pass over the sidebar ----
    if app.sidebar_visible
        && layout.palette.is_none()
        && app.sidebar_view == SidebarView::Extensions
    {
        let region = ext_list_region(layout.tree_region());
        // The panel supplies the draw data; the GPU pass plumbing stays here.
        let ep = app.extensions_panel.as_ref();
        let mut eq: Vec<Quad> = Vec::new();
        let mut efg: Vec<Quad> = Vec::new(); // scrollbar thumb (clipped by the scissor below)
        let mut einst: Vec<icon::IconInstance> = Vec::new();
        if let Some(ep) = ep {
            ep.list_pass_data(layout.tree_region(), now, &mut eq, &mut efg, &mut einst);
        }
        gpu.quad_renderer.prepare(&gpu.device, &gpu.queue, &eq, &efg, (cfg_w, cfg_h));
        gpu.icon_atlas.prepare(&gpu.device, &gpu.queue, &einst, (cfg_w, cfg_h));
        let mut eareas: Vec<TextArea> = Vec::new();
        if let Some(ep) = ep {
            ep.list_areas(layout.tree_region(), &mut eareas);
        }
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
                gpu.quad_renderer.render_fg(&mut pass); // scrollbar thumb on top
            }
        }
        gpu.queue.submit(Some(enc.finish()));
    }

    // ---- Context menu overlay (second pass, drawn over everything) ----
    if let Some(cm) = app.explorer.context_menu.as_ref() {
        let menu = gpu.ui.menu.rect(cm.anchor, (cfg_w as f32, cfg_h as f32));
        let mut mq: Vec<Quad> = Vec::new();
        gpu.ui.menu.draw_bg(menu, app.explorer.hovered_menu_item, &mut mq);
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

    // ---- Top menu-bar dropdown overlay (drawn over everything) ----
    if let Some(open) = app.open_menu {
        let rects = gpu.menubar.item_rects(layout.menu_bar_rect());
        if let Some(r) = rects.get(open) {
            let anchor = (r.x, r.y + r.h);
            let menu = gpu.ui.menu_dropdown.rect(anchor, (cfg_w as f32, cfg_h as f32));
            let mut mq: Vec<Quad> = Vec::new();
            gpu.ui.menu_dropdown.draw_bg(menu, app.menu_dd_hover, &mut mq);
            gpu.quad_renderer
                .prepare(&gpu.device, &gpu.queue, &mq, &[], (cfg_w, cfg_h));
            let mut mareas: Vec<TextArea> = Vec::new();
            gpu.ui.menu_dropdown.draw(menu, &mut mareas);
            gpu.text_renderer.prepare(
                &gpu.device,
                &gpu.queue,
                &mut gpu.font_system,
                &mut gpu.atlas,
                &gpu.viewport,
                mareas,
                &mut gpu.swash_cache,
            )?;
            let mut encm = gpu.device.create_command_encoder(&CommandEncoderDescriptor {
                label: Some("nova-menudd-pass"),
            });
            {
                let mut pass = encm.begin_render_pass(&RenderPassDescriptor {
                    label: Some("nova-menudd"),
                    color_attachments: &[Some(RenderPassColorAttachment {
                        view: &view,
                        resolve_target: None,
                        ops: Operations { load: LoadOp::Load, store: StoreOp::Store },
                    })],
                    depth_stencil_attachment: None,
                    timestamp_writes: None,
                    occlusion_query_set: None,
                });
                gpu.quad_renderer.render_bg(&mut pass);
                gpu.text_renderer.render(&gpu.atlas, &gpu.viewport, &mut pass)?;
            }
            gpu.queue.submit(Some(encm.finish()));
        }
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

    // ---- Feedback form overlay (topmost modal) ----
    if let Some(form) = app.feedback_form.as_ref() {
        let win = (cfg_w as f32, cfg_h as f32);
        let mut bg: Vec<Quad> = Vec::new();
        let mut fg: Vec<Quad> = Vec::new();
        form.draw_quads(win, app.cursor_blink_on, &mut bg, &mut fg);
        gpu.quad_renderer.prepare(&gpu.device, &gpu.queue, &bg, &fg, (cfg_w, cfg_h));
        let mut fareas: Vec<TextArea> = Vec::new();
        form.draw_text(win, &mut fareas);
        gpu.text_renderer.prepare(
            &gpu.device,
            &gpu.queue,
            &mut gpu.font_system,
            &mut gpu.atlas,
            &gpu.viewport,
            fareas,
            &mut gpu.swash_cache,
        )?;
        let mut encf = gpu.device.create_command_encoder(&CommandEncoderDescriptor {
            label: Some("nova-feedback-pass"),
        });
        {
            let mut pass = encf.begin_render_pass(&RenderPassDescriptor {
                label: Some("nova-feedback"),
                color_attachments: &[Some(RenderPassColorAttachment {
                    view: &view,
                    resolve_target: None,
                    ops: Operations { load: LoadOp::Load, store: StoreOp::Store },
                })],
                depth_stencil_attachment: None,
                timestamp_writes: None,
                occlusion_query_set: None,
            });
            gpu.quad_renderer.render_bg(&mut pass);
            gpu.text_renderer.render(&gpu.atlas, &gpu.viewport, &mut pass)?;
            gpu.quad_renderer.render_fg(&mut pass);
        }
        gpu.queue.submit(Some(encf.finish()));
    }

    // Feedback screenshot: read back the offscreen frame as PNG and hand it to the
    // off-thread uploader, then drop the (unrendered) surface frame and repaint
    // normally next frame. Otherwise present as usual.
    if let (Some(tex), Some((title, body))) = (cap_tex, capture) {
        let png = capture_texture_png(gpu, &tex);
        crate::feedback_upload::submit_async(png, title, body, crate::gh_program(), app.worker_tx.clone());
        gpu.window.request_redraw();
    } else {
        frame.present();
    }
    gpu.atlas.trim();
    Ok(())
}

/// Read an offscreen RGBA/BGRA texture back into PNG bytes. Returns None on any
/// GPU/encode error (the issue is still filed, just without the screenshot).
fn capture_texture_png(gpu: &crate::gpu::GpuState, tex: &wgpu::Texture) -> Option<Vec<u8>> {
    use std::io::Cursor;
    let (w, h) = (gpu.config.width, gpu.config.height);
    let unpadded = w * 4;
    let align = wgpu::COPY_BYTES_PER_ROW_ALIGNMENT;
    let padded = unpadded.div_ceil(align) * align;
    let buffer = gpu.device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("nova-capture-readback"),
        size: (padded * h) as u64,
        usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
        mapped_at_creation: false,
    });
    let mut enc = gpu
        .device
        .create_command_encoder(&CommandEncoderDescriptor { label: Some("nova-capture-copy") });
    enc.copy_texture_to_buffer(
        wgpu::ImageCopyTexture {
            texture: tex,
            mip_level: 0,
            origin: wgpu::Origin3d::ZERO,
            aspect: wgpu::TextureAspect::All,
        },
        wgpu::ImageCopyBuffer {
            buffer: &buffer,
            layout: wgpu::ImageDataLayout {
                offset: 0,
                bytes_per_row: Some(padded),
                rows_per_image: Some(h),
            },
        },
        wgpu::Extent3d { width: w, height: h, depth_or_array_layers: 1 },
    );
    gpu.queue.submit(Some(enc.finish()));

    let slice = buffer.slice(..);
    let (tx, rx) = std::sync::mpsc::channel();
    slice.map_async(wgpu::MapMode::Read, move |r| {
        let _ = tx.send(r);
    });
    gpu.device.poll(wgpu::Maintain::Wait);
    rx.recv().ok()?.ok()?;

    let data = slice.get_mapped_range();
    let mut rgba = Vec::with_capacity((unpadded * h) as usize);
    for row in 0..h {
        let start = (row * padded) as usize;
        rgba.extend_from_slice(&data[start..start + unpadded as usize]);
    }
    drop(data);
    buffer.unmap();

    // Surface textures are typically BGRA — swap to RGBA for the PNG encoder.
    if matches!(gpu.config.format, wgpu::TextureFormat::Bgra8Unorm | wgpu::TextureFormat::Bgra8UnormSrgb) {
        for px in rgba.chunks_exact_mut(4) {
            px.swap(0, 2);
        }
    }
    let img = image::RgbaImage::from_raw(w, h, rgba)?;
    let mut png = Vec::new();
    image::DynamicImage::ImageRgba8(img)
        .write_to(&mut Cursor::new(&mut png), image::ImageFormat::Png)
        .ok()?;
    Some(png)
}
