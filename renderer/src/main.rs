// Hide the console window — without this the binary uses the console subsystem
// and Windows spawns a terminal alongside the GUI. We still capture stderr when
// we launch nova via a redirected pipe, so no debug visibility is lost.
#![windows_subsystem = "windows"]

// Nova — Phase 1 vertical slice with VSCode-shaped UI shell.
// Activity bar, sidebar file tree, tab strip, editor (gutter + text),
// status bar, command palette (Ctrl+Shift+P), find bar (Ctrl+F).

mod commands;
mod diff;
mod document;
mod ext_detail;
mod ext_runtime;
mod extensions;
mod feedback_upload;
mod gpu;
mod icon;
mod git;
mod layout;
mod lsp;
mod markdown;
mod marketplace;
mod menus;
mod perf;
mod update;
mod media;
mod quad;
mod render;
mod search;
mod settings;
mod state;
mod syntax;
mod terminal;
mod textmate;
mod theme;
mod ui;
mod widgets;
mod workspace;

use std::path::PathBuf;
use std::sync::mpsc::{Receiver, Sender};
use std::sync::Arc;
use std::time::{Duration, Instant};

use anyhow::Result;
use arboard::Clipboard;
use winit::{
    application::ApplicationHandler,
    dpi::{LogicalSize, PhysicalPosition},
    event::{ElementState, MouseButton, MouseScrollDelta, WindowEvent},
    event_loop::{ActiveEventLoop, ControlFlow, EventLoop},
    keyboard::{Key, ModifiersState, NamedKey},
    window::{CursorIcon, Window, WindowId},
};

use commands::{Command, FindBarState, PaletteState};
use document::Document;
use extensions::{ExtKind, Extension, OpenExt};
use marketplace::WorkerMsg;
use gpu::GpuState;
use layout::Layout;
// Region-geometry helpers live in `layout`; re-exported at the crate root so
// existing `crate::<fn>` references (render.rs, panels) keep resolving.
pub(crate) use layout::{
    active_activity_idx, create_row_geometry, ext_filter_rect, ext_list_region,
    terminal_content, terminal_grid_size, terminal_header_button_rects, terminal_pane_area,
    terminal_pane_rects, terminal_tab_close_rect, terminal_tablist_rect, x_range_in_run,
    TERMINAL_TABLIST_W,
};
pub(crate) use terminal::translate_terminal_key;
pub(crate) use widgets::{edit_input, Rect, Splitter};
use workspace::Workspace;


// ---------- App ----------

pub(crate) struct UiCache {
    pub(crate) tabs: String,
}

impl UiCache {
    fn new() -> Self {
        Self {
            tabs: String::new(),
        }
    }
}

/// In-progress inline name entry in the tree — either a New File/Folder
/// (`rename_from` = None, inserts a row) or a Rename (`rename_from` = Some,
/// replaces the target's row).
pub(crate) struct PendingCreate {
    pub(crate) is_dir: bool,
    pub(crate) parent: PathBuf,
    pub(crate) row: usize,   // tree row the field occupies
    pub(crate) depth: usize, // indent level of the inline field
    pub(crate) rename_from: Option<PathBuf>,
}

#[derive(Clone, Copy)]
pub(crate) enum MenuAction {
    NewFile,
    NewFolder,
    Rename,
    Delete,
    CopyPath,
}

pub(crate) const MENU_ACTIONS: &[(MenuAction, &str)] = &[
    (MenuAction::NewFile, "New File"),
    (MenuAction::NewFolder, "New Folder"),
    (MenuAction::Rename, "Rename"),
    (MenuAction::Delete, "Delete"),
    (MenuAction::CopyPath, "Copy Path"),
];

/// Rasterize the bundled Nova logo (SVG) to a window/taskbar icon. The SVG is the
/// single source of truth; returns None if rendering fails (icon is non-critical).
fn app_icon() -> Option<winit::window::Icon> {
    use resvg::{tiny_skia, usvg};
    const SIZE: u32 = 256;
    let tree = usvg::Tree::from_str(include_str!("../assets/logo.svg"), &usvg::Options::default()).ok()?;
    let mut pixmap = tiny_skia::Pixmap::new(SIZE, SIZE)?;
    let s = tree.size();
    let scale = (SIZE as f32 / s.width()).min(SIZE as f32 / s.height());
    resvg::render(&tree, tiny_skia::Transform::from_scale(scale, scale), &mut pixmap.as_mut());
    // tiny-skia is premultiplied RGBA; winit wants straight (un-premultiplied) alpha.
    let mut rgba = pixmap.take();
    for px in rgba.chunks_exact_mut(4) {
        let a = px[3] as u32;
        if a > 0 && a < 255 {
            px[0] = ((px[0] as u32 * 255) / a) as u8;
            px[1] = ((px[1] as u32 * 255) / a) as u8;
            px[2] = ((px[2] as u32 * 255) / a) as u8;
        }
    }
    winit::window::Icon::from_rgba(rgba, SIZE, SIZE).ok()
}

/// Image file types the `image` crate can decode (SVG is text/XML — opened as text).
fn is_image_path(path: &std::path::Path) -> bool {
    matches!(
        path.extension().and_then(|e| e.to_str()).map(|e| e.to_ascii_lowercase()).as_deref(),
        Some("png" | "jpg" | "jpeg" | "gif" | "bmp" | "webp" | "ico" | "tiff" | "tif")
    )
}

/// An open right-click context menu over the file tree.
pub(crate) struct ContextMenu {
    pub(crate) anchor: (f32, f32),
    pub(crate) target: Option<usize>, // tree node index; None = empty area (root scope)
}

/// Which sidebar view the activity bar has selected.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum SidebarView {
    Explorer,
    Search,
    SourceControl,
    Extensions,
}

/// What a modal dialog confirms.
pub(crate) enum DialogAction {
    DeleteNode(usize),
    CloseDoc(usize),
    InstallUpdate,
    Dismiss, // info-only dialog; any button just closes it
}

pub(crate) struct DialogState {
    pub(crate) action: DialogAction,
    pub(crate) has_check: bool,
    pub(crate) checked: bool,
    pub(crate) hovered: Option<usize>,
}

pub(crate) struct App {
    pub(crate) cwd: PathBuf,
    pub(crate) initial_file: Option<PathBuf>,
    pub(crate) workspace: Workspace,
    pub(crate) gpu: Option<GpuState>,
    pub(crate) mouse_pos: PhysicalPosition<f64>,
    pub(crate) mouse_pressed: bool,
    /// Editor view interaction state (drag-select, multi-click). Logic lives in
    /// `ui::editor_view::EditorView`; accessed as `self.editor`.
    pub(crate) editor: ui::editor_view::EditorView,
    pub(crate) mods: ModifiersState,
    pub(crate) clipboard: Option<Clipboard>,
    pub(crate) sidebar_visible: bool,
    pub(crate) sidebar_split: Splitter,
    pub(crate) palette: PaletteState,
    pub(crate) find: FindBarState,
    pub(crate) ui_cache: UiCache,
    pub(crate) hovered_tab: Option<usize>,
    pub(crate) hovered_tab_close: Option<usize>,
    pub(crate) hovered_tree: Option<usize>,
    pub(crate) hovered_activity: Option<usize>,
    pub(crate) hovered_titlebtn: Option<usize>,
    pub(crate) hovered_search: bool,
    pub(crate) hovered_menu: Option<usize>,
    pub(crate) open_menu: Option<usize>,        // which top menu's dropdown is open
    pub(crate) menu_dd_hover: Option<usize>,    // hovered entry within the open dropdown
    pub(crate) feedback_form: Option<ui::feedback_form::FeedbackForm>, // modal feedback form
    /// Pending feedback issue (title, body) awaiting a screenshot capture on the
    /// next render frame; consumed by `render` which captures + uploads off-thread.
    pub(crate) pending_capture: Option<(String, String)>,
    pub(crate) update_available: Option<String>, // newer release version, if any
    pub(crate) hovered_layout: Option<usize>,
    pub(crate) hovered_explorer: Option<usize>,
    pub(crate) selected_tree: Option<usize>,
    /// Explorer file-tree panel — owns the inline create/rename field state.
    /// Accessed as `self.explorer.creating`.
    pub(crate) explorer: ui::explorer_panel::ExplorerPanel,
    pub(crate) dialog: Option<DialogState>,
    pub(crate) skip_delete_confirm: bool,
    pub(crate) last_click: Instant,
    pub(crate) last_click_pos: (f32, f32),
    pub(crate) sidebar_view: SidebarView,
    // Find-in-files (Search view): a self-contained panel (built once the GPU/font
    // system exists, in `resumed`). Owns all of its own state + buffers.
    pub(crate) search: Option<ui::search_panel::SearchPanel>,
    pub(crate) source_control: Option<ui::source_control_panel::SourceControlPanel>,
    pub(crate) extensions_panel: Option<ui::extensions_panel::ExtensionsPanel>,
    pub(crate) extensions: Vec<Extension>,
    pub(crate) text_drag: Option<InputId>, // active mouse drag-selection in a text input
    pub(crate) image_drag_last: Option<(f32, f32)>, // last cursor pos while panning an image
    pub(crate) ext_remote: Vec<marketplace::RemoteExt>, // current marketplace search results
    pub(crate) worker_tx: Sender<WorkerMsg>,
    pub(crate) worker_rx: Receiver<WorkerMsg>,
    /// Language-server clients (ESLint, …) — owns child processes + routing.
    pub(crate) lsp: lsp::LspManager,
    /// Last debounced-didChange tick, so we don't flood the server while typing.
    pub(crate) lsp_last_sync: Instant,
    /// Pull-diagnostics request id → document URI (ESLint uses the pull model).
    pub(crate) lsp_diag_pending: std::collections::HashMap<i64, String>,
    /// Name of the extension currently being installed (drives the "Installing…"
    /// button state and blocks duplicate clicks).
    pub(crate) installing: Option<String>,
    /// Extension detail page (README/CHANGELOG/Features). All its state lives in
    /// `ui::ext_detail_view::ExtDetailView`; accessed as `self.detail.*`.
    pub(crate) detail: ui::ext_detail_view::ExtDetailView,
    pub(crate) pending_close: bool,
    // Integrated terminal tabs. Each group is a tab (`+` adds one); within a tab,
    // panes are shown side-by-side (split). Only the active group is visible; the
    // rest keep running in the background. No shell is ever discarded.
    /// Integrated terminal state — see `ui::terminal_panel::TerminalPanel`. Accessed
    /// as `self.terminal.{groups,active,visible,focused,split,maximized}`.
    pub(crate) terminal: ui::terminal_panel::TerminalPanel,
    // Real monospace cell advance (px), measured from the shaped terminal buffer.
    // The cursor and grid sizing use this instead of an estimate so the block
    // cursor lands exactly on the glyph cell (no per-column drift).
    pub(crate) terminal_cell_w: f32,
    pub(crate) cursor_blink_on: bool,
    pub(crate) last_blink: Instant,
    pub(crate) last_edit: Instant,  // for files.autoSave (afterDelay)
    pub(crate) anim_start: Instant, // monotonic clock for GIF playback
    pub(crate) cursor_icon: CursorIcon,
}

impl App {
    fn new(root: PathBuf, initial_file: Option<PathBuf>) -> Self {
        let (worker_tx, worker_rx) = std::sync::mpsc::channel();
        Self {
            cwd: root.clone(),
            initial_file,
            workspace: Workspace::new(root.clone()),
            gpu: None,
            mouse_pos: PhysicalPosition::new(0.0, 0.0),
            mouse_pressed: false,
            editor: ui::editor_view::EditorView::new(),
            mods: ModifiersState::empty(),
            clipboard: Clipboard::new().ok(),
            sidebar_visible: true,
            sidebar_split: Splitter::new(
                theme::SIDEBAR_WIDTH(),
                theme::SIDEBAR_MIN_WIDTH(),
                theme::SIDEBAR_MAX_WIDTH(),
                widgets::Axis::Horizontal,
            ),
            palette: PaletteState::new(),
            find: FindBarState::new(),
            ui_cache: UiCache::new(),
            hovered_tab: None,
            hovered_tab_close: None,
            hovered_tree: None,
            hovered_activity: None,
            hovered_titlebtn: None,
            hovered_search: false,
            hovered_menu: None,
            open_menu: None,
            menu_dd_hover: None,
            feedback_form: None,
            pending_capture: None,
            update_available: None,
            hovered_layout: None,
            hovered_explorer: None,
            selected_tree: None,
            explorer: ui::explorer_panel::ExplorerPanel::new(),
            dialog: None,
            skip_delete_confirm: false,
            last_click: Instant::now(),
            last_click_pos: (0.0, 0.0),
            sidebar_view: SidebarView::Explorer,
            search: None, // built in `resumed` once the font system exists
            source_control: None, // built in `resumed`
            extensions_panel: None, // built in `resumed`
            extensions: Vec::new(),
            text_drag: None,
            image_drag_last: None,
            ext_remote: Vec::new(),
            worker_tx,
            worker_rx,
            lsp: lsp::LspManager::new(),
            lsp_last_sync: Instant::now(),
            lsp_diag_pending: std::collections::HashMap::new(),
            installing: None,
            detail: ui::ext_detail_view::ExtDetailView::new(),
            pending_close: false,
            terminal: ui::terminal_panel::TerminalPanel::new(root.clone()),
            terminal_cell_w: theme::FONT_SIZE() * 0.6, // refined after first shape
            cursor_blink_on: true,
            last_blink: Instant::now(),
            last_edit: Instant::now(),
            anim_start: Instant::now(),
            cursor_icon: CursorIcon::Default,
        }
    }

    fn reset_blink(&mut self) {
        self.cursor_blink_on = true;
        self.last_blink = Instant::now();
    }

