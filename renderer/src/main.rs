// Hide the console window — without this the binary uses the console subsystem
// and Windows spawns a terminal alongside the GUI. We still capture stderr when
// we launch nova via a redirected pipe, so no debug visibility is lost.
#![windows_subsystem = "windows"]

// Nova — Phase 1 vertical slice with VSCode-shaped UI shell.
// Activity bar, sidebar file tree, tab strip, editor (gutter + text),
// status bar, command palette (Ctrl+Shift+P), find bar (Ctrl+F).

mod commands;
mod document;
mod ext_runtime;
mod extensions;
mod gpu;
mod icon;
mod layout;
mod marketplace;
mod quad;
mod syntax;
mod textmate;
mod theme;
mod widgets;
mod workspace;

use std::path::PathBuf;
use std::sync::mpsc::{Receiver, Sender};
use std::sync::Arc;
use std::time::{Duration, Instant};

use anyhow::Result;
use arboard::Clipboard;
use glyphon::{
    Attrs, Buffer, Family, Resolution, Shaping, TextArea, TextBounds,
};
use wgpu::{
    CommandEncoderDescriptor, LoadOp, Operations, RenderPassColorAttachment,
    RenderPassDescriptor, StoreOp, TextureViewDescriptor,
};
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
use extensions::{open_ext_view, ExtKind, Extension, OpenExt};
use marketplace::WorkerMsg;
use gpu::GpuState;
use layout::Layout;
use quad::Quad;
use widgets::{Axis, Rect, Scrollbar, Splitter, VAlign};
use workspace::Workspace;


// ---------- App ----------

struct UiCache {
    tabs: String,
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
struct PendingCreate {
    is_dir: bool,
    parent: PathBuf,
    row: usize,   // tree row the field occupies
    depth: usize, // indent level of the inline field
    rename_from: Option<PathBuf>,
}

#[derive(Clone, Copy)]
enum MenuAction {
    NewFile,
    NewFolder,
    Rename,
    Delete,
    CopyPath,
}

const MENU_ACTIONS: &[(MenuAction, &str)] = &[
    (MenuAction::NewFile, "New File"),
    (MenuAction::NewFolder, "New Folder"),
    (MenuAction::Rename, "Rename"),
    (MenuAction::Delete, "Delete"),
    (MenuAction::CopyPath, "Copy Path"),
];

/// An open right-click context menu over the file tree.
struct ContextMenu {
    anchor: (f32, f32),
    target: Option<usize>, // tree node index; None = empty area (root scope)
}

/// Which sidebar view the activity bar has selected.
#[derive(Clone, Copy, PartialEq, Eq)]
enum SidebarView {
    Explorer,
    Extensions,
}

/// What a modal dialog confirms.
enum DialogAction {
    DeleteNode(usize),
    CloseDoc(usize),
}

struct DialogState {
    action: DialogAction,
    buttons: &'static [&'static str],
    has_check: bool,
    checked: bool,
    hovered: Option<usize>,
}

