// Hide the console window — without this the binary uses the console subsystem
// and Windows spawns a terminal alongside the GUI. We still capture stderr when
// we launch nova via a redirected pipe, so no debug visibility is lost.
#![windows_subsystem = "windows"]

// Nova — Phase 1 vertical slice with VSCode-shaped UI shell.
// Activity bar, sidebar file tree, tab strip, editor (gutter + text),
// status bar, command palette (Ctrl+Shift+P), find bar (Ctrl+F).

mod commands;
mod document;
mod ext_detail;
mod ext_runtime;
mod extensions;
mod gpu;
mod icon;
mod layout;
mod markdown;
mod marketplace;
mod media;
mod quad;
mod render;
mod settings;
mod syntax;
mod terminal;
mod textmate;
mod theme;
mod widgets;
mod workspace;

use std::collections::HashSet;
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

use commands::{Command, COMMANDS, FindBarState, PaletteState};
use document::Document;
use extensions::{ExtKind, Extension, OpenExt};
use marketplace::WorkerMsg;
use gpu::GpuState;
use layout::Layout;
use widgets::{Axis, Rect, Scrollbar, Splitter};
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

/// An open right-click context menu over the file tree.
pub(crate) struct ContextMenu {
    pub(crate) anchor: (f32, f32),
    pub(crate) target: Option<usize>, // tree node index; None = empty area (root scope)
}

/// Which sidebar view the activity bar has selected.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum SidebarView {
    Explorer,
    Extensions,
}

/// What a modal dialog confirms.
pub(crate) enum DialogAction {
    DeleteNode(usize),
    CloseDoc(usize),
}