    fn recompute_hover(&mut self) {
        let layout = self.layout();
        let p = (self.mouse_pos.x as f32, self.mouse_pos.y as f32);
        let mut changed = false;

        // While the modal feedback form is open it owns the cursor (text in fields,
        // pointer on controls) — don't fall through to the editor's I-beam.
        if self.feedback_form.is_some() {
            let win = self.gpu.as_ref().map_or((1280.0, 800.0), |g| (g.config.width as f32, g.config.height as f32));
            let c = self.feedback_form.as_ref().unwrap().cursor(p, win);
            if c != self.cursor_icon {
                self.cursor_icon = c;
                if let Some(g) = self.gpu.as_ref() {
                    g.window.set_cursor(c);
                }
            }
            return;
        }

        // Dialog (topmost modal) captures hover.
        if let Some(has_check) = self.dialog.as_ref().map(|d| d.has_check) {
            let new_hover = self.gpu.as_ref().and_then(|g| {
                let win = (g.config.width as f32, g.config.height as f32);
                let box_ = g.ui.dialog.box_rect(win, has_check);
                g.ui.dialog.button_at(box_, p)
            });
            if self.dialog.as_ref().map(|d| d.hovered) != Some(new_hover) {
                if let Some(d) = self.dialog.as_mut() {
                    d.hovered = new_hover;
                }
                self.redraw();
            }
            let cursor = if new_hover.is_some() {
                CursorIcon::Pointer
            } else {
                CursorIcon::Default
            };
            if cursor != self.cursor_icon {
                self.cursor_icon = cursor;
                if let Some(g) = self.gpu.as_ref() {
                    g.window.set_cursor(cursor);
                }
            }
            return;
        }

        // Context menu (modal) captures hover when open.
        if self.explorer.menu_open() {
            let new_item = self.gpu.as_ref().and_then(|g| self.explorer.menu_item_at(p, g));
            if new_item != self.explorer.hovered_menu_item {
                self.explorer.hovered_menu_item = new_item;
                self.redraw();
            }
            let cursor = if new_item.is_some() {
                CursorIcon::Pointer
            } else {
                CursorIcon::Default
            };
            if cursor != self.cursor_icon {
                self.cursor_icon = cursor;
                if let Some(g) = self.gpu.as_ref() {
                    g.window.set_cursor(cursor);
                }
            }
            return;
        }

        let new_titlebtn = self.title_btn_at(p.0, p.1, &layout);
        if new_titlebtn != self.hovered_titlebtn {
            self.hovered_titlebtn = new_titlebtn;
            changed = true;
        }

        let new_activity = layout.activity_rects().iter().position(|r| r.contains(p));
        if new_activity != self.hovered_activity {
            self.hovered_activity = new_activity;
            changed = true;
        }

        let new_tree = if self.sidebar_visible {
            self.gpu.as_ref().and_then(|gpu| {
                gpu.ui.sidebar.row_at_scrolled(
                    layout.tree_region(),
                    self.explorer.scroll.offset().1,
                    p,
                    self.workspace.tree.nodes.len(),
                )
            })
        } else {
            None
        };
        if new_tree != self.hovered_tree {
            self.hovered_tree = new_tree;
            changed = true;
        }

        let tab_rects = layout.tab_rects(self.tab_count());
        let new_tab = tab_rects.iter().position(|r| r.contains(p));
        let new_close =
            new_tab.filter(|&i| Layout::tab_close_rect(tab_rects[i]).contains(p));
        if new_tab != self.hovered_tab {
            self.hovered_tab = new_tab;
            changed = true;
        }
        if new_close != self.hovered_tab_close {
            self.hovered_tab_close = new_close;
            changed = true;
        }

        let new_search = layout.palette.is_none() && layout.header_search_rect().contains(p);
        if new_search != self.hovered_search {
            self.hovered_search = new_search;
            changed = true;
        }

        let new_menu = if layout.palette.is_none() {
            self.gpu
                .as_ref()
                .and_then(|g| g.menubar.item_at(layout.menu_bar_rect(), p))
        } else {
            None
        };
        if new_menu != self.hovered_menu {
            self.hovered_menu = new_menu;
            changed = true;
        }
        // While a dropdown is open, hovering another title switches to it (VSCode
        // behaviour), and track the hovered entry for the highlight.
        if self.open_menu.is_some() {
            if let Some(t) = new_menu {
                if self.open_menu != Some(t) {
                    self.open_app_menu(t);
                }
            }
            let hov = self.menu_dd_item_at(p.0, p.1);
            if hov != self.menu_dd_hover {
                self.menu_dd_hover = hov;
                changed = true;
            }
        }

        let new_layout = if layout.palette.is_none() {
            layout.layout_btn_rects().iter().position(|r| r.contains(p))
        } else {
            None
        };
        if new_layout != self.hovered_layout {
            self.hovered_layout = new_layout;
            changed = true;
        }

        let new_explorer = if self.sidebar_visible
            && layout.palette.is_none()
            && self.sidebar_view == SidebarView::Explorer
        {
            layout.explorer_action_rects().iter().position(|r| r.contains(p))
        } else {
            None
        };
        if new_explorer != self.hovered_explorer {
            self.hovered_explorer = new_explorer;
            changed = true;
        }

        // Extensions panel owns its own row-hover state; drive it (and the scroll
        // fade) below in the hover section.
        if self.sidebar_visible
            && self.sidebar_view == SidebarView::Extensions
            && layout.palette.is_none()
        {
            let region = layout.tree_region();
            if let Some(ep) = self.extensions_panel.as_mut() {
                if ep.hover(p, region) {
                    changed = true;
                }
            }
        }

        let new_page_install = if self.detail.open_extension.is_some() {
            let region = render::editor_region(&layout);
            self.gpu.as_ref().map(|g| g.ui.ext_detail.hit_install(region, p)).unwrap_or(false)
        } else {
            false
        };

        let new_detail_tab = if self.detail.open_extension.is_some() {
            let region = render::editor_region(&layout);
            self.gpu.as_ref().and_then(|g| g.ui.ext_detail.hit_tab(region, p))
        } else {
            None
        };
        if new_detail_tab != self.detail.hovered_detail_tab {
            self.detail.hovered_detail_tab = new_detail_tab;
            changed = true;
        }
        if new_page_install != self.detail.hovered_page_install {
            self.detail.hovered_page_install = new_page_install;
            changed = true;
        }

        // Hovering a README link → pointer cursor.
        let over_detail_link = if self.detail.open_extension.is_some() {
            let region = render::editor_region(&layout);
            let scroll = self.detail.ext_detail_scroll.offset().1;
            self.gpu
                .as_ref()
                .map(|g| {
                    g.ui.ext_detail
                        .link_rects(region, scroll, &|k| g.media.size(k))
                        .iter()
                        .any(|(r, _)| r.contains(p))
                })
                .unwrap_or(false)
        } else {
            false
        };

        // Drive each scroll region's hover (for the auto-hide fade) and detect
        // whether the pointer is over a scrollbar thumb (so the cursor stays the
        // default arrow rather than the editor I-beam).
        let mut over_scroll_thumb = false;
        let editing = self.detail.open_extension.is_none();
        let ed_inside = editing && layout.editor_text.contains(p);
        if let Some(d) = self.workspace.active_doc_mut() {
            if d.scroll.hover(ed_inside) {
                changed = true;
            }
            if ed_inside && d.scroll.cursor(p).is_some() {
                over_scroll_thumb = true;
            }
        }
        let (term_changed, term_thumb) = self.terminal.hover_panes(p, &layout);
        if term_changed {
            changed = true;
        }
        if term_thumb {
            over_scroll_thumb = true;
        }
        if self.sidebar_visible && self.sidebar_view == SidebarView::Explorer {
            let inside = layout.tree_region().contains(p);
            if self.explorer.scroll.hover(inside) {
                changed = true;
            }
            if inside && self.explorer.scroll.cursor(p).is_some() {
                over_scroll_thumb = true;
            }
        }
        if self.sidebar_visible && self.sidebar_view == SidebarView::Search {
            if let Some(sp) = self.search.as_mut() {
                if sp.hover(p, layout.tree_region()) {
                    changed = true;
                }
            }
        }
        if self.sidebar_visible && self.sidebar_view == SidebarView::SourceControl {
            if let Some(scp) = self.source_control.as_mut() {
                if scp.hover(p, layout.tree_region()) {
                    changed = true;
                }
            }
        }
        let det_inside = self.detail.open_extension.is_some() && layout.editor_text.contains(p);
        if self.detail.ext_detail_scroll.hover(det_inside) {
            changed = true;
        }
        if det_inside && self.detail.ext_detail_scroll.cursor(p).is_some() {
            over_scroll_thumb = true;
        }

        // Resolve the cursor by asking whichever widget is under the pointer for
        // its own cursor; regions without a widget (editor, empty chrome) fall
        // back to explicit defaults.
        let over_handle = self.sidebar_visible
            && layout.palette.is_none()
            && self.sidebar_split.handle_rect(layout.sidebar).contains(p);
        let over_term_handle = layout.palette.is_none()
            && layout
                .terminal_panel
                .map_or(false, |panel| self.terminal.split.handle_rect(panel).contains(p));
        // Terminal panel header buttons + tab-list rows are clickable IconButtons/rows.
        let over_term_btn = self.terminal.visible
            && layout.palette.is_none()
            && layout.terminal_panel.map_or(false, |panel| {
                terminal_header_button_rects(panel).iter().any(|r| r.contains(p))
                    || terminal_tablist_rect(terminal_content(panel), self.terminal.groups.len())
                        .map_or(false, |tl| tl.contains(p))
            });
        let new_cursor = if self.sidebar_split.is_dragging() || over_handle {
            self.sidebar_split.cursor()
        } else if self.terminal.split.is_dragging() || over_term_handle {
            self.terminal.split.cursor()
        } else if over_term_btn {
            CursorIcon::Pointer
        } else if new_search {
            self.gpu
                .as_ref()
                .map(|g| g.search.cursor())
                .unwrap_or(CursorIcon::Default)
        } else if new_menu.is_some() {
            self.gpu
                .as_ref()
                .map(|g| g.menubar.cursor())
                .unwrap_or(CursorIcon::Default)
        } else if let Some(i) = new_layout {
            self.gpu
                .as_ref()
                .map(|g| g.layout_btns[i].cursor())
                .unwrap_or(CursorIcon::Default)
        } else if let Some(i) = new_explorer {
            self.gpu
                .as_ref()
                .map(|g| g.explorer_btns[i].cursor())
                .unwrap_or(CursorIcon::Default)
        } else if let Some(c) = (self.sidebar_visible && self.sidebar_view == SidebarView::Search)
            .then(|| self.search.as_ref().and_then(|sp| sp.cursor(p, layout.tree_region())))
            .flatten()
        {
            // The Search panel resolves its own cursor (toggles/results = pointer,
            // inputs = text).
            c
        } else if let Some(c) = (self.sidebar_visible && self.sidebar_view == SidebarView::Extensions)
            .then(|| {
                self.extensions_panel
                    .as_ref()
                    .and_then(|ep| ep.cursor(p, layout.tree_region()))
            })
            .flatten()
        {
            // The Extensions panel resolves its own cursor (filter = text, rows =
            // pointer, scrollbar/empty = arrow).
            c
        } else if self.focused_input_at(&layout, p).is_some() {
            CursorIcon::Text
        } else if new_page_install || new_detail_tab.is_some() || over_detail_link {
            CursorIcon::Pointer
        } else if self.detail.open_extension.is_some() && layout.editor_text.contains(p) {
            CursorIcon::Default
        } else if let Some(pal) = layout.palette.as_ref() {
            // Palette is modal: pointer over a row, arrow elsewhere.
            self.gpu
                .as_ref()
                .and_then(|g| {
                    g.ui
                        .palette_list
                        .row_at(pal.list, p, self.palette.filtered.len())
                        .map(|_| g.ui.palette_list.cursor())
                })
                .unwrap_or(CursorIcon::Default)
        } else if let Some(g) = self.gpu.as_ref() {
            if let Some(i) = new_titlebtn {
                g.titlebar_btns[i].cursor()
            } else if let Some(i) = new_activity {
                g.activity_btns[i].cursor()
            } else if new_close.is_some() {
                g.tab_close_btn.cursor()
            } else if new_tab.is_some() {
                // Tab body is clickable but has no dedicated widget.
                CursorIcon::Pointer
            } else if new_tree.is_some() {
                g.ui.sidebar.cursor()
            } else if over_scroll_thumb {
                CursorIcon::Default
            } else if layout.editor_text.contains(p) {
                // Editor text area: I-beam (not a component).
                CursorIcon::Text
            } else {
                CursorIcon::Default
            }
        } else {
            CursorIcon::Default
        };
        if new_cursor != self.cursor_icon {
            self.cursor_icon = new_cursor;
            if let Some(g) = self.gpu.as_ref() {
                g.window.set_cursor(new_cursor);
            }
        }

        if changed {
            self.redraw();
        }
    }

    fn open_initial(&mut self) {
        // Load user settings and apply the startup-time ones.
        let s = settings::reload();
        self.sidebar_visible = s.workbench_sidebar_visible;
        self.apply_theme_by_name(&s.workbench_color_theme);

        // Restore the persisted UI zoom before the first layout/draw.
        if let Some(z) = state::State::load().zoom {
            self.set_zoom(z);
        }

        let Some(gpu) = self.gpu.as_mut() else {
            return;
        };
        // Open the file passed on the command line, else PRD.md if present.
        if let Some(f) = self.initial_file.clone() {
            let _ = self.workspace.open_file(&f, &mut gpu.font_system);
        } else {
            let prd = self.cwd.join("PRD.md");
            if prd.exists() {
                let _ = self.workspace.open_file(&prd, &mut gpu.font_system);
            }
        }
        if self.workspace.documents.is_empty() {
            let doc = Document::new(
                None,
                "Welcome to Nova\n\nUse the sidebar to open files.\nCtrl+Shift+P for command palette.\n"
                    .to_string(),
                &mut gpu.font_system,
            );
            self.workspace.documents.push(doc);
            self.workspace.active = Some(0);
        }
    }

    fn layout(&self) -> Layout {
        let (w, h) = match self.gpu.as_ref() {
            Some(g) => (g.config.width as f32, g.config.height as f32),
            None => (1280.0, 800.0),
        };
        Layout::compute(
            w,
            h,
            self.sidebar_visible,
            self.sidebar_split.size(),
            self.find.active,
            self.palette.active,
            self.terminal.panel_height(),
            self.workspace.active_doc().map_or(false, |d| d.diff.is_some()),
        )
    }

    fn ensure_cursor_visible(&mut self) {
        let layout = self.layout();
        if let Some(doc) = self.workspace.active_doc_mut() {
            ui::editor_view::EditorView::ensure_cursor_visible(doc, &layout);
        }
    }

    fn redraw(&self) {
        if let Some(g) = self.gpu.as_ref() {
            g.window.request_redraw();
        }
    }