struct App {
    cwd: PathBuf,
    initial_file: Option<PathBuf>,
    workspace: Workspace,
    gpu: Option<GpuState>,
    mouse_pos: PhysicalPosition<f64>,
    mouse_pressed: bool,
    dragging_editor: bool,
    mods: ModifiersState,
    clipboard: Option<Clipboard>,
    sidebar_visible: bool,
    sidebar_split: Splitter,
    palette: PaletteState,
    find: FindBarState,
    ui_cache: UiCache,
    hovered_tab: Option<usize>,
    hovered_tab_close: Option<usize>,
    hovered_tree: Option<usize>,
    hovered_activity: Option<usize>,
    hovered_titlebtn: Option<usize>,
    hovered_search: bool,
    hovered_menu: Option<usize>,
    hovered_layout: Option<usize>,
    hovered_explorer: Option<usize>,
    selected_tree: Option<usize>,
    creating: Option<PendingCreate>,
    context_menu: Option<ContextMenu>,
    hovered_menu_item: Option<usize>,
    dialog: Option<DialogState>,
    skip_delete_confirm: bool,
    editor_scroll: Scrollbar,
    editor_hscroll: Scrollbar,
    hovered_scrollbar: bool,
    last_click: Instant,
    last_click_pos: (f32, f32),
    click_count: u32,
    sidebar_view: SidebarView,
    extensions: Vec<Extension>,
    hovered_ext: Option<usize>,
    text_drag: Option<InputId>, // active mouse drag-selection in a text input
    ext_filter_active: bool,
    ext_visible: Vec<usize>, // displayed row index -> index into the active source
    ext_scroll: f32,
    ext_remote: Vec<marketplace::RemoteExt>, // current marketplace search results
    ext_showing_remote: bool,                // true while a search query is active
    search_gen: u64,                         // discards stale background search results
    worker_tx: Sender<WorkerMsg>,
    worker_rx: Receiver<WorkerMsg>,
    open_extension: Option<OpenExt>, // extension detail page open in the editor area
    hovered_page_install: bool,
    pending_close: bool,
    cursor_blink_on: bool,
    last_blink: Instant,
    cursor_icon: CursorIcon,
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
            hovered_page_install: false,
            pending_close: false,
            cursor_blink_on: true,
            last_blink: Instant::now(),
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

        let tab_rects = layout.tab_rects(self.workspace.documents.len());
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

