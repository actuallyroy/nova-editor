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
    App, SidebarView,
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

/// Width of the per-block action column reserved just right of the pane divider
/// (holds the vertically-stacked Stage/Revert/Unstage buttons so they never
/// overlap the diff text or line numbers).
fn diff_block_col_w() -> f32 {
    theme::zpx(22.0)
}

/// The action column rect (just right of the divider) for the diff at `region`.
pub(crate) fn diff_block_col(region: Rect) -> Rect {
    let half = (region.w * 0.5).floor();
    Rect { x: region.x + half, y: region.y, w: diff_block_col_w(), h: region.h }
}

/// Per-block Stage/Revert button rects for the change block at visible rows
/// `[vbs, vbe)`, stacked vertically (with a gap) inside the action column and
/// centered on the block. `count` buttons (2 = revert+stage for a working-tree
/// diff, 1 = unstage for an index diff). Returns None when the block is fully
/// scrolled out of view.
pub(crate) fn diff_block_btn_rects(region: Rect, vbs: usize, vbe: usize, scroll_y: f32, count: usize) -> Option<Vec<Rect>> {
    let lh = theme::LINE_HEIGHT();
    let block_top = region.y + theme::EDITOR_PAD() + vbs as f32 * lh - scroll_y;
    let block_h = (vbe.saturating_sub(vbs)).max(1) as f32 * lh;
    if block_top + block_h <= region.y || block_top >= region.y + region.h {
        return None;
    }
    let col = diff_block_col(region);
    let bw = theme::zpx(18.0);
    let gap = theme::zpx(7.0);
    let stack_h = count as f32 * bw + (count.saturating_sub(1)) as f32 * gap;
    let start = block_top + (block_h - stack_h) * 0.5; // center the stack on the block
    let x = col.x + (col.w - bw) * 0.5; // center horizontally in the column
    Some((0..count).map(|i| Rect { x, y: start + i as f32 * (bw + gap), w: bw, h: bw }).collect())
}

/// Side-by-side diff geometry over the full editor region: the two gutters and two
/// text panes `(left_gutter, left_text, right_gutter, right_text)`. The right side
/// reserves a `diff_block_col_w()` action column just right of the divider (before
/// the right line-numbers), so per-block buttons never cover the text. Single
/// source of truth shared by drawing, the scrollbars, and input hit-testing.
pub(crate) fn diff_pane_rects(region: Rect) -> (Rect, Rect, Rect, Rect) {
    let half = (region.w * 0.5).floor();
    let g = theme::GUTTER_WIDTH();
    let bw = diff_block_col_w();
    let lg = Rect { x: region.x, y: region.y, w: g, h: region.h };
    let lt = Rect { x: region.x + g, y: region.y, w: (half - g).max(0.0), h: region.h };
    // Right side: [divider][action column bw][number gutter g][text].
    let rt_x = region.x + half + bw + g;
    let rt = Rect { x: rt_x, y: region.y, w: (region.x + region.w - rt_x).max(0.0), h: region.h };
    let rg = Rect { x: rt_x - g, y: region.y, w: g, h: region.h };
    (lg, lt, rg, rt)
}