    /// Soonest time any auto-hide scrollbar needs a redraw to advance its fade,
    /// across every region. None when nothing is fading.
    fn scroll_next_wake(&self, now: Instant) -> Option<Instant> {
        let mut earliest: Option<Instant> = None;
        let mut consider = |w: Option<Instant>| {
            if let Some(t) = w {
                earliest = Some(earliest.map_or(t, |x: Instant| x.min(t)));
            }
        };
        consider(self.workspace.active_doc().and_then(|d| d.scroll.next_wake(now)));
        consider(self.detail.ext_detail_scroll.next_wake(now));
        if let Some(ep) = self.extensions_panel.as_ref() {
            consider(ep.next_wake(now));
        }
        if let Some(sp) = self.search.as_ref() {
            consider(sp.next_wake(now));
        }
        if let Some(g) = self.terminal.groups.get(self.terminal.active) {
            for p in &g.panes {
                consider(p.scroll.next_wake(now));
            }
        }
        earliest
    }

    /// Total tabs in the strip: documents plus the open extension page (if any),
    /// which lives in its own tab after the documents (VSCode-style).
    pub(crate) fn tab_count(&self) -> usize {
        self.workspace.documents.len() + self.detail.open_extension.is_some() as usize
    }

    /// The tab index of the open extension page, if any.
    pub(crate) fn ext_tab_index(&self) -> Option<usize> {
        self.detail.open_extension.map(|_| self.workspace.documents.len())
    }

    /// Begin an inline New File / New Folder, scoped to the selected folder (or
    /// the parent of a selected file, or the workspace root). Expands the target
    /// folder so the field appears at the top of its contents.
    // Inline create/rename lives on `ui::explorer_panel::ExplorerPanel`; these are
    // thin glue that supply the shared workspace + gpu and trigger a redraw.
    fn begin_create(&mut self, is_dir: bool) {
        let sel = self.selected_tree;
        if let Some(g) = self.gpu.as_mut() {
            self.explorer.begin_create(is_dir, sel, &mut self.workspace, g);
        }
        self.redraw();
    }
    fn begin_rename(&mut self, idx: usize) {
        if let Some(g) = self.gpu.as_mut() {
            self.explorer.begin_rename(idx, &self.workspace, g);
        }
        self.redraw();
    }
    fn commit_create(&mut self) {
        if let Some(g) = self.gpu.as_mut() {
            self.explorer.commit_create(&mut self.workspace, g);
        }
        self.redraw();
    }
    fn cancel_create(&mut self) {
        if let Some(g) = self.gpu.as_mut() {
            self.explorer.cancel_create(g);
        }
        self.redraw();
    }

    // ---- Context menu ----

    fn on_right_press(&mut self, x: f32, y: f32) {
        let layout = self.layout();
        if !self.sidebar_visible || !layout.sidebar.contains((x, y)) {
            return;
        }
        let target = self.gpu.as_ref().and_then(|g| {
            g.ui.sidebar.row_at_scrolled(
                layout.tree_region(),
                self.explorer.scroll.offset().1,
                (x, y),
                self.workspace.tree.nodes.len(),
            )
        });
        self.selected_tree = target;
        self.explorer.open_menu((x, y), target);
        self.redraw();
    }

    fn close_context_menu(&mut self) {
        self.explorer.close_menu();
        self.redraw();
    }

    // ---- Top menu-bar dropdowns (File / Edit / …) ----

    /// Open the dropdown for top-level menu `idx`, loading its entries into the
    /// shared dropdown widget. Closes any open file-explorer context menu.
    fn open_app_menu(&mut self, idx: usize) {
        let labels: Vec<&str> = menus::entries(idx).iter().map(|e| e.label).collect();
        if let Some(g) = self.gpu.as_mut() {
            g.ui.menu_dropdown.set_items(&mut g.font_system, &labels);
        }
        self.explorer.close_menu();
        self.open_menu = Some(idx);
        self.menu_dd_hover = None;
        self.redraw();
    }

    fn close_app_menu(&mut self) {
        if self.open_menu.take().is_some() {
            self.menu_dd_hover = None;
            self.redraw();
        }
    }

    /// Screen rect of the currently open dropdown box (anchored under its title).
    fn menu_dd_rect(&self) -> Option<crate::widgets::Rect> {
        let idx = self.open_menu?;
        let g = self.gpu.as_ref()?;
        let layout = self.layout();
        let rects = g.menubar.item_rects(layout.menu_bar_rect());
        let r = rects.get(idx)?;
        let win = (g.config.width as f32, g.config.height as f32);
        Some(g.ui.menu_dropdown.rect((r.x, r.y + r.h), win))
    }

    fn menu_dd_item_at(&self, x: f32, y: f32) -> Option<usize> {
        let r = self.menu_dd_rect()?;
        let g = self.gpu.as_ref()?;
        g.ui.menu_dropdown.item_at(r, (x, y))
    }

    fn exec_menu_cmd(&mut self, m: menus::MenuCmd) {
        self.close_app_menu();
        match m {
            menus::MenuCmd::Cmd(c) => self.exec_command(c),
            menus::MenuCmd::Palette => self.open_palette(),
            menus::MenuCmd::Feedback => self.open_feedback(),
            menus::MenuCmd::CheckUpdate => {
                self.close_app_menu();
                match self.update_available.clone() {
                    Some(v) => self.show_update_prompt(&v),
                    None => {
                        update::check_async(self.worker_tx.clone(), true);
                        self.show_info_dialog("Checking for updates…");
                    }
                }
            }
            menus::MenuCmd::About => {
                self.close_app_menu();
                self.show_info_dialog(&format!(
                    "Nova v{} · {} ({})",
                    update::current_version(),
                    std::env::consts::OS,
                    std::env::consts::ARCH
                ));
            }
            menus::MenuCmd::Exit => self.pending_close = true,
        }
    }

    fn exec_menu_action(&mut self, action: MenuAction) {
        let target = self.explorer.menu_target();
        self.close_context_menu();
        match action {
            MenuAction::NewFile => self.begin_create(false),
            MenuAction::NewFolder => self.begin_create(true),
            MenuAction::Rename => {
                if let Some(t) = target {
                    self.begin_rename(t);
                }
            }
            MenuAction::Delete => {
                if let Some(t) = target {
                    self.request_delete(t);
                }
            }
            MenuAction::CopyPath => {
                if let Some(t) = target {
                    if let Some(n) = self.workspace.tree.nodes.get(t) {
                        let s = n.path.display().to_string();
                        if let Some(cb) = self.clipboard.as_mut() {
                            let _ = cb.set_text(s);
                        }
                    }
                }
            }
        }
    }

    /// "Install" a supported extension into Nova. For color themes this loads and
    /// applies the theme immediately; other supported kinds just mark installed
    /// (their declarative contributions aren't loaded yet).
    fn install_extension(&mut self, i: usize) {
        let (kind, theme_path, grammar_paths) = match self.extensions.get(i) {
            Some(e) => (e.kind, e.theme_path.clone(), e.grammar_paths.clone()),
            None => return,
        };
        match kind {
            ExtKind::Theme => {
                if let Some(p) = theme_path {
                    if let Some(t) = theme::load_vscode(&p) {
                        theme::set(t);
                    }
                }
            }
            ExtKind::Grammar => {
                // Parse each TextMate grammar and register it natively, then
                // re-highlight open documents so a matching file lights up.
                for gp in &grammar_paths {
                    if let Some(g) = textmate::Grammar::load(gp) {
                        textmate::register(g, &[]);
                    }
                }
                if let Some(g) = self.gpu.as_mut() {
                    for d in self.workspace.documents.iter_mut() {
                        d.reshape(&mut g.font_system);
                    }
                }
            }
            _ => {}
        }
        if let Some(e) = self.extensions.get_mut(i) {
            e.installed = true;
        }
        self.rebuild_ext_rows();
        self.redraw();
    }

    /// Rebuild the sidebar's extension-row widgets from current extension data.
    /// Thin wrapper: the `ExtensionsPanel` owns the rows + filter; it borrows the
    /// shared `extensions`/`ext_remote` data to rebuild.
    fn rebuild_ext_rows(&mut self) {
        if let (Some(ep), Some(g)) = (self.extensions_panel.as_mut(), self.gpu.as_mut()) {
            ep.rebuild(g, &self.extensions, &self.ext_remote);
        }
    }


    /// Re-read git status into the Source Control panel and redraw.
    fn refresh_source_control(&mut self) {
        if let (Some(scp), Some(g)) = (self.source_control.as_mut(), self.gpu.as_mut()) {
            scp.refresh(&mut g.font_system);
        }
        self.redraw();
    }

    /// Apply a side-effect requested by a panel (centralizes cross-cutting actions).
    pub(crate) fn apply_intent(&mut self, intent: ui::Intent) {
        match intent {
            ui::Intent::OpenFile { path, line, col } => self.open_file_at(path, line, col),
            ui::Intent::OpenDiff { path, staged, untracked } => {
                if let Some(g) = self.gpu.as_mut() {
                    let d = diff::compute(&self.cwd, &path, staged, untracked);
                    self.workspace.open_diff(d, &mut g.font_system);
                    self.detail.open_extension = None;
                }
                self.ensure_cursor_visible();
                self.redraw();
            }
            ui::Intent::OpenExtDetail(which) => self.open_ext_detail(which),
            ui::Intent::GitCommit { msg, stage_all } => {
                if git::commit(&self.cwd, &msg, stage_all) {
                    if let (Some(scp), Some(g)) = (self.source_control.as_mut(), self.gpu.as_mut()) {
                        scp.clear_message(&mut g.font_system);
                        scp.refresh(&mut g.font_system);
                    }
                }
                self.redraw();
            }
            ui::Intent::GitStage(path) => {
                git::stage(&self.cwd, &path);
                self.refresh_source_control();
            }
            ui::Intent::GitUnstage(path) => {
                git::unstage(&self.cwd, &path);
                self.refresh_source_control();
            }
            ui::Intent::GitDiscard { path, untracked } => {
                git::discard(&self.cwd, &path, untracked);
                self.refresh_source_control();
            }
            ui::Intent::GitStageAll => {
                git::stage_all(&self.cwd);
                self.refresh_source_control();
            }
            ui::Intent::GitUnstageAll => {
                git::unstage_all(&self.cwd);
                self.refresh_source_control();
            }
            ui::Intent::GitDiscardAll => {
                git::discard_all(&self.cwd);
                self.refresh_source_control();
            }
            ui::Intent::GitRefresh => self.refresh_source_control(),
            ui::Intent::GitCommitPush { msg, stage_all } => {
                if git::commit(&self.cwd, &msg, stage_all) {
                    git::push(&self.cwd);
                    if let (Some(scp), Some(g)) = (self.source_control.as_mut(), self.gpu.as_mut()) {
                        scp.clear_message(&mut g.font_system);
                        scp.refresh(&mut g.font_system);
                    }
                }
                self.redraw();
            }
            ui::Intent::ReloadOpenDocs => {
                if let Some(gpu) = self.gpu.as_mut() {
                    for d in self.workspace.documents.iter_mut() {
                        if let Some(p) = d.path.clone() {
                            if let Ok(text) = std::fs::read_to_string(&p) {
                                d.set_text_external(&text, &mut gpu.font_system);
                            }
                        }
                    }
                }
            }
        }
    }

    /// Open `path` and place the caret at (1-based `line`, byte `col`).
    fn open_file_at(&mut self, path: PathBuf, line: usize, col: usize) {
        if is_image_path(&path) {
            self.open_image(path);
            return;
        }
        if let Some(gpu) = self.gpu.as_mut() {
            if self.workspace.open_file(&path, &mut gpu.font_system).is_ok() {
                self.detail.open_extension = None;
                if let Some(d) = self.workspace.active_doc_mut() {
                    let li = line.saturating_sub(1);
                    if li < d.rope.len_lines() {
                        let ls = d.rope.line_to_byte(li);
                        let ll = d.rope.line(li).len_bytes();
                        d.place(ls + col.min(ll), false);
                    }
                }
            }
        }
        self.ensure_cursor_visible();
        self.redraw();
    }

    /// Open the modal feedback / report-issue form (Help → Send Feedback).
    fn open_feedback(&mut self) {
        if let Some(g) = self.gpu.as_mut() {
            self.feedback_form = Some(ui::feedback_form::FeedbackForm::new(&mut g.font_system));
            self.close_app_menu();
        }
        self.redraw();
    }

    /// File a GitHub issue via the user's `gh` CLI login. Falls back to opening the
    /// prefilled new-issue page in the browser if `gh` isn't available.
    fn submit_issue(&mut self, title: String, body: String) {
        const REPO: &str = "actuallyroy/nova-editor";
        let mut cmd = std::process::Command::new(gh_program());
        cmd.args(["issue", "create", "--repo", REPO, "--title", &title, "--body", &body]);
        #[cfg(windows)]
        {
            use std::os::windows::process::CommandExt;
            cmd.creation_flags(0x0800_0000); // no console flash
        }
        match cmd.output() {
            Ok(o) if o.status.success() => {
                let url = String::from_utf8_lossy(&o.stdout).trim().to_string();
                if url.starts_with("http") {
                    open_url(&url); // show the created issue
                }
            }
            _ => {
                // No gh / not authed / error → open the prefilled new-issue page.
                let url = format!(
                    "https://github.com/{}/issues/new?title={}&body={}",
                    REPO,
                    urlencode(&title),
                    urlencode(&body)
                );
                open_url(&url);
            }
        }
        self.redraw();
    }

    /// Apply the result of a feedback-form input event.
    fn handle_feedback_action(&mut self, act: ui::feedback_form::FormAction) {
        use ui::feedback_form::FormAction;
        match act {
            FormAction::Submit => {
                let form = self.feedback_form.as_ref();
                if let Some((title, body)) = form.and_then(|f| f.issue()) {
                    let shot = form.map_or(false, |f| f.wants_screenshot());
                    self.feedback_form = None;
                    if shot {
                        // Defer: the next render frame captures the editor, then
                        // uploads + files the issue off-thread (see render.rs).
                        self.pending_capture = Some((title, body));
                    } else {
                        self.submit_issue(title, body);
                    }
                }
                // Empty title → keep the form open.
            }
            FormAction::Close => self.feedback_form = None,
            FormAction::None => {}
        }
        self.redraw();
    }

    /// Open an image file as a read-only image tab: decode + upload it to the media
    /// renderer (once), then add/focus its tab.
    fn open_image(&mut self, path: PathBuf) {
        let key = path.to_string_lossy().to_string();
        let Some(g) = self.gpu.as_mut() else { return };
        if !g.media.has(&key) {
            match std::fs::read(&path) {
                Ok(bytes) => {
                    let frames = crate::media::decode_full(&bytes);
                    g.media.upload_frames(&g.device, &g.queue, &key, frames);
                }
                Err(_) => return,
            }
        }
        self.workspace.open_image(&path, key, &mut g.font_system);
        self.detail.open_extension = None;
        self.redraw();
    }