        let new_page_install = if let Some(v) = open_ext_view(self.open_extension, &self.extensions, &self.ext_remote) {
            let region = Rect {
                x: layout.gutter.x,
                y: layout.gutter.y,
                w: layout.gutter.w + layout.editor_text.w,
                h: layout.gutter.h,
            };
            v.supported && !v.installed && page_install_rect(region).contains(p)
        } else {
            false
        };
        if new_page_install != self.hovered_page_install {
            self.hovered_page_install = new_page_install;
            changed = true;
        }

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
        let new_cursor = if self.sidebar_split.is_dragging() || over_handle {
            self.sidebar_split.cursor()
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
        } else if new_ext.is_some() || new_page_install {
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
        )
    }

    /// (track rect, content height, viewport height, scroll) for the editor
    /// scrollbar, or None when the active doc fits (no scrollbar needed).
    fn editor_scroll_metrics(&self, layout: &Layout) -> Option<(Rect, f32, f32, f32)> {
        let d = self.workspace.active_doc()?;
        let view = layout.editor_text.h;
        let content = d.rope.len_lines() as f32 * theme::LINE_HEIGHT + theme::EDITOR_PAD * 2.0;
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
        let cursor_top = line as f32 * theme::LINE_HEIGHT;
        let cursor_bottom = cursor_top + theme::LINE_HEIGHT;
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
                if let Some(d) = self.workspace.active_doc_mut() {
                    let _ = d.save();
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
        }
        self.redraw();
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
            // Layout toggles: primary sidebar is wired; panel/secondary are
            // placeholders until those regions exist.
            if let Some(i) = layout.layout_btn_rects().iter().position(|r| r.contains((x, y))) {
                if i == 0 {
                    self.sidebar_visible = !self.sidebar_visible;
                    self.redraw();
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
                // The filter box is focused while the Extensions panel is showing.
                let ext_focus = self.sidebar_visible && v == SidebarView::Extensions;
                self.set_ext_filter_focus(ext_focus);
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
                    self.open_extension = Some(if self.ext_showing_remote {
                        OpenExt::Remote(src)
                    } else {
                        OpenExt::Local(src)
                    });
                    self.redraw();
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
            let tab_rects = layout.tab_rects(self.workspace.documents.len());
            if let Some(idx) = tab_rects.iter().position(|r| r.contains((x, y))) {
                if Layout::tab_close_rect(tab_rects[idx]).contains((x, y)) {
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
        if let Some(v) = open_ext_view(self.open_extension, &self.extensions, &self.ext_remote) {
            let region = Rect {
                x: layout.gutter.x,
                y: layout.gutter.y,
                w: layout.gutter.w + layout.editor_text.w,
                h: layout.gutter.h,
            };
            if region.contains((x, y)) {
                if v.supported && !v.installed && page_install_rect(region).contains((x, y)) {
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
        if self.dragging_editor && self.mouse_pressed {
            let layout = self.layout();
            self.editor_click(x, y, true, layout);
        }
    }

    fn on_mouse_release(&mut self) {
        self.dragging_editor = false;
        self.text_drag = None;
        self.sidebar_split.release();
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
        if !layout.editor_text.contains(p) {
            // Could route to sidebar tree, but flat list fits fine for now.
            return;
        }
        let Some(d) = self.workspace.active_doc_mut() else {
            return;
        };
        let total_lines = d.rope.len_lines() as f32;
        let max = (total_lines * theme::LINE_HEIGHT - (layout.editor_text.h - theme::EDITOR_PAD * 2.0)).max(0.0);
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

        // Single-authority keyboard dispatch: route to whatever element has focus.
        // Each non-editor arm fully handles its keys and returns, so nothing leaks.
        // The ExtFilter arm lets Ctrl-combos fall through to global shortcuts; the
        // Editor arm falls through to the shortcut + editor-key handling below.
        match self.focus() {
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

        // Ctrl+Shift+P opens palette.
        if ctrl && self.mods.shift_key() {
            if let Key::Character(c) = event.logical_key.as_ref() {
                if c == "p" || c == "P" {
                    self.open_palette();
                    return;
                }
            }
        }

        if ctrl {
            if let Key::Character(c) = event.logical_key.as_ref() {
                match c {
                    "a" | "A" => {
                        self.exec_command(Command::SelectAll);
                        return;
                    }
                    "c" | "C" => {
                        self.copy();
                        return;
                    }
                    "x" | "X" => {
                        self.cut();
                        return;
                    }
                    "v" | "V" => {
                        self.paste();
                        return;
                    }
                    "s" | "S" => {
                        self.exec_command(Command::Save);
                        return;
                    }
                    "w" | "W" => {
                        self.exec_command(Command::Close);
                        return;
                    }
                    "z" | "Z" => {
                        self.exec_command(Command::Undo);
                        return;
                    }
                    "y" | "Y" => {
                        self.exec_command(Command::Redo);
                        return;
                    }
                    "f" | "F" => {
                        self.exec_command(Command::Find);
                        return;
                    }
                    "b" | "B" => {
                        self.exec_command(Command::ToggleSidebar);
                        return;
                    }
                    "n" | "N" => {
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
                d.insert_str("    ", &mut gpu.font_system);
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
        self.ensure_cursor_visible();
        self.redraw();
    }
}

// ---------- Rendering ----------

/// Geometry of the inline New File/Folder row within tree region `tr`:
/// returns (row rect, icon rect, text-field rect) for the given insert row/depth.
fn create_row_geometry(tr: Rect, row: usize, depth: usize) -> (Rect, Rect, Rect) {
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
fn active_activity_idx(sidebar_visible: bool, view: SidebarView) -> Option<usize> {
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
    use winit::keyboard::{Key, NamedKey};
    if ctrl {
        if let Key::Character(c) = event.logical_key.as_ref() {
            match c.to_lowercase().as_str() {
                "a" => {
                    input.select_all();
                    return Some(false);
                }
                "c" => {
                    if let Some(cb) = clip {
                        let _ = cb.set_text(input.selected_text().to_string());
                    }
                    return Some(false);
                }
                "x" => {
                    if input.has_selection() {
                        if let Some(cb) = clip {
                            let _ = cb.set_text(input.selected_text().to_string());
                        }
                        input.backspace(fs);
                        return Some(true);
                    }
                    return Some(false);
                }
                "v" => {
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
fn ext_filter_rect(tree: Rect) -> Rect {
    Rect { x: tree.x + 10.0, y: tree.y + 8.0, w: tree.w - 20.0, h: 30.0 }
}

/// The scrollable extension-row list region (below the filter box).
fn ext_list_region(tree: Rect) -> Rect {
    const STRIP: f32 = 46.0; // filter box + padding
    Rect { x: tree.x, y: tree.y + STRIP, w: tree.w, h: (tree.h - STRIP).max(0.0) }
}

/// The Install button rect on the extension details page (top-right of region).
fn page_install_rect(region: Rect) -> Rect {
    Rect { x: region.x + region.w - 180.0, y: region.y + 28.0, w: 130.0, h: 34.0 }
}

fn cursor_pos_in_buffer(buffer: &Buffer, line: usize, col_byte: usize) -> (f32, f32, f32) {
    let mut x = 0.0f32;
    let mut y = line as f32 * theme::LINE_HEIGHT;
    let mut h = theme::LINE_HEIGHT;
    for run in buffer.layout_runs() {
        if run.line_i != line {
            continue;
        }
        y = run.line_top;
        h = run.line_height;
        let mut last_end = 0.0f32;
        let mut placed = false;
        for glyph in run.glyphs.iter() {
            if (glyph.start as usize) >= col_byte {
                x = glyph.x;
                placed = true;
                break;
            }
            last_end = glyph.x + glyph.w;
        }
        if !placed {
            x = last_end;
        }
        break;
    }
    (x, y, h)
}

fn x_range_in_run(
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

fn render(app: &mut App) -> Result<()> {
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
    );

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
        gpu.ui.sidebar_header.set(fs, header, theme::UI_FAMILY);

        // Extension detail page text (works for local + marketplace extensions).
        if let Some(v) = open_ext_view(app.open_extension, &app.extensions, &app.ext_remote) {
            let title = Attrs::new().family(Family::Name(theme::UI_FAMILY)).color(theme::FG_ACTIVE());
            let dim = Attrs::new().family(Family::Name(theme::UI_FAMILY)).color(theme::FG_DIM());
            let body = Attrs::new().family(Family::Name(theme::UI_FAMILY)).color(theme::FG_TEXT());
            let note_col = if v.supported { theme::SYN_COMMENT() } else { theme::FG_DIM() };
            let note = Attrs::new().family(Family::Name(theme::UI_FAMILY)).color(note_col);
            let support = if v.remote {
                "From Open VSX marketplace".to_string()
            } else if v.supported {
                format!("Supported in Nova — {}", v.category)
            } else {
                "Not supported yet — needs the extension runtime".to_string()
            };
            let desc = if v.description.is_empty() { "(no description)".to_string() } else { v.description.clone() };
            let spans = vec![
                (format!("{}\n", v.name), title),
                (format!("{} · {}\n\n", v.publisher, v.category), dim),
                (format!("{}\n\n", desc), body),
                (support, note),
            ];
            let key = format!("{}{}{}", v.name, v.installed, v.remote);
            gpu.ui.ext_detail.set_rich(fs, &key, &spans, body);
            gpu.ui.ext_install.set(fs, "Install", theme::UI_FAMILY);
            gpu.ui.ext_installed.set(fs, "Installed", theme::UI_FAMILY);
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
                Attrs::new().family(Family::Name(theme::UI_FAMILY)).color(theme::FG_TEXT()),
            ),
        ];
        gpu.ui
            .root_label
            .set_rich(fs, &ws_name, &root_spans, Attrs::new().family(Family::Name(theme::UI_FAMILY)));

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
                .family(Family::Name(theme::UI_FAMILY))
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

        // Tab strip.
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
        if cache.tabs != tab_text {
            // Wide (no wrap) + tall so every tab's label line is shaped on its own
            // line; per-tab bounds clip horizontally & vertically.
            gpu.ui.tabs.set_size(fs, Some(4000.0), Some(4000.0));
            gpu.ui.tabs.set_text(
                fs,
                &tab_text,
                Attrs::new().family(Family::Name(theme::UI_FAMILY)),
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
            (
                format!(" {}{}", path, dirty),
                format!("Ln {}, Col {}    Spaces: 4    UTF-8    LF    {}    ", line + 1, col + 1, lang),
            )
        } else {
            ("Nova".to_string(), String::new())
        };
        gpu.ui.status.set(fs, &status_text, theme::UI_FAMILY);
        gpu.ui.status_right.set(fs, &status_right_text, theme::UI_FAMILY);

        // Line numbers.
        let line_count = app
            .workspace
            .active
            .and_then(|i| app.workspace.documents.get(i))
            .map(|d| d.rope.len_lines().max(1))
            .unwrap_or(0);
        gpu.ui.line_numbers.set(fs, line_count);

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
    // Per-tab styling — geometry from the single-source tab rects.
    let n_tabs = app.workspace.documents.len();
    let tab_rects = layout.tab_rects(n_tabs);
    for (i, tab) in tab_rects.iter().enumerate() {
        let active = app.workspace.active == Some(i);
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
    let editor_full = Rect {
        x: layout.gutter.x,
        y: layout.gutter.y,
        w: layout.gutter.w + layout.editor_text.w,
        h: layout.gutter.h,
    };
    bg_quads.push(editor_full.quad([
        theme::BG_EDITOR().r as f32,
        theme::BG_EDITOR().g as f32,
        theme::BG_EDITOR().b as f32,
        theme::BG_EDITOR().a as f32,
    ]));

    // Extension detail page Install button (when the page is open).
    if let Some(v) = open_ext_view(app.open_extension, &app.extensions, &app.ext_remote) {
        if v.supported && !v.installed {
            let c = if app.hovered_page_install {
                theme::DIALOG_BTN_HOVER()
            } else {
                theme::DIALOG_BTN()
            };
            bg_quads.push(page_install_rect(editor_full).quad(c));
        }
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
        // Current line highlight across full editor width.
        let line_y = layout.editor_text.y + theme::EDITOR_PAD
            + cur_line as f32 * theme::LINE_HEIGHT
            - d.scroll_y;
        if let Some((qy, qh)) = clip_v(line_y, theme::LINE_HEIGHT) {
            bg_quads.push(Quad::new(
                editor_full.x,
                qy,
                editor_full.w,
                qh,
                theme::LINE_HIGHLIGHT(),
            ));
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
            let (cur_line2, cur_col_byte) = d.head_line_col();
            let (cx, cy, ch) = cursor_pos_in_buffer(&d.buffer, cur_line2, cur_col_byte);
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
        let content = d.rope.len_lines() as f32 * theme::LINE_HEIGHT + theme::EDITOR_PAD * 2.0;
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
    gpu.viewport.update(
        &gpu.queue,
        Resolution {
            width: cfg_w,
            height: cfg_h,
        },
    );

    let ui = &gpu.ui;
    let mut areas: Vec<TextArea> = Vec::new();

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
        let active = app.workspace.active == Some(i);
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
    if let Some(v) = open_ext_view(app.open_extension, &app.extensions, &app.ext_remote) {
        let region = Rect {
            x: layout.gutter.x,
            y: layout.gutter.y,
            w: layout.gutter.w + layout.editor_text.w,
            h: layout.gutter.h,
        };
        let text_rect = Rect { x: region.x + 30.0, y: region.y + 24.0, w: region.w - 60.0, h: region.h - 48.0 };
        ui.ext_detail.push(text_rect.x, text_rect, theme::FG_TEXT(), &mut areas);
        // Install / Installed button label.
        if v.supported {
            let pill = page_install_rect(region);
            if v.installed {
                ui.ext_installed.draw_center(pill, theme::FG_DIM(), &mut areas);
            } else {
                ui.ext_install.draw_center(pill, theme::FG_ACTIVE(), &mut areas);
            }
        }
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
        gpu.text_renderer
            .render(&gpu.atlas, &gpu.viewport, &mut pass)?;
        gpu.quad_renderer.render_fg(&mut pass);
    }
    gpu.queue.submit(Some(encoder.finish()));

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
            }
        }

        let interval = Duration::from_millis(theme::BLINK_MS);
        let now = Instant::now();
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
                        (x * theme::LINE_HEIGHT * 3.0, y * theme::LINE_HEIGHT * 3.0)
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
                if let Err(e) = render(self) {
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