/// Visible (non-collapsed) line ranges `[a, b]` inclusive, in order — the gaps are
/// the folded regions. Used to render the editor text/gutter as fold-aware segments.
pub(crate) fn fold_segments(d: &crate::document::Document, total: usize) -> Vec<(usize, usize)> {
    if total == 0 {
        return Vec::new();
    }
    if d.folds.is_empty() {
        return vec![(0, total - 1)];
    }
    // Maximal runs of non-hidden lines (robust to overlapping/nested fold ranges).
    let mut segs = Vec::new();
    let mut start: Option<usize> = None;
    for line in 0..total {
        if d.is_line_hidden(line) {
            if let Some(s) = start.take() {
                segs.push((s, line - 1));
            }
        } else if start.is_none() {
            start = Some(line);
        }
    }
    if let Some(s) = start {
        segs.push((s, total - 1));
    }
    if segs.is_empty() {
        segs.push((0, total - 1));
    }
    segs
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
    // Refresh the selection-occurrence highlight before borrowing gpu (needs &mut app).
    app.recompute_selection_highlight();
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
        if app.right_sidebar_visible { app.right_split.size() } else { 0.0 },
        (app.sidebar_visible && app.sidebar_view == SidebarView::Explorer)
            .then_some(app.outline_open),
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
                // Hold off while a re-attach backlog is in flight: those bytes
                // target the pty's pre-restart dimensions; resizing first would
                // replay TUI frames onto the wrong rows (#32). The resize (and its
                // SIGWINCH repaint) happens right after the backlog lands.
                if (dc != cols || dr != rows) && !pane.term.pending_backlog {
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
            ep.update(&mut gpu.font_system, layout.tree_region());
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
            scp.update(&mut gpu.font_system, layout.panel_region());
        }
    }
    // Explorer OUTLINE section: created on first use, rows rebuilt from the
    // active document (keyed by name/version — cheap when unchanged).
    if let (Some(hdr), Some(body)) = (layout.outline_header_rect(), layout.outline_body_rect()) {
        let region = Rect { x: hdr.x, y: hdr.y, w: hdr.w, h: hdr.h + body.h };
        let open = app.outline_open;
        let panel = app
            .outline
            .get_or_insert_with(|| crate::ui::outline_panel::OutlinePanel::new(&mut gpu.font_system));
        panel.update(&mut gpu.font_system, region, app.workspace.active_doc(), open);
    }
    // Secondary sidebar (AI chat): shapes its message bubbles + input.
    if app.right_sidebar_visible && layout.right_sidebar.w > 0.0 {
        let panel = app
            .chat
            .get_or_insert_with(|| crate::ui::chat_panel::ChatPanel::new(&mut gpu.font_system));
        panel.update(&mut gpu.font_system, layout.right_sidebar);
    }
    if app.detail.open_extension.is_some() {
        let vp = crate::ext_detail::ExtensionDetail::body_viewport(editor_region(&layout));
        let content_h = gpu.ui.ext_detail.body_content_height(&|k| gpu.media.size(k));
        app.detail.ext_detail_scroll.set_metrics(vp, (vp.w, content_h));
    }
    // Info tabs (Welcome / Tips / Shortcuts): reshape on zoom/width change and size
    // the scroll viewport from the page's height. Regular documents take the
    // editor-metrics branch below instead.
    if app.workspace.active_doc().map_or(false, |d| d.info.is_some()) {
        if let Some(d) = app.workspace.active_doc_mut() {
            if let Some(page) = d.info.as_mut() {
                let region = editor_region(&layout);
                let body = crate::ui::info_page::InfoPage::body(region);
                page.shape(&mut gpu.font_system, body.w);
                let ch = page.content_height();
                d.scroll.set_metrics(region, (region.w, ch + theme::zpx(64.0)));
            }
        }
    } else if let Some(d) = app.workspace.active_doc_mut() {
        // Editor: size the document's scroll viewport (offset clamps here, thumbs
        // position from these metrics). Content height uses logical lines + padding.
        // Collapsed fold regions remove their hidden lines from the scrollable height.
        let hidden = d.hidden_above(d.rope.len_lines());
        let content_h = (d.rope.len_lines().saturating_sub(hidden)) as f32 * theme::LINE_HEIGHT() + theme::EDITOR_PAD() * 2.0;
        // Side-by-side diffs scroll each pane horizontally on its own (two scrollbars,
        // see `diff_h*` on Document), so the document ScrollView only owns vertical
        // here — content width 0 keeps its horizontal thumb hidden; the full-width
        // viewport puts the vertical bar at the right edge.
        let content_w = if d.diff.is_some() {
            0.0
        } else {
            d.max_line_width() + theme::EDITOR_PAD() * 2.0
        };
        d.scroll.set_metrics(layout.editor_text, (content_w, content_h));
        // Side-by-side diff: window both buffers to the viewport so glyphon only
        // processes visible rows (multi-file diffs were O(all lines) per frame).
        if d.diff.is_some() {
            let vh = layout.editor_text.h - theme::EDITOR_PAD();
            d.window_diff(&mut gpu.font_system, d.scroll_y(), vh);
        }
        // Large-file mode: keep the shaped window covering the viewport (no-op
        // while the view stays inside; reshapes ~1.5k lines when it drifts out).
        if d.large {
            let first = (d.scroll_y() / theme::LINE_HEIGHT()).max(0.0) as usize;
            let visible = (layout.editor_text.h / theme::LINE_HEIGHT()).ceil() as usize + 2;
            d.ensure_window(&mut gpu.font_system, first, visible);
        }
    }

    // ---- Update UI buffer texts (only on cache miss) ----
    {
        let fs = &mut gpu.font_system;
        let cache = &mut app.ui_cache;

        // Header command-center label — always the root workspace folder name,
        // shown in all caps (VSCode shows the active file, but the user prefers
        // the project root here as a stable command-center identity).
        let header_label = app
            .cwd
            .file_name()
            .map(|n| n.to_string_lossy().to_uppercase())
            .unwrap_or_else(|| "SEARCH".into());
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
                v.downloads, v.rating, v.supported, v.installed, v.is_theme, uv, app.detail.ext_readme.as_deref(),
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
        // icon glyphs in the icon font, names in the UI font). The IconList owns the
        // centered-icon rendering; we just feed it one row spec per node. Folders use
        // an enlarged (1.25×) chevron; files a 1.0× file-type glyph.
        {
            let mut sidebar_key = String::new();
            let name_c = theme::FG_TEXT();
            let rows: Vec<crate::widgets::IconRow> = app
                .workspace
                .tree
                .nodes
                .iter()
                .map(|node| {
                    let (glyph, color, scale) = if node.is_dir {
                        let (g, c) = theme::folder_icon(&node.name, node.expanded);
                        (g, c, 1.25)
                    } else {
                        let (g, c) = theme::file_icon(&node.name);
                        (g, c, 1.0)
                    };
                    sidebar_key.push_str(&format!("{}{}{}\n", node.depth, glyph, node.name));
                    crate::widgets::IconRow {
                        depth: node.depth,
                        icon: Some((glyph, color, scale)),
                        label: vec![(node.name.clone(), name_c)],
                    }
                })
                .collect();
            // Lay the buffer out tall enough for every row (not just the visible
            // viewport) so scrolling past the first screenful isn't clipped.
            let full_h = app.workspace.tree.nodes.len() as f32 * theme::TREE_ROW_HEIGHT() + 200.0;
            gpu.ui.sidebar.set_rows(fs, &sidebar_key, &rows, layout.sidebar.w, full_h.max(layout.sidebar.h));
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
            // Ensure a cached icon overlay exists for each tab's file type.
            for d in &app.workspace.documents {
                let g = theme::file_icon(&d.name).0;
                gpu.ui
                    .tab_icons
                    .entry(g)
                    .or_insert_with(|| crate::widgets::IconButton::new(fs, g, theme::icon_family(g), theme::UI_FONT_SIZE()));
            }
            if app.detail.open_extension.is_some() {
                let g = theme::ICON_EXTENSIONS;
                gpu.ui
                    .tab_icons
                    .entry(g)
                    .or_insert_with(|| crate::widgets::IconButton::new(fs, g, theme::icon_family(g), theme::UI_FONT_SIZE()));
            }
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
            ("Aether".to_string(), String::new())
        };
        // Diagnostic hover is shown as a floating card (see the overlay pass below),
        // not in the status bar — the status bar keeps showing the file path.
        gpu.ui.status.set(fs, &status_text, theme::UI_FAMILY());
        gpu.ui.status_right.set(fs, &status_right_text, theme::UI_FAMILY());
        // Git-branch indicator (far left of the status bar). Empty when not a repo.
        let branch_name = app
            .source_control
            .as_ref()
            .and_then(|s| s.branch_name())
            .unwrap_or("")
            .to_string();
        gpu.ui.branch_icon.set(fs, &theme::ICON_SOURCE_CONTROL.to_string(), theme::ICON_FAMILY);
        gpu.ui.branch.set(fs, &branch_name, theme::UI_FAMILY());
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
                // Large docs number the shaped window only (the buffer holds a
                // window of lines, so "from buffer" would always start at 1).
                None if d.large => gpu.ui.line_numbers.set_range(
                    fs,
                    d.buf_first_line(),
                    d.buf_window_lines(),
                    d.head_line_col().0,
                ),
                None => gpu.ui.line_numbers.set_from_buffer(fs, &d.buffer, d.head_line_col().0),
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
                sp.update(fs, layout.sidebar);
            }
        }

        // Palette list (the input owns its own text now). Rows come from the fixed
        // command set or, in quick-pick mode, the dynamic item list.
        if let Some(pal) = layout.palette.as_ref() {
            // Shape the FULL list height (not just the visible band) so scrolling
            // reveals every row; draw_at clips to the visible region.
            let content_h = (app.palette.filtered.len() as f32 * theme::PALETTE_ROW_HEIGHT() + theme::zpx(40.0)).max(pal.list.h);
            if app.palette.mode == crate::commands::PaletteMode::Symbols {
                // Rich rows: a colored kind icon (method/variable/const/…) + name,
                // instead of textual [const]/[let] tags.
                let ui_attrs = Attrs::new().family(Family::Name(theme::UI_FAMILY())).color(theme::FG_TEXT());
                let mut key = String::from("SYM\n");
                let mut spans: Vec<(String, Attrs)> = Vec::new();
                for &i in app.palette.filtered.iter() {
                    if let Some(it) = app.palette.items.get(i) {
                        let (g, col) = theme::symbol_icon(&it.detail);
                        spans.push((format!(" {}  ", g), Attrs::new().family(Family::Name(theme::ICON_FAMILY)).color(col)));
                        spans.push((format!("{}\n", it.label), ui_attrs));
                        key.push_str(&it.detail);
                        key.push(' ');
                        key.push_str(&it.label);
                        key.push('\n');
                    }
                }
                gpu.ui.palette_list.set_rich(fs, &key, &spans, pal.list.w, content_h);
            } else {
                let mut list_text = String::new();
                match app.palette.mode {
                    crate::commands::PaletteMode::Commands => {
                        // Labels only — shortcuts draw as right-aligned keycap pills
                        // in the palette's late pass (see Keycaps below).
                        for &i in app.palette.filtered.iter() {
                            list_text.push_str(&format!(" {}\n", COMMANDS[i].1));
                        }
                    }
                    // `%` rows already carry file:line in the label — no detail tag.
                    crate::commands::PaletteMode::TextSearch => {
                        for &i in app.palette.filtered.iter() {
                            if let Some(it) = app.palette.items.get(i) {
                                list_text.push_str(&format!(" {}\n", it.label));
                            }
                        }
                    }
                    // Other item-based modes (files / go-to-line / quick-pick).
                    _ => {
                        for &i in app.palette.filtered.iter() {
                            if let Some(it) = app.palette.items.get(i) {
                                if it.detail.is_empty() {
                                    list_text.push_str(&format!(" {}\n", it.label));
                                } else {
                                    list_text.push_str(&format!(" {}   [{}]\n", it.label, it.detail));
                                }
                            }
                        }
                    }
                }
                gpu.ui.palette_list.set_text(fs, &list_text, pal.list.w, content_h);
            }
        }
    }

    // ---- Build quad lists ----
    let mut bg_quads: Vec<Quad> = Vec::new();
    let mut fg_quads: Vec<Quad> = Vec::new();
    // A modal overlay (palette / dialog / feedback form) covers the center but not the
    // edges, so every underlying overlay quad (scrollbars, detail icon, link underlines)
    // must be suppressed while one is open — otherwise it bleeds through around the modal.
    let modal_open = layout.palette.is_some() || app.dialog.is_some() || app.feedback_form.is_some() || app.settings_editor.open;

    // Info-tab page chrome (zebra rows, keycap pills, section rules, link
    // underlines) — under the text, in the editor region.
    if app.detail.open_extension.is_none() {
        if let Some(d) = app.workspace.active_doc() {
            if let Some(page) = d.info.as_ref() {
                let body = crate::ui::info_page::InfoPage::body(editor_region(&layout));
                page.quads(body, d.scroll_y(), &mut bg_quads);
            }
        }
    }

    // Title bar bg + window-control hover (hover rect == the button rect).
    bg_quads.push(layout.title_bar.quad(theme::TITLE_BAR_BG()));
    // Header command-center search box.
    gpu.search
        .draw_bg(layout.header_search_rect(), app.hovered_search, &mut bg_quads);
    // Menu-bar hover + layout-toggle hover. The open dropdown's title stays lit.
    // On macOS the menu bar and window-control buttons are native (system menu bar +
    // traffic lights), so we don't render our own.
    if !cfg!(target_os = "macos") {
        gpu.menubar
            .draw_bg(layout.menu_bar_rect(), app.open_menu.or(app.hovered_menu), &mut bg_quads);
        if let Some(b) = app.hovered_titlebtn {
            let color = if b == 2 {
                theme::TITLE_CLOSE_HOVER()
            } else {
                theme::TITLE_BTN_HOVER()
            };
            bg_quads.push(layout.title_btn_rects()[b].quad(color));
        }
    }
    if let Some(i) = app.hovered_layout {
        bg_quads.push(layout.layout_btn_rects()[i].quad(theme::TITLE_BTN_HOVER()));
    }

    // Activity bar bg + hover (hover rect == the button rect).
    bg_quads.push(layout.activity_bar.quad(theme::ACTIVITY_BAR_BG()));
    let act_rects = layout.activity_rects();
    if let Some(idx) = app.hovered_activity {
        bg_quads.push(act_rects[idx].quad(theme::ACTIVITY_BAR_ACTIVE()));
    }
    // Active view: subtle background highlight + the themed accent stripe on its
    // left edge (VS Code's activityBar.activeBorder — Dracula's pink).
    if let Some(ai) = active_activity_idx(app.sidebar_visible, app.sidebar_view) {
        let r = act_rects[ai];
        bg_quads.push(r.quad(theme::ACTIVITY_BAR_ACTIVE()));
        bg_quads.push(Quad::new(r.x, r.y, theme::zpx(2.0).max(2.0), r.h, theme::ACTIVITY_ACTIVE_BORDER()));
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
            // Tree row hover (below the header).
            if let Some(idx) = app.hovered_tree {
                if let Some(rr) = clip_row(gpu.ui.sidebar.row_rect(tr, idx)) {
                    bg_quads.push(rr.quad(theme::TREE_HOVER()));
                }
            }
            // Drag-and-drop target folder highlight (while dragging a tree entry).
            if let Some(tp) = app.tree_drop_target.as_ref() {
                if let Some(idx) = app.workspace.tree.nodes.iter().position(|n| n.is_dir && &n.path == tp) {
                    if let Some(rr) = clip_row(gpu.ui.sidebar.row_rect(tr, idx)) {
                        bg_quads.push(rr.quad(theme::ACCENT_DIM()));
                    }
                }
            }
            // Indent guides (hover-revealed + animated, owned by the IconList). The
            // open file's parent guide stays highlighted even when not hovering.
            {
                let active = app.workspace.active_doc().and_then(|d| d.path.clone()).and_then(|path| {
                    app.workspace.tree.nodes.iter().position(|n| n.path == path)
                });
                gpu.ui.sidebar.set_active_row(active);
                let sy = app.explorer.scroll.offset().1;
                gpu.ui.sidebar.draw_guides(tr, tr.y - sy, now, &mut bg_quads);
            }
            // Auto-hiding file-tree scrollbar.
            if !modal_open {
                app.explorer.scroll.draw(now, &mut fg_quads);
            }
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
                    layout.sidebar,
                    app.cursor_blink_on,
                    std::time::Instant::now(),
                    &mut bg_quads,
                    &mut fg_quads,
                );
            }
        } else if app.sidebar_view == SidebarView::SourceControl {
            if let Some(scp) = app.source_control.as_ref() {
                scp.draw_quads(layout.panel_region(), app.cursor_blink_on, now, &mut bg_quads, &mut fg_quads);
            }
        }
    }
    // Explorer OUTLINE section chrome: header divider, hover row, scrollbar.
    // (After the sidebar bg so the hover/divider quads aren't painted over.)
    if let (Some(hdr), Some(body)) = (layout.outline_header_rect(), layout.outline_body_rect()) {
        if let Some(o) = app.outline.as_ref() {
            let region = Rect { x: hdr.x, y: hdr.y, w: hdr.w, h: hdr.h + body.h };
            o.draw_quads(region, now, &mut bg_quads, &mut fg_quads);
        }
    }
    // Secondary sidebar (AI chat) chrome: bg, input box, role rails, scrollbar.
    if app.right_sidebar_visible && layout.right_sidebar.w > 0.0 {
        if let Some(c) = app.chat.as_ref() {
            c.draw_quads(layout.right_sidebar, now, &mut bg_quads, &mut fg_quads);
        }
    }
    // Low-contrast dividers between the workbench's sections (single source:
    // theme::PANEL_BORDER) — a visible seam instead of relying on adjacent
    // background colors alone. The terminal panel draws its own top edge.
    {
        let d = theme::PANEL_BORDER();
        let tb = layout.title_bar;
        bg_quads.push(Quad::new(tb.x, tb.y + tb.h - 1.0, tb.w, 1.0, d)); // title bar ↔ content
        if layout.activity_bar.w > 0.0 {
            let ab = layout.activity_bar;
            bg_quads.push(Quad::new(ab.x + ab.w - 1.0, ab.y, 1.0, ab.h, d)); // activity ↔ sidebar
        }
        if app.sidebar_visible && layout.sidebar.w > 0.0 {
            let sb = layout.sidebar;
            bg_quads.push(Quad::new(sb.x + sb.w - 1.0, sb.y, 1.0, sb.h, d)); // sidebar ↔ editor
        }
        if layout.right_sidebar.w > 0.0 {
            let rs = layout.right_sidebar;
            bg_quads.push(Quad::new(rs.x, rs.y, 1.0, rs.h, d)); // editor ↔ chat
        }
        // (status-bar seam drawn after its bg fill, further down)
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
        // Top accent stripe for active tab (indigo identity).
        if active {
            bg_quads.push(Quad::new(tab.x, tab.y, tab.w, theme::zpx(2.0), theme::ACCENT()));
        }
        // Subtle vertical divider between tabs.
        if i + 1 < n_tabs {
            bg_quads.push(Quad::new(
                tab.x + tab.w - 1.0,
                tab.y + theme::zpx(6.0),
                1.0,
                tab.h - theme::zpx(12.0),
                [1.0, 1.0, 1.0, 0.06],
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
    // Screen anchor (top-left, just below the caret line) for the completion popup,
    // captured during the editor draw where the fold offset + scroll are in scope.
    let mut completion_anchor: Option<(f32, f32)> = None;
    if let Some(d) = app.detail.open_extension.is_none().then(|| app.workspace.active_doc()).flatten() {
        let etop = layout.editor_text.y;
        let ebot = layout.editor_text.y + layout.editor_text.h;
        let clip_v = |y: f32, h: f32| -> Option<(f32, f32)> {
            let top = y.max(etop);
            let bot = (y + h).min(ebot);
            (bot > top).then_some((top, bot - top))
        };
        // Horizontal clamp to the editor text column — keeps highlight boxes from
        // bleeding into the gutter/sidebar when scrolled right.
        let (ex_lo, ex_hi) = (layout.editor_text.x, layout.editor_text.x + layout.editor_text.w);
        let clip_h = |x: f32, w: f32| -> Option<(f32, f32)> {
            let l = x.max(ex_lo);
            let r = (x + w).min(ex_hi);
            (r > l).then_some((l, r - l))
        };
        // Code folding shifts every line up by the height of collapsed regions above
        // it; `foff(line)` is that pixel offset (0 when nothing is folded). Hidden
        // lines (`d.is_line_hidden`) are skipped entirely by the per-line loops below.
        let fold_lh = theme::LINE_HEIGHT();
        let foff = |line: usize| -> f32 { fold_lh * d.hidden_above(line) as f32 };

        let (cur_line, _) = d.head_line_col();
        // `@`-symbol preview: tint the whole previewed block (declaration → end of
        // its body) so the region reads at a glance while arrowing through results.
        if let Some((r0, r1)) = app.palette_preview_region {
            let (top0, _) = d.line_visual_bounds(r0.min(r1));
            let (top1, h1) = d.line_visual_bounds(r0.max(r1));
            let y = layout.editor_text.y + theme::EDITOR_PAD() + top0 - d.scroll_y() - foff(r0.min(r1));
            let h = (top1 + h1) - top0;
            if let Some((qy, qh)) = clip_v(y, h) {
                bg_quads.push(Quad::new(layout.editor_text.x, qy, layout.editor_text.w, qh, [0.35, 0.48, 0.95, 0.08]));
            }
        }
        // Current line highlight across full editor width (editor.renderLineHighlight).
        // Wrap-aware via the document's visual bounds (covers all wrapped rows).
        // Skipped in diff views (the per-line add/del backgrounds carry the meaning).
        if crate::settings::render_line_highlight() && d.diff.is_none() {
            let (ltop, lh) = d.line_visual_bounds(cur_line);
            let line_y = layout.editor_text.y + theme::EDITOR_PAD() + ltop - d.scroll_y() - foff(cur_line);
            if let Some((qy, qh)) = clip_v(line_y, lh) {
                bg_quads.push(Quad::new(editor_full.x, qy, editor_full.w, qh, theme::LINE_HIGHLIGHT()));
            }
        }

        // Commit graph: lane lines + node dots in the left margin (text drawn offset
        // past them in the areas phase). Right-angle style (axis-aligned quads).
        if let Some(g) = d.graph.as_ref() {
            use crate::graph::{Half, Seg};
            let lh = theme::LINE_HEIGHT();
            let lane_w = theme::zpx(14.0);
            let lw = theme::zpx(2.0).max(1.0);
            let x0 = editor_full.x + theme::EDITOR_PAD();
            let scroll_y = d.scroll_y();
            let cx = |col: u16| x0 + col as f32 * lane_w + lane_w * 0.5;
            for (i, row) in g.rows.iter().enumerate() {
                let band_top = editor_full.y + theme::EDITOR_PAD() + i as f32 * lh - scroll_y;
                if band_top + lh <= editor_full.y {
                    continue;
                }
                if band_top > editor_full.y + editor_full.h {
                    break;
                }
                let mid = band_top + lh * 0.5;
                for seg in &row.segs {
                    match seg {
                        Seg::V { col, half, color } => {
                            let (y, h) = match half {
                                Half::Full => (band_top, lh),
                                Half::Top => (band_top, lh * 0.5),
                                Half::Bottom => (mid, lh * 0.5),
                            };
                            if let Some((qy, qh)) = clip_v(y, h) {
                                bg_quads.push(Quad::new(cx(*col) - lw * 0.5, qy, lw, qh, theme::GRAPH_LANE(*color)));
                            }
                        }
                        Seg::H { a, b, color } => {
                            let (xa, xb) = (cx(*a).min(cx(*b)), cx(*a).max(cx(*b)));
                            if let Some((qy, qh)) = clip_v(mid - lw * 0.5, lw) {
                                bg_quads.push(Quad::new(xa, qy, (xb - xa) + lw, qh, theme::GRAPH_LANE(*color)));
                            }
                        }
                        Seg::Bend { col, top, color } => {
                            if band_top >= editor_full.y && band_top + lh <= editor_full.y + editor_full.h {
                                for q in crate::graph::bend_quads(cx(row.node_col), cx(*col), band_top, band_top + lh, mid, *top, lw, theme::GRAPH_LANE(*color)) {
                                    bg_quads.push(q);
                                }
                            } else {
                                let (xa, xb) = (cx(*col).min(cx(row.node_col)), cx(*col).max(cx(row.node_col)));
                                if let Some((qy, qh)) = clip_v(mid - lw * 0.5, lw) {
                                    bg_quads.push(Quad::new(xa, qy, (xb - xa) + lw, qh, theme::GRAPH_LANE(*color)));
                                }
                            }
                        }
                    }
                }
                let r = theme::zpx(4.5);
                if let Some((qy, qh)) = clip_v(mid - r, r * 2.0) {
                    bg_quads.push(Quad::rounded(cx(row.node_col) - r, qy, r * 2.0, qh, theme::GRAPH_LANE(row.color), r));
                }
            }
        }

        // Side-by-side diff: two panes over the full editor region — old (left) /
        // new (right). Per-row backgrounds: del=red on left, add=green on right, the
        // opposite side filled "no line" grey; hunk headers span both. Colours go on
        // the text sub-rects only so the per-pane gutter numbers stay readable.
        if let Some(diff) = d.diff.as_ref() {
            use crate::diff::RowKind;
            let half = (editor_full.w * 0.5).floor();
            let (_, lt_rect, _, rt_rect) = diff_pane_rects(editor_full);
            let (lt_x, lt_w) = (lt_rect.x, lt_rect.w);
            let (rt_x, rt_w) = (rt_rect.x, rt_rect.w);
            for run in d.buffer.layout_runs() {
                let Some(row) = diff.rows.get(run.line_i) else { continue };
                // line_top is viewport-relative (window_diff sets the buffer scroll).
                let y = editor_full.y + theme::EDITOR_PAD() + run.line_top;
                if y > editor_full.y + editor_full.h {
                    break; // runs are ordered top→bottom; nothing more is visible
                }
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
                    // Combined-view file header: a full-width band (clickable to collapse).
                    RowKind::File => bg_quads.push(Quad::new(editor_full.x, qy, editor_full.w, qh, theme::TAB_BAR_BG())),
                    // Collapsed-unchanged separator: a full-width neutral gray band
                    // (click to expand, drag to reveal) — lighter than the dark filler
                    // so it stands out from the diff background.
                    RowKind::Gap => bg_quads.push(Quad::new(editor_full.x, qy, editor_full.w, qh, theme::DIFF_GAP_BG())),
                    RowKind::Context => {}
                }
            }
            // Vertical divider between the two panes.
            bg_quads.push(Quad::new(editor_full.x + half, editor_full.y, 1.0, editor_full.h, theme::BORDER()));
            // Per-block Stage/Revert (or Unstage) button backgrounds for the hovered
            // change block — packed against the divider at the block's top row.
            if d.diff_path.is_some() {
                if let Some((vbs, vbe)) = app.hovered_diff_block {
                    let count = if d.diff_staged { 1 } else { 2 };
                    if let Some(rects) = diff_block_btn_rects(editor_full, vbs, vbe, d.scroll_y(), count) {
                        for r in &rects {
                            bg_quads.push(r.rounded_quad(theme::ACCENT_DIM(), theme::zpx(3.0)));
                        }
                    }
                }
            }
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

        // Indentation guides: a faint vertical line at each indent stop a line is
        // indented past (VSCode's editor.guides.indentation). Skipped in large-file
        // mode (the buffer holds a window, so line_i ≠ document line) and in diffs.
        if !d.large && d.diff.is_none() {
            // `tab` is used only to expand tab characters into columns; the guide
            // STEP is the unit detected from the file's own indentation.
            let tab = crate::settings::current().editor_tab_size.max(1);
            let unit = d.indent_unit(tab).max(1);
            let char_w = theme::FONT_SIZE() * 0.6;
            let gw = theme::zpx(1.0).max(1.0);
            let faint = [0.50, 0.52, 0.62, 0.14];
            let active_c = [0.58, 0.62, 0.78, 0.5]; // active block's guide, like the trees
            let ex0 = layout.editor_text.x;
            let ex1 = layout.editor_text.x + layout.editor_text.w;
            let nlines = d.rope.len_lines();
            // Indent level of a line (None for a blank/whitespace-only line).
            let level_of = |line: usize| -> Option<usize> {
                let mut cols = 0usize;
                for c in d.rope.line(line).chars() {
                    match c {
                        ' ' => cols += 1,
                        '\t' => cols += tab - (cols % tab),
                        '\n' | '\r' => return None,
                        _ => return Some(cols / unit),
                    }
                }
                None
            };
            // Active guide: the guide of the block the caret is in — column (cursor
            // level − 1), spanning the contiguous lines indented at least that deep
            // (interior blank lines bridged, edge blanks trimmed). Like VSCode's
            // editor.guides.highlightActiveIndentation.
            let active = level_of(cur_line).filter(|&la| la >= 1).map(|la| {
                let mut s = cur_line;
                while s > 0 && level_of(s - 1).map_or(true, |l| l >= la) {
                    s -= 1;
                }
                while s < cur_line && level_of(s).is_none() {
                    s += 1; // trim leading blank lines
                }
                let mut e = cur_line;
                while e + 1 < nlines && level_of(e + 1).map_or(true, |l| l >= la) {
                    e += 1;
                }
                while e > cur_line && level_of(e).is_none() {
                    e -= 1; // trim trailing blank lines
                }
                (la - 1, s, e)
            });
            for run in d.buffer.layout_runs() {
                let line = run.line_i;
                if line >= nlines || d.is_line_hidden(line) {
                    continue;
                }
                let levels = level_of(line).unwrap_or(0);
                if levels == 0 {
                    continue;
                }
                let y = layout.editor_text.y + theme::EDITOR_PAD() + run.line_top - d.scroll_y() - foff(line);
                if let Some((qy, qh)) = clip_v(y, run.line_height) {
                    for k in 0..levels {
                        let gx = (ex0 + theme::EDITOR_PAD() + (k * unit) as f32 * char_w - d.scroll_x()).round();
                        if gx < ex0 || gx >= ex1 {
                            continue;
                        }
                        let hot = active.map_or(false, |(col, s, e)| k == col && line >= s && line <= e);
                        bg_quads.push(Quad::new(gx, qy, gw, qh, if hot { active_c } else { faint }));
                    }
                }
            }
        }

        // Diff: indent guides on BOTH panes (VSCode shows them in the diff editor
        // too). The right pane has no rope, so read indentation straight from each
        // buffer's line text; guides scroll with their pane's own horizontal offset.
        if let (Some(_), Some(right)) = (d.diff.as_ref(), d.diff_right.as_ref()) {
            let tab = crate::settings::current().editor_tab_size.max(1);
            let unit = d.indent_unit(tab).max(1);
            let char_w = theme::FONT_SIZE() * 0.6;
            let gw = theme::zpx(1.0).max(1.0);
            let faint = [0.50, 0.52, 0.62, 0.14];
            let (_, lt, _, rt) = diff_pane_rects(editor_full);
            let leading_levels = |line: &str| -> usize {
                let mut cols = 0usize;
                for c in line.chars() {
                    match c {
                        ' ' => cols += 1,
                        '\t' => cols += tab - (cols % tab),
                        _ => break,
                    }
                }
                cols / unit
            };
            for (pi, (buf, pane)) in [(&d.buffer, lt), (right, rt)].into_iter().enumerate() {
                for run in buf.layout_runs() {
                    let Some(bl) = buf.lines.get(run.line_i) else { continue };
                    let levels = leading_levels(bl.text());
                    if levels == 0 {
                        continue;
                    }
                    // line_top is viewport-relative (window_diff sets the buffer scroll).
                    let y = pane.y + theme::EDITOR_PAD() + run.line_top;
                    let Some((qy, qh)) = clip_v(y, run.line_height) else { continue };
                    for k in 0..levels {
                        let gx = (pane.x + theme::EDITOR_PAD() + (k * unit) as f32 * char_w - d.diff_hx(pi)).round();
                        if gx < pane.x || gx >= pane.x + pane.w {
                            continue;
                        }
                        bg_quads.push(Quad::new(gx, qy, gw, qh, faint));
                    }
                }
            }
        }

        // Match highlights: every find result (when the find widget is open) OR
        // every occurrence of the current word-like selection (VSCode selection
        // highlight). A translucent box behind each; the current one shows via the
        // selection on top. Only visible matches are mapped to rects.
        let hl_matches: &[(usize, usize)] = if app.find.active { &app.find.matches } else { &app.sel_matches };
        if !hl_matches.is_empty() && d.diff.is_none() {
            let lh = theme::LINE_HEIGHT().max(1.0);
            let first = (d.scroll_y() / lh) as usize;
            let last = first + (layout.editor_text.h / lh) as usize + 2;
            let len = d.rope.len_bytes();
            for &(s, e) in hl_matches {
                if e <= s {
                    continue;
                }
                let lo_line = d.rope.byte_to_line(s.min(len));
                if lo_line + 1 < first || lo_line > last {
                    continue; // off-screen
                }
                let hi = e.min(len);
                let hi_line = d.rope.byte_to_line(hi);
                let lo_col = s - d.rope.line_to_byte(lo_line);
                let hi_col = hi - d.rope.line_to_byte(hi_line);
                for run in d.buffer.layout_runs() {
                    let line = run.line_i;
                    if line < lo_line || line > hi_line || d.is_line_hidden(line) {
                        continue;
                    }
                    let (cs, ce) = if lo_line == hi_line {
                        (lo_col, hi_col)
                    } else if line == lo_line {
                        (lo_col, usize::MAX)
                    } else if line == hi_line {
                        (0, hi_col)
                    } else {
                        (0, usize::MAX)
                    };
                    let (xs, xe) = x_range_in_run(&run, cs, ce);
                    let w = (xe - xs).max(2.0);
                    let my = layout.editor_text.y + theme::EDITOR_PAD() + run.line_top - d.scroll_y() - foff(line);
                    let mx = layout.editor_text.x + theme::EDITOR_PAD() + xs - d.scroll_x();
                    if let (Some((qy, qh)), Some((x0, cw))) = (clip_v(my, run.line_height), clip_h(mx, w)) {
                        bg_quads.push(Quad::new(x0, qy, cw, qh, theme::FIND_MATCH()));
                    }
                }
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
                if line < lo_line || line > hi_line || d.is_line_hidden(line) {
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
                let sel_y = layout.editor_text.y + theme::EDITOR_PAD() + run.line_top - d.scroll_y() - foff(line);
                let sx = layout.editor_text.x + theme::EDITOR_PAD() + xs - d.scroll_x();
                if let (Some((qy, qh)), Some((x0, cw))) = (clip_v(sel_y, run.line_height), clip_h(sx, w)) {
                    bg_quads.push(Quad::new(x0, qy, cw, qh, theme::SELECTION()));
                }
            }
        }

        // Matching-bracket highlight (VSCode editorBracketMatch): when the caret
        // sits beside a bracket, box both that bracket and its match — faint fill
        // plus a 1px outline. Skipped in diffs/read-only views (no caret there).
        if d.diff.is_none() && !d.read_only {
            if let Some((a, b)) = d.bracket_highlight() {
                for pos in [a, b] {
                    let line = d.rope.byte_to_line(pos.min(d.rope.len_bytes()));
                    if d.is_line_hidden(line) {
                        continue;
                    }
                    let col = pos - d.rope.line_to_byte(line);
                    for run in d.buffer.layout_runs() {
                        if run.line_i != line {
                            continue;
                        }
                        let (xs, xe) = x_range_in_run(&run, col, col + 1);
                        let w = (xe - xs).max(2.0);
                        let by = layout.editor_text.y + theme::EDITOR_PAD() + run.line_top - d.scroll_y() - foff(line);
                        let bx = layout.editor_text.x + theme::EDITOR_PAD() + xs - d.scroll_x();
                        // Clip horizontally to the editor: when scrolled right, a
                        // bracket left of the viewport must not bleed into the gutter/sidebar.
                        if bx + w <= layout.editor_text.x || bx >= layout.editor_text.x + layout.editor_text.w {
                            continue;
                        }
                        if let Some((qy, qh)) = clip_v(by, run.line_height) {
                            let bw = 1.0_f32.max(theme::ui_zoom().floor());
                            let bc = theme::BRACKET_MATCH_BORDER();
                            bg_quads.push(Quad::new(bx, qy, w, qh, theme::BRACKET_MATCH_BG()));
                            bg_quads.push(Quad::new(bx, qy, w, bw, bc));
                            bg_quads.push(Quad::new(bx, qy + qh - bw, w, bw, bc));
                            bg_quads.push(Quad::new(bx, qy + bw, bw, qh - 2.0 * bw, bc));
                            bg_quads.push(Quad::new(bx + w - bw, qy + bw, bw, qh - 2.0 * bw, bc));
                        }
                    }
                }
            }
        }

        // Diagnostic underlines (LSP). One ~2px bar at the bottom of each covered
        // visual row, colored by severity — reuses the selection byte→x mapping.
        if !d.diagnostics.is_empty() {
            let uz = theme::ui_zoom();
            for diag in &d.diagnostics {
                let (lo, hi) = d.diag_byte_range(diag);
                let lo_line = d.rope.byte_to_line(lo);
                let hi_line = d.rope.byte_to_line(hi.min(d.rope.len_bytes()));
                let lo_col = lo - d.rope.line_to_byte(lo_line);
                let hi_col = hi.saturating_sub(d.rope.line_to_byte(hi_line));
                let color = match diag.severity {
                    crate::lsp::Severity::Error => theme::DIAGNOSTIC_ERROR(),
                    crate::lsp::Severity::Warning => theme::DIAGNOSTIC_WARNING(),
                    _ => theme::DIAGNOSTIC_INFO(),
                };
                for run in d.buffer.layout_runs() {
                    let line = run.line_i;
                    if line < lo_line || line > hi_line || d.is_line_hidden(line) {
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
                    let w = (xe - xs).max(3.0 * uz);
                    let line_y = layout.editor_text.y + theme::EDITOR_PAD() + run.line_top - d.scroll_y() - foff(line);
                    let under_y = line_y + run.line_height - 2.0 * uz;
                    let ux = layout.editor_text.x + theme::EDITOR_PAD() + xs - d.scroll_x();
                    let in_x = ux + w > layout.editor_text.x && ux < layout.editor_text.x + layout.editor_text.w;
                    if let Some((qy, qh)) = clip_v(under_y, 2.0 * uz).filter(|_| in_x) {
                        fg_quads.push(Quad::new(ux, qy, w, qh, color));
                    }
                }
            }
        }

        // Cursor (foreground so it sits over glyphs) — gated by blink. Read-only
        // tabs (images, diffs) have nothing to edit, so they show no caret. Also
        // suppressed when a modal (palette/dialog) owns the screen or the find
        // widget has focus, so the editor caret never bleeds over them.
        if app.cursor_blink_on && !d.read_only && !modal_open && !app.find.focused {
            let (cx, cy, ch) = d.caret_visual();
            let cursor_y = layout.editor_text.y + theme::EDITOR_PAD() + cy - d.scroll_y() - foff(cur_line);
            let cursor_x = layout.editor_text.x + theme::EDITOR_PAD() + cx - d.scroll_x();
            // Don't draw a caret scrolled off the left into the gutter/sidebar.
            let cursor_in = cursor_x >= layout.editor_text.x && cursor_x < layout.editor_text.x + layout.editor_text.w;
            if let Some((qy, qh)) = clip_v(cursor_y, ch).filter(|_| cursor_in) {
                fg_quads.push(Quad::new(
                    cursor_x,
                    qy,
                    theme::CURSOR_WIDTH(),
                    qh,
                    theme::CURSOR(),
                ));
            }
        }

        // Text drag-and-drop: an insertion caret at the drop target while dragging.
        if let Some(tm) = app.editor.text_move.as_ref() {
            if tm.active {
                if let Some(drop) = tm.drop {
                    let (cx, cy, ch) = d.byte_visual(drop);
                    let line = d.rope.byte_to_line(drop.min(d.rope.len_bytes()));
                    let y0 = layout.editor_text.y + theme::EDITOR_PAD() + cy - d.scroll_y() - foff(line);
                    if let Some((qy, qh)) = clip_v(y0, ch) {
                        fg_quads.push(Quad::new(
                            layout.editor_text.x + theme::EDITOR_PAD() + cx - d.scroll_x(),
                            qy,
                            theme::CURSOR_WIDTH(),
                            qh,
                            theme::ACCENT(),
                        ));
                    }
                }
            }
        }

        // Anchor the completion popup just below the caret's line.
        if app.completion.active {
            let (cx, cy, ch) = d.caret_visual();
            let px = layout.editor_text.x + theme::EDITOR_PAD() + cx - d.scroll_x();
            let py = layout.editor_text.y + theme::EDITOR_PAD() + cy - d.scroll_y() - foff(cur_line) + ch;
            completion_anchor = Some((px, py));
        }

        // Editor scrollbars (auto-hide overlay; vertical + horizontal).
        if !modal_open {
            d.scroll.draw(now, &mut fg_quads);
        }
        // Side-by-side diff: a horizontal scrollbar under each pane (independent —
        // the right pane's long lines stay reachable when the left side is short).
        if d.diff.is_some() && !modal_open {
            let (_, lt, _, rt) = diff_pane_rects(editor_region(&layout));
            for (i, pane) in [lt, rt].into_iter().enumerate() {
                if let Some(th) = d.diff_hthumb(i, pane) {
                    fg_quads.push(th.rounded_quad(theme::SCROLLBAR_THUMB(), th.h * 0.5));
                }
            }
        }
        // Diff overview ruler: a green/red mark on the vertical scrollbar track per
        // added/removed row, so the distribution of changes across the whole file is
        // visible at a glance (VSCode's diff overview ruler). Consecutive same-kind
        // rows merge into one bar so a big diff stays a handful of quads.
        if let Some(diff) = d.diff.as_ref() {
            if !modal_open {
                use crate::diff::RowKind;
                let track = d.scroll.vtrack_rect();
                let total = diff.rows.len().max(1) as f32;
                let add = [0.26, 0.78, 0.40, 0.9];
                let del = [0.92, 0.34, 0.34, 0.9];
                let mut i = 0usize;
                while i < diff.rows.len() {
                    let kind = diff.rows[i].kind;
                    let color = match kind {
                        RowKind::Add => Some(add),
                        RowKind::Del => Some(del),
                        _ => None,
                    };
                    let Some(color) = color else { i += 1; continue };
                    let start = i;
                    while i < diff.rows.len() && diff.rows[i].kind == kind {
                        i += 1;
                    }
                    let y0 = track.y + (start as f32 / total) * track.h;
                    let y1 = track.y + (i as f32 / total) * track.h;
                    let h = (y1 - y0).max(theme::zpx(2.0));
                    fg_quads.push(Quad::new(track.x, y0, track.w, h, color));
                }
            }
        }
        // Overview markers: a tick on the scrollbar track per match — find results
        // (current one brighter) or selection occurrences — so you can see where
        // they sit in the whole file (VSCode's overview ruler).
        let ov_matches: &[(usize, usize)] = if app.find.active { &app.find.matches } else { &app.sel_matches };
        if !ov_matches.is_empty() && !modal_open {
            let track = d.scroll.vtrack_rect();
            let total = d.rope.len_lines().max(1) as f32;
            let mh = theme::zpx(2.0).max(1.0);
            let inset = theme::zpx(1.0);
            let base = theme::FIND_MATCH();
            for (i, &(s, _)) in ov_matches.iter().enumerate() {
                let line = d.rope.byte_to_line(s.min(d.rope.len_bytes())) as f32;
                let y = track.y + (line / total) * track.h - mh * 0.5;
                let current = app.find.active && app.find.index == Some(i);
                let color = if current { theme::ACCENT() } else { base };
                let w = if current { track.w } else { track.w - inset * 2.0 };
                let x = if current { track.x } else { track.x + inset };
                fg_quads.push(Quad::new(x, y, w, mh, color));
            }
        }
        // Matching-bracket ticks on the same track (VSCode's
        // editorOverviewRuler.bracketMatchForeground) — one per end of the pair,
        // so an off-screen match is still locatable at a glance.
        if !modal_open && d.diff.is_none() && !d.read_only {
            if let Some((a, b)) = d.bracket_highlight() {
                let track = d.scroll.vtrack_rect();
                let total = d.rope.len_lines().max(1) as f32;
                let mh = theme::zpx(2.0).max(1.0);
                let inset = theme::zpx(1.0);
                for pos in [a, b] {
                    let line = d.rope.byte_to_line(pos.min(d.rope.len_bytes())) as f32;
                    let y = track.y + (line / total) * track.h - mh * 0.5;
                    fg_quads.push(Quad::new(track.x + inset, y, track.w - inset * 2.0, mh, theme::BRACKET_MATCH_BORDER()));
                }
            }
        }
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
            bg_quads.push(Quad::new(r.x, header.y + header.h - theme::zpx(2.0), r.w, theme::zpx(2.0), theme::ACCENT()));
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
                // Text selection highlight (normalized ends; full width for lines
                // spanned in the middle of a multi-line selection).
                if let Some((a, b)) = pane.sel {
                    let (lo, hi) = if a <= b { (a, b) } else { (b, a) };
                    let (cols, rows) = pane.term.dims();
                    for r in 0..rows {
                        let abs = top_line + r;
                        if abs < lo.0 || abs > hi.0 {
                            continue;
                        }
                        let c0 = if abs == lo.0 { lo.1 } else { 0 };
                        let c1 = if abs == hi.0 { hi.1 } else { cols };
                        if c1 <= c0 {
                            continue;
                        }
                        let x = rect.x + theme::zpx(8.0) + c0 as f32 * char_w;
                        let w = ((c1 - c0) as f32 * char_w).min((right - x).max(0.0));
                        let y = rect.y + theme::zpx(4.0) + r as f32 * line_h;
                        if w > 0.0 && y + line_h <= rect.y + rect.h {
                            bg_quads.push(Quad::new(x, y, w, line_h, theme::SELECTION()));
                        }
                    }
                }
                // Thin blinking caret (editor-style) in the focused pane, when the
                // shell shows the cursor (DECTCEM) and we're at the live bottom.
                // CRITICAL: the caret must live in the SAME coordinate space as the
                // text — visual row = (scrollback + grid row) - top_line. Assuming
                // the window starts exactly at scrollback.len() drifts one row off
                // whenever the scroll offset isn't a perfect line multiple, painting
                // the caret beside the wrong line while the grid is fully correct.
                let focused = app.terminal.focused && i == g.focused;
                if focused && pane.term.cursor_visible() && at_bottom && app.term_blink_on {
                    let (cc, cr) = pane.term.cursor();
                    let (_, grid_rows) = pane.term.dims();
                    let back = pane.term.total_lines().saturating_sub(grid_rows);
                    // Same clamp as window_from: the text never starts past `back`.
                    let eff_top = top_line.min(back);
                    let visual = (back + cr) as isize - eff_top as isize;
                    // Debug builds: dump the caret-draw inputs (overwrite, tiny) so a
                    // misplaced caret report can be matched against a grid replay.
                    #[cfg(debug_assertions)]
                    {
                        let _ = std::fs::write(
                            "/tmp/aether_caret_dbg.txt",
                            format!(
                                "cc={cc} cr={cr} grid_rows={grid_rows} total={} back={back} top_line={top_line} eff_top={eff_top} visual={visual} scroll={:.2} rect.y={:.1} rect.h={:.1} line_h={line_h:.2}\n",
                                pane.term.total_lines(),
                                pane.scroll.offset().1,
                                rect.y,
                                rect.h,
                            ),
                        );
                    }
                    if visual >= 0 {
                        let cx = rect.x + theme::zpx(8.0) + cc as f32 * char_w;
                        let cy = rect.y + theme::zpx(4.0) + visual as f32 * line_h;
                        let caret_w = theme::zpx(2.0).max(1.0);
                        if cx < right && cy + line_h <= rect.y + rect.h {
                            bg_quads.push(Quad::new(cx, cy, caret_w, line_h, theme::CURSOR()));
                        }
                    }
                }
                // Auto-hiding scrollback scrollbar (overlay) for this pane.
                if !modal_open {
                    pane.scroll.draw(now, &mut fg_quads);
                }
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
    if app.detail.open_extension.is_some() && !modal_open {
        app.detail.ext_detail_scroll.draw(now, &mut fg_quads);
    }

    // Status bar (+ its low-contrast top seam, over the fill)
    bg_quads.push(layout.status_bar.quad(theme::STATUS_BAR_BG()));
    if layout.status_bar.h > 0.0 {
        let st = layout.status_bar;
        bg_quads.push(Quad::new(st.x, st.y, st.w, 1.0, theme::PANEL_BORDER()));
    }

    // (The find/replace widget draws in its own late pass — see below.)

    // Palette dim overlay + box
    // (The command palette card/text draws in its own late pass — see below.)

    // Text-input carets (blink-gated, drawn on top via fg_quads).
    if app.cursor_blink_on {
        if let Some(pc) = app.explorer.creating.as_ref() {
            let (_, _, field) = create_row_geometry(layout.tree_region(), pc.row, pc.depth);
            fg_quads.push(gpu.create_input.caret_quad(field, 0.0));
        }
        // (The Extensions and Search panels draw their carets in their own draw_quads.)
    }
    // (The Extensions and Search panels draw their selection highlights in their own draw_quads.)

    // Drag ghost (quads + label shaping): the dragged entry's name floats beside the
    // cursor so the grab is visible and the parent-folder drop highlight reads as
    // "dropping into here". The label's text area is pushed in the areas section.
    let mut drag_ghost_pill: Option<Rect> = None;
    if let Some((path, _, true)) = app.tree_drag.as_ref() {
        let name = path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
        gpu.ui.drag_ghost.set(&mut gpu.font_system, &name, theme::UI_FAMILY());
        let mp = (app.mouse_pos.x as f32, app.mouse_pos.y as f32);
        let pill = Rect {
            x: mp.0 + theme::zpx(12.0),
            y: mp.1 + theme::zpx(10.0),
            w: gpu.ui.drag_ghost.width() + theme::zpx(16.0),
            h: theme::TREE_ROW_HEIGHT(),
        };
        bg_quads.push(
            Rect { x: pill.x - 1.0, y: pill.y - 1.0, w: pill.w + 2.0, h: pill.h + 2.0 }
                .rounded_quad(theme::ACCENT_DIM(), theme::zpx(7.0)),
        );
        bg_quads.push(pill.rounded_quad(theme::SEARCH_BG(), theme::zpx(6.0)));
        drag_ghost_pill = Some(pill);
    }

    // ---- Build text areas ----
    let active_idx = app.workspace.active;

    let (cfg_w, cfg_h) = (gpu.config.width, gpu.config.height);
    gpu.quad_renderer
        .prepare(&gpu.device, &gpu.queue, &bg_quads, &fg_quads, (cfg_w, cfg_h));
    // The detail-page header icon is drawn via the atlas in the main pass. Suppress it
    // when a modal is open, else its atlas quad draws over the modal.
    let mut detail_icons: Vec<icon::IconInstance> = Vec::new();
    if app.detail.open_extension.is_some() && !modal_open {
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

    // The command palette renders in its own late pass on top of the live
    // background (VSCode-style — the file stays visible so symbol/line navigation
    // can show the highlighted region), so the background always draws here.
    {
    // Title bar: menu bar (left) + centered search box + layout toggles and
    // window controls (right). On macOS the menu bar + window controls are native.
    if !cfg!(target_os = "macos") {
        gpu.menubar.draw(layout.menu_bar_rect(), &mut areas);
    }
    // The static search label hides while the palette types into the pill.
    if layout.palette.is_none() {
        gpu.search.draw(layout.header_search_rect(), &mut areas);
    }
    let layout_rects = layout.layout_btn_rects();
    for (i, btn) in gpu.layout_btns.iter().enumerate() {
        btn.draw(layout_rects[i], theme::TITLE_FG(), &mut areas);
    }
    // Window controls — IconButton widgets at their layout rects (the same
    // rects the hover bg used above; glyph is centered in each). Native on macOS.
    if !cfg!(target_os = "macos") {
        let tb_rects = layout.title_btn_rects();
        for (b, btn) in gpu.titlebar_btns.iter().enumerate() {
            let color = if app.hovered_titlebtn == Some(b) {
                theme::FG_ACTIVE()
            } else {
                theme::TITLE_FG()
            };
            btn.draw(tb_rects[b], color, &mut areas);
        }
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
        // Reserve the action-icon strip (Explorer only) so the title can't render under
        // the icons when the sidebar is narrow.
        let mut hdr = layout.sidebar_header_rect();
        if app.sidebar_view == SidebarView::Explorer {
            hdr.w = (hdr.w - (4.0 * theme::zpx(26.0) + theme::zpx(10.0))).max(0.0);
        }
        ui.sidebar_header
            .push(layout.sidebar.x + theme::zpx(12.0), hdr, theme::FG_DIM(), &mut areas);
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
            // The IconList draws each row's text + centered icon overlay together.
            // Inline-create splits the list into two slices (above/below the insert
            // slot), with the create field drawn between them.
            if let Some(pc) = app.explorer.creating.as_ref() {
                let rowh = theme::TREE_ROW_HEIGHT();
                // Scrolled tree origin: rows (and the inline create field) shift up by `sy`.
                let stop = tr.y - sy;
                let (_, icon_rect, field) = create_row_geometry(Rect { y: stop, ..tr }, pc.row, pc.depth);
                let split = stop + pc.row as f32 * rowh; // top of the create row
                if split > tr.y {
                    let clip_a = Rect { x: tr.x, y: tr.y, w: tr.w, h: (split - tr.y).min(tr.h) };
                    ui.sidebar.draw_slice(clip_a, stop, theme::FG_TEXT(), &mut areas);
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
                // Rows at/after the insert slot shift down one row; draw the buffer
                // shifted so row (pc.row) lands one slot lower.
                ui.sidebar.draw_slice(clip_b, stop + rowh, theme::FG_TEXT(), &mut areas);
            } else {
                ui.sidebar.draw_slice(tr, tr.y - sy, theme::FG_TEXT(), &mut areas);
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
                sp.draw_text(layout.sidebar, &mut areas);
            }
        } else if app.sidebar_view == SidebarView::SourceControl {
            if let Some(scp) = app.source_control.as_ref() {
                scp.draw_text(layout.panel_region(), &mut areas);
            }
        }
    }
    // Explorer OUTLINE section text: chevron header + symbol rows.
    if let (Some(hdr), Some(body)) = (layout.outline_header_rect(), layout.outline_body_rect()) {
        if let Some(o) = app.outline.as_ref() {
            let region = Rect { x: hdr.x, y: hdr.y, w: hdr.w, h: hdr.h + body.h };
            o.draw_text(region, &mut areas);
        }
    }
    // Secondary sidebar (AI chat) text: header, messages, input.
    if app.right_sidebar_visible && layout.right_sidebar.w > 0.0 {
        if let Some(c) = app.chat.as_ref() {
            c.draw_text(layout.right_sidebar, &mut areas);
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
        // File-type icon overlay at the tab's left (Seti brand glyph), centered.
        let icon = if i < app.workspace.documents.len() {
            Some(theme::file_icon(&app.workspace.documents[i].name))
        } else if app.detail.open_extension.is_some() {
            Some((theme::ICON_EXTENSIONS, theme::FG_DIM()))
        } else {
            None
        };
        let label_x = if let Some((glyph, gcolor)) = icon {
            if let Some(ib) = ui.tab_icons.get(&glyph) {
                let slot = Rect { x: tab.x + theme::zpx(8.0), y: tab.y, w: theme::zpx(20.0), h: tab.h };
                ib.draw_clipped(slot, *tab, gcolor, &mut areas);
            }
            tab.x + theme::zpx(30.0)
        } else {
            tab.x + theme::zpx(12.0)
        };
        let label_top = tab.text_top(theme::UI_LINE_HEIGHT(), VAlign::Center);
        areas.push(TextArea {
            buffer: &ui.tabs,
            left: label_x,
            top: label_top - line_top,
            scale: 1.0,
            // Clip to just this label's line band (the buffer holds every tab's
            // label, one per line) so neighbours don't bleed in.
            bounds: TextBounds {
                left: label_x as i32,
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
        } else if let Some(g) = d.graph.as_ref() {
            // Commit graph: lanes are drawn in the quad phase; here draw the per-row
            // text (refs + subject + author/date) offset past the lane columns.
            let et = layout.editor_text;
            let lane_w = theme::zpx(14.0);
            let graph_w = (g.max_col + 1) as f32 * lane_w + theme::zpx(10.0);
            let clip = TextBounds {
                left: et.x as i32,
                top: et.y as i32,
                right: (et.x + et.w) as i32,
                bottom: (et.y + et.h) as i32,
            };
            areas.push(TextArea {
                buffer: &d.buffer,
                left: et.x + theme::EDITOR_PAD() + graph_w - d.scroll_x(),
                top: et.y + theme::EDITOR_PAD() - d.scroll_y(),
                scale: 1.0,
                bounds: clip,
                default_color: theme::FG_TEXT(),
                custom_glyphs: &[],
            });
        } else if let Some(page) = d.info.as_ref() {
            // Info tab: the hand-designed page draws its own text in a centered
            // reading column (its quads are added in the quad-build phase above).
            let body = crate::ui::info_page::InfoPage::body(editor_region(&layout));
            page.draw(body, d.scroll_y(), &mut areas);
        } else if let Some(right) = d.diff_right.as_ref() {
            // Side-by-side diff: two gutters + two text panes over the full region.
            // Geometry (incl. the reserved per-block action column) comes from the
            // shared helper so numbers/text never collide with the buttons.
            let full = editor_region(&layout);
            let (lg, lt, rg, rt) = diff_pane_rects(full);
            ui.line_numbers.draw(lg, d.scroll_y(), theme::FG_GUTTER(), &mut areas);
            ui.line_numbers2.draw(rg, d.scroll_y(), theme::FG_GUTTER(), &mut areas);
            // Each pane scrolls horizontally on its own; clamp this frame (widths
            // shift as rows window in/out) then offset the text by the pane's own x.
            d.diff_clamp_h(0, lt.w);
            d.diff_clamp_h(1, rt.w);
            for (i, (buf, r)) in [(&d.buffer, lt), (right, rt)].into_iter().enumerate() {
                areas.push(TextArea {
                    buffer: buf,
                    left: r.x + theme::EDITOR_PAD() - d.diff_hx(i),
                    // Vertical offset is handled by the buffer's own scroll (window_diff),
                    // so run positions are already viewport-relative.
                    top: r.y + theme::EDITOR_PAD(),
                    scale: 1.0,
                    bounds: TextBounds { left: r.x as i32, top: r.y as i32, right: (r.x + r.w) as i32, bottom: (r.y + r.h) as i32 },
                    default_color: theme::FG_TEXT(),
                    custom_glyphs: &[],
                });
            }
            // Combined view: overlay the codicon twistie on each visible file header
            // (same chevron as the sidebar tree), flipped to match collapse state.
            if let Some(diff) = d.diff.as_ref() {
                if diff.combined {
                    for run in d.buffer.layout_runs() {
                        // line_top is viewport-relative (window_diff sets the buffer scroll).
                        let y = lt.y + theme::EDITOR_PAD() + run.line_top;
                        if y > lt.y + lt.h {
                            break; // ordered runs; past the visible area
                        }
                        let Some(row) = diff.rows.get(run.line_i) else { continue };
                        if row.kind != crate::diff::RowKind::File || y + run.line_height < lt.y {
                            continue;
                        }
                        let chev = if d.diff_collapsed.contains(&row.file) {
                            &ui.diff_chev_right
                        } else {
                            &ui.diff_chev_down
                        };
                        let cr = Rect { x: lt.x + theme::EDITOR_PAD(), y, w: theme::zpx(18.0), h: run.line_height };
                        chev.push(cr.x, cr, theme::FG_TEXT(), &mut areas);
                    }
                }
                // Single-file view: an "unfold" (expand-all) button centered in each
                // gutter on a collapsed-gap separator row.
                if !diff.combined {
                    for run in d.buffer.layout_runs() {
                        let y = lt.y + theme::EDITOR_PAD() + run.line_top;
                        if y > lt.y + lt.h {
                            break;
                        }
                        let Some(row) = diff.rows.get(run.line_i) else { continue };
                        if row.kind != crate::diff::RowKind::Gap || y + run.line_height < lt.y {
                            continue;
                        }
                        for gutter in [lg, rg] {
                            let r = Rect { x: gutter.x, y, w: gutter.w, h: run.line_height };
                            ui.diff_unfold.draw_center(r, theme::FG_GUTTER_ACTIVE(), &mut areas);
                        }
                    }
                }
            }
            // Per-block Stage/Revert (or Unstage) button glyphs for the hovered block.
            if d.diff_path.is_some() {
                if let Some((vbs, vbe)) = app.hovered_diff_block {
                    let icons: &[&crate::widgets::TextLabel] = if d.diff_staged {
                        &[&ui.diff_unstage]
                    } else {
                        &[&ui.diff_revert, &ui.diff_stage]
                    };
                    if let Some(rects) = diff_block_btn_rects(full, vbs, vbe, d.scroll_y(), icons.len()) {
                        for (icon, r) in icons.iter().zip(rects) {
                            icon.draw_center(r, theme::FG_TEXT(), &mut areas);
                        }
                    }
                }
            }
        } else if d.large {
            // Large-file mode: the buffer holds a shaped window of lines; draw it
            // shifted to the window's document position (no folds, no wrap).
            let et = layout.editor_text;
            let g = layout.gutter;
            let off = d.buf_offset_px();
            let clip = TextBounds {
                left: et.x as i32,
                top: et.y as i32,
                right: (et.x + et.w) as i32,
                bottom: (et.y + et.h) as i32,
            };
            areas.push(TextArea {
                buffer: &d.buffer,
                left: et.x + theme::EDITOR_PAD() - d.scroll_x(),
                top: et.y + theme::EDITOR_PAD() - d.scroll_y() + off,
                scale: 1.0,
                bounds: clip,
                default_color: theme::FG_TEXT(),
                custom_glyphs: &[],
            });
            ui.line_numbers.draw_clipped(
                Rect { x: g.x, y: g.y, w: g.w, h: g.h },
                g.y + theme::EDITOR_PAD() - d.scroll_y() + off,
                theme::FG_GUTTER(),
                &mut areas,
            );
        } else {
            // Fold-aware rendering: draw the text + gutter as visible line segments
            // (collapsed regions are simply not drawn, and everything below shifts up).
            let et = layout.editor_text;
            let g = layout.gutter;
            let lh = theme::LINE_HEIGHT();
            let total = d.rope.len_lines();
            let (etop, ebot) = (et.y, et.y + et.h);
            for (a, b) in fold_segments(d, total) {
                let off = lh * d.hidden_above(a) as f32;
                let seg_top = et.y + theme::EDITOR_PAD() + a as f32 * lh - d.scroll_y() - off;
                let seg_bot = seg_top + (b - a + 1) as f32 * lh;
                let cy0 = seg_top.max(etop);
                let cy1 = seg_bot.min(ebot);
                if cy1 <= cy0 {
                    continue; // segment fully scrolled off
                }
                let text_top = et.y + theme::EDITOR_PAD() - d.scroll_y() - off;
                let clip = Rect { x: et.x, y: cy0, w: et.w, h: cy1 - cy0 };
                areas.push(TextArea {
                    buffer: &d.buffer,
                    left: et.x + theme::EDITOR_PAD() - d.scroll_x(),
                    top: text_top,
                    scale: 1.0,
                    bounds: TextBounds {
                        left: clip.x as i32,
                        top: clip.y as i32,
                        right: (clip.x + clip.w) as i32,
                        bottom: (clip.y + clip.h) as i32,
                    },
                    default_color: theme::FG_TEXT(),
                    custom_glyphs: &[],
                });
                let g_top = g.y + theme::EDITOR_PAD() - d.scroll_y() - off;
                ui.line_numbers
                    .draw_clipped(Rect { x: g.x, y: cy0, w: g.w, h: cy1 - cy0 }, g_top, theme::FG_GUTTER(), &mut areas);
            }
            // Fold chevrons in the gutter: ▸ for folded headers, ▾ for foldable ones.
            // Walk the SAME visible segments the text is drawn in — folded lines take
            // no screen space but DO consume raw line numbers, so a fixed raw-line
            // window stops short of the bottom and drops every chevron below a fold.
            if d.diff.is_none() {
                for (a, b) in fold_segments(d, total) {
                    let off = lh * d.hidden_above(a) as f32;
                    let seg_top = et.y + theme::EDITOR_PAD() + a as f32 * lh - d.scroll_y() - off;
                    let seg_bot = seg_top + (b - a + 1) as f32 * lh;
                    if seg_bot < etop || seg_top > ebot {
                        continue; // segment fully off-screen
                    }
                    // Scan only the lines whose row is actually visible.
                    let lo = if seg_top < etop { a + ((etop - seg_top) / lh) as usize } else { a };
                    let hi = if seg_bot > ebot { a + ((ebot - seg_top) / lh) as usize } else { b };
                    for line in lo..=hi.min(b) {
                        if !d.is_foldable(line) {
                            continue;
                        }
                        let y = et.y + theme::EDITOR_PAD() + line as f32 * lh - d.scroll_y() - off;
                        let folded = d.is_folded(line);
                        let chev = if folded { &ui.diff_chev_right } else { &ui.diff_chev_down };
                        let color = if folded { theme::FG_TEXT() } else { theme::FG_DIM() };
                        let cr = Rect { x: g.x + g.w - theme::zpx(16.0), y, w: theme::zpx(14.0), h: lh };
                        chev.push(cr.x, cr, color, &mut areas);
                    }
                }
            }
        }
    }

    // Drag ghost label (pill quads pushed before the quad prepare above).
    if let Some(pill) = drag_ghost_pill {
        ui.drag_ghost.push(pill.x + theme::zpx(8.0), pill, theme::FG_TEXT(), &mut areas);
    }

    // Status bar — left: path; right: position/encoding/etc. Both via the
    // reusable TextLabel (left-padded and right-padded alignment helpers).
    // Git-branch indicator at the far left; the path is shifted right past it.
    let sb = layout.status_bar;
    let branch_block_w = if ui.branch.width() > 0.0 {
        let icon_x = sb.x + theme::zpx(10.0);
        let name_x = icon_x + ui.branch_icon.width() + theme::zpx(2.0);
        let sfg0 = theme::STATUS_BAR_FG();
        ui.branch_icon.push(icon_x, sb, sfg0, &mut areas);
        ui.branch.push(name_x, sb, sfg0, &mut areas);
        (name_x + ui.branch.width() + theme::zpx(12.0)) - sb.x
    } else {
        0.0
    };
    ui.status
        .draw_left(layout.status_bar, branch_block_w.max(theme::zpx(12.0)), theme::STATUS_BAR_FG(), &mut areas);
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

    // (The find/replace widget draws in its own late pass — see below.)

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
    } // end: background text

    // (The command palette draws in its own late pass — see below.)

    let t_prep = std::time::Instant::now();
    let n_areas = areas.len();
    gpu.text_renderer.prepare(
        &gpu.device,
        &gpu.queue,
        &mut gpu.font_system,
        &mut gpu.atlas,
        &gpu.viewport,
        areas,
        &mut gpu.swash_cache,
    )?;
    let prep = t_prep.elapsed();
    if prep > std::time::Duration::from_millis(8) {
        crate::perf::log(&format!("frame text prepare: {prep:?} ({n_areas} areas)"));
    }

    // ---- Submit ----
    let frame = gpu.surface.get_current_texture()?;
    // A pending feedback screenshot redirects this frame into an offscreen
    // COPY_SRC texture (the surface texture can't be read back), so we can grab it
    // as PNG after the passes run.
    let capture = app.pending_capture.take();
    let cap_tex = capture.is_some().then(|| {
        gpu.device.create_texture(&wgpu::TextureDescriptor {
            label: Some("aether-capture"),
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
        label: Some("aether-encoder"),
    });
    {
        let mut pass = encoder.begin_render_pass(&RenderPassDescriptor {
            label: Some("aether-pass"),
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
    // Suppressed under a modal (palette/dialog/feedback) — these are quads, so the
    // text-suppression guard above doesn't cover them and they'd bleed through.
    if app.detail.open_extension.is_some() && !modal_open {
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
                label: Some("aether-media-pass"),
            });
            {
                let mut pass = enc.begin_render_pass(&RenderPassDescriptor {
                    label: Some("aether-media"),
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
                label: Some("aether-image-pass"),
            });
            {
                let mut pass = enc.begin_render_pass(&RenderPassDescriptor {
                    label: Some("aether-image"),
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
            label: Some("aether-ext-pass"),
        });
        {
            let mut pass = enc.begin_render_pass(&RenderPassDescriptor {
                label: Some("aether-ext"),
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

    // ---- Code-completion popup (late pass over the editor) ----
    if let (Some((ax, ay)), true) = (completion_anchor, app.completion.active && !app.completion.items.is_empty()) {
        let total = app.completion.items.len();
        let visible = total.min(crate::completion::VISIBLE_ROWS);
        let row_h = theme::TREE_ROW_HEIGHT();
        let pad = theme::zpx(4.0);
        let pw = theme::zpx(340.0);
        let ph = pad * 2.0 + row_h * visible as f32;
        // Keep on-screen: clamp right, and flip above the line if there's no room below.
        let x = (ax).min(cfg_w as f32 - pw - theme::zpx(8.0)).max(theme::zpx(8.0));
        let y = if ay + ph > cfg_h as f32 - theme::zpx(8.0) {
            (ay - row_h - ph).max(theme::zpx(8.0))
        } else {
            ay
        };
        let card = crate::widgets::Rect { x, y, w: pw, h: ph };
        let inner = crate::widgets::Rect { x: card.x + pad, y: card.y + pad, w: card.w - pad * 2.0, h: ph - pad * 2.0 };
        let r = theme::zpx(6.0);
        let mut cq: Vec<Quad> = Vec::new();
        cq.push(crate::widgets::Rect { x: card.x - 1.0, y: card.y - 1.0, w: card.w + 2.0, h: card.h + 2.0 }
            .rounded_quad(theme::SEARCH_BORDER(), r + 1.0));
        cq.push(card.rounded_quad(theme::SEARCH_BG(), r));
        // Selected-row highlight (relative to the scroll window).
        let sel_rel = app.completion.selected.saturating_sub(app.completion.scroll);
        let sy = inner.y + sel_rel as f32 * row_h;
        cq.push(crate::widgets::Rect { x: card.x + pad * 0.5, y: sy, w: card.w - pad, h: row_h }
            .rounded_quad(theme::MENU_HOVER(), theme::zpx(4.0)));

        // Shape the rows into the shared list buffer (tall enough for all rows): a
        // colored kind icon + the label, like VSCode's suggest widget.
        let ui_attrs = Attrs::new().family(Family::Name(theme::UI_FAMILY())).color(theme::FG_TEXT());
        let mut key = String::from("CMP\n");
        let mut spans: Vec<(String, Attrs)> = Vec::new();
        for it in &app.completion.items {
            let (g, col) = crate::completion::kind_icon(it.kind);
            spans.push((format!("{}  ", g), Attrs::new().family(Family::Name(theme::ICON_FAMILY)).color(col)));
            spans.push((format!("{}\n", it.label), ui_attrs));
            key.push_str(&it.label);
            key.push('\n');
        }
        let content_h = total as f32 * row_h + row_h;
        gpu.ui.completion_list.set_rich(&mut gpu.font_system, &key, &spans, inner.w, content_h);
        let scroll_px = app.completion.scroll as f32 * row_h;

        gpu.quad_renderer.prepare(&gpu.device, &gpu.queue, &cq, &[], (cfg_w, cfg_h));
        let mut careas: Vec<TextArea> = Vec::new();
        gpu.ui.completion_list.draw_at(inner, inner.y - scroll_px, theme::FG_TEXT(), &mut careas);
        gpu.text_renderer.prepare(
            &gpu.device,
            &gpu.queue,
            &mut gpu.font_system,
            &mut gpu.atlas,
            &gpu.viewport,
            careas,
            &mut gpu.swash_cache,
        )?;
        let mut encc = gpu.device.create_command_encoder(&CommandEncoderDescriptor {
            label: Some("aether-completion-pass"),
        });
        {
            let mut pass = encc.begin_render_pass(&RenderPassDescriptor {
                label: Some("aether-completion"),
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
            // Clip the list text to the inner card so scrolled rows don't spill.
            let sx = inner.x.max(0.0) as u32;
            let syc = inner.y.max(0.0) as u32;
            let sw = inner.w.max(0.0) as u32;
            let sh = inner.h.max(0.0) as u32;
            if sw > 0 && sh > 0 {
                pass.set_scissor_rect(sx, syc, sw, sh);
                gpu.text_renderer.render(&gpu.atlas, &gpu.viewport, &mut pass)?;
            }
        }
        gpu.queue.submit(Some(encc.finish()));
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
                label: Some("aether-menudd-pass"),
            });
            {
                let mut pass = encm.begin_render_pass(&RenderPassDescriptor {
                    label: Some("aether-menudd"),
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

    // ---- Command palette overlay ----
    // Its own pass over the live background (no dim) so the file stays visible for
    // symbol/line navigation; the card's shadow + border keep it legible on top.
    if let Some(pal) = layout.palette.as_ref() {
        let mut pq: Vec<Quad> = Vec::new();
        let mut pfg: Vec<Quad> = Vec::new();
        let radius = theme::zpx(12.0);
        for i in 1..=7 {
            let s = i as f32 * theme::zpx(2.5);
            let a = 0.22 * (1.0 - (i as f32 - 1.0) / 7.0);
            pq.push(
                Rect { x: pal.box_.x - s, y: pal.box_.y - s + theme::zpx(4.0), w: pal.box_.w + s * 2.0, h: pal.box_.h + s * 2.0 }
                    .rounded_quad([0.0, 0.0, 0.0, a], radius + s),
            );
        }
        pq.push(Rect { x: pal.box_.x - 1.0, y: pal.box_.y - 1.0, w: pal.box_.w + 2.0, h: pal.box_.h + 2.0 }.rounded_quad(theme::PALETTE_BORDER(), radius + 1.0));
        pq.push(pal.box_.rounded_quad(theme::PALETTE_BG(), radius));
        // The input is the title-bar pill: re-skin it as a focused input (accent
        // border + input bg) right where the static search label normally sits.
        let ir = pal.input.h * 0.5;
        pq.push(Rect { x: pal.input.x - 1.0, y: pal.input.y - 1.0, w: pal.input.w + 2.0, h: pal.input.h + 2.0 }.rounded_quad(theme::ACCENT_DIM(), ir + 1.0));
        pq.push(pal.input.rounded_quad(theme::PALETTE_INPUT_BG(), ir));
        // Scroll the list so the selection stays in view; clamp to content.
        let row_h = theme::PALETTE_ROW_HEIGHT();
        let n = app.palette.filtered.len();
        let content_h = n as f32 * row_h;
        let mut scroll = app.palette.scroll;
        // Follow the selection into view only when it just moved (so the mouse wheel
        // can scroll freely without snapping back to the selection).
        if app.palette.follow_selection {
            let sel_top = app.palette.selected as f32 * row_h;
            if sel_top < scroll {
                scroll = sel_top;
            }
            if sel_top + row_h > scroll + pal.list.h {
                scroll = sel_top + row_h - pal.list.h;
            }
            app.palette.follow_selection = false;
        }
        scroll = scroll.clamp(0.0, (content_h - pal.list.h).max(0.0));
        app.palette.scroll = scroll;
        if !app.palette.filtered.is_empty() {
            let r = gpu.ui.palette_list.row_rect(pal.list, app.palette.selected);
            let py = r.y - scroll;
            // Only draw the selection pill when it's within the visible list band.
            if py + r.h > pal.list.y && py < pal.list.y + pal.list.h {
                let pill = Rect { x: r.x + theme::zpx(4.0), y: py + theme::zpx(1.0), w: r.w - theme::zpx(8.0), h: (r.h - theme::zpx(2.0)).max(2.0) };
                pq.push(pill.rounded_quad(theme::PALETTE_SELECTED(), theme::zpx(6.0)));
            }
        }
        gpu.ui.palette_input.selection_quads(pal.input, theme::zpx(14.0), &mut pq);
        if app.cursor_blink_on {
            pfg.push(gpu.ui.palette_input.caret_quad(pal.input, theme::zpx(14.0)));
        }
        // Shortcut keycap pills, right-aligned per Commands-mode row. Built here so
        // their fills land in `pq` (drawn under the glyphs); the `Keycaps` own their
        // glyph buffers, so they're kept in `row_caps` until the text pass prepares.
        let mut row_caps: Vec<crate::widgets::Keycaps> = Vec::new();
        if app.palette.mode == crate::commands::PaletteMode::Commands {
            let right = pal.list.x + pal.list.w - theme::zpx(12.0);
            for (pos, &i) in app.palette.filtered.iter().enumerate() {
                let shortcut = COMMANDS[i].2;
                if shortcut.is_empty() {
                    continue;
                }
                let r = gpu.ui.palette_list.row_rect(pal.list, pos);
                let cy = r.y - scroll + r.h * 0.5;
                // Skip rows scrolled out of the visible list band.
                if cy + r.h * 0.5 < pal.list.y || cy - r.h * 0.5 > pal.list.y + pal.list.h {
                    continue;
                }
                let caps = crate::widgets::Keycaps::new(&mut gpu.font_system, shortcut, right, cy);
                caps.quads(&mut pq);
                row_caps.push(caps);
            }
        }
        // Scrollbar thumb for the list (when it overflows).
        if content_h > pal.list.h {
            let track_h = pal.list.h;
            let thumb_h = (track_h * (track_h / content_h)).max(theme::zpx(20.0));
            let thumb_y = pal.list.y + (scroll / (content_h - track_h)) * (track_h - thumb_h);
            let tw = theme::zpx(4.0);
            pq.push(Rect { x: pal.list.x + pal.list.w - tw - theme::zpx(2.0), y: thumb_y, w: tw, h: thumb_h }.rounded_quad([0.6, 0.64, 0.78, 0.5], tw * 0.5));
        }
        gpu.quad_renderer.prepare(&gpu.device, &gpu.queue, &pq, &pfg, (cfg_w, cfg_h));
        let mut pareas: Vec<TextArea> = Vec::new();
        gpu.ui.palette_input.draw(pal.input, theme::zpx(14.0), theme::FG_TEXT(), &mut pareas);
        gpu.ui.palette_list.draw_at(pal.list, pal.list.y - scroll, theme::FG_TEXT(), &mut pareas);
        for caps in &row_caps {
            caps.draw(theme::FG_TEXT(), &mut pareas);
        }
        gpu.text_renderer.prepare(&gpu.device, &gpu.queue, &mut gpu.font_system, &mut gpu.atlas, &gpu.viewport, pareas, &mut gpu.swash_cache)?;
        let mut encp = gpu.device.create_command_encoder(&CommandEncoderDescriptor { label: Some("aether-palette-pass") });
        {
            let mut pass = encp.begin_render_pass(&RenderPassDescriptor {
                label: Some("aether-palette"),
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
        gpu.queue.submit(Some(encp.finish()));
    }

    // ---- Settings editor overlay ----
    if app.settings_editor.open {
        draw_settings_editor(&mut app.settings_editor, gpu, &view, cfg_w, cfg_h, app.cursor_blink_on)?;
    }

    // ---- Find/replace widget overlay ----
    // Floats over the editor's top-right; its own pass so editor text never bleeds
    // over the card. Suppressed while a centered modal owns the screen.
    if app.find.active && layout.palette.is_none() && app.dialog.is_none() && app.feedback_form.is_none() && !app.settings_editor.open {
        let er = editor_region(&layout);
        let fl = crate::ui::find_widget::FindWidget::layout(er, app.find.replace_open);
        let opts = [app.find.opts.case_sensitive, app.find.opts.whole_word, app.find.opts.regex];
        let mut fq: Vec<Quad> = Vec::new();
        let mut ffg: Vec<Quad> = Vec::new();
        gpu.ui.find.draw_quads(&fl, app.find.focused, app.find.on_replace, opts, app.cursor_blink_on, &mut fq, &mut ffg);
        gpu.quad_renderer.prepare(&gpu.device, &gpu.queue, &fq, &ffg, (cfg_w, cfg_h));
        let mut fareas: Vec<TextArea> = Vec::new();
        gpu.ui.find.draw_text(&fl, app.find.replace_open, opts, &mut fareas);
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
            label: Some("aether-find-pass"),
        });
        {
            let mut pass = encf.begin_render_pass(&RenderPassDescriptor {
                label: Some("aether-find"),
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

    // ---- Small label tooltip ----
    // Reused for the per-block diff buttons (Stage/Revert/Unstage) and the editor
    // tab full-name on hover (tabs truncate to fit). Only one is set at a time.
    if app.dialog.is_none() && app.feedback_form.is_none() {
        if let Some((text, ax, ay)) = app.block_tip.clone().or_else(|| app.tab_tip.clone()) {
            gpu.ui.block_tip.set(&mut gpu.font_system, &text, theme::UI_FAMILY());
            let padx = theme::zpx(7.0);
            let w = gpu.ui.block_tip.width() + padx * 2.0;
            let h = theme::UI_LINE_HEIGHT() + theme::zpx(4.0);
            // Prefer to the right of the button; flip left if it would overflow.
            let mut bx = ax + theme::zpx(6.0);
            if bx + w > cfg_w as f32 {
                bx = ax - theme::zpx(26.0) - w;
            }
            let box_ = Rect { x: bx, y: ay, w, h };
            let mut tq: Vec<Quad> = Vec::new();
            let ir = theme::zpx(5.0);
            tq.push(Rect { x: box_.x - 1.0, y: box_.y - 1.0, w: box_.w + 2.0, h: box_.h + 2.0 }.rounded_quad(theme::SEARCH_BORDER(), ir + 1.0));
            tq.push(box_.rounded_quad(theme::PANEL_BG(), ir));
            gpu.quad_renderer.prepare(&gpu.device, &gpu.queue, &tq, &[], (cfg_w, cfg_h));
            let mut tareas: Vec<TextArea> = Vec::new();
            gpu.ui.block_tip.draw_left(box_, padx, theme::FG_TEXT(), &mut tareas);
            gpu.text_renderer.prepare(&gpu.device, &gpu.queue, &mut gpu.font_system, &mut gpu.atlas, &gpu.viewport, tareas, &mut gpu.swash_cache)?;
            let mut entt = gpu.device.create_command_encoder(&CommandEncoderDescriptor { label: Some("aether-blocktip-pass") });
            {
                let mut pass = entt.begin_render_pass(&RenderPassDescriptor {
                    label: Some("aether-blocktip"),
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
            gpu.queue.submit(Some(entt.finish()));
        }
    }

    // ---- Commit-graph message hover card ----
    if app.dialog.is_none() && app.feedback_form.is_none() {
        if let Some((msg, hx, hy)) = app.commit_tip.clone() {
            gpu.ui.commit_card.set_text(&mut gpu.font_system, &msg);
            let screen = Rect { x: 0.0, y: 0.0, w: cfg_w as f32, h: cfg_h as f32 };
            let card = gpu.ui.commit_card.rect((hx, hy), screen);
            let mut hq: Vec<Quad> = Vec::new();
            gpu.ui.commit_card.draw_quads(card, &mut hq);
            gpu.quad_renderer.prepare(&gpu.device, &gpu.queue, &hq, &[], (cfg_w, cfg_h));
            let mut hareas: Vec<TextArea> = Vec::new();
            gpu.ui.commit_card.draw_text(card, &mut hareas);
            gpu.text_renderer.prepare(&gpu.device, &gpu.queue, &mut gpu.font_system, &mut gpu.atlas, &gpu.viewport, hareas, &mut gpu.swash_cache)?;
            let mut enc = gpu.device.create_command_encoder(&CommandEncoderDescriptor { label: Some("aether-commitcard-pass") });
            {
                let mut pass = enc.begin_render_pass(&RenderPassDescriptor {
                    label: Some("aether-commitcard"),
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
            gpu.queue.submit(Some(enc.finish()));
        }
    }

    // ---- Diagnostic hover card overlay ----
    // Floats above the editor, below any modal. Only when the pointer rests on a
    // squiggle and no modal is open.
    if app.dialog.is_none() && app.feedback_form.is_none() {
        if let Some((info, hx, hy)) = app.hover_tip.clone() {
            gpu.ui.diag_hover.set(&mut gpu.font_system, &info);
            let screen = Rect { x: 0.0, y: 0.0, w: cfg_w as f32, h: cfg_h as f32 };
            let card = gpu.ui.diag_hover.rect((hx, hy), screen);
            let mut hq: Vec<Quad> = Vec::new();
            gpu.ui.diag_hover.draw_quads(card, &mut hq);
            gpu.quad_renderer.prepare(&gpu.device, &gpu.queue, &hq, &[], (cfg_w, cfg_h));
            let mut hareas: Vec<TextArea> = Vec::new();
            gpu.ui.diag_hover.draw_text(card, &mut hareas);
            gpu.text_renderer.prepare(
                &gpu.device,
                &gpu.queue,
                &mut gpu.font_system,
                &mut gpu.atlas,
                &gpu.viewport,
                hareas,
                &mut gpu.swash_cache,
            )?;
            let mut ench = gpu.device.create_command_encoder(&CommandEncoderDescriptor {
                label: Some("aether-hovercard-pass"),
            });
            {
                let mut pass = ench.begin_render_pass(&RenderPassDescriptor {
                    label: Some("aether-hovercard"),
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
            gpu.queue.submit(Some(ench.finish()));
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
            label: Some("aether-dialog-pass"),
        });
        {
            let mut pass = enc3.begin_render_pass(&RenderPassDescriptor {
                label: Some("aether-dialog"),
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
            label: Some("aether-feedback-pass"),
        });
        {
            let mut pass = encf.begin_render_pass(&RenderPassDescriptor {
                label: Some("aether-feedback"),
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

    // ---- Generic right-click context menu (editor / tabs / SCM / settings dropdowns) ----
    // Drawn last so it sits above every modal (palette / settings / dialog / feedback).
    if let Some((anchor, _)) = app.ctx_menu {
        let menu = gpu.ui.ctx.rect(anchor, (cfg_w as f32, cfg_h as f32));
        let mut mq: Vec<Quad> = Vec::new();
        gpu.ui.ctx.draw_bg(menu, app.ctx_hover, &mut mq);
        gpu.quad_renderer.prepare(&gpu.device, &gpu.queue, &mq, &[], (cfg_w, cfg_h));
        let mut mareas: Vec<TextArea> = Vec::new();
        gpu.ui.ctx.draw(menu, &mut mareas);
        gpu.text_renderer.prepare(
            &gpu.device,
            &gpu.queue,
            &mut gpu.font_system,
            &mut gpu.atlas,
            &gpu.viewport,
            mareas,
            &mut gpu.swash_cache,
        )?;
        let mut encx = gpu.device.create_command_encoder(&CommandEncoderDescriptor {
            label: Some("aether-ctx-pass"),
        });
        {
            let mut pass = encx.begin_render_pass(&RenderPassDescriptor {
                label: Some("aether-ctx"),
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
        gpu.queue.submit(Some(encx.finish()));
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
        label: Some("aether-capture-readback"),
        size: (padded * h) as u64,
        usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
        mapped_at_creation: false,
    });
    let mut enc = gpu
        .device
        .create_command_encoder(&CommandEncoderDescriptor { label: Some("aether-capture-copy") });
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

/// Draw the Settings editor modal in its own pass over the live frame. Mirrors the
/// command-palette pass: builds its own quad + text lists, then submits a Load pass.
/// Also writes back the per-frame hit geometry (`rows_cache`, `cat_cache`,
/// `content_h`) the click handler reads.
fn draw_settings_editor(
    ed: &mut crate::ui::settings_editor::SettingsEditor,
    gpu: &mut crate::gpu::GpuState,
    view: &wgpu::TextureView,
    cfg_w: u32,
    cfg_h: u32,
    blink: bool,
) -> Result<()> {
    use crate::ui::settings_editor as se;
    use glyphon::{Buffer, Color, Metrics};

    fn make_buf(fs: &mut glyphon::FontSystem, text: &str, family: &str, fz: f32, lh: f32, wrap: Option<f32>) -> Buffer {
        let mut b = Buffer::new(fs, Metrics::new(fz, lh));
        b.set_size(fs, wrap, None);
        b.set_text(fs, text, Attrs::new().family(Family::Name(family)), Shaping::Advanced);
        b.shape_until_scroll(fs, false);
        b
    }
    fn buf_lines(b: &Buffer) -> usize {
        b.layout_runs().count().max(1)
    }

    struct T {
        buf: Buffer,
        left: f32,
        top: f32,
        color: Color,
        clip: TextBounds,
    }

    let screen = Rect { x: 0.0, y: 0.0, w: cfg_w as f32, h: cfg_h as f32 };
    let lay = se::layout(screen);
    let ui_fam = theme::UI_FAMILY();
    let icon_fam = theme::ICON_FAMILY;
    let fz = theme::UI_FONT_SIZE();
    let lh = theme::UI_LINE_HEIGHT();
    let fg_text = theme::FG_TEXT();
    let fg_dim = theme::FG_DIM();
    let radius = theme::zpx(10.0);

    let bounds = |r: Rect| TextBounds {
        left: r.x.floor() as i32,
        top: r.y.floor() as i32,
        right: (r.x + r.w).ceil() as i32,
        bottom: (r.y + r.h).ceil() as i32,
    };

    let mut q: Vec<Quad> = Vec::new();
    let fg: Vec<Quad> = Vec::new();
    let mut texts: Vec<T> = Vec::new();

    let fs = &mut gpu.font_system;

    // Scrim + card shadow + border + fill.
    q.push(screen.quad(theme::DIALOG_OVERLAY()));
    for i in 1..=7 {
        let s = i as f32 * theme::zpx(2.5);
        let a = 0.22 * (1.0 - (i as f32 - 1.0) / 7.0);
        q.push(
            Rect { x: lay.card.x - s, y: lay.card.y - s + theme::zpx(4.0), w: lay.card.w + s * 2.0, h: lay.card.h + s * 2.0 }
                .rounded_quad([0.0, 0.0, 0.0, a], radius + s),
        );
    }
    q.push(Rect { x: lay.card.x - 1.0, y: lay.card.y - 1.0, w: lay.card.w + 2.0, h: lay.card.h + 2.0 }.rounded_quad(theme::PALETTE_BORDER(), radius + 1.0));
    q.push(lay.card.rounded_quad(theme::PALETTE_BG(), radius));

    // Header: gear glyph + "Settings" title + close button.
    let pad = theme::zpx(16.0);
    let gear = make_buf(fs, &theme::ICON_SETTINGS.to_string(), icon_fam, fz, lh, None);
    texts.push(T { buf: gear, left: lay.header.x + pad, top: lay.header.y + (lay.header.h - lh) * 0.5, color: fg_dim, clip: bounds(lay.header) });
    let title = make_buf(fs, "Settings", ui_fam, fz, lh, None);
    texts.push(T { buf: title, left: lay.header.x + pad + theme::zpx(22.0), top: lay.header.y + (lay.header.h - lh) * 0.5, color: fg_text, clip: bounds(lay.header) });
    let close = make_buf(fs, &theme::ICON_CLOSE.to_string(), icon_fam, fz, lh, None);
    texts.push(T { buf: close, left: lay.close.x + (lay.close.w - theme::zpx(14.0)) * 0.5, top: lay.close.y + (lay.close.h - lh) * 0.5, color: fg_dim, clip: bounds(lay.close) });

    // Search box (background + border). Text/caret drawn from the widget below.
    let sr = lay.search;
    q.push(Rect { x: sr.x - 1.0, y: sr.y - 1.0, w: sr.w + 2.0, h: sr.h + 2.0 }.rounded_quad(theme::PALETTE_BORDER(), theme::zpx(5.0)));
    q.push(sr.rounded_quad(theme::PALETTE_INPUT_BG(), theme::zpx(4.0)));

    // Left accordion tree: collapsible category headers (chevron) + their settings
    // as indented leaves, like the Explorer. Clipped to the left column.
    let accent = theme::ACCENT();
    let cats = se::categories();
    let row_h = theme::zpx(26.0);
    let querying = !ed.query.trim().is_empty();
    let lclip = bounds(lay.left);
    ed.nav_cache.clear();
    let mut cy = lay.left.y;
    for (ci, name) in cats.iter().enumerate() {
        let has_children = ci != 0;
        let expanded = ci == 0 || ed.expanded.get(ci).copied().unwrap_or(true);
        let r = Rect { x: lay.left.x, y: cy, w: lay.left.w, h: row_h };
        // A category header is "active" when it's the selected category and no leaf
        // is the scroll target.
        let active = ci == ed.category && !querying;
        if active {
            q.push(r.rounded_quad(theme::TREE_SELECTED(), theme::zpx(4.0)));
        }
        let color = if active { fg_text } else { fg_dim };
        if has_children {
            let chev = make_buf(fs, &if expanded { theme::ICON_CHEVRON_DOWN } else { theme::ICON_CHEVRON_RIGHT }.to_string(), icon_fam, fz * 0.85, row_h, None);
            texts.push(T { buf: chev, left: r.x + theme::zpx(6.0), top: r.y + (row_h - lh) * 0.5, color: fg_dim, clip: lclip });
        }
        let nb = make_buf(fs, name, ui_fam, fz, row_h, None);
        texts.push(T { buf: nb, left: r.x + theme::zpx(22.0), top: r.y + (row_h - lh) * 0.5, color, clip: lclip });
        ed.nav_cache.push((r, se::Nav::Category(ci)));
        cy += row_h;
        if has_children && expanded {
            for idx in se::settings_in(ci) {
                let lr = Rect { x: lay.left.x, y: cy, w: lay.left.w, h: row_h };
                let leaf_active = ed.selected_key == Some(se::SCHEMA[idx].key);
                if leaf_active {
                    q.push(lr.rounded_quad(theme::TREE_SELECTED(), theme::zpx(4.0)));
                }
                let lc = if leaf_active { fg_text } else { fg_dim };
                // Strip the "Category: " prefix so the leaf reads like VSCode's short name.
                let title = se::SCHEMA[idx].title.split_once(": ").map(|(_, t)| t).unwrap_or(se::SCHEMA[idx].title);
                let lb = make_buf(fs, title, ui_fam, fz, row_h, None);
                texts.push(T { buf: lb, left: lr.x + theme::zpx(38.0), top: lr.y + (row_h - lh) * 0.5, color: lc, clip: lclip });
                ed.nav_cache.push((lr, se::Nav::Setting(idx)));
                cy += row_h;
            }
        }
    }

    // Right viewport: heading + setting rows (scrolled, clipped to the viewport).
    let right = lay.right;
    let cx = right.x;
    let content_w = right.w;
    let rclip = bounds(right);
    let mut yy = 0.0_f32; // content offset from the top of the scroll region
    let screen_y = |off: f32| right.y - ed.scroll + off;

    let heading = if querying { "Search Results".to_string() } else { cats[ed.category].to_string() };
    let head_lh = lh * 1.7;
    {
        let sy = screen_y(yy);
        if sy + head_lh > right.y && sy < right.y + right.h {
            let b = make_buf(fs, &heading, ui_fam, fz * 1.5, head_lh, None);
            texts.push(T { buf: b, left: cx, top: sy, color: fg_text, clip: rclip });
        }
    }
    yy += head_lh + theme::zpx(6.0);

    let visible = ed.visible();
    ed.rows_cache.clear();
    // Dropdowns (enum/theme) draw their box in the quad phase here and their text in
    // the area phase below; collect which to draw + where.
    let mut dd_draws: Vec<(&'static str, Rect)> = Vec::new();
    // Content offset of a left-tree-clicked setting, so we can scroll it into view.
    let mut scroll_target_off: Option<f32> = None;
    let desc_w = content_w - theme::zpx(8.0);
    let input_w = theme::zpx(300.0).min(content_w * 0.7);
    let input_h = theme::zpx(28.0);
    let box_radius = theme::zpx(4.0);

    for &idx in &visible {
        let d = &se::SCHEMA[idx];
        let cur = crate::settings::value_json(d.key);
        // Measure the description's wrapped height first so the control sits below it.
        let desc_buf = make_buf(fs, d.desc, ui_fam, fz, lh, Some(desc_w));
        let desc_h = buf_lines(&desc_buf) as f32 * lh;

        let title_off = yy;
        if ed.scroll_to == Some(d.key) {
            scroll_target_off = Some(title_off);
        }
        let desc_off = title_off + lh + theme::zpx(2.0);
        let control_off = desc_off + desc_h + theme::zpx(8.0);
        let control_h = match d.control {
            se::Control::Bool => theme::zpx(20.0),
            _ => input_h,
        };
        let row_bottom_off = control_off + control_h + theme::zpx(20.0);

        let row_top = screen_y(title_off);
        let control_y = screen_y(control_off);
        let visible_band = screen_y(row_bottom_off) > right.y && row_top < right.y + right.h;

        // Control geometry (used both to draw and to hit-test).
        let control_rect = match d.control {
            se::Control::Bool => Rect { x: cx, y: control_y, w: theme::zpx(20.0), h: theme::zpx(20.0) },
            _ => Rect { x: cx, y: control_y, w: input_w, h: input_h },
        };

        // The control's box is a quad (no per-quad clipping), so only draw it when
        // fully inside the viewport — otherwise it bleeds past the card edges.
        let control_visible = control_rect.y >= right.y && control_rect.y + control_rect.h <= right.y + right.h;

        if visible_band {
            // Title (brighter) + description (dim, wrapped).
            let tb = make_buf(fs, d.title, ui_fam, fz, lh, None);
            texts.push(T { buf: tb, left: cx, top: row_top, color: fg_text, clip: rclip });
            texts.push(T { buf: desc_buf, left: cx, top: screen_y(desc_off), color: fg_dim, clip: rclip });
        }
        // A single-line value field's static text sits exactly where the TextInput
        // component would draw its own — same pad + vertical centering — so display
        // and edit modes don't drift.
        let field_pad = theme::zpx(crate::widgets::TextInput::FIELD_PAD);
        let field_text = |buf: glyphon::Buffer, rect: Rect, color: Color| T {
            buf,
            left: rect.x + field_pad,
            top: crate::widgets::TextInput::field_text_top(rect),
            color,
            clip: rclip,
        };
        if control_visible {
            match d.control {
                se::Control::Bool => {
                    let on = cur == "true";
                    crate::widgets::field_box(control_rect, box_radius, &mut q);
                    if on {
                        q.push(control_rect.rounded_quad(accent, box_radius));
                        let chk = make_buf(fs, &theme::ICON_CHECK.to_string(), icon_fam, fz, control_rect.h, None);
                        texts.push(T { buf: chk, left: control_rect.x + theme::zpx(3.0), top: control_rect.y, color: Color::rgb(0xFF, 0xFF, 0xFF), clip: rclip });
                    }
                }
                se::Control::Enum(opts) => {
                    let label = opts.iter().find(|(v, _)| format!("{:?}", v) == cur).map(|(_, l)| *l).unwrap_or("");
                    let dd = gpu.ui.settings_dropdowns.entry(d.key).or_insert_with(|| crate::widgets::Dropdown::new(fs));
                    dd.set(fs, label);
                    dd.draw_box(control_rect, box_radius, &mut q);
                    dd_draws.push((d.key, control_rect));
                }
                se::Control::Theme => {
                    let dd = gpu.ui.settings_dropdowns.entry(d.key).or_insert_with(|| crate::widgets::Dropdown::new(fs));
                    dd.set(fs, cur.trim_matches('"'));
                    dd.draw_box(control_rect, box_radius, &mut q);
                    dd_draws.push((d.key, control_rect));
                }
                se::Control::Number | se::Control::Text => {
                    crate::widgets::field_box(control_rect, box_radius, &mut q);
                    // When editing this row, the TextInput component renders itself
                    // (text + selection + caret) below; otherwise show the value.
                    if ed.edit_key != Some(d.key) {
                        let b = make_buf(fs, cur.trim_matches('"'), ui_fam, fz, lh, None);
                        texts.push(field_text(b, control_rect, fg_text));
                    }
                }
            }
        }

        ed.rows_cache.push(se::RowHit {
            idx,
            row: Rect { x: cx, y: row_top, w: content_w, h: screen_y(row_bottom_off) - row_top },
            control: control_rect,
        });
        yy = row_bottom_off;
    }
    ed.content_h = yy;

    // One-shot: scroll a left-tree-selected setting into view (clamped), then clear.
    if ed.scroll_to.is_some() {
        if let Some(off) = scroll_target_off {
            let max = (ed.content_h - right.h).max(0.0);
            ed.scroll = off.clamp(0.0, max);
        }
        ed.scroll_to = None;
    }
    // Clamp the scroll (content may have shrunk) and draw the viewport scrollbar.
    let max_scroll = (ed.content_h - right.h).max(0.0);
    ed.scroll = ed.scroll.clamp(0.0, max_scroll);
    if ed.content_h > right.h {
        let track_h = right.h;
        let thumb_h = (track_h * (track_h / ed.content_h)).max(theme::zpx(24.0));
        let thumb_y = right.y + (ed.scroll / max_scroll.max(1.0)) * (track_h - thumb_h);
        let tw = theme::zpx(6.0);
        q.push(Rect { x: right.x + right.w - tw, y: thumb_y, w: tw, h: thumb_h }.rounded_quad([0.6, 0.64, 0.78, 0.5], tw * 0.5));
    }

    // Done shaping owned buffers — release the font_system borrow.
    let _ = fs;

    // Selection highlights (drawn under the text, so they go in the bg quad list).
    gpu.ui.settings_search.selection_quads(lay.search, theme::zpx(10.0), &mut q);
    if let Some(key) = ed.edit_key {
        if let Some(h) = ed.rows_cache.iter().find(|h| se::SCHEMA[h.idx].key == key) {
            gpu.ui.settings_input.selection_quads(h.control, theme::zpx(8.0), &mut q);
        }
    }

    gpu.quad_renderer.prepare(&gpu.device, &gpu.queue, &q, &fg, (cfg_w, cfg_h));

    let mut sareas: Vec<TextArea> = texts
        .iter()
        .map(|t| TextArea {
            buffer: &t.buf,
            left: t.left,
            top: t.top,
            scale: 1.0,
            bounds: t.clip,
            default_color: t.color,
            custom_glyphs: &[],
        })
        .collect();
    // Search box widget (placeholder/text/caret/selection).
    gpu.ui.settings_search.set_placeholder(&mut gpu.font_system, "Search settings");
    gpu.ui.settings_search.draw(lay.search, theme::zpx(10.0), fg_text, &mut sareas);
    // Inline number/text editor, when active, draws on top of its control box.
    if let Some(key) = ed.edit_key {
        if let Some(h) = ed.rows_cache.iter().find(|h| se::SCHEMA[h.idx].key == key) {
            gpu.ui.settings_input.draw(h.control, theme::zpx(8.0), fg_text, &mut sareas);
        }
    }
    // Dropdown value + chevron (their boxes were drawn in the quad phase).
    for (key, rect) in &dd_draws {
        if let Some(dd) = gpu.ui.settings_dropdowns.get(key) {
            dd.draw_text(*rect, theme::zpx(8.0), &mut sareas);
        }
    }

    gpu.text_renderer.prepare(&gpu.device, &gpu.queue, &mut gpu.font_system, &mut gpu.atlas, &gpu.viewport, sareas, &mut gpu.swash_cache)?;

    // Carets (drawn as fg quads in a tiny second prepare so they sit over the text).
    let mut caretq: Vec<Quad> = Vec::new();
    if blink {
        if gpu.ui.settings_search.focused() {
            caretq.push(gpu.ui.settings_search.caret_quad(lay.search, theme::zpx(10.0)));
        }
        if let Some(key) = ed.edit_key {
            if gpu.ui.settings_input.focused() {
                if let Some(h) = ed.rows_cache.iter().find(|h| se::SCHEMA[h.idx].key == key) {
                    caretq.push(gpu.ui.settings_input.caret_quad(h.control, theme::zpx(8.0)));
                }
            }
        }
    }

    let mut enc = gpu.device.create_command_encoder(&CommandEncoderDescriptor { label: Some("aether-settings-pass") });
    {
        let mut pass = enc.begin_render_pass(&RenderPassDescriptor {
            label: Some("aether-settings"),
            color_attachments: &[Some(RenderPassColorAttachment {
                view,
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
    gpu.queue.submit(Some(enc.finish()));

    // Caret pass on top.
    if !caretq.is_empty() {
        gpu.quad_renderer.prepare(&gpu.device, &gpu.queue, &[], &caretq, (cfg_w, cfg_h));
        let mut enc2 = gpu.device.create_command_encoder(&CommandEncoderDescriptor { label: Some("aether-settings-caret") });
        {
            let mut pass = enc2.begin_render_pass(&RenderPassDescriptor {
                label: Some("aether-settings-caret"),
                color_attachments: &[Some(RenderPassColorAttachment {
                    view,
                    resolve_target: None,
                    ops: Operations { load: LoadOp::Load, store: StoreOp::Store },
                })],
                depth_stencil_attachment: None,
                timestamp_writes: None,
                occlusion_query_set: None,
            });
            gpu.quad_renderer.render_fg(&mut pass);
        }
        gpu.queue.submit(Some(enc2.finish()));
    }

    Ok(())
}