    /// Download + install a marketplace extension on a background thread.
    fn install_remote(&mut self, idx: usize) {
        if self.installing.is_some() {
            return; // an install is already in flight
        }
        let Some(ext) = self.ext_remote.get(idx).cloned() else { return };
        let Some(root) = extensions::dir() else {
            self.show_info_dialog("Couldn't locate the extensions folder (~/.vscode/extensions).");
            return;
        };
        let label = if ext.display.is_empty() { ext.name.clone() } else { ext.display.clone() };
        self.installing = Some(label);
        self.show_info_dialog("Installing… downloading from the marketplace.");
        marketplace::install_async(self.worker_tx.clone(), ext, root);
    }

    /// Open the detail page for an extension. The view owns the load logic; this is
    /// thin glue that supplies gpu + the shared extension data and redraws.
    fn open_ext_detail(&mut self, which: OpenExt) {
        if let Some(g) = self.gpu.as_mut() {
            self.detail.open(which, g, &self.extensions, &self.ext_remote, &self.worker_tx);
        }
        self.redraw();
    }

    /// Install whatever the detail page currently shows.
    fn install_open(&mut self) {
        match self.detail.open_extension {
            Some(OpenExt::Local(i)) => self.install_extension(i),
            Some(OpenExt::Remote(i)) => self.install_remote(i),
            None => {}
        }
    }

    /// The currently focused element (single source of truth for key routing).
    /// Precedence matches modal nesting: inline rename > palette > find > the
    /// extensions filter > the editor.
    fn focus(&self) -> Focus {
        if self.explorer.creating.is_some() {
            Focus::Rename
        } else if self.palette.active {
            Focus::Palette
        } else if self.find.active {
            Focus::Find
        } else if self.terminal.visible && self.terminal.focused && !self.terminal.groups.is_empty() {
            Focus::Terminal
        } else if self.extensions_panel.as_ref().map_or(false, |ep| ep.focused()) {
            Focus::ExtFilter
        } else if self.search.as_ref().map_or(false, |sp| sp.focused()) {
            Focus::Search
        } else if self.source_control.as_ref().map_or(false, |s| s.focused()) {
            Focus::SourceControl
        } else {
            Focus::Editor
        }
    }

    fn set_ext_filter_focus(&mut self, on: bool) {
        if let Some(ep) = self.extensions_panel.as_mut() {
            ep.set_focus(on);
        }
    }

    /// Record a click; returns true if it's a double-click (within 400ms and 4px).
    /// Shares the editor's double-click state so the two can't both fire.
    fn register_click(&mut self, x: f32, y: f32) -> bool {
        let now = Instant::now();
        let double = now.duration_since(self.last_click) < Duration::from_millis(400)
            && (x - self.last_click_pos.0).abs() < 4.0
            && (y - self.last_click_pos.1).abs() < 4.0;
        self.last_click = now;
        self.last_click_pos = (x, y);
        double
    }

    /// (rect, left-pad) of a given input, if it's currently shown.
    fn input_rect_for(&self, id: InputId, layout: &Layout) -> Option<(Rect, f32)> {
        match id {
            InputId::Palette => layout.palette.as_ref().map(|p| (p.input, 6.0)),
            InputId::Find => layout.find_bar.as_ref().map(|fb| (*fb, 8.0)),
        }
    }

    /// The focused input under point `p` (for click-to-position / drag-select).
    fn focused_input_at(&self, layout: &Layout, p: (f32, f32)) -> Option<(InputId, Rect, f32)> {
        for id in [InputId::Palette, InputId::Find] {
            if let Some((rect, pad)) = self.input_rect_for(id, layout) {
                if rect.contains(p) {
                    return Some((id, rect, pad));
                }
            }
        }
        None
    }


    // ---- Modal dialogs ----

    fn request_delete(&mut self, target: usize) {
        if self.skip_delete_confirm {
            self.perform_delete(target);
            return;
        }
        let name = self
            .workspace
            .tree
            .nodes
            .get(target)
            .map(|n| n.name.clone())
            .unwrap_or_default();
        let msg = format!("Are you sure you want to delete '{}'?", name);
        if let Some(g) = self.gpu.as_mut() {
            g.ui.dialog.set(
                &mut g.font_system,
                &msg,
                &["Delete", "Cancel"],
                Some("Do not ask me again"),
            );
        }
        self.dialog = Some(DialogState {
            action: DialogAction::DeleteNode(target),
            has_check: true,
            checked: false,
            hovered: None,
        });
        self.redraw();
    }

    fn perform_delete(&mut self, target: usize) {
        if let Some(n) = self.workspace.tree.nodes.get(target) {
            let path = n.path.clone();
            let res = if n.is_dir {
                std::fs::remove_dir_all(&path)
            } else {
                std::fs::remove_file(&path)
            };
            if res.is_ok() {
                self.workspace.tree.refresh();
            }
        }
        self.redraw();
    }

    fn request_close(&mut self, idx: usize) {
        let dirty = self.workspace.documents.get(idx).map(|d| d.dirty).unwrap_or(false);
        if !dirty {
            self.workspace.close_idx(idx);
            self.redraw();
            return;
        }
        let name = self
            .workspace
            .documents
            .get(idx)
            .map(|d| d.name.clone())
            .unwrap_or_default();
        let msg = format!("Do you want to save the changes you made to {}?", name);
        if let Some(g) = self.gpu.as_mut() {
            g.ui.dialog
                .set(&mut g.font_system, &msg, &["Save", "Don't Save", "Cancel"], None);
        }
        self.dialog = Some(DialogState {
            action: DialogAction::CloseDoc(idx),
            has_check: false,
            checked: false,
            hovered: None,
        });
        self.redraw();
    }

    fn request_close_active(&mut self) {
        if let Some(i) = self.workspace.active {
            self.request_close(i);
        }
    }

    fn close_dialog(&mut self) {
        self.dialog = None;
        self.redraw();
    }

    fn dialog_click(&mut self, i: usize) {
        let Some(ds) = self.dialog.take() else {
            return;
        };
        match ds.action {
            DialogAction::DeleteNode(t) => {
                // 0 = Delete, 1 = Cancel
                if i == 0 {
                    if ds.checked {
                        self.skip_delete_confirm = true;
                    }
                    self.perform_delete(t);
                }
            }
            DialogAction::CloseDoc(idx) => {
                // 0 = Save, 1 = Don't Save, 2 = Cancel
                match i {
                    0 => {
                        if let Some(d) = self.workspace.documents.get_mut(idx) {
                            let _ = d.save();
                        }
                        self.workspace.close_idx(idx);
                    }
                    1 => self.workspace.close_idx(idx),
                    _ => {}
                }
            }
            DialogAction::InstallUpdate => {
                // 0 = Install & Restart, 1 = Later
                if i == 0 {
                    update::install_async(self.worker_tx.clone());
                    self.show_info_dialog("Downloading update… the app will restart when it's ready.");
                }
            }
            DialogAction::Dismiss => {}
        }
        self.redraw();
    }

    /// Prompt to install an available update.
    fn show_update_prompt(&mut self, version: &str) {
        let msg = format!(
            "Nova v{version} is available (you have v{}). Install and restart?",
            update::current_version()
        );
        if let Some(g) = self.gpu.as_mut() {
            g.ui.dialog.set(&mut g.font_system, &msg, &["Install & Restart", "Later"], None);
        }
        self.dialog = Some(DialogState { action: DialogAction::InstallUpdate, has_check: false, checked: false, hovered: None });
        self.redraw();
    }

    /// Show an info-only dialog with a single dismiss button.
    fn show_info_dialog(&mut self, msg: &str) {
        if let Some(g) = self.gpu.as_mut() {
            g.ui.dialog.set(&mut g.font_system, msg, &["OK"], None);
        }
        self.dialog = Some(DialogState { action: DialogAction::Dismiss, has_check: false, checked: false, hovered: None });
        self.redraw();
    }

    /// Relaunch the (freshly updated) executable and exit this process.
    fn restart_app(&self) {
        if let Ok(exe) = std::env::current_exe() {
            let _ = std::process::Command::new(exe).spawn();
        }
        std::process::exit(0);
    }

    /// Open the command palette and focus its input.
    fn open_palette(&mut self) {
        self.palette.open();
        if let Some(g) = self.gpu.as_mut() {
            g.ui.palette_input.clear(&mut g.font_system);
            g.ui.palette_input.focus(true);
        }
        self.redraw();
    }

    /// Re-filter the palette from its input's current text.
    fn refilter_palette(&mut self) {
        let q = self
            .gpu
            .as_ref()
            .map(|g| g.ui.palette_input.text().to_string())
            .unwrap_or_default();
        self.palette.refilter(&q);
    }

    fn exec_command(&mut self, cmd: Command) {
        match cmd {
            Command::Save => {
                let saved_path = self.workspace.active_doc().and_then(|d| d.path.clone());
                if let Some(d) = self.workspace.active_doc_mut() {
                    let _ = d.save();
                }
                // Saving settings.json applies the new values immediately.
                if let Some(p) = saved_path {
                    if settings::is_user_settings(&p) {
                        self.apply_settings();
                    }
                }
                self.refresh_source_control(); // a save changes git status → update badge
            }
            Command::Close => {
                self.request_close_active();
            }
            Command::Find => {
                self.find.active = true;
                if let Some(g) = self.gpu.as_mut() {
                    g.ui.find_input.clear(&mut g.font_system);
                    g.ui.find_input.set_placeholder(&mut g.font_system, " Find...");
                    g.ui.find_input.focus(true);
                }
            }
            Command::Undo => {
                if let Some(gpu) = self.gpu.as_mut() {
                    if let Some(d) = self.workspace.active_doc_mut() {
                        d.undo(&mut gpu.font_system);
                    }
                }
                self.ensure_cursor_visible();
            }
            Command::Redo => {
                if let Some(gpu) = self.gpu.as_mut() {
                    if let Some(d) = self.workspace.active_doc_mut() {
                        d.redo(&mut gpu.font_system);
                    }
                }
                self.ensure_cursor_visible();
            }
            Command::SelectAll => {
                if let Some(d) = self.workspace.active_doc_mut() {
                    d.select_all();
                }
            }
            Command::ToggleSidebar => {
                self.sidebar_visible = !self.sidebar_visible;
            }
            Command::NewFile => {
                if let Some(gpu) = self.gpu.as_mut() {
                    let d = Document::new(None, String::new(), &mut gpu.font_system);
                    self.workspace.documents.push(d);
                    self.workspace.active = Some(self.workspace.documents.len() - 1);
                }
            }
            Command::OpenSettings => self.open_settings_file(settings::user_settings_path()),
            Command::OpenDefaultSettings => self.open_settings_file(settings::default_settings_path()),
            Command::ToggleTerminal => self.toggle_terminal(),
            Command::OpenFolder => {
                if let Some(folder) = rfd::FileDialog::new().pick_folder() {
                    self.open_folder(folder);
                }
            }
        }
        self.redraw();
    }

    /// Switch the workspace to `folder`: re-root the file tree (and the find-in-files
    /// root), update the explorer header, and clear stale search state. Open editors
    /// are kept, like VSCode.
    fn open_folder(&mut self, folder: PathBuf) {
        self.cwd = folder.clone();
        self.terminal.set_cwd(folder.clone()); // new shells start in the new root
        if let Some(scp) = self.source_control.as_mut() {
            scp.set_root(folder.clone());
        }
        self.workspace.tree = crate::workspace::FileTree::new(folder);
        self.sidebar_view = SidebarView::Explorer;
        self.sidebar_visible = true;
        if let Some(sp) = self.search.as_mut() {
            sp.reset();
        }
        self.refresh_source_control(); // update the change-count badge for the new repo
        self.persist_state(); // remember this folder for the next launch
        self.redraw();
    }

    /// Persist machine-managed session state (zoom + last workspace) to
    /// `~/.nova/state.json` so the next launch restores it.
    fn persist_state(&self) {
        state::State {
            zoom: Some(theme::ui_zoom()),
            last_workspace: Some(self.cwd.clone()),
        }
        .save();
    }

    // Integrated-terminal actions live on `ui::terminal_panel::TerminalPanel`; these
    // The terminal panel owns its tab/pane/split actions (driven from its own
    // `content_press`); `App` only handles toggling the panel's visibility.
    /// Show/hide the integrated terminal, spawning the first tab on first open.
    fn toggle_terminal(&mut self) {
        if self.terminal.toggle() {
            // Panel rect is only non-None now that `visible` is true.
            let panel = self.layout().terminal_panel;
            self.terminal.spawn_initial(panel, self.terminal_cell_w);
        }
        self.redraw();
    }

    /// Re-read settings.json and apply everything: sidebar visibility, color theme,
    /// and editor font (size/family/line-height) by reshaping open documents and the
    /// gutter. tabSize/insertSpaces/cursorBlinking are read on demand elsewhere.
    /// Set the global UI zoom and re-shape every cached text buffer at the new size.
    fn set_zoom(&mut self, zoom: f32) {
        theme::set_ui_zoom(zoom); // bumps the shape epoch
        if let Some(g) = self.gpu.as_mut() {
            for d in self.workspace.documents.iter_mut() {
                d.reshape(&mut g.font_system);
            }
            g.ui.line_numbers.invalidate();
            g.ui.line_numbers2.invalidate();
            g.menubar.reshape(&mut g.font_system);
            for b in g.activity_btns.iter_mut() {
                b.reshape(&mut g.font_system);
            }
            for b in g.explorer_btns.iter_mut() {
                b.reshape(&mut g.font_system);
            }
            for b in g.titlebar_btns.iter_mut() {
                b.reshape(&mut g.font_system);
            }
            for b in g.layout_btns.iter_mut() {
                b.reshape(&mut g.font_system);
            }
            for b in g.terminal_btns.iter_mut() {
                b.reshape(&mut g.font_system);
            }
            g.tab_close_btn.reshape(&mut g.font_system);
            g.search.reshape(&mut g.font_system); // top command-center search bar
            for l in g.terminal_tabs.iter_mut() {
                l.reshape(&mut g.font_system); // panel header tabs (PROBLEMS/OUTPUT/…)
            }
            // Tabs buffer re-shapes only on content change — force it on zoom.
            self.ui_cache.tabs.clear();
            g.ui.img_minus.reshape(&mut g.font_system);
            g.ui.img_plus.reshape(&mut g.font_system);
            g.ui.img_fit.reshape(&mut g.font_system);
            g.ui.zoom_minus.reshape(&mut g.font_system);
            g.ui.zoom_plus.reshape(&mut g.font_system);
            g.ui.palette_input.rezoom(&mut g.font_system);
            g.ui.find_input.rezoom(&mut g.font_system);
            g.create_input.rezoom(&mut g.font_system);
            g.ui.ext_detail.reshape(&mut g.font_system);
        }
        if let (Some(scp), Some(g)) = (self.source_control.as_mut(), self.gpu.as_mut()) {
            scp.reshape(&mut g.font_system);
        }
        if let (Some(f), Some(g)) = (self.feedback_form.as_mut(), self.gpu.as_mut()) {
            f.rezoom(&mut g.font_system);
        }
        if let (Some(sp), Some(g)) = (self.search.as_mut(), self.gpu.as_mut()) {
            sp.rezoom(&mut g.font_system);
        }
        if let (Some(ep), Some(g)) = (self.extensions_panel.as_mut(), self.gpu.as_mut()) {
            ep.rezoom(&mut g.font_system);
        }
        // Terminal: re-seed the cell advance and mark panes dirty so their grids
        // re-shape + reflow (cols/rows) at the new font size.
        self.terminal_cell_w = theme::FONT_SIZE() * 0.6;
        for grp in self.terminal.groups.iter_mut() {
            for pane in grp.panes.iter_mut() {
                pane.dirty = true;
            }
        }
        self.persist_state(); // remember the zoom level for the next launch
        self.redraw();
    }