pub(crate) struct DialogState {
    pub(crate) action: DialogAction,
    pub(crate) buttons: &'static [&'static str],
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
    pub(crate) dragging_editor: bool,
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
    pub(crate) hovered_layout: Option<usize>,
    pub(crate) hovered_explorer: Option<usize>,
    pub(crate) selected_tree: Option<usize>,
    pub(crate) creating: Option<PendingCreate>,
    pub(crate) context_menu: Option<ContextMenu>,
    pub(crate) hovered_menu_item: Option<usize>,
    pub(crate) dialog: Option<DialogState>,
    pub(crate) skip_delete_confirm: bool,
    pub(crate) editor_scroll: Scrollbar,
    pub(crate) editor_hscroll: Scrollbar,
    pub(crate) hovered_scrollbar: bool,
    pub(crate) last_click: Instant,
    pub(crate) last_click_pos: (f32, f32),
    pub(crate) click_count: u32,
    pub(crate) sidebar_view: SidebarView,
    pub(crate) extensions: Vec<Extension>,
    pub(crate) hovered_ext: Option<usize>,
    pub(crate) text_drag: Option<InputId>, // active mouse drag-selection in a text input
    pub(crate) ext_filter_active: bool,
    pub(crate) ext_visible: Vec<usize>, // displayed row index -> index into the active source
    pub(crate) ext_scroll: f32,
    pub(crate) ext_remote: Vec<marketplace::RemoteExt>, // current marketplace search results
    pub(crate) ext_showing_remote: bool,                // true while a search query is active
    pub(crate) search_gen: u64,                         // discards stale background search results
    pub(crate) worker_tx: Sender<WorkerMsg>,
    pub(crate) worker_rx: Receiver<WorkerMsg>,
    pub(crate) open_extension: Option<OpenExt>, // extension detail page open in the editor area
    pub(crate) ext_readme: Option<String>,      // README text for the open detail page
    pub(crate) ext_changelog: Option<String>,   // CHANGELOG text for the open detail page
    pub(crate) ext_features: String,            // generated Features-tab markdown
    pub(crate) ext_doc_gen: u64,                // discards stale async README/changelog fetches
    pub(crate) ext_img_dir: Option<PathBuf>,    // base dir for relative README images (local)
    pub(crate) ext_img_base: Option<String>,    // base URL for relative README images (remote)
    pub(crate) requested_images: HashSet<String>, // README image keys already fetched/loading
    pub(crate) ext_detail_scroll: f32,          // detail-page body scroll offset
    pub(crate) hovered_detail_tab: Option<ext_detail::DetailTab>,
    pub(crate) hovered_page_install: bool,
    pub(crate) pending_close: bool,
    pub(crate) terminal: Option<terminal::Terminal>,
    pub(crate) terminal_visible: bool,
    pub(crate) terminal_focused: bool,
    pub(crate) terminal_split: Splitter, // draggable panel height
    // Real monospace cell advance (px), measured from the shaped terminal buffer.
    // The cursor and grid sizing use this instead of an estimate so the block
    // cursor lands exactly on the glyph cell (no per-column drift).
    pub(crate) terminal_cell_w: f32,
    // Set when shell output arrives or the panel resizes; render reshapes the
    // terminal text only when this is set, then clears it. Avoids re-shaping a
    // full screen of rich text on every unrelated redraw.
    pub(crate) terminal_dirty: bool,
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
            workspace: Workspace::new(root),
            gpu: None,
            mouse_pos: PhysicalPosition::new(0.0, 0.0),
            mouse_pressed: false,
            dragging_editor: false,
            mods: ModifiersState::empty(),
            clipboard: Clipboard::new().ok(),
            sidebar_visible: true,
            sidebar_split: Splitter::new(
                theme::SIDEBAR_WIDTH,
                theme::SIDEBAR_MIN_WIDTH,
                theme::SIDEBAR_MAX_WIDTH,
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
            hovered_layout: None,
            hovered_explorer: None,
            selected_tree: None,
            creating: None,
            context_menu: None,
            hovered_menu_item: None,
            dialog: None,
            skip_delete_confirm: false,
            editor_scroll: Scrollbar::new(Axis::Vertical),
            editor_hscroll: Scrollbar::new(Axis::Horizontal),
            hovered_scrollbar: false,
            last_click: Instant::now(),
            last_click_pos: (0.0, 0.0),
            click_count: 0,
            sidebar_view: SidebarView::Explorer,
            extensions: Vec::new(),
            hovered_ext: None,
            text_drag: None,
            ext_filter_active: false,
            ext_visible: Vec::new(),
            ext_scroll: 0.0,
            ext_remote: Vec::new(),
            ext_showing_remote: false,
            search_gen: 0,
            worker_tx,
            worker_rx,
            open_extension: None,
            ext_readme: None,
            ext_changelog: None,
            ext_features: String::new(),
            ext_doc_gen: 0,
            ext_img_dir: None,
            ext_img_base: None,
            requested_images: HashSet::new(),
            ext_detail_scroll: 0.0,
            hovered_detail_tab: None,
            hovered_page_install: false,
            pending_close: false,
            terminal: None,
            terminal_visible: false,
            terminal_focused: false,
            terminal_split: Splitter::new(
                theme::TERMINAL_HEIGHT,
                theme::TERMINAL_MIN_HEIGHT,
                theme::TERMINAL_MAX_HEIGHT,
                widgets::Axis::Vertical,
            ),
            terminal_cell_w: theme::FONT_SIZE() * 0.6, // refined after first shape
            terminal_dirty: true,
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
        if self.context_menu.is_some() {
            let new_item = self.context_menu_item_at(p);
            if new_item != self.hovered_menu_item {
                self.hovered_menu_item = new_item;
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
                gpu.ui
                    .sidebar
                    .row_at(layout.tree_region(), p, self.workspace.tree.nodes.len())
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

        let new_ext = if self.sidebar_visible
            && layout.palette.is_none()
            && self.sidebar_view == SidebarView::Extensions
        {
            let region = ext_list_region(layout.tree_region());
            let scroll = self.ext_scroll;
            self.gpu.as_ref().and_then(|g| g.ui.ext_rows.hit(region, scroll, p))
        } else {
            None
        };
        if new_ext != self.hovered_ext {
            self.hovered_ext = new_ext;
            changed = true;
        }

        let new_page_install = if self.open_extension.is_some() {
            let region = render::editor_region(&layout);
            self.gpu.as_ref().map(|g| g.ui.ext_detail.hit_install(region, p)).unwrap_or(false)
        } else {
            false
        };

        let new_detail_tab = if self.open_extension.is_some() {
            let region = render::editor_region(&layout);
            self.gpu.as_ref().and_then(|g| g.ui.ext_detail.hit_tab(region, p))
        } else {
            None
        };
        if new_detail_tab != self.hovered_detail_tab {
            self.hovered_detail_tab = new_detail_tab;
            changed = true;
        }
        if new_page_install != self.hovered_page_install {
            self.hovered_page_install = new_page_install;
            changed = true;
        }

        // Hovering a README link → pointer cursor.
        let over_detail_link = if self.open_extension.is_some() {
            let region = render::editor_region(&layout);
            let scroll = self.ext_detail_scroll;
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

        let over_scrollbar = self
            .editor_scroll_metrics(&layout)
            .and_then(|(t, c, v, s)| self.editor_scroll.thumb(t, c, v, s))
            .map(|th| th.contains(p))
            .unwrap_or(false);
        if over_scrollbar != self.hovered_scrollbar {
            self.hovered_scrollbar = over_scrollbar;
            changed = true;
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
                .map_or(false, |panel| self.terminal_split.handle_rect(panel).contains(p));
        let new_cursor = if self.sidebar_split.is_dragging() || over_handle {
            self.sidebar_split.cursor()
        } else if self.terminal_split.is_dragging() || over_term_handle {
            self.terminal_split.cursor()
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
        } else if self.focused_input_at(&layout, p).is_some() {
            CursorIcon::Text
        } else if new_ext.is_some() || new_page_install || new_detail_tab.is_some() || over_detail_link {
            CursorIcon::Pointer
        } else if self.open_extension.is_some() && layout.editor_text.contains(p) {
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
            } else if over_scrollbar {
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
            if self.terminal_visible { Some(self.terminal_split.size()) } else { None },
        )
    }

    /// (track rect, content height, viewport height, scroll) for the editor
    /// scrollbar, or None when the active doc fits (no scrollbar needed).
    fn editor_scroll_metrics(&self, layout: &Layout) -> Option<(Rect, f32, f32, f32)> {
        let d = self.workspace.active_doc()?;
        let view = layout.editor_text.h;
        let content = d.rope.len_lines() as f32 * theme::LINE_HEIGHT() + theme::EDITOR_PAD * 2.0;
        if content <= view {
            return None;
        }
        let track = Rect {
            x: layout.editor_text.x + layout.editor_text.w - theme::SCROLLBAR_WIDTH,
            y: layout.editor_text.y,
            w: theme::SCROLLBAR_WIDTH,
            h: layout.editor_text.h,
        };
        Some((track, content, view, d.scroll_y))
    }

    /// (track, content width, viewport width, scroll_x) for the horizontal
    /// scrollbar, or None when the widest line fits.
    fn editor_hscroll_metrics(&self, layout: &Layout) -> Option<(Rect, f32, f32, f32)> {
        let d = self.workspace.active_doc()?;
        let view = layout.editor_text.w;
        let content = d.max_line_width() + theme::EDITOR_PAD * 2.0;
        if content <= view {
            return None;
        }
        let track = Rect {
            x: layout.editor_text.x,
            y: layout.editor_text.y + layout.editor_text.h - theme::SCROLLBAR_WIDTH,
            w: layout.editor_text.w - theme::SCROLLBAR_WIDTH,
            h: theme::SCROLLBAR_WIDTH,
        };
        Some((track, content, view, d.scroll_x))
    }

    fn ensure_cursor_visible(&mut self) {
        let layout = self.layout();
        let editor_inner_h = layout.editor_text.h - theme::EDITOR_PAD * 2.0;
        if editor_inner_h <= 0.0 {
            return;
        }
        let Some(doc) = self.workspace.active_doc_mut() else {
            return;
        };
        let (line, _) = doc.head_line_col();
        let cursor_top = line as f32 * theme::LINE_HEIGHT();
        let cursor_bottom = cursor_top + theme::LINE_HEIGHT();
        if cursor_top < doc.scroll_y {
            doc.scroll_y = cursor_top.max(0.0);
        } else if cursor_bottom > doc.scroll_y + editor_inner_h {
            doc.scroll_y = cursor_bottom - editor_inner_h;
        }
    }

    fn redraw(&self) {
        if let Some(g) = self.gpu.as_ref() {
            g.window.request_redraw();
        }
    }

    /// Total tabs in the strip: documents plus the open extension page (if any),
    /// which lives in its own tab after the documents (VSCode-style).
    pub(crate) fn tab_count(&self) -> usize {
        self.workspace.documents.len() + self.open_extension.is_some() as usize
    }

    /// The active tab index — the extension page when open, else the active document.
    pub(crate) fn active_tab(&self) -> Option<usize> {
        if self.open_extension.is_some() {
            Some(self.workspace.documents.len())
        } else {
            self.workspace.active
        }
    }

    /// The tab index of the open extension page, if any.
    pub(crate) fn ext_tab_index(&self) -> Option<usize> {
        self.open_extension.map(|_| self.workspace.documents.len())
    }

    /// Begin an inline New File / New Folder, scoped to the selected folder (or
    /// the parent of a selected file, or the workspace root). Expands the target
    /// folder so the field appears at the top of its contents.
    fn begin_create(&mut self, is_dir: bool) {
        let nodes = &self.workspace.tree.nodes;
        let (parent, row, depth) = match self.selected_tree.and_then(|i| nodes.get(i).map(|n| (i, n))) {
            Some((i, n)) if n.is_dir => {
                let path = n.path.clone();
                let depth = n.depth + 1;
                self.workspace.tree.expand(&path);
                (path, i + 1, depth)
            }
            Some((i, n)) => {
                let parent = n
                    .path
                    .parent()
                    .map(|p| p.to_path_buf())
                    .unwrap_or_else(|| self.workspace.tree.root.clone());
                (parent, i, n.depth)
            }
            None => (self.workspace.tree.root.clone(), 0, 0),
        };
        self.creating = Some(PendingCreate {
            is_dir,
            parent,
            row,
            depth,
            rename_from: None,
        });
        if let Some(g) = self.gpu.as_mut() {
            g.create_input.clear(&mut g.font_system);
            g.create_input
                .set_placeholder(&mut g.font_system, if is_dir { " folder name" } else { " file name" });
            g.create_input.focus(true);
        }
        self.redraw();
    }

    /// Begin an inline rename of a tree node: the field replaces the node's row,
    /// pre-filled with its current name.
    fn begin_rename(&mut self, idx: usize) {
        let Some(n) = self.workspace.tree.nodes.get(idx) else {
            return;
        };
        let parent = n
            .path
            .parent()
            .map(|p| p.to_path_buf())
            .unwrap_or_else(|| self.workspace.tree.root.clone());
        let name = n.name.clone();
        let pc = PendingCreate {
            is_dir: n.is_dir,
            parent,
            row: idx,
            depth: n.depth,
            rename_from: Some(n.path.clone()),
        };
        self.creating = Some(pc);
        if let Some(g) = self.gpu.as_mut() {
            g.create_input.set_text(&mut g.font_system, &name);
            g.create_input.focus(true);
        }
        self.redraw();
    }

    /// Finish an inline create/rename: apply if a non-empty name was typed
    /// (opening new files), otherwise just dismiss the field.
    fn commit_create(&mut self) {
        let Some(pc) = self.creating.take() else {
            return;
        };
        let name = self
            .gpu
            .as_ref()
            .map(|g| g.create_input.text().trim().to_string())
            .unwrap_or_default();
        if let Some(g) = self.gpu.as_mut() {
            g.create_input.focus(false);
        }
        if !name.is_empty() {
            if let Some(from) = pc.rename_from {
                let to = pc.parent.join(&name);
                if to != from && std::fs::rename(&from, &to).is_ok() {
                    self.workspace.tree.refresh();
                    // Re-point any open document at the renamed path.
                    for d in self.workspace.documents.iter_mut() {
                        if d.path.as_deref() == Some(from.as_path()) {
                            d.path = Some(to.clone());
                            d.name = name.clone();
                        }
                    }
                }
            } else if let Ok(path) = self.workspace.create_entry(&pc.parent, &name, pc.is_dir) {
                if !pc.is_dir {
                    if let Some(g) = self.gpu.as_mut() {
                        let _ = self.workspace.open_file(&path, &mut g.font_system);
                    }
                }
            }
        }
        self.redraw();
    }

    fn cancel_create(&mut self) {
        self.creating = None;
        if let Some(g) = self.gpu.as_mut() {
            g.create_input.focus(false);
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
            g.ui
                .sidebar
                .row_at(layout.tree_region(), (x, y), self.workspace.tree.nodes.len())
        });
        self.selected_tree = target;
        self.context_menu = Some(ContextMenu { anchor: (x, y), target });
        self.redraw();
    }

    /// Which menu item is under `p`, delegating geometry to the Menu widget.
    fn context_menu_item_at(&self, p: (f32, f32)) -> Option<usize> {
        let cm = self.context_menu.as_ref()?;
        let g = self.gpu.as_ref()?;
        let win = (g.config.width as f32, g.config.height as f32);
        let menu = g.ui.menu.rect(cm.anchor, win);
        g.ui.menu.item_at(menu, p)
    }

    fn close_context_menu(&mut self) {
        self.context_menu = None;
        self.hovered_menu_item = None;
        self.redraw();
    }

    fn exec_menu_action(&mut self, action: MenuAction) {
        let target = self.context_menu.as_ref().and_then(|m| m.target);
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
    /// Called when the data changes (after a scan or an install), not per frame.
    fn rebuild_ext_rows(&mut self) {
        let Some(gpu) = self.gpu.as_mut() else { return };
        let query = gpu.ui.ext_filter.text().trim().to_lowercase();
        self.ext_showing_remote = !query.is_empty();
        let mut visible = Vec::new();
        let mut specs: Vec<widgets::ExtSpec> = Vec::new();
        if self.ext_showing_remote {
            // Marketplace results (already filtered by the remote query).
            for (idx, e) in self.ext_remote.iter().enumerate() {
                let name = if e.display.is_empty() { e.name.clone() } else { e.display.clone() };
                let meta = format!("{} · Marketplace", e.namespace);
                let desc: String = e.description.chars().take(80).collect();
                let uv = e
                    .icon
                    .as_ref()
                    .and_then(|b| gpu.icon_atlas.load_bytes(&gpu.queue, &e.id(), b));
                visible.push(idx);
                specs.push((name, meta, desc, uv));
            }
        } else {
            // Locally installed extensions.
            for (idx, e) in self.extensions.iter().enumerate() {
                let meta = if e.installed {
                    format!("{} · installed", e.publisher)
                } else {
                    format!("{} · {}", e.publisher, e.category())
                };
                let desc: String = e.description.chars().take(80).collect();
                let uv = e.icon_path.as_ref().and_then(|p| gpu.icon_atlas.load(&gpu.queue, &e.name, p));
                visible.push(idx);
                specs.push((e.name.clone(), meta, desc, uv));
            }
        }
        gpu.ui.ext_rows.rebuild(&mut gpu.font_system, &specs);
        self.ext_visible = visible;
        self.clamp_ext_scroll();
    }

    /// Kick off a background OpenVSX search for the current filter text.
    fn trigger_search(&mut self) {
        let query = self
            .gpu
            .as_ref()
            .map(|g| g.ui.ext_filter.text().trim().to_string())
            .unwrap_or_default();
        if query.is_empty() {
            return;
        }
        self.search_gen += 1;
        marketplace::search_async(self.worker_tx.clone(), query, self.search_gen);
    }

    /// Download + install a marketplace extension on a background thread.
    fn install_remote(&mut self, idx: usize) {
        let Some(ext) = self.ext_remote.get(idx).cloned() else { return };
        let Some(root) = extensions::dir() else { return };
        marketplace::install_async(self.worker_tx.clone(), ext, root);
    }

    /// Open the detail page for an extension and load its README (local read /
    /// remote fetch), resetting the page scroll.
    fn open_ext_detail(&mut self, which: OpenExt) {
        self.open_extension = Some(which);
        self.ext_detail_scroll = 0.0;
        self.ext_readme = None;
        self.ext_changelog = None;
        self.ext_img_dir = None;
        self.ext_img_base = None;
        self.requested_images.clear();
        if let Some(g) = self.gpu.as_mut() {
            g.ui.ext_detail.set_tab(ext_detail::DetailTab::Details);
        }
        self.ext_features = self.build_features_md(which);
        self.ext_doc_gen += 1;
        let gen = self.ext_doc_gen;
        match which {
            OpenExt::Local(i) => {
                let readme = self.extensions.get(i).and_then(|e| e.readme_path.clone());
                self.ext_img_dir = readme.as_ref().and_then(|p| p.parent().map(|d| d.to_path_buf()));
                self.ext_readme = readme.and_then(|p| std::fs::read_to_string(&p).ok());
                self.ext_changelog = self
                    .extensions
                    .get(i)
                    .and_then(|e| e.changelog_path.clone())
                    .and_then(|p| std::fs::read_to_string(&p).ok());
            }
            OpenExt::Remote(i) => {
                if let Some(e) = self.ext_remote.get(i) {
                    // Relative README images resolve against the readme URL's dir.
                    self.ext_img_base = e.readme_url.clone();
                    if let Some(url) = e.readme_url.clone() {
                        marketplace::readme_async(self.worker_tx.clone(), url, gen);
                    }
                    if let Some(url) = e.changelog_url.clone() {
                        marketplace::changelog_async(self.worker_tx.clone(), url, gen);
                    }
                }
            }
        }
        self.redraw();
    }

    /// Build the Features-tab markdown from what Nova knows about the extension.
    fn build_features_md(&self, which: OpenExt) -> String {
        match which {
            OpenExt::Local(i) => {
                let Some(e) = self.extensions.get(i) else { return String::new() };
                let mut s = String::new();
                match e.kind {
                    ExtKind::Theme => s.push_str("### Color Theme\nContributes a color theme Nova can apply natively.\n\n"),
                    ExtKind::Grammar => s.push_str("### Syntax Highlighting\nShips TextMate grammars Nova runs natively for syntax coloring.\n\n"),
                    ExtKind::Declarative => s.push_str("### Language Support\nContributes snippets / language configuration.\n\n"),
                    ExtKind::Code => s.push_str("### Code Extension\nNeeds the JavaScript extension runtime (not yet supported in Nova).\n\n"),
                }
                if !e.grammar_paths.is_empty() {
                    s.push_str(&format!("- {} grammar file(s)\n", e.grammar_paths.len()));
                }
                if e.theme_path.is_some() {
                    s.push_str("- 1 color theme\n");
                }
                s
            }
            OpenExt::Remote(_) => {
                "Feature details are available after install.".to_string()
            }
        }
    }

    /// Install whatever the detail page currently shows.
    fn install_open(&mut self) {
        match self.open_extension {
            Some(OpenExt::Local(i)) => self.install_extension(i),
            Some(OpenExt::Remote(i)) => self.install_remote(i),
            None => {}
        }
    }

    /// The currently focused element (single source of truth for key routing).
    /// Precedence matches modal nesting: inline rename > palette > find > the
    /// extensions filter > the editor.
    fn focus(&self) -> Focus {
        if self.creating.is_some() {
            Focus::Rename
        } else if self.palette.active {
            Focus::Palette
        } else if self.find.active {
            Focus::Find
        } else if self.terminal_visible && self.terminal_focused {
            Focus::Terminal
        } else if self.ext_filter_active {
            Focus::ExtFilter
        } else {
            Focus::Editor
        }
    }

    fn set_ext_filter_focus(&mut self, on: bool) {
        self.ext_filter_active = on;
        if let Some(g) = self.gpu.as_mut() {
            if on {
                g.ui.ext_filter.set_placeholder(&mut g.font_system, " Search Extensions");
            }
            g.ui.ext_filter.focus(on);
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
            InputId::ExtFilter => {
                (self.sidebar_visible && self.sidebar_view == SidebarView::Extensions)
                    .then(|| (ext_filter_rect(layout.tree_region()), 6.0))
            }
        }
    }

    /// The focused input under point `p` (for click-to-position / drag-select).
    fn focused_input_at(&self, layout: &Layout, p: (f32, f32)) -> Option<(InputId, Rect, f32)> {
        for id in [InputId::Palette, InputId::Find, InputId::ExtFilter] {
            if let Some((rect, pad)) = self.input_rect_for(id, layout) {
                if rect.contains(p) {
                    return Some((id, rect, pad));
                }
            }
        }
        None
    }

    fn clamp_ext_scroll(&mut self) {
        let layout = self.layout();
        let region = ext_list_region(layout.tree_region());
        let content = self
            .gpu
            .as_ref()
            .map(|g| g.ui.ext_rows.content_height())
            .unwrap_or(0.0);
        let max = (content - region.h).max(0.0);
        self.ext_scroll = self.ext_scroll.clamp(0.0, max);
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
            buttons: &["Delete", "Cancel"],
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
            buttons: &["Save", "Don't Save", "Cancel"],
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
        }
        self.redraw();
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
        }
        self.redraw();
    }

    /// Show/hide the integrated terminal, spawning the shell on first open and
    /// focusing it so keystrokes go to the shell.
    fn toggle_terminal(&mut self) {
        self.terminal_visible = !self.terminal_visible;
        self.terminal_focused = self.terminal_visible;
        self.terminal_dirty = true;
        if self.terminal_visible && self.terminal.is_none() {
            let layout = self.layout();
            if let Some(panel) = layout.terminal_panel {
                let (rows, cols) = terminal_grid_size(panel, self.terminal_cell_w);
                self.terminal = terminal::Terminal::spawn(rows, cols);
            }
        }
        self.redraw();
    }

    /// Re-read settings.json and apply everything: sidebar visibility, color theme,
    /// and editor font (size/family/line-height) by reshaping open documents and the
    /// gutter. tabSize/insertSpaces/cursorBlinking are read on demand elsewhere.
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
                self.open_extension = None;
            }
        }
    }

    fn copy(&mut self) {
        let Some(text) = self.workspace.active_doc().and_then(|d| d.selected_text()) else {
            return;
        };
        if let Some(cb) = self.clipboard.as_mut() {
            let _ = cb.set_text(text);
        }
    }

    fn paste(&mut self) {
        let text = match self.clipboard.as_mut().and_then(|cb| cb.get_text().ok()) {
            Some(t) => t,
            None => return,
        };
        if let Some(gpu) = self.gpu.as_mut() {
            if let Some(d) = self.workspace.active_doc_mut() {
                d.insert_str(&text, &mut gpu.font_system);
            }
        }
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
        if self.context_menu.is_some() {
            if let Some(i) = self.context_menu_item_at((x, y)) {
                self.exec_menu_action(MENU_ACTIONS[i].0);
            } else {
                self.close_context_menu();
            }
            return;
        }

        // A click anywhere while an inline create field is open commits it
        // (creates if a name was typed, discards if empty), then consumes the click.
        if self.creating.is_some() {
            self.commit_create();
            return;
        }

        // Terminal panel resize handle (top edge) — let its Splitter claim the
        // press before the focus/scroll handlers. The handle straddles the panel
        // edge, so check it ahead of the in-panel focus test below.
        if layout.palette.is_none() {
            if let Some(panel) = layout.terminal_panel {
                if self.terminal_split.press((x, y), panel) {
                    return;
                }
            }
        }

        // Terminal panel takes keyboard focus on click; clicking elsewhere releases it.
        if self.terminal_visible {
            let in_term = layout.terminal_panel.map(|p| p.contains((x, y))).unwrap_or(false);
            self.terminal_focused = in_term;
            if in_term {
                self.redraw();
                return;
            }
        }

        // Click inside a focused text input: position the caret and begin a
        // drag-selection. (Handled before other regions so it wins the click.)
        if let Some((id, rect, pad)) = self.focused_input_at(&layout, (x, y)) {
            // Clicking the extensions filter box focuses it; clicking another input
            // (palette/find) takes focus away from it.
            self.ext_filter_active = id == InputId::ExtFilter;
            let double = self.register_click(x, y);
            if let Some(g) = self.gpu.as_mut() {
                let inp = match id {
                    InputId::Palette => &mut g.ui.palette_input,
                    InputId::Find => &mut g.ui.find_input,
                    InputId::ExtFilter => &mut g.ui.ext_filter,
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
            // Menu items open the command palette for now (dropdown menus TBD).
            let on_menu = self
                .gpu
                .as_ref()
                .map(|g| g.menubar.item_at(layout.menu_bar_rect(), (x, y)).is_some())
                .unwrap_or(false);
            if on_menu {
                self.open_palette();
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
                let cmd = COMMANDS[self.palette.filtered[idx]].0;
                self.palette.close();
                self.exec_command(cmd);
            }
            return;
        }

        if layout.status_bar.contains((x, y)) {
            return;
        }

        if let Some(idx) = layout.activity_rects().iter().position(|r| r.contains((x, y))) {
            // 0 = Explorer, 4 = Extensions. Clicking the active view's icon toggles
            // the sidebar; clicking another switches to it (and shows the sidebar).
            let view = match idx {
                0 => Some(SidebarView::Explorer),
                4 => Some(SidebarView::Extensions),
                _ => None,
            };
            if let Some(v) = view {
                if v == SidebarView::Extensions && self.extensions.is_empty() {
                    self.extensions = extensions::scan();
                    self.rebuild_ext_rows();
                }
                if self.sidebar_view == v && self.sidebar_visible {
                    self.sidebar_visible = false;
                } else {
                    self.sidebar_view = v;
                    self.sidebar_visible = true;
                }
                // Opening the panel does NOT focus the filter — focus (and its
                // caret) only happens when the user clicks the box. Switching views
                // clears any prior filter focus.
                self.set_ext_filter_focus(false);
                self.redraw();
            }
            return;
        }

        // Extensions panel: clicking a row opens its detail page (Install lives there).
        if self.sidebar_visible
            && self.sidebar_view == SidebarView::Extensions
            && layout.sidebar.contains((x, y))
        {
            let region = ext_list_region(layout.tree_region());
            let scroll = self.ext_scroll;
            let hit = self.gpu.as_ref().and_then(|g| g.ui.ext_rows.hit(region, scroll, (x, y)));
            if let Some(i) = hit {
                if let Some(&src) = self.ext_visible.get(i) {
                    let which = if self.ext_showing_remote {
                        OpenExt::Remote(src)
                    } else {
                        OpenExt::Local(src)
                    };
                    self.open_ext_detail(which);
                }
            }
            return;
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
            let row = self.gpu.as_ref().and_then(|gpu| {
                gpu.ui
                    .sidebar
                    .row_at(layout.tree_region(), (x, y), self.workspace.tree.nodes.len())
            });
            if let Some(idx) = row {
                self.selected_tree = Some(idx);
                let is_dir = self.workspace.tree.nodes[idx].is_dir;
                if is_dir {
                    self.workspace.tree.toggle(idx);
                } else {
                    let path = self.workspace.tree.nodes[idx].path.clone();
                    if let Some(gpu) = self.gpu.as_mut() {
                        let _ = self.workspace.open_file(&path, &mut gpu.font_system);
                    }
                    self.open_extension = None; // opening a file dismisses the ext page
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
                        self.open_extension = None;
                    }
                } else if closing {
                    self.request_close(idx);
                } else {
                    self.workspace.switch_to(idx);
                    self.open_extension = None;
                }
                self.redraw();
            }
            return;
        }

        // Extension details page (in the editor area): handle its Install button
        // and consume other clicks so they don't fall through to the editor.
        if self.open_extension.is_some() {
            let region = render::editor_region(&layout);
            if region.contains((x, y)) {
                // A click on a README link opens it in the browser.
                let scroll = self.ext_detail_scroll;
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
                    self.ext_detail_scroll = 0.0; // each tab scrolls from the top
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

        // Editor scrollbar thumb drag (vertical then horizontal).
        if let Some((track, content, view, scroll)) = self.editor_scroll_metrics(&layout) {
            if self.editor_scroll.press((x, y), track, content, view, scroll) {
                return;
            }
        }
        if let Some((track, content, view, scroll)) = self.editor_hscroll_metrics(&layout) {
            if self.editor_hscroll.press((x, y), track, content, view, scroll) {
                return;
            }
        }

        if layout.editor_text.contains((x, y)) {
            self.ext_filter_active = false; // editor takes keyboard focus
            let now = Instant::now();
            let consecutive = now.duration_since(self.last_click) < Duration::from_millis(400)
                && (x - self.last_click_pos.0).abs() < 4.0
                && (y - self.last_click_pos.1).abs() < 4.0;
            // 1 = place, 2 = word, 3 = line, 4 = whole document (cycles).
            self.click_count = if consecutive { (self.click_count % 4) + 1 } else { 1 };
            self.last_click = now;
            self.last_click_pos = (x, y);
            self.editor_click(x, y, self.mods.shift_key(), layout);
            if self.click_count >= 2 {
                if let Some(d) = self.workspace.active_doc_mut() {
                    let b = d.sel.head;
                    match self.click_count {
                        2 => d.select_word(b),
                        3 => d.select_line(b),
                        _ => d.select_all(),
                    }
                }
                self.dragging_editor = false;
            } else {
                self.dragging_editor = true;
            }
            self.redraw();
            return;
        }
    }

    fn on_mouse_move(&mut self, x: f32, y: f32) {
        // Drag-select within a text input.
        if let Some(id) = self.text_drag {
            if self.mouse_pressed {
                let layout = self.layout();
                if let Some((rect, pad)) = self.input_rect_for(id, &layout) {
                    if let Some(g) = self.gpu.as_mut() {
                        let inp = match id {
                            InputId::Palette => &mut g.ui.palette_input,
                            InputId::Find => &mut g.ui.find_input,
                            InputId::ExtFilter => &mut g.ui.ext_filter,
                        };
                        inp.extend_to_x(rect, pad, x);
                    }
                    self.redraw();
                }
                return;
            }
        }
        if self.editor_scroll.is_dragging() && self.mouse_pressed {
            let layout = self.layout();
            if let Some((track, content, view, _)) = self.editor_scroll_metrics(&layout) {
                if let Some(s) = self.editor_scroll.drag((x, y), track, content, view) {
                    if let Some(d) = self.workspace.active_doc_mut() {
                        d.scroll_y = s;
                    }
                    self.redraw();
                }
            }
            return;
        }
        if self.editor_hscroll.is_dragging() && self.mouse_pressed {
            let layout = self.layout();
            if let Some((track, content, view, _)) = self.editor_hscroll_metrics(&layout) {
                if let Some(s) = self.editor_hscroll.drag((x, y), track, content, view) {
                    if let Some(d) = self.workspace.active_doc_mut() {
                        d.scroll_x = s;
                    }
                    self.redraw();
                }
            }
            return;
        }
        if self.sidebar_split.is_dragging() && self.mouse_pressed {
            if self.sidebar_split.drag(x, theme::ACTIVITY_BAR_WIDTH) {
                self.redraw();
            }
            return;
        }
        if self.terminal_split.is_dragging() && self.mouse_pressed {
            // Height is measured up from the panel's bottom edge (status bar top).
            let origin = self.layout().status_bar.y;
            if self.terminal_split.drag(y, origin) {
                self.redraw();
            }
            return;
        }
        if self.dragging_editor && self.mouse_pressed {
            let layout = self.layout();
            self.editor_click(x, y, true, layout);
        }
    }

    fn on_mouse_release(&mut self) {
        self.dragging_editor = false;
        self.text_drag = None;
        self.sidebar_split.release();
        self.terminal_split.release();
        self.editor_scroll.release();
        self.editor_hscroll.release();
    }

    fn editor_click(&mut self, x: f32, y: f32, extend: bool, layout: Layout) {
        let Some(gpu) = self.gpu.as_mut() else {
            return;
        };
        let Some(d) = self.workspace.active_doc_mut() else {
            return;
        };
        let buf_x = x - (layout.editor_text.x + theme::EDITOR_PAD) + d.scroll_x;
        let buf_y = y - (layout.editor_text.y + theme::EDITOR_PAD) + d.scroll_y;
        if let Some(hit) = d.buffer.hit(buf_x, buf_y) {
            let line = hit.line;
            if line < d.rope.len_lines() {
                let line_start = d.rope.line_to_byte(line);
                let line_len = d.rope.line(line).len_bytes();
                let col = hit.index.min(line_len);
                d.place(line_start + col, extend);
            }
        }
        let _ = gpu;
        self.redraw();
    }

    fn on_scroll(&mut self, dy: f32) {
        let layout = self.layout();
        let p = (self.mouse_pos.x as f32, self.mouse_pos.y as f32);
        // Terminal scrollback: wheel up goes back in history. dy is in pixels;
        // convert to whole lines. Consumes the event so the editor doesn't scroll.
        if self.terminal_visible {
            if let Some(panel) = layout.terminal_panel {
                if panel.contains(p) {
                    let lines = (dy / theme::LINE_HEIGHT()).round() as i64;
                    if lines != 0 {
                        if let Some(t) = self.terminal.as_mut() {
                            if t.scroll_by_lines(lines) {
                                self.terminal_dirty = true;
                                self.redraw();
                            }
                        }
                    }
                    return;
                }
            }
        }
        // Extensions list scrolls when the cursor is over its region.
        if self.sidebar_visible && self.sidebar_view == SidebarView::Extensions {
            let region = ext_list_region(layout.tree_region());
            if region.contains(p) {
                self.ext_scroll -= dy;
                self.clamp_ext_scroll();
                self.redraw();
                return;
            }
        }
        // The extension detail page (README) scrolls when it's open and the cursor
        // is over the editor area.
        if self.open_extension.is_some() && layout.editor_text.contains(p) {
            let region = render::editor_region(&layout);
            let max = self
                .gpu
                .as_ref()
                .map(|g| {
                    let ch = g.ui.ext_detail.body_content_height(&|k| g.media.size(k));
                    (ch - ext_detail::ExtensionDetail::body_viewport_height(region)).max(0.0)
                })
                .unwrap_or(0.0);
            self.ext_detail_scroll = (self.ext_detail_scroll - dy).clamp(0.0, max);
            self.redraw();
            return;
        }
        if !layout.editor_text.contains(p) {
            // Could route to sidebar tree, but flat list fits fine for now.
            return;
        }
        let Some(d) = self.workspace.active_doc_mut() else {
            return;
        };
        let total_lines = d.rope.len_lines() as f32;
        let max = (total_lines * theme::LINE_HEIGHT() - (layout.editor_text.h - theme::EDITOR_PAD * 2.0)).max(0.0);
        d.scroll_y = (d.scroll_y - dy).clamp(0.0, max);
        self.redraw();
    }

    fn on_scroll_h(&mut self, dx: f32) {
        let layout = self.layout();
        let Some((_, content, view, _)) = self.editor_hscroll_metrics(&layout) else {
            return;
        };
        let max = (content - view).max(0.0);
        if let Some(d) = self.workspace.active_doc_mut() {
            d.scroll_x = (d.scroll_x - dx).clamp(0.0, max);
            self.redraw();
        }
    }

    fn on_key(&mut self, event: winit::event::KeyEvent) {
        if event.state != ElementState::Pressed {
            return;
        }
        let extend = self.mods.shift_key();
        let ctrl = self.mods.control_key();

        // A modal dialog swallows keys; Escape cancels it.
        if self.dialog.is_some() {
            if matches!(event.logical_key.as_ref(), Key::Named(NamedKey::Escape)) {
                self.close_dialog();
            }
            return;
        }

        // Escape closes an open context menu first.
        if self.context_menu.is_some() && matches!(event.logical_key.as_ref(), Key::Named(NamedKey::Escape)) {
            self.close_context_menu();
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
                    if let Some(t) = self.terminal.as_mut() {
                        t.write(&bytes);
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
                        if !self.palette.filtered.is_empty() {
                            self.palette.selected =
                                (self.palette.selected + 1) % self.palette.filtered.len();
                        }
                        self.redraw();
                        return;
                    }
                    Key::Named(NamedKey::ArrowUp) => {
                        if !self.palette.filtered.is_empty() {
                            if self.palette.selected == 0 {
                                self.palette.selected = self.palette.filtered.len() - 1;
                            } else {
                                self.palette.selected -= 1;
                            }
                        }
                        self.redraw();
                        return;
                    }
                    Key::Named(NamedKey::Enter) => {
                        if let Some(&i) = self.palette.filtered.get(self.palette.selected) {
                            let cmd = COMMANDS[i].0;
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
                if matches!(event.logical_key.as_ref(), Key::Named(NamedKey::Escape)) {
                    if let Some(g) = self.gpu.as_mut() {
                        g.ui.ext_filter.clear(&mut g.font_system);
                    }
                    self.ext_scroll = 0.0;
                    self.rebuild_ext_rows();
                    self.redraw();
                    return;
                }
                let consumed = self.gpu.as_mut().and_then(|g| {
                    edit_input(&mut g.ui.ext_filter, &mut g.font_system, self.clipboard.as_mut(), &event, ctrl, extend)
                });
                match consumed {
                    Some(changed) => {
                        if changed {
                            self.ext_scroll = 0.0;
                            self.trigger_search();
                            self.rebuild_ext_rows();
                        }
                        self.redraw();
                        return;
                    }
                    None => {
                        // Swallow other plain keys (Enter, arrows…) so they can't leak
                        // to the editor; let Ctrl-combos fall through to shortcuts.
                        if !ctrl {
                            return;
                        }
                    }
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
pub(crate) fn create_row_geometry(tr: Rect, row: usize, depth: usize) -> (Rect, Rect, Rect) {
    let row_y = tr.y + row as f32 * theme::TREE_ROW_HEIGHT;
    // Match the file tree: 12px left pad + ~8px per depth, left-aligned icon.
    let indent = 12.0 + depth as f32 * 8.0;
    let icon_w = 16.0;
    let row_rect = Rect { x: tr.x, y: row_y, w: tr.w, h: theme::TREE_ROW_HEIGHT };
    let icon_rect = Rect { x: tr.x + indent, y: row_y, w: icon_w, h: theme::TREE_ROW_HEIGHT };
    let field = Rect {
        x: tr.x + indent + icon_w + 4.0,
        y: row_y,
        w: (tr.w - indent - icon_w - 4.0).max(0.0),
        h: theme::TREE_ROW_HEIGHT,
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
        SidebarView::Extensions => Some(4),
    }
}

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
    Terminal,  // integrated terminal
}

/// Open a URL in the OS default browser. Best-effort, http(s) only (so README
/// link text can't launch arbitrary commands).
fn open_url(url: &str) {
    use std::os::windows::process::CommandExt;
    const CREATE_NO_WINDOW: u32 = 0x0800_0000; // suppress the console window flash
    if url.starts_with("http://") || url.starts_with("https://") {
        let _ = std::process::Command::new("cmd")
            .args(["/c", "start", "", url])
            .creation_flags(CREATE_NO_WINDOW)
            .spawn();
    }
}

/// Translate a key event into the bytes a shell expects on its PTY input. Returns
/// None for keys we don't forward.
fn translate_terminal_key(event: &winit::event::KeyEvent, ctrl: bool, _shift: bool) -> Option<Vec<u8>> {
    use winit::keyboard::{Key, NamedKey};
    match event.logical_key.as_ref() {
        Key::Named(NamedKey::Enter) => return Some(b"\r".to_vec()),
        Key::Named(NamedKey::Backspace) => return Some(vec![0x7f]),
        Key::Named(NamedKey::Tab) => return Some(b"\t".to_vec()),
        Key::Named(NamedKey::Escape) => return Some(vec![0x1b]),
        Key::Named(NamedKey::ArrowUp) => return Some(b"\x1b[A".to_vec()),
        Key::Named(NamedKey::ArrowDown) => return Some(b"\x1b[B".to_vec()),
        Key::Named(NamedKey::ArrowRight) => return Some(b"\x1b[C".to_vec()),
        Key::Named(NamedKey::ArrowLeft) => return Some(b"\x1b[D".to_vec()),
        Key::Named(NamedKey::Home) => return Some(b"\x1b[H".to_vec()),
        Key::Named(NamedKey::End) => return Some(b"\x1b[F".to_vec()),
        Key::Named(NamedKey::Delete) => return Some(b"\x1b[3~".to_vec()),
        Key::Named(NamedKey::Space) => return Some(b" ".to_vec()),
        _ => {}
    }
    // Ctrl+<letter> → control byte (Ctrl+C = 0x03, etc.).
    if ctrl {
        if let winit::keyboard::PhysicalKey::Code(code) = event.physical_key {
            use winit::keyboard::KeyCode;
            let letter = match code {
                KeyCode::KeyA => Some(b'a'),
                KeyCode::KeyB => Some(b'b'),
                KeyCode::KeyC => Some(b'c'),
                KeyCode::KeyD => Some(b'd'),
                KeyCode::KeyE => Some(b'e'),
                KeyCode::KeyK => Some(b'k'),
                KeyCode::KeyL => Some(b'l'),
                KeyCode::KeyU => Some(b'u'),
                KeyCode::KeyZ => Some(b'z'),
                _ => None,
            };
            if let Some(l) = letter {
                return Some(vec![l & 0x1f]);
            }
        }
        return None;
    }
    // Printable text.
    if let Some(t) = event.text.as_ref() {
        let s: &str = t;
        if !s.is_empty() && !s.chars().any(|c| c.is_control()) {
            return Some(s.as_bytes().to_vec());
        }
    }
    None
}

/// Identifies the text input under the cursor for click/drag selection.
#[derive(Clone, Copy, PartialEq, Eq)]
enum InputId {
    Palette,
    Find,
    ExtFilter,
}

/// Apply a common editing/navigation key to a focused text input. Returns
/// `None` if the key wasn't consumed, or `Some(text_changed)` if it was (so the
/// caller can re-filter only when the content actually changed). Shared by every
/// input so selection, clipboard, and caret movement behave identically.
fn edit_input(
    input: &mut widgets::TextInput,
    fs: &mut glyphon::FontSystem,
    clip: Option<&mut Clipboard>,
    event: &winit::event::KeyEvent,
    ctrl: bool,
    shift: bool,
) -> Option<bool> {
    use winit::keyboard::{Key, KeyCode, NamedKey, PhysicalKey};
    if ctrl {
        // Match the physical key (Ctrl can turn the logical key into a control char).
        if let PhysicalKey::Code(code) = event.physical_key {
            match code {
                KeyCode::KeyA => {
                    input.select_all();
                    return Some(false);
                }
                KeyCode::KeyC => {
                    if let Some(cb) = clip {
                        let _ = cb.set_text(input.selected_text().to_string());
                    }
                    return Some(false);
                }
                KeyCode::KeyX => {
                    if input.has_selection() {
                        if let Some(cb) = clip {
                            let _ = cb.set_text(input.selected_text().to_string());
                        }
                        input.backspace(fs);
                        return Some(true);
                    }
                    return Some(false);
                }
                KeyCode::KeyV => {
                    if let Some(cb) = clip {
                        if let Ok(t) = cb.get_text() {
                            let t: String = t.chars().filter(|c| *c != '\n' && *c != '\r').collect();
                            input.insert(fs, &t);
                            return Some(true);
                        }
                    }
                    return Some(false);
                }
                _ => return None,
            }
        }
        return None;
    }
    match event.logical_key.as_ref() {
        Key::Named(NamedKey::ArrowLeft) => {
            input.move_left(shift);
            Some(false)
        }
        Key::Named(NamedKey::ArrowRight) => {
            input.move_right(shift);
            Some(false)
        }
        Key::Named(NamedKey::Home) => {
            input.move_home(shift);
            Some(false)
        }
        Key::Named(NamedKey::End) => {
            input.move_end(shift);
            Some(false)
        }
        Key::Named(NamedKey::Delete) => {
            input.delete_forward(fs);
            Some(true)
        }
        Key::Named(NamedKey::Backspace) => {
            input.backspace(fs);
            Some(true)
        }
        _ => {
            if let Some(t) = event.text.as_ref() {
                let s: &str = t;
                if !s.chars().any(|c| c.is_control()) {
                    input.insert(fs, s);
                    return Some(true);
                }
            }
            None
        }
    }
}

/// The search/filter box rect at the top of the Extensions sidebar.
pub(crate) fn ext_filter_rect(tree: Rect) -> Rect {
    Rect { x: tree.x + 10.0, y: tree.y + 8.0, w: tree.w - 20.0, h: 30.0 }
}

/// Grid (rows, cols) that fits the terminal panel at the editor font metrics.
/// Rows/cols that fit `panel` for a monospace cell of `char_w` px wide. Using the
/// real measured advance keeps the PTY's column count matched to what's actually
/// rendered, so TUIs (e.g. Claude Code) fill the panel and the cursor lands right.
pub(crate) fn terminal_grid_size(panel: Rect, char_w: f32) -> (usize, usize) {
    let char_w = char_w.max(1.0);
    let cols = (((panel.w - 16.0) / char_w) as usize).clamp(8, 400);
    let rows = (((panel.h - 8.0) / theme::LINE_HEIGHT()) as usize).clamp(2, 200);
    (rows, cols)
}

/// The scrollable extension-row list region (below the filter box).
pub(crate) fn ext_list_region(tree: Rect) -> Rect {
    const STRIP: f32 = 46.0; // filter box + padding
    Rect { x: tree.x, y: tree.y + STRIP, w: tree.w, h: (tree.h - STRIP).max(0.0) }
}

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


// ---------- winit glue ----------

impl ApplicationHandler for App {
    fn about_to_wait(&mut self, el: &ActiveEventLoop) {
        // Drain background worker results (marketplace search/install).
        while let Ok(msg) = self.worker_rx.try_recv() {
            match msg {
                WorkerMsg::Search { gen, results } => {
                    if gen == self.search_gen {
                        self.ext_remote = results;
                        self.rebuild_ext_rows();
                        self.redraw();
                    }
                }
                WorkerMsg::Installed { result } => {
                    if result.is_ok() {
                        self.extensions = extensions::scan();
                    }
                    self.rebuild_ext_rows();
                    self.redraw();
                }
                WorkerMsg::Readme { gen, text } => {
                    if gen == self.ext_doc_gen {
                        self.ext_readme = text;
                        self.redraw();
                    }
                }
                WorkerMsg::Changelog { gen, text } => {
                    if gen == self.ext_doc_gen {
                        self.ext_changelog = text;
                        self.redraw();
                    }
                }
                WorkerMsg::Image { key, frames } => {
                    if let Some(g) = self.gpu.as_mut() {
                        g.media.upload_frames(&g.device, &g.queue, &key, frames);
                    }
                    self.redraw();
                }
            }
        }

        let now = Instant::now();

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

        // Integrated terminal: drain shell output, and keep ticking while it's open
        // so new output appears promptly.
        if self.terminal_visible {
            let changed = self.terminal.as_mut().map(|t| t.poll()).unwrap_or(false);
            if changed {
                self.terminal_dirty = true;
                self.redraw();
            }
            self.cursor_blink_on = true;
            el.set_control_flow(ControlFlow::WaitUntil(now + Duration::from_millis(30)));
            return;
        }

        // If an animated GIF is visible on the detail page, tick ~20fps to play it.
        let animating = self.open_extension.is_some()
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

        // Solid cursor (editor.cursorBlinking: "solid") stays on without blinking.
        if !settings::current().editor_cursor_blink {
            if !self.cursor_blink_on {
                self.cursor_blink_on = true;
                self.redraw();
            }
            // Without blink wakeups, still wake to run the auto-save idle timer.
            let autosave_pending = settings::auto_save()
                && self.workspace.documents.iter().any(|d| d.dirty && d.path.is_some());
            el.set_control_flow(if autosave_pending {
                ControlFlow::WaitUntil(self.last_edit + Duration::from_millis(1100))
            } else {
                ControlFlow::Wait
            });
            return;
        }

        let interval = Duration::from_millis(theme::BLINK_MS);
        if now.duration_since(self.last_blink) >= interval {
            self.cursor_blink_on = !self.cursor_blink_on;
            self.last_blink = now;
            self.redraw();
        }
        el.set_control_flow(ControlFlow::WaitUntil(self.last_blink + interval));
    }

    fn resumed(&mut self, el: &ActiveEventLoop) {
        if self.gpu.is_some() {
            return;
        }
        let attrs = Window::default_attributes()
            .with_title("Nova")
            .with_decorations(false)
            .with_inner_size(LogicalSize::new(1400.0, 900.0));
        let window = Arc::new(el.create_window(attrs).expect("create window"));
        match pollster::block_on(GpuState::new(window)) {
            Ok(gpu) => {
                self.gpu = Some(gpu);
                self.open_initial();
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
        _ => (
            std::env::current_dir().unwrap_or_else(|_| PathBuf::from(".")),
            None,
        ),
    };
    let event_loop = EventLoop::new()?;
    let mut app = App::new(root, initial_file);
    event_loop.run_app(&mut app)?;
    Ok(())
}