    fn zoom_step(&mut self, delta: f32) {
        let z = (theme::ui_zoom() + delta).clamp(0.5, 3.0);
        self.set_zoom(z);
    }

    /// Drive language-server document sync from the idle tick: open any served
    /// document not yet sent to a server, and send a debounced full-text didChange
    /// for edited ones. Centralized here so file-open/edit paths stay LSP-agnostic.
    fn sync_lsp(&mut self) {
        let Some(ext_dir) = crate::extensions::extensions_dir() else { return };
        let ext_roots = [ext_dir];
        let now = Instant::now();
        let debounce = now.duration_since(self.lsp_last_sync) > Duration::from_millis(250);

        struct Work { uri: String, lang: &'static str, version: i32, text: String, open: bool }
        let mut work: Vec<Work> = Vec::new();
        for d in &self.workspace.documents {
            let (Some(lang), Some(uri)) = (d.language_id(), d.uri()) else { continue };
            if lsp::server_for_language(lang).is_none() {
                continue;
            }
            let open = !d.lsp_open;
            let change = d.lsp_dirty && debounce;
            if open || change {
                work.push(Work { uri, lang, version: d.version, text: d.text(), open });
            }
        }
        if work.is_empty() {
            return;
        }
        // Ensure the (single, Phase-1) ESLint server is running for this workspace.
        if let Err(e) = self.lsp.ensure_eslint(&self.cwd, &ext_roots, &self.worker_tx) {
            eprintln!("[lsp:eslint] not started: {e}");
            // Don't retry every tick once we know it can't start; mark docs handled.
            for d in self.workspace.documents.iter_mut() {
                d.lsp_open = true;
                d.lsp_dirty = false;
            }
            return;
        }
        for w in &work {
            if w.open {
                self.lsp.did_open("eslint", &w.uri, w.lang, w.version, &w.text);
            } else {
                self.lsp.did_change("eslint", &w.uri, w.version, &w.text);
            }
            // ESLint uses pull diagnostics — request them and map the id back to the URI.
            if let Some(id) = self.lsp.pull_diagnostics("eslint", &w.uri) {
                self.lsp_diag_pending.insert(id, w.uri.clone());
            }
        }
        for d in self.workspace.documents.iter_mut() {
            if let Some(uri) = d.uri() {
                if work.iter().any(|w| w.uri == uri) {
                    d.lsp_open = true;
                    d.lsp_dirty = false;
                }
            }
        }
        self.lsp_last_sync = now;
    }

    fn apply_settings(&mut self) {
        let s = settings::reload();
        self.sidebar_visible = s.workbench_sidebar_visible;
        self.apply_theme_by_name(&s.workbench_color_theme);
        if let Some(gpu) = self.gpu.as_mut() {
            gpu.ui.line_numbers.invalidate();
            for d in self.workspace.documents.iter_mut() {
                d.reshape(&mut gpu.font_system);
            }
        }
        self.redraw();
    }

    /// Apply a color theme by its `workbench.colorTheme` name. "Nova Dark" is the
    /// built-in default; other names match against installed theme extensions.
    fn apply_theme_by_name(&self, name: &str) {
        if name.eq_ignore_ascii_case("Nova Dark") || name.is_empty() {
            theme::set(theme::Theme::dark());
            return;
        }
        for e in &self.extensions {
            if e.kind == ExtKind::Theme && e.name.eq_ignore_ascii_case(name) {
                if let Some(p) = &e.theme_path {
                    if let Some(t) = theme::load_vscode(p) {
                        theme::set(t);
                        return;
                    }
                }
            }
        }
        // Unknown theme name — keep the current theme.
    }

    /// Open a settings file (user or default) as a document tab, dismissing any
    /// open extension page so it shows in the editor area.
    fn open_settings_file(&mut self, path: Option<PathBuf>) {
        let Some(path) = path else { return };
        if let Some(gpu) = self.gpu.as_mut() {
            if self.workspace.open_file(&path, &mut gpu.font_system).is_ok() {
                self.detail.open_extension = None;
            }
        }
    }

    fn copy(&mut self) {
        let Some(text) = self.workspace.active_doc().and_then(|d| d.selected_text()) else {
            return;
        };
        let n = text.len();
        if let Some(cb) = self.clipboard.as_mut() {
            let t = std::time::Instant::now();
            let _ = cb.set_text(text);
            perf::log(&format!("copy: clipboard set_text({n}B) {:?}", t.elapsed()));
        }
    }

    fn paste(&mut self) {
        let t0 = std::time::Instant::now();
        let text = match self.clipboard.as_mut().and_then(|cb| cb.get_text().ok()) {
            Some(t) => t,
            None => return,
        };
        let t_clip = t0.elapsed();
        let n = text.len();
        let t1 = std::time::Instant::now();
        if let Some(gpu) = self.gpu.as_mut() {
            if let Some(d) = self.workspace.active_doc_mut() {
                d.insert_str(&text, &mut gpu.font_system);
            }
        }
        perf::log(&format!("paste: clipboard get_text {:?}, insert+reshape({n}B) {:?}", t_clip, t1.elapsed()));
        self.ensure_cursor_visible();
    }

    fn cut(&mut self) {
        self.copy();
        if let Some(gpu) = self.gpu.as_mut() {
            if let Some(d) = self.workspace.active_doc_mut() {
                d.delete_selection(&mut gpu.font_system);
            }
        }
        self.ensure_cursor_visible();
    }

    fn find_step(&mut self, forward: bool) {
        let needle = self
            .gpu
            .as_ref()
            .map(|g| g.ui.find_input.text().to_string())
            .unwrap_or_default();
        if needle.is_empty() {
            return;
        }
        let Some(gpu) = self.gpu.as_mut() else {
            return;
        };
        let Some(d) = self.workspace.active_doc_mut() else {
            return;
        };
        let from = if forward {
            d.sel.head
        } else {
            let (lo, _) = d.sel.range();
            lo
        };
        let result = if forward {
            d.find_next(&needle, from + if d.sel.is_empty() { 0 } else { needle.len() })
        } else {
            d.find_prev(&needle, from)
        };
        if let Some(pos) = result {
            d.sel.anchor = pos;
            d.sel.head = pos + needle.len();
            d.sel.desired_col = None;
            self.find.last_match = Some(pos);
            let _ = gpu;
        }
        self.ensure_cursor_visible();
    }

    // ---- Input dispatch ----

    fn title_btn_at(&self, x: f32, y: f32, layout: &Layout) -> Option<usize> {
        layout.title_btn_rects().iter().position(|r| r.contains((x, y)))
    }

    fn on_mouse_press(&mut self, x: f32, y: f32) {
        let layout = self.layout();

        // The feedback form is modal: it swallows all clicks while open.
        if self.feedback_form.is_some() {
            let win = self.gpu.as_ref().map_or((1280.0, 800.0), |g| (g.config.width as f32, g.config.height as f32));
            let clicks = if self.register_click(x, y) { 2 } else { 1 };
            let act = self
                .feedback_form
                .as_mut()
                .map(|f| f.on_press((x, y), win, clicks))
                .unwrap_or(ui::feedback_form::FormAction::None);
            self.handle_feedback_action(act);
            return;
        }

        // A modal dialog swallows all clicks: checkbox toggles, buttons act.
        if let Some(has_check) = self.dialog.as_ref().map(|d| d.has_check) {
            let hit = self.gpu.as_ref().map(|g| {
                let win = (g.config.width as f32, g.config.height as f32);
                let box_ = g.ui.dialog.box_rect(win, has_check);
                if has_check && g.ui.dialog.check_hit(box_, (x, y)) {
                    (true, usize::MAX)
                } else if let Some(i) = g.ui.dialog.button_at(box_, (x, y)) {
                    (false, i)
                } else {
                    (false, usize::MAX)
                }
            });
            match hit {
                Some((true, _)) => {
                    if let Some(d) = self.dialog.as_mut() {
                        d.checked = !d.checked;
                    }
                    self.redraw();
                }
                Some((false, i)) if i != usize::MAX => self.dialog_click(i),
                _ => {}
            }
            return;
        }

        // A click while the context menu is open selects an item or dismisses it.
        if self.explorer.menu_open() {
            let item = self.gpu.as_ref().and_then(|g| self.explorer.menu_item_at((x, y), g));
            if let Some(i) = item {
                self.exec_menu_action(MENU_ACTIONS[i].0);
            } else {
                self.close_context_menu();
            }
            return;
        }

        // A click while a top menu-bar dropdown is open: another title switches,
        // a dropdown entry runs, anywhere else dismisses.
        if let Some(open) = self.open_menu {
            let layout = self.layout();
            let title = self
                .gpu
                .as_ref()
                .and_then(|g| g.menubar.item_at(layout.menu_bar_rect(), (x, y)));
            if let Some(t) = title {
                if t == open {
                    self.close_app_menu();
                } else {
                    self.open_app_menu(t);
                }
            } else if let Some(i) = self.menu_dd_item_at(x, y) {
                if let Some(e) = menus::entries(open).get(i) {
                    let cmd = e.cmd;
                    self.exec_menu_cmd(cmd);
                }
            } else {
                self.close_app_menu();
            }
            return;
        }

        // Image tab: zoom-control buttons, else begin drag-to-pan.
        if let Some(key) = self.workspace.active_doc().and_then(|d| d.image.clone()) {
            let region = render::editor_region(&layout);
            if region.contains((x, y)) {
                let cells = render::image_ctrl_cells(region);
                let hit = cells.iter().position(|c| c.contains((x, y)));
                if let Some(h) = hit {
                    let c = (region.x + region.w * 0.5, region.y + region.h * 0.5);
                    let size = self.gpu.as_ref().and_then(|g| g.media.size(&key));
                    if let Some(d) = self.workspace.active_doc_mut() {
                        match h {
                            0 => {
                                if let Some((iw, ih)) = size {
                                    d.image_zoom_at(c, region, iw, ih, 1.0 / 1.25);
                                }
                            }
                            1 => d.image_actual(),
                            2 => {
                                if let Some((iw, ih)) = size {
                                    d.image_zoom_at(c, region, iw, ih, 1.25);
                                }
                            }
                            _ => d.image_fit(),
                        }
                    }
                    self.redraw();
                } else {
                    self.image_drag_last = Some((x, y)); // start panning
                }
                return;
            }
        }

        // A click anywhere while an inline create field is open commits it
        // (creates if a name was typed, discards if empty), then consumes the click.
        if self.explorer.creating.is_some() {
            self.commit_create();
            return;
        }

        // Scrollbar thumb/track press — the scrollbar is an overlay, so it gets the
        // click before any region handler (terminal focus, extension rows, editor).
        // Guarded by visibility so a stale off-screen thumb can't grab the click.
        if layout.palette.is_none() {
            if self.terminal.pane_scroll_press((x, y)) {
                return;
            }
            if self.detail.open_extension.is_some() {
                if self.detail.ext_detail_scroll.press((x, y)) {
                    self.redraw();
                    return;
                }
            } else if let Some(d) = self.workspace.active_doc_mut() {
                if d.scroll.press((x, y)) {
                    self.redraw();
                    return;
                }
            }
        }

        // Terminal panel resize handle (top edge) — let its Splitter claim the
        // press before the focus/scroll handlers. The handle straddles the panel
        // edge, so check it ahead of the in-panel focus test below.
        if layout.palette.is_none() {
            if let Some(panel) = layout.terminal_panel {
                if self.terminal.split.press((x, y), panel) {
                    self.terminal.maximized = false; // dragging restores from maximized
                    return;
                }
            }
        }

        // Terminal panel: header buttons, tab list, and pane-focus — the panel owns
        // its region's press handling. Clicking elsewhere while visible drops focus
        // (handled inside) without consuming the click.
        if self.terminal.content_press((x, y), &layout, self.terminal_cell_w) {
            self.redraw();
            return;
        }

        // Click inside a focused text input: position the caret and begin a
        // drag-selection. (Handled before other regions so it wins the click.)
        if let Some((id, rect, pad)) = self.focused_input_at(&layout, (x, y)) {
            let double = self.register_click(x, y);
            if let Some(g) = self.gpu.as_mut() {
                let inp = match id {
                    InputId::Palette => &mut g.ui.palette_input,
                    InputId::Find => &mut g.ui.find_input,
                };
                if double {
                    inp.select_word_at(rect, pad, x);
                } else {
                    inp.set_caret_from_x(rect, pad, x);
                }
            }
            self.text_drag = Some(id);
            self.redraw();
            return;
        }

        // Sidebar resize handle — let the Splitter claim the press.
        if self.sidebar_visible
            && layout.palette.is_none()
            && self.sidebar_split.press((x, y), layout.sidebar)
        {
            return;
        }

        // Title bar: window controls or drag.
        if layout.palette.is_none() && layout.title_bar.contains((x, y)) {
            // Command-center search box opens the palette (VSCode quick open).
            if layout.header_search_rect().contains((x, y)) {
                self.open_palette();
                return;
            }
            // Layout toggles: [0] primary (left) sidebar, [1] bottom panel
            // (integrated terminal), [2] secondary sidebar is still a placeholder.
            if let Some(i) = layout.layout_btn_rects().iter().position(|r| r.contains((x, y))) {
                match i {
                    0 => {
                        self.sidebar_visible = !self.sidebar_visible;
                        self.redraw();
                    }
                    1 => self.toggle_terminal(),
                    _ => {}
                }
                return;
            }
            // Menu titles open their dropdown.
            if let Some(idx) = self
                .gpu
                .as_ref()
                .and_then(|g| g.menubar.item_at(layout.menu_bar_rect(), (x, y)))
            {
                self.open_app_menu(idx);
                return;
            }
            match self.title_btn_at(x, y, &layout) {
                Some(0) => {
                    if let Some(g) = self.gpu.as_ref() {
                        g.window.set_minimized(true);
                    }
                }
                Some(1) => {
                    if let Some(g) = self.gpu.as_ref() {
                        let m = g.window.is_maximized();
                        g.window.set_maximized(!m);
                    }
                }
                Some(2) => {
                    self.pending_close = true;
                }
                _ => {
                    if let Some(g) = self.gpu.as_ref() {
                        let _ = g.window.drag_window();
                    }
                }
            }
            return;
        }

        // Palette
        if let Some(pal) = layout.palette.as_ref() {
            if !pal.box_.contains((x, y)) {
                self.palette.close();
                self.redraw();
                return;
            }
            let row = self
                .gpu
                .as_ref()
                .and_then(|gpu| gpu.ui.palette_list.row_at(pal.list, (x, y), self.palette.filtered.len()));
            if let Some(idx) = row {
                self.palette.selected = idx;
                if let Some(cmd) = self.palette.selected_command() {
                    self.palette.close();
                    self.exec_command(cmd);
                }
            }
            return;
        }

        if layout.status_bar.contains((x, y)) {
            let cells = render::zoom_ctrl_cells(layout.status_bar);
            if cells[0].contains((x, y)) {
                self.zoom_step(-0.1);
            } else if cells[2].contains((x, y)) {
                self.zoom_step(0.1);
            } else if cells[1].contains((x, y)) {
                self.set_zoom(1.0); // click the % to reset
            }
            return;
        }

        if let Some(idx) = layout.activity_rects().iter().position(|r| r.contains((x, y))) {
            // 0 = Explorer, 4 = Extensions. Clicking the active view's icon toggles
            // the sidebar; clicking another switches to it (and shows the sidebar).
            let view = match idx {
                0 => Some(SidebarView::Explorer),
                1 => Some(SidebarView::Search),
                2 => Some(SidebarView::SourceControl),
                4 => Some(SidebarView::Extensions),
                _ => None,
            };
            if let Some(v) = view {
                if v == SidebarView::Extensions && self.extensions.is_empty() {
                    self.extensions = extensions::scan();
                    self.rebuild_ext_rows();
                }
                if v == SidebarView::SourceControl {
                    if let (Some(scp), Some(g)) = (self.source_control.as_mut(), self.gpu.as_mut()) {
                        scp.refresh(&mut g.font_system);
                    }
                }
                if self.sidebar_view == v && self.sidebar_visible {
                    self.sidebar_visible = false;
                } else {
                    self.sidebar_view = v;
                    self.sidebar_visible = true;
                }
                // Switching views clears any prior input focus.
                self.set_ext_filter_focus(false);
                if let Some(sp) = self.search.as_mut() {
                    sp.set_unfocused();
                }
                if let Some(scp) = self.source_control.as_mut() {
                    scp.set_unfocused();
                }
                self.redraw();
            }
            return;
        }

        // Extensions panel owns its sidebar-content region (filter box, scrollbar,
        // rows). Only hand it presses inside the sidebar so the chrome stays clickable.
        if self.sidebar_visible
            && self.sidebar_view == SidebarView::Extensions
            && layout.sidebar.contains((x, y))
        {
            let region = layout.tree_region();
            let double = self.register_click(x, y);
            let mut intents = Vec::new();
            let consumed = self
                .extensions_panel
                .as_mut()
                .map_or(false, |ep| ep.on_press((x, y), region, double, &mut intents));
            for i in intents {
                self.apply_intent(i);
            }
            if consumed {
                self.redraw();
                return;
            }
        }

        // Search view: the panel owns its sidebar-content region (query/replace
        // inputs, option toggles, scrollbar, results). Only hand it presses that
        // land inside the sidebar — so the activity bar, title bar, splitter and
        // editor (handled above/below) stay clickable.
        if self.sidebar_visible
            && self.sidebar_view == SidebarView::Search
            && layout.sidebar.contains((x, y))
        {
            let region = layout.tree_region();
            let double = self.register_click(x, y);
            let root = self.cwd.clone();
            let mut intents = Vec::new();
            let mut consumed = false;
            if let (Some(sp), Some(g)) = (self.search.as_mut(), self.gpu.as_mut()) {
                consumed = sp.on_press(
                    (x, y),
                    region,
                    double,
                    &mut g.font_system,
                    root,
                    &self.worker_tx,
                    &mut intents,
                );
            }
            for i in intents {
                self.apply_intent(i);
            }
            if consumed {
                self.redraw();
                return;
            }
        }

        // Source Control: click a changed-file row to open it.
        if self.sidebar_visible
            && self.sidebar_view == SidebarView::SourceControl
            && layout.sidebar.contains((x, y))
        {
            let region = layout.tree_region();
            let mut intents = Vec::new();
            let consumed = self
                .source_control
                .as_mut()
                .map_or(false, |scp| scp.on_press((x, y), region, &mut intents));
            for i in intents {
                self.apply_intent(i);
            }
            if consumed {
                self.redraw();
                return;
            }
        }

        // Explorer header action buttons (New File / New Folder / Refresh / Collapse).
        if self.sidebar_visible
            && self.sidebar_view == SidebarView::Explorer
        {
            if let Some(i) = layout
                .explorer_action_rects()
                .iter()
                .position(|r| r.contains((x, y)))
            {
                match i {
                    0 => self.begin_create(false),
                    1 => self.begin_create(true),
                    2 => self.workspace.tree.refresh(),
                    3 => self.workspace.tree.collapse_all(),
                    _ => {}
                }
                self.redraw();
                return;
            }
        }

        if self.sidebar_visible && layout.sidebar.contains((x, y)) {
            // Tree scrollbar thumb/track press claims the click before row selection.
            if self.sidebar_view == SidebarView::Explorer && self.explorer.scroll.press((x, y)) {
                self.redraw();
                return;
            }
            let row = self.gpu.as_ref().and_then(|gpu| {
                gpu.ui.sidebar.row_at_scrolled(
                    layout.tree_region(),
                    self.explorer.scroll.offset().1,
                    (x, y),
                    self.workspace.tree.nodes.len(),
                )
            });
            if let Some(idx) = row {
                self.selected_tree = Some(idx);
                let is_dir = self.workspace.tree.nodes[idx].is_dir;
                if is_dir {
                    self.workspace.tree.toggle(idx);
                } else {
                    let path = self.workspace.tree.nodes[idx].path.clone();
                    // Route through open_file_at so images open as image tabs.
                    self.open_file_at(path, 1, 0);
                }
                self.redraw();
            }
            return;
        }

        if layout.tab_strip.contains((x, y)) {
            let tab_rects = layout.tab_rects(self.tab_count());
            let ext_idx = self.ext_tab_index();
            if let Some(idx) = tab_rects.iter().position(|r| r.contains((x, y))) {
                let closing = Layout::tab_close_rect(tab_rects[idx]).contains((x, y));
                if Some(idx) == ext_idx {
                    // The extension page's own tab: close it, or it's already shown.
                    if closing {
                        self.detail.open_extension = None;
                    }
                } else if closing {
                    self.request_close(idx);
                } else {
                    self.workspace.switch_to(idx);
                    self.detail.open_extension = None;
                }
                self.redraw();
            }
            return;
        }

        // Extension details page (in the editor area): handle its Install button
        // and consume other clicks so they don't fall through to the editor.
        if self.detail.open_extension.is_some() {
            let region = render::editor_region(&layout);
            if region.contains((x, y)) {
                // A click on a README link opens it in the browser.
                let scroll = self.detail.ext_detail_scroll.offset().1;
                let link = self.gpu.as_ref().and_then(|g| {
                    g.ui.ext_detail
                        .link_rects(region, scroll, &|k| g.media.size(k))
                        .into_iter()
                        .find(|(r, _)| r.contains((x, y)))
                        .map(|(_, url)| url)
                });
                if let Some(url) = link {
                    open_url(&url);
                    return;
                }
                let tab = self.gpu.as_ref().and_then(|g| g.ui.ext_detail.hit_tab(region, (x, y)));
                if let Some(tab) = tab {
                    if let Some(g) = self.gpu.as_mut() {
                        g.ui.ext_detail.set_tab(tab);
                    }
                    self.detail.ext_detail_scroll.scroll_to_y(0.0); // each tab scrolls from the top
                    self.redraw();
                    return;
                }
                let hit = self.gpu.as_ref().map(|g| g.ui.ext_detail.hit_install(region, (x, y))).unwrap_or(false);
                if hit {
                    self.install_open();
                }
                return;
            }
        }

        if let Some(fb) = layout.find_bar.as_ref() {
            if fb.contains((x, y)) {
                return;
            }
        }

        if layout.editor_text.contains((x, y)) {
            self.set_ext_filter_focus(false); // editor takes keyboard focus
            if let Some(sp) = self.search.as_mut() {
                sp.set_unfocused();
            }
            if let Some(scp) = self.source_control.as_mut() {
                scp.set_unfocused();
            }
            let consecutive = self.register_click(x, y);
            let extend = self.mods.shift_key();
            if let Some(d) = self.workspace.active_doc_mut() {
                self.editor.on_press(d, &layout, x, y, extend, consecutive);
            }
            self.redraw();
            return;
        }
    }

    fn on_mouse_move(&mut self, x: f32, y: f32) {
        // Feedback form: drag-select within the focused field.
        if self.mouse_pressed && self.feedback_form.is_some() {
            let win = self.gpu.as_ref().map_or((1280.0, 800.0), |g| (g.config.width as f32, g.config.height as f32));
            if let Some(f) = self.feedback_form.as_mut() {
                f.on_drag((x, y), win);
            }
            self.redraw();
            return;
        }
        // Pan an image tab while dragging.
        if let Some((lx, ly)) = self.image_drag_last {
            if self.mouse_pressed {
                if let Some(d) = self.workspace.active_doc_mut() {
                    d.image_pan_by(x - lx, y - ly);
                }
                self.image_drag_last = Some((x, y));
                self.redraw();
                return;
            }
        }
        // Drag-select within a text input.
        if let Some(id) = self.text_drag {
            if self.mouse_pressed {
                let layout = self.layout();
                if let Some((rect, pad)) = self.input_rect_for(id, &layout) {
                    if let Some(g) = self.gpu.as_mut() {
                        let inp = match id {
                            InputId::Palette => &mut g.ui.palette_input,
                            InputId::Find => &mut g.ui.find_input,
                        };
                        inp.extend_to_x(rect, pad, x);
                    }
                    self.redraw();
                }
                return;
            }
        }
        // Scrollbar thumb drags — one ScrollView is dragging at a time.
        if self.mouse_pressed && self.terminal.pane_scroll_drag((x, y)) {
            self.redraw();
            return;
        }
        if self.mouse_pressed && self.sidebar_view == SidebarView::Extensions {
            let region = self.layout().tree_region();
            if let Some(ep) = self.extensions_panel.as_mut() {
                if ep.on_drag((x, y), region) {
                    self.redraw();
                    return;
                }
            }
        }
        if self.mouse_pressed && self.sidebar_view == SidebarView::Search {
            let region = self.layout().tree_region();
            if let Some(sp) = self.search.as_mut() {
                if sp.on_drag((x, y), region) {
                    self.redraw();
                    return;
                }
            }
        }
        // File-tree scrollbar thumb drag.
        if self.mouse_pressed && self.explorer.scroll.is_dragging() {
            if self.explorer.scroll.drag((x, y)) {
                self.redraw();
            }
            return;
        }
        if self.detail.ext_detail_scroll.is_dragging() && self.mouse_pressed {
            if self.detail.ext_detail_scroll.drag((x, y)) {
                self.redraw();
            }
            return;
        }
        if self.mouse_pressed {
            if let Some(d) = self.workspace.active_doc_mut() {
                if d.scroll.is_dragging() {
                    if d.scroll.drag((x, y)) {
                        self.redraw();
                    }
                    return;
                }
            }
        }
        if self.sidebar_split.is_dragging() && self.mouse_pressed {
            if self.sidebar_split.drag(x, theme::ACTIVITY_BAR_WIDTH()) {
                self.redraw();
            }
            return;
        }
        if self.terminal.split.is_dragging() && self.mouse_pressed {
            // Height is measured up from the panel's bottom edge (status bar top).
            let origin = self.layout().status_bar.y;
            if self.terminal.split.drag(y, origin) {
                self.redraw();
            }
            return;
        }
        if self.editor.dragging && self.mouse_pressed {
            let layout = self.layout();
            if let Some(d) = self.workspace.active_doc_mut() {
                if self.editor.on_drag(d, &layout, x, y) {
                    self.redraw();
                }
            }
        }
    }

    fn on_mouse_release(&mut self) {
        self.editor.on_release();
        self.text_drag = None;
        self.image_drag_last = None;
        if let Some(f) = self.feedback_form.as_mut() {
            f.end_drag();
        }
        self.sidebar_split.release();
        self.terminal.split.release();
        self.terminal.release_scrolls();
        self.detail.ext_detail_scroll.release();
        self.explorer.scroll.release();
        if let Some(ep) = self.extensions_panel.as_mut() {
            ep.on_release();
        }
        if let Some(sp) = self.search.as_mut() {
            sp.on_release();
        }
        if let Some(d) = self.workspace.active_doc_mut() {
            d.scroll.release();
        }
    }

    fn on_scroll(&mut self, dy: f32) {
        let layout = self.layout();
        let p = (self.mouse_pos.x as f32, self.mouse_pos.y as f32);
        // Terminal scrollback: the panel owns its pane ScrollViews; consumes the
        // event (when over the content) so the editor doesn't scroll underneath.
        if self.terminal.on_scroll(p, &layout, dy) {
            self.redraw();
            return;
        }
        // File tree scrolls when the cursor is over the tree region.
        if self.sidebar_visible && self.sidebar_view == SidebarView::Explorer {
            let region = layout.tree_region();
            if region.contains(p) && self.explorer.scroll.on_wheel(0.0, dy) {
                self.redraw();
                return;
            }
        }
        // Extensions list scrolls when the cursor is over its region (the panel
        // owns the ScrollView; metrics are set each frame in render).
        if self.sidebar_visible && self.sidebar_view == SidebarView::Extensions {
            let region = layout.tree_region();
            if let Some(ep) = self.extensions_panel.as_mut() {
                if ep.on_wheel(p, region, dy) {
                    self.redraw();
                    return;
                }
            }
        }
        // Search results scroll when the cursor is over the results region.
        if self.sidebar_visible && self.sidebar_view == SidebarView::Search {
            let region = layout.tree_region();
            if let Some(sp) = self.search.as_mut() {
                if sp.on_wheel(p, region, dy) {
                    self.redraw();
                    return;
                }
            }
        }
        // The extension detail page (README) scrolls when it's open and the cursor
        // is over the editor area.
        if self.detail.open_extension.is_some() && layout.editor_text.contains(p) {
            if self.detail.ext_detail_scroll.on_wheel(0.0, dy) {
                self.redraw();
            }
            return;
        }
        // Image tab: the wheel zooms about the cursor instead of scrolling.
        if let Some(key) = self.workspace.active_doc().and_then(|d| d.image.clone()) {
            let region = render::editor_region(&layout);
            if region.contains(p) {
                if let Some((iw, ih)) = self.gpu.as_ref().and_then(|g| g.media.size(&key)) {
                    let factor = if dy > 0.0 { 1.1 } else { 1.0 / 1.1 };
                    if let Some(d) = self.workspace.active_doc_mut() {
                        d.image_zoom_at(p, region, iw, ih, factor);
                    }
                    self.redraw();
                }
            }
            return;
        }
        if !layout.editor_text.contains(p) {
            // Could route to sidebar tree, but flat list fits fine for now.
            return;
        }
        // Editor: the active document's ScrollView owns the offset/clamp (metrics
        // are set each frame in render).
        if let Some(d) = self.workspace.active_doc_mut() {
            if d.scroll.on_wheel(0.0, dy) {
                self.redraw();
            }
        }
    }

    fn on_scroll_h(&mut self, dx: f32) {
        if let Some(d) = self.workspace.active_doc_mut() {
            if d.scroll.on_wheel(dx, 0.0) {
                self.redraw();
            }
        }
    }

    fn on_key(&mut self, event: winit::event::KeyEvent) {
        if event.state != ElementState::Pressed {
            return;
        }
        let extend = self.mods.shift_key();
        // The primary shortcut modifier: Ctrl everywhere, plus Cmd (super) on macOS
        // so Cmd+C/V/A/S/F/Enter/zoom work natively there.
        let ctrl = self.mods.control_key() || (cfg!(target_os = "macos") && self.mods.super_key());

        // The feedback form is modal: route keys to it (Esc closes, Ctrl+Enter submits).
        if self.feedback_form.is_some() {
            let mut act = ui::feedback_form::FormAction::None;
            if let (Some(form), Some(g)) = (self.feedback_form.as_mut(), self.gpu.as_mut()) {
                act = form.on_key(&event, ctrl, extend, &mut g.font_system, self.clipboard.as_mut());
            }
            self.handle_feedback_action(act);
            return;
        }

        // A modal dialog swallows keys; Escape cancels it.
        if self.dialog.is_some() {
            if matches!(event.logical_key.as_ref(), Key::Named(NamedKey::Escape)) {
                self.close_dialog();
            }
            return;
        }

        // Escape closes an open context menu first.
        if self.explorer.menu_open() && matches!(event.logical_key.as_ref(), Key::Named(NamedKey::Escape)) {
            self.close_context_menu();
            return;
        }

        if self.open_menu.is_some() && matches!(event.logical_key.as_ref(), Key::Named(NamedKey::Escape)) {
            self.close_app_menu();
            return;
        }

        // Ctrl+` toggles the terminal from anywhere (incl. while it's focused).
        if ctrl {
            if let winit::keyboard::PhysicalKey::Code(winit::keyboard::KeyCode::Backquote) =
                event.physical_key
            {
                self.toggle_terminal();
                return;
            }
        }

        // Single-authority keyboard dispatch: route to whatever element has focus.
        // Each non-editor arm fully handles its keys and returns, so nothing leaks.
        // The ExtFilter arm lets Ctrl-combos fall through to global shortcuts; the
        // Editor arm falls through to the shortcut + editor-key handling below.
        match self.focus() {
            Focus::Terminal => {
                if let Some(bytes) = translate_terminal_key(&event, ctrl, extend) {
                    if let Some(g) = self.terminal.groups.get_mut(self.terminal.active) {
                        if let Some(p) = g.panes.get_mut(g.focused) {
                            p.term.write(&bytes);
                            p.scroll.scroll_to_end(); // typing snaps to the live bottom
                            p.dirty = true;
                        }
                    }
                    self.redraw();
                }
                return;
            }
            Focus::Rename => {
                match event.logical_key.as_ref() {
                    Key::Named(NamedKey::Escape) => {
                        self.cancel_create();
                        return;
                    }
                    Key::Named(NamedKey::Backspace) => {
                        if let Some(g) = self.gpu.as_mut() {
                            g.create_input.backspace(&mut g.font_system);
                        }
                        self.redraw();
                        return;
                    }
                    Key::Named(NamedKey::Enter) => {
                        self.commit_create();
                        return;
                    }
                    _ => {}
                }
                if let Some(t) = event.text.as_ref() {
                    let s: &str = t;
                    if !s.chars().any(|c| c.is_control()) && !s.contains('/') && !s.contains('\\') {
                        if let Some(g) = self.gpu.as_mut() {
                            g.create_input.insert(&mut g.font_system, s);
                        }
                        self.redraw();
                    }
                }
                return;
            }
            Focus::Palette => {
                match event.logical_key.as_ref() {
                    Key::Named(NamedKey::Escape) => {
                        self.palette.close();
                        self.redraw();
                        return;
                    }
                    Key::Named(NamedKey::ArrowDown) => {
                        self.palette.select_next();
                        self.redraw();
                        return;
                    }
                    Key::Named(NamedKey::ArrowUp) => {
                        self.palette.select_prev();
                        self.redraw();
                        return;
                    }
                    Key::Named(NamedKey::Enter) => {
                        if let Some(cmd) = self.palette.selected_command() {
                            self.palette.close();
                            self.exec_command(cmd);
                        }
                        return;
                    }
                    _ => {}
                }
                let consumed = self.gpu.as_mut().and_then(|g| {
                    edit_input(&mut g.ui.palette_input, &mut g.font_system, self.clipboard.as_mut(), &event, ctrl, extend)
                });
                if let Some(changed) = consumed {
                    if changed {
                        self.refilter_palette();
                    }
                    self.redraw();
                }
                return;
            }
            Focus::Find => {
                match event.logical_key.as_ref() {
                    Key::Named(NamedKey::Escape) => {
                        self.find.active = false;
                        if let Some(g) = self.gpu.as_mut() {
                            g.ui.find_input.focus(false);
                        }
                        self.redraw();
                        return;
                    }
                    Key::Named(NamedKey::Enter) => {
                        self.find_step(!extend);
                        self.redraw();
                        return;
                    }
                    _ => {}
                }
                let consumed = self.gpu.as_mut().and_then(|g| {
                    edit_input(&mut g.ui.find_input, &mut g.font_system, self.clipboard.as_mut(), &event, ctrl, extend)
                });
                if consumed.is_some() {
                    self.redraw();
                }
                return;
            }
            Focus::ExtFilter => {
                // The Extensions panel owns its filter box; route the key to it. It
                // re-runs the marketplace search + rebuilds its rows on change.
                let mut handled = false;
                if let (Some(ep), Some(g)) = (self.extensions_panel.as_mut(), self.gpu.as_mut()) {
                    handled = ep.on_key(
                        &event,
                        ctrl,
                        extend,
                        g,
                        &self.extensions,
                        &self.ext_remote,
                        &self.worker_tx,
                        self.clipboard.as_mut(),
                    );
                }
                if handled {
                    self.redraw();
                    return;
                }
            }
            Focus::Search => {
                // The Search panel owns both its query and replace boxes; route the
                // key to it and apply any cross-cutting intents it returns.
                let root = self.cwd.clone();
                let mut intents = Vec::new();
                let mut handled = false;
                if let (Some(sp), Some(g)) = (self.search.as_mut(), self.gpu.as_mut()) {
                    handled = sp.on_key(
                        &event,
                        ctrl,
                        extend,
                        &mut g.font_system,
                        self.clipboard.as_mut(),
                        root,
                        &self.worker_tx,
                        &mut intents,
                    );
                }
                for i in intents {
                    self.apply_intent(i);
                }
                if handled {
                    self.redraw();
                    return;
                }
            }
            Focus::SourceControl => {
                // Route to the commit message box; Ctrl+Enter commits.
                let mut intents = Vec::new();
                let mut handled = false;
                if let (Some(scp), Some(g)) = (self.source_control.as_mut(), self.gpu.as_mut()) {
                    handled = scp.on_key(&event, ctrl, extend, &mut g.font_system, self.clipboard.as_mut(), &mut intents);
                }
                for i in intents {
                    self.apply_intent(i);
                }
                if handled {
                    self.redraw();
                    return;
                }
            }
            Focus::Editor => {}
        }

        // Ctrl shortcuts — matched on the PHYSICAL key, not the logical character.
        // With Ctrl held, winit's logical_key can arrive as a control character
        // (e.g. U+0003 for Ctrl+C), so `== "c"` would silently miss; the physical
        // KeyCode is reliable.
        if ctrl {
            use winit::keyboard::{KeyCode, PhysicalKey};
            if let PhysicalKey::Code(code) = event.physical_key {
                let shift = self.mods.shift_key();
                match code {
                    KeyCode::KeyP if shift => {
                        self.open_palette();
                        return;
                    }
                    KeyCode::KeyA => {
                        self.exec_command(Command::SelectAll);
                        return;
                    }
                    KeyCode::Equal => {
                        self.zoom_step(0.1); // Ctrl+= / Ctrl++ zoom in
                        return;
                    }
                    KeyCode::Minus => {
                        self.zoom_step(-0.1); // Ctrl+- zoom out
                        return;
                    }
                    KeyCode::Digit0 => {
                        self.set_zoom(1.0); // Ctrl+0 reset zoom
                        return;
                    }
                    KeyCode::KeyC => {
                        self.copy();
                        return;
                    }
                    KeyCode::KeyX => {
                        self.cut();
                        return;
                    }
                    KeyCode::KeyV => {
                        self.paste();
                        return;
                    }
                    KeyCode::KeyS => {
                        self.exec_command(Command::Save);
                        return;
                    }
                    KeyCode::KeyW => {
                        self.exec_command(Command::Close);
                        return;
                    }
                    KeyCode::KeyO => {
                        self.exec_command(Command::OpenFolder);
                        return;
                    }
                    KeyCode::KeyZ => {
                        self.exec_command(Command::Undo);
                        return;
                    }
                    KeyCode::KeyY => {
                        self.exec_command(Command::Redo);
                        return;
                    }
                    KeyCode::KeyF => {
                        self.exec_command(Command::Find);
                        return;
                    }
                    KeyCode::KeyB => {
                        self.exec_command(Command::ToggleSidebar);
                        return;
                    }
                    KeyCode::KeyN => {
                        self.exec_command(Command::NewFile);
                        return;
                    }
                    _ => {}
                }
            }
        }

        // Editor-targeted keys.
        let Some(gpu) = self.gpu.as_mut() else {
            return;
        };
        let Some(d) = self.workspace.active_doc_mut() else {
            return;
        };

        match event.logical_key.as_ref() {
            Key::Named(NamedKey::ArrowLeft) => {
                if ctrl {
                    d.move_word_left(extend);
                } else {
                    d.move_left(extend);
                }
            }
            Key::Named(NamedKey::ArrowRight) => {
                if ctrl {
                    d.move_word_right(extend);
                } else {
                    d.move_right(extend);
                }
            }
            Key::Named(NamedKey::ArrowUp) => {
                d.move_up(extend);
            }
            Key::Named(NamedKey::ArrowDown) => {
                d.move_down(extend);
            }
            Key::Named(NamedKey::Home) => {
                d.move_home(extend);
            }
            Key::Named(NamedKey::End) => {
                d.move_end(extend);
            }
            Key::Named(NamedKey::Backspace) => {
                if ctrl {
                    d.delete_word_back(&mut gpu.font_system);
                } else {
                    d.backspace(&mut gpu.font_system);
                }
            }
            Key::Named(NamedKey::Delete) => {
                d.delete_forward(&mut gpu.font_system);
            }
            Key::Named(NamedKey::Enter) => {
                d.insert_str("\n", &mut gpu.font_system);
            }
            Key::Named(NamedKey::Tab) => {
                let s = settings::current();
                let tab = if s.editor_insert_spaces {
                    " ".repeat(s.editor_tab_size)
                } else {
                    "\t".to_string()
                };
                d.insert_str(&tab, &mut gpu.font_system);
            }
            Key::Named(NamedKey::PageUp) => {
                let (line, _) = d.head_line_col();
                let lines_per_page = 20;
                d.move_to_line(line.saturating_sub(lines_per_page), extend);
            }
            Key::Named(NamedKey::PageDown) => {
                let (line, _) = d.head_line_col();
                d.move_to_line(line + 20, extend);
            }
            _ => {
                if ctrl {
                    return;
                }
                if let Some(t) = event.text.as_ref() {
                    let s: &str = t;
                    if !s.is_empty() && !s.chars().any(|c| c.is_control()) {
                        d.insert_str(s, &mut gpu.font_system);
                    }
                }
            }
        }
        let _ = (d, gpu);
        self.last_edit = Instant::now(); // for files.autoSave idle timer
        self.ensure_cursor_visible();
        self.redraw();
    }
}

// ---------- Rendering ----------

/// Geometry of the inline New File/Folder row within tree region `tr`:
/// returns (row rect, icon rect, text-field rect) for the given insert row/depth.
/// The single source of truth for keyboard focus: which element receives keys.
/// Derived from the open/active UI state via `App::focus()`, so there's exactly
/// one answer to "what is focused?" and `on_key` dispatches on it (no implicit
/// fallthrough that can leak keystrokes between elements).
#[derive(Clone, Copy, PartialEq, Eq)]
enum Focus {
    Editor,
    Rename,    // inline new-file/folder name entry
    Palette,   // command palette
    Find,      // find bar
    ExtFilter, // extensions search box
    Search,    // find-in-files panel (owns its own query/replace boxes)
    SourceControl, // git commit message box
    Terminal,  // integrated terminal
}

/// Open a URL in the OS default browser. Best-effort, http(s) only (so README
/// link text can't launch arbitrary commands).
/// Resolve the `gh` executable. macOS GUI apps don't inherit the shell PATH, so a
/// bare "gh" often isn't found (it lives in /opt/homebrew/bin etc.) — check the
/// common locations and fall back to a login-shell lookup before giving up.
pub(crate) fn gh_program() -> String {
    #[cfg(not(windows))]
    {
        for p in ["/opt/homebrew/bin/gh", "/usr/local/bin/gh", "/usr/bin/gh", "/home/linuxbrew/.linuxbrew/bin/gh"] {
            if std::path::Path::new(p).exists() {
                return p.to_string();
            }
        }
        if let Ok(out) = std::process::Command::new("/bin/sh").args(["-lc", "command -v gh"]).output() {
            let s = String::from_utf8_lossy(&out.stdout).trim().to_string();
            if !s.is_empty() {
                return s;
            }
        }
    }
    "gh".to_string()
}

/// Percent-encode a string for use in a URL query value.
fn urlencode(s: &str) -> String {
    let mut out = String::with_capacity(s.len() * 2);
    for b in s.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => out.push(b as char),
            _ => out.push_str(&format!("%{:02X}", b)),
        }
    }
    out
}

fn open_url(url: &str) {
    if !(url.starts_with("http://") || url.starts_with("https://")) {
        return;
    }
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        const CREATE_NO_WINDOW: u32 = 0x0800_0000; // suppress the console window flash
        let _ = std::process::Command::new("cmd")
            .args(["/c", "start", "", url])
            .creation_flags(CREATE_NO_WINDOW)
            .spawn();
    }
    #[cfg(target_os = "linux")]
    {
        let _ = std::process::Command::new("xdg-open").arg(url).spawn();
    }
    #[cfg(target_os = "macos")]
    {
        let _ = std::process::Command::new("open").arg(url).spawn();
    }
}

/// Identifies the text input under the cursor for click/drag selection.
#[derive(Clone, Copy, PartialEq, Eq)]
enum InputId {
    Palette,
    Find,
}

/// The earlier of two optional wake times (whichever is present).
fn min_instant(a: Option<Instant>, b: Option<Instant>) -> Option<Instant> {
    match (a, b) {
        (Some(x), Some(y)) => Some(x.min(y)),
        (x, None) => x,
        (None, y) => y,
    }
}


// ---------- winit glue ----------

impl ApplicationHandler for App {
    fn about_to_wait(&mut self, el: &ActiveEventLoop) {
        // Drain background worker results (marketplace search/install).
        while let Ok(msg) = self.worker_rx.try_recv() {
            match msg {
                WorkerMsg::Search { gen, results } => {
                    if self.extensions_panel.as_ref().map_or(false, |ep| ep.search_gen() == gen) {
                        self.ext_remote = results;
                        if let Some(ep) = self.extensions_panel.as_mut() {
                            ep.finish_search();
                        }
                        self.rebuild_ext_rows();
                        self.redraw();
                    }
                }
                WorkerMsg::Installed { result } => {
                    self.installing = None;
                    match result {
                        Ok(()) => {
                            self.extensions = extensions::scan();
                            self.rebuild_ext_rows();
                            self.show_info_dialog("Extension installed. Reload the window (or reopen) to activate it.");
                        }
                        Err(e) => {
                            self.show_info_dialog(&format!("Install failed: {e}"));
                        }
                    }
                    self.redraw();
                }
                WorkerMsg::Readme { gen, text } => {
                    if gen == self.detail.ext_doc_gen {
                        self.detail.ext_readme = text;
                        self.redraw();
                    }
                }
                WorkerMsg::Changelog { gen, text } => {
                    if gen == self.detail.ext_doc_gen {
                        self.detail.ext_changelog = text;
                        self.redraw();
                    }
                }
                WorkerMsg::Image { key, frames } => {
                    if let Some(g) = self.gpu.as_mut() {
                        g.media.upload_frames(&g.device, &g.queue, &key, frames);
                    }
                    self.redraw();
                }
                WorkerMsg::SearchHits { gen, files } => {
                    if let Some(sp) = self.search.as_mut() {
                        sp.ingest(gen, files);
                        self.redraw();
                    }
                }
                WorkerMsg::SearchDone { gen } => {
                    if let Some(sp) = self.search.as_mut() {
                        sp.search_done(gen);
                        self.redraw();
                    }
                }
                WorkerMsg::UpdateAvailable { version } => {
                    self.update_available = Some(version.clone());
                    self.show_update_prompt(&version);
                }
                WorkerMsg::UpdateNone => {
                    self.show_info_dialog(&format!("You're on the latest version (v{}).", update::current_version()));
                }
                WorkerMsg::UpdateDone { ok } => {
                    if ok {
                        self.restart_app();
                    } else {
                        self.show_info_dialog("Update failed. Please try again or download from GitHub.");
                    }
                }
                WorkerMsg::LspInitialized => {
                    self.lsp.on_initialized();
                }
                WorkerMsg::LspDiagnostics { uri, diags } => {
                    if let Some(d) = self
                        .workspace
                        .documents
                        .iter_mut()
                        .find(|d| d.uri().map_or(false, |u| lsp::same_uri(&u, &uri)))
                    {
                        d.diagnostics = diags;
                        self.redraw();
                    }
                }
                WorkerMsg::LspDiagnosticReport { id, diags } => {
                    if let Some(uri) = self.lsp_diag_pending.remove(&id) {
                        if let Some(d) = self
                            .workspace
                            .documents
                            .iter_mut()
                            .find(|d| d.uri().map_or(false, |u| lsp::same_uri(&u, &uri)))
                        {
                            d.diagnostics = diags;
                            self.redraw();
                        }
                    }
                }
                WorkerMsg::LspLog { server, message } => {
                    eprintln!("[lsp:{server}] {message}");
                }
                WorkerMsg::LspExited { server } => {
                    self.lsp.drop_server(server);
                }
                WorkerMsg::FeedbackDone { result } => match result {
                    Ok(url) if url.starts_with("http") => open_url(&url),
                    Ok(_) => self.show_info_dialog("Thanks! Your feedback was submitted."),
                    Err(_) => self.show_info_dialog(
                        "Couldn't submit feedback. Check that GitHub CLI (gh) is installed and you're logged in.",
                    ),
                },
            }
        }

        let now = Instant::now();

        // Language-server document sync (open + debounced didChange).
        self.sync_lsp();

        // files.autoSave (afterDelay): save dirty docs ~1s after the last edit.
        if settings::auto_save() && now.duration_since(self.last_edit) > Duration::from_millis(1000) {
            let mut saved = false;
            for d in self.workspace.documents.iter_mut() {
                if d.dirty && d.path.is_some() {
                    let _ = d.save();
                    saved = true;
                }
            }
            if saved {
                self.redraw();
            }
        }

        // Integrated terminal: drain every pane's shell output, and keep ticking
        // while open so new output appears promptly.
        if self.terminal.visible {
            let mut changed = false;
            for g in &mut self.terminal.groups {
                for p in &mut g.panes {
                    if p.term.poll() {
                        p.dirty = true;
                        changed = true;
                    }
                }
            }
            if changed {
                self.redraw();
            }
            self.cursor_blink_on = true;
            el.set_control_flow(ControlFlow::WaitUntil(now + Duration::from_millis(30)));
            return;
        }

        // While a find-in-files search is streaming, keep waking to drain its
        // results from the worker channel (otherwise idle ControlFlow::Wait would
        // never poll them).
        if self.search.as_ref().map_or(false, |sp| sp.pending()) {
            el.set_control_flow(ControlFlow::WaitUntil(now + Duration::from_millis(30)));
            return;
        }

        // If an animated GIF is visible on the detail page, tick ~20fps to play it.
        let animating = self.detail.open_extension.is_some()
            && self
                .gpu
                .as_ref()
                .map(|g| g.ui.ext_detail.image_urls().iter().any(|u| g.media.is_animated(u)))
                .unwrap_or(false);
        if animating {
            self.redraw();
            el.set_control_flow(ControlFlow::WaitUntil(now + Duration::from_millis(66)));
            return;
        }

        // Auto-hide scrollbar fades: while any is fading, keep redrawing until done.
        let scroll_wake = self.scroll_next_wake(now);
        if scroll_wake.is_some() {
            self.redraw();
        }

        // Solid cursor (editor.cursorBlinking: "solid") stays on without blinking.
        if !settings::current().editor_cursor_blink {
            if !self.cursor_blink_on {
                self.cursor_blink_on = true;
                self.redraw();
            }
            // Without blink wakeups, still wake to run the auto-save idle timer.
            let autosave_pending = settings::auto_save()
                && self.workspace.documents.iter().any(|d| d.dirty && d.path.is_some());
            let autosave_wake =
                autosave_pending.then(|| self.last_edit + Duration::from_millis(1100));
            el.set_control_flow(match min_instant(scroll_wake, autosave_wake) {
                Some(w) => ControlFlow::WaitUntil(w),
                None => ControlFlow::Wait,
            });
            return;
        }

        let interval = Duration::from_millis(theme::BLINK_MS);
        if now.duration_since(self.last_blink) >= interval {
            self.cursor_blink_on = !self.cursor_blink_on;
            self.last_blink = now;
            self.redraw();
        }
        let blink_wake = self.last_blink + interval;
        let wake = scroll_wake.map_or(blink_wake, |s| s.min(blink_wake));
        el.set_control_flow(ControlFlow::WaitUntil(wake));
    }

    fn resumed(&mut self, el: &ActiveEventLoop) {
        if self.gpu.is_some() {
            return;
        }
        let attrs = Window::default_attributes()
            .with_title("Nova")
            .with_decorations(false)
            .with_window_icon(app_icon())
            .with_inner_size(LogicalSize::new(1400.0, 900.0));
        let window = Arc::new(el.create_window(attrs).expect("create window"));
        match pollster::block_on(GpuState::new(window)) {
            Ok(gpu) => {
                self.gpu = Some(gpu);
                if let Some(g) = self.gpu.as_mut() {
                    self.search = Some(ui::search_panel::SearchPanel::new(&mut g.font_system));
                    self.extensions_panel =
                        Some(ui::extensions_panel::ExtensionsPanel::new(&mut g.font_system));
                    self.source_control = Some(ui::source_control_panel::SourceControlPanel::new(
                        &mut g.font_system,
                        self.cwd.clone(),
                    ));
                }
                self.open_initial();
                // Populate the Source Control change-count badge at startup.
                self.refresh_source_control();
                // Check GitHub for a newer release in the background.
                update::check_async(self.worker_tx.clone(), false);
            }
            Err(e) => {
                eprintln!("init failed: {e:?}");
                el.exit();
            }
        }
    }

    fn window_event(&mut self, el: &ActiveEventLoop, _id: WindowId, event: WindowEvent) {
        match event {
            WindowEvent::CloseRequested => el.exit(),
            WindowEvent::ModifiersChanged(m) => {
                self.mods = m.state();
            }
            WindowEvent::Resized(size) => {
                if let Some(g) = self.gpu.as_mut() {
                    g.resize(size.width, size.height);
                }
                self.redraw();
            }
            WindowEvent::CursorMoved { position, .. } => {
                self.mouse_pos = position;
                self.on_mouse_move(position.x as f32, position.y as f32);
                self.recompute_hover();
            }
            WindowEvent::CursorLeft { .. } => {
                self.hovered_tab = None;
                self.hovered_tab_close = None;
                self.hovered_tree = None;
                self.hovered_activity = None;
                self.redraw();
            }
            WindowEvent::MouseInput { state, button, .. } => {
                let (px, py) = (self.mouse_pos.x as f32, self.mouse_pos.y as f32);
                match (button, state) {
                    (MouseButton::Left, ElementState::Pressed) => {
                        self.mouse_pressed = true;
                        self.reset_blink();
                        self.on_mouse_press(px, py);
                        if self.pending_close {
                            el.exit();
                        }
                    }
                    (MouseButton::Left, ElementState::Released) => {
                        self.mouse_pressed = false;
                        self.on_mouse_release();
                    }
                    (MouseButton::Right, ElementState::Pressed) => {
                        self.on_right_press(px, py);
                    }
                    _ => {}
                }
            }
            WindowEvent::MouseWheel { delta, .. } => {
                let (mut dx, mut dy) = match delta {
                    MouseScrollDelta::LineDelta(x, y) => {
                        (x * theme::LINE_HEIGHT() * 3.0, y * theme::LINE_HEIGHT() * 3.0)
                    }
                    MouseScrollDelta::PixelDelta(p) => (p.x as f32, p.y as f32),
                };
                // Shift turns the vertical wheel into horizontal scroll.
                if self.mods.shift_key() && dx == 0.0 {
                    dx = dy;
                    dy = 0.0;
                }
                if dy != 0.0 {
                    self.on_scroll(dy);
                }
                if dx != 0.0 {
                    self.on_scroll_h(dx);
                }
            }
            WindowEvent::KeyboardInput { event, .. } => {
                self.reset_blink();
                self.on_key(event);
            }
            WindowEvent::RedrawRequested => {
                if let Err(e) = render::render(self) {
                    eprintln!("render: {e}");
                }
            }
            _ => {}
        }
    }
}

fn main() -> Result<()> {
    env_logger::init();
    // Optional path arg: a directory becomes the workspace root; a file is opened
    // (and its parent becomes the root). Falls back to the current directory.
    let arg = std::env::args().nth(1).map(PathBuf::from);
    let (root, initial_file) = match arg {
        Some(p) if p.is_dir() => (p, None),
        Some(p) if p.is_file() => {
            let parent = p
                .parent()
                .map(|x| x.to_path_buf())
                .unwrap_or_else(|| PathBuf::from("."));
            (parent, Some(p))
        }
        // No path arg: reopen the last workspace folder if it still exists,
        // else fall back to the current directory.
        _ => {
            let last = state::State::load().last_workspace.filter(|p| p.is_dir());
            (
                last.unwrap_or_else(|| std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."))),
                None,
            )
        }
    };
    let event_loop = EventLoop::new()?;
    let mut app = App::new(root, initial_file);
    event_loop.run_app(&mut app)?;
    Ok(())
}
