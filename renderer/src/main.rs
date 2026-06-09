// Hide the console window — without this the binary uses the console subsystem
// and Windows spawns a terminal alongside the GUI. We still capture stderr when
// we launch aether via a redirected pipe, so no debug visibility is lost.
#![windows_subsystem = "windows"]

// Aether — Phase 1 vertical slice with VSCode-shaped UI shell.
// Activity bar, sidebar file tree, tab strip, editor (gutter + text),
// status bar, command palette (Ctrl+Shift+P), find bar (Ctrl+F).

mod ai;
mod commands;
mod completion;
mod dap;
mod debug_config;
mod diff;
mod document;
mod encoding;
mod ext_detail;
mod ext_runtime;
mod extensions;
mod feedback_upload;
mod gpu;
mod graph;
mod icon;
mod git;
mod highlight;
mod layout;
mod lsp;
mod markdown;
mod marketplace;
mod menus;
mod nav;
#[cfg(target_os = "macos")]
mod macos_menu;
mod perf;
mod update;
mod media;
mod ptyhost;
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

use std::path::{Path, PathBuf};
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

/// How long the pointer must rest on a diagnostic before its hover card appears
/// (matches VS Code's editor.hover.delay default; stops the card chasing the cursor).
const HOVER_DELAY: Duration = Duration::from_millis(300);
/// Rest time before the inline-blame full-commit card appears (GitLens-style).
const BLAME_DELAY: Duration = Duration::from_millis(1000);

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

/// Window title shown in the title bar and — more importantly on macOS — in the
/// Dock's right-click window list and Mission Control. VSCode-style "<root folder>
/// — Aether" so the active project is identifiable from the Dock. (The Dock hover
/// tooltip itself is the static bundle name and can't be made folder-dynamic.)
fn window_title(cwd: &std::path::Path) -> String {
    match cwd.file_name() {
        Some(name) => format!("{} — Aether", name.to_string_lossy()),
        None => "Aether".to_string(),
    }
}

/// Rasterize the bundled Aether logo (SVG) to a window/taskbar icon. The SVG is the
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
/// Which sidebar view the activity bar has selected.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum SidebarView {
    Explorer,
    Search,
    SourceControl,
    Debug,
    Extensions,
}

/// What a modal dialog confirms.
pub(crate) enum DialogAction {
    DeleteNode(usize),
    CloseDoc(usize),
    GitDiscard { path: String, untracked: bool },
    GitDiscardAll,
    GitStash,
    /// Revert a single change block in the working tree (reverse-apply its patch).
    RevertDiffBlock { patch: String },
    InstallUpdate,
    /// Closing the window while terminals have running processes: kill them, keep
    /// them running in the background (daemon), or cancel the close.
    CloseWindowBusy,
    Dismiss, // info-only dialog; any button just closes it
}

pub(crate) struct DialogState {
    pub(crate) action: DialogAction,
    pub(crate) has_check: bool,
    pub(crate) checked: bool,
    pub(crate) hovered: Option<usize>,
}

/// One row in the generic right-click context menu.
#[derive(Clone)]
pub(crate) struct CtxEntry {
    pub label: String,
    pub hint: &'static str,
    pub action: CtxAction,
}

impl CtxEntry {
    fn new(label: impl Into<String>, action: CtxAction) -> Self {
        Self { label: label.into(), hint: "", action }
    }
    fn key(label: impl Into<String>, action: CtxAction, hint: &'static str) -> Self {
        Self { label: label.into(), hint, action }
    }
    fn sep() -> Self {
        Self { label: String::new(), hint: "", action: CtxAction::Separator }
    }
    fn stub(label: &'static str) -> Self {
        Self { label: label.to_string(), hint: "", action: CtxAction::Stub(label) }
    }
}

/// What a context-menu row does, across every surface that opens one.
#[derive(Clone)]
pub(crate) enum CtxAction {
    Separator,
    Stub(&'static str),
    Command(Command),          // editor: cut/copy/paste/find/select-all/palette…
    MenuCmd(menus::MenuCmd),    // reuse the menu-bar command routing (gear/manage menu)
    SetSetting(&'static str, String), // settings-editor enum dropdown: write key = json value
    MarkdownPreviewTab(usize),        // open a markdown preview of a specific tab
    MarkdownPreviewPath(PathBuf),     // open a markdown preview of a tree file (open it first)
    Cut,
    Copy,
    Paste,
    Palette,
    CloseTab(usize),
    CloseOtherTabs(usize),
    CloseTabsRight(usize),
    CloseAllTabs,
    CopyDocPath(usize),
    RevealInOs(PathBuf),
    ScmIntent(ui::Intent),     // stage/unstage/discard/open — reuses apply_intent
    CopyText(String),          // copy an arbitrary string (paths)
    CloseSavedTabs,
    TreeNewFile,               // explorer flows (selected_tree is set by the right-click)
    TreeNewFolder,
    TreeRename(usize),
    TreeDelete(usize),
    OpenTerminalAt(PathBuf),   // new terminal tab whose shell starts in this folder
    GitIgnore(String),         // append a repo-relative path to .gitignore
    FileCut(PathBuf),          // explorer clipboard (move on paste)
    FileCopy(PathBuf),         // explorer clipboard (copy on paste)
    FilePaste(PathBuf),        // paste the clipboard entry into this directory
    SelectForCompare(PathBuf), // remember one side of a two-file diff
    CompareWith(PathBuf),      // diff the remembered file against this one
    OpenAtHead(String),        // read-only tab with the file's HEAD version (repo-relative)
    RevealInTree(PathBuf),     // select + scroll this path in the explorer
    MoveToNewWindow(usize),    // reopen this tab's file in a fresh window, close it here
    TermRename(usize),         // terminal tab: open the rename input
    TermSplit(usize),          // terminal tab: split it
    TermKill(usize),           // terminal tab: kill it (all panes)
    TermNew,                   // terminal: new tab
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
    /// Diagnostic hover tooltip: (info, screen x, screen y) when the pointer rests
    /// over a diagnostic range in the editor (or over the card itself, for persistence).
    pub(crate) hover_tip: Option<(crate::lsp::DiagHover, f32, f32)>,
    /// A diagnostic the pointer is resting on, awaiting the hover delay before it's
    /// promoted to a visible `hover_tip`. (info, x, y, when the rest started.)
    pub(crate) hover_pending: Option<(crate::lsp::DiagHover, f32, f32, Instant)>,
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
    pub(crate) click_streak: u32, // 1=single, 2=double, 3=triple… (consecutive clicks)
    // In-flight drag on a diff's collapsed-unchanged separator: (gap index, anchor
    // cursor y, gap `top`+`bot` reveal at press, dragged-past-threshold). Drag down
    // grows `top` (reveal after the upper block); drag up grows `bot` (reveal before
    // the lower block).
    pub(crate) gap_drag: Option<(usize, f32, usize, usize, bool)>,
    // The diff change block under the cursor, as visible `[start, end)` rows — drives
    // the per-block Stage/Revert hover buttons. None ⇒ not hovering a block.
    pub(crate) hovered_diff_block: Option<(usize, usize)>,
    // Tooltip for the hovered per-block button: (label, anchor x, anchor y).
    pub(crate) block_tip: Option<(String, f32, f32)>,
    // Commit-graph: full message of the hovered commit row + anchor (x, y).
    pub(crate) commit_tip: Option<(String, f32, f32)>,
    /// Inline-blame hover card staged while the pointer rests on the annotation;
    /// promoted into `commit_tip` after `BLAME_DELAY`. (text, x, y, since.)
    pub(crate) blame_pending: Option<(String, f32, f32, Instant)>,
    // Editor tab hover: full file name + anchor (x, y) — tabs truncate to fit.
    pub(crate) tab_tip: Option<(String, f32, f32)>,
    // Explorer / Source Control row hover: full path when the label is ellipsized.
    pub(crate) row_tip: Option<(String, f32, f32)>,
    pub(crate) sidebar_view: SidebarView,
    // Find-in-files (Search view): a self-contained panel (built once the GPU/font
    // system exists, in `resumed`). Owns all of its own state + buffers.
    pub(crate) search: Option<ui::search_panel::SearchPanel>,
    pub(crate) source_control: Option<ui::source_control_panel::SourceControlPanel>,
    /// Commit author name (`git config user.name`), resolved once — blame shows
    /// "You" instead of this name. `None` = not a repo / git missing.
    pub(crate) git_user: Option<String>,
    /// Filesystem watcher on the workspace root (kept alive so it keeps firing).
    pub(crate) fs_watcher: Option<notify::RecommendedWatcher>,
    /// Debounce for fs-change handling: paths changed since the last flush + when
    /// to flush them (refresh Source Control + reload externally-edited open docs).
    pub(crate) fs_dirty: std::collections::HashSet<PathBuf>,
    pub(crate) fs_flush_due: Option<Instant>,
    pub(crate) settings_editor: ui::settings_editor::SettingsEditor,
    pub(crate) debug: Option<ui::debug_panel::DebugPanel>,
    pub(crate) dap: Option<dap::DapClient>,
    pub(crate) debug_thread: Option<i64>, // thread the session is stopped on
    pub(crate) debug_config: Option<debug_config::LaunchConfig>, // active config (for the handshake)
    pub(crate) debug_handshook: bool,         // initialize response seen (adapter is healthy)
    pub(crate) debug_pending_pause: bool,     // a Pause was requested; pause once threads arrive
    pub(crate) extensions_panel: Option<ui::extensions_panel::ExtensionsPanel>,
    pub(crate) extensions: Vec<Extension>,
    pub(crate) text_drag: Option<InputId>, // active mouse drag-selection in a text input
    pub(crate) find_drag: Option<bool>,    // find-widget drag-select (Some(true)=replace input)
    // Selection-occurrence highlight (VSCode-style): all matches of the current
    // word-like selection, recomputed when the selection text / doc version changes.
    pub(crate) sel_matches: Vec<(usize, usize)>,
    pub(crate) sel_hl_text: String,
    pub(crate) sel_hl_version: i32,
    // Code-completion popup. Word-based fills it instantly; an async LSP request
    // (rust-analyzer/tsserver/…) upgrades it. `completion_req` is the in-flight LSP
    // request (id, prefix_start) so a stale response can't apply to a moved cursor.
    pub(crate) completion: completion::Completion,
    pub(crate) completion_req: Option<(i64, usize)>,
    // Drag-and-drop state: explorer entry being dragged (path, press pos, past the
    // activation threshold), the folder it would drop into, and a tab drag-reorder.
    pub(crate) tree_drag: Option<(PathBuf, (f32, f32), bool)>,
    pub(crate) tree_drop_target: Option<PathBuf>,
    pub(crate) tab_drag: Option<(usize, (f32, f32), bool)>,
    /// Caret byte to restore if the palette's symbol preview is dismissed (Esc).
    pub(crate) palette_preview_return: Option<usize>,
    /// Generation of the palette's in-flight `%` text search (offset far above the
    /// Search panel's gens so streamed results route to the right consumer).
    pub(crate) palette_search_gen: u64,
    /// Cached go-to-file index (the full workspace file walk). Built lazily on the
    /// first Files-mode use and reused across mode switches / palette opens so that
    /// deleting the `>` prefix (Commands → Files) is instant instead of re-walking
    /// the tree on the UI thread. Invalidated to `None` whenever files are
    /// added/removed/renamed or the workspace folder changes.
    pub(crate) palette_file_cache: Option<Vec<commands::PickItem>>,
    /// Line range (0-based, inclusive) tinted while previewing an `@` symbol — the
    /// symbol's whole block (via the indentation fold range), not just its name.
    pub(crate) palette_preview_region: Option<(usize, usize)>,
    /// Last lone-Shift press, for the double-Shift palette shortcut.
    pub(crate) last_shift: Option<Instant>,
    // Generic right-click context menu (editor / tabs / SCM rows / …). The explorer
    // keeps its older dedicated menu for now.
    pub(crate) ctx_menu: Option<((f32, f32), Vec<CtxEntry>)>,
    pub(crate) ctx_hover: Option<usize>,
    // Native macOS menu bar — kept alive here; map resolves a click to a MenuCmd.
    #[cfg(target_os = "macos")]
    pub(crate) macos_menu: Option<(muda::Menu, std::collections::HashMap<String, menus::MenuCmd>)>,
    pub(crate) image_drag_last: Option<(f32, f32)>, // last cursor pos while panning an image
    pub(crate) ext_remote: Vec<marketplace::RemoteExt>, // current marketplace search results
    pub(crate) worker_tx: Sender<WorkerMsg>,
    pub(crate) worker_rx: Receiver<WorkerMsg>,
    /// Language servers (ESLint diagnostics, TS semantic tokens) — the manager owns
    /// the clients, the sync loop, and response routing (see lsp.rs).
    pub(crate) lsp: lsp::LspManager,
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
    pub(crate) term_blink_on: bool,    // blink phase for the terminal block cursor
    pub(crate) term_last_blink: Instant,
    pub(crate) last_edit: Instant,  // for files.autoSave (afterDelay)
    pub(crate) nav: nav::NavState,  // Go > Back / Forward jump list
    pub(crate) zen_saved: Option<(bool, bool)>, // pre-Zen (sidebar, terminal) visibility
    pub(crate) right_sidebar_visible: bool,     // secondary sidebar (AI chat)
    pub(crate) right_split: Splitter,           // its width, resizable from its left edge
    pub(crate) outline: Option<ui::outline_panel::OutlinePanel>, // created with the first FontSystem
    pub(crate) outline_open: bool,              // explorer OUTLINE section expanded
    pub(crate) chat: Option<ui::chat_panel::ChatPanel>, // right-sidebar AI chat (created with fs)
    pub(crate) pending_rename: Option<(String, &'static str, u32, u32)>, // (uri, lang, line, col) for the open rename input
    pub(crate) pending_term_rename: Option<usize>, // terminal tab awaiting its rename input
    pub(crate) file_clipboard: Option<(PathBuf, bool)>, // explorer Cut/Copy: (path, is_cut)
    pub(crate) compare_select: Option<PathBuf>, // explorer "Select for Compare" anchor
    pub(crate) lsp_log: std::collections::VecDeque<String>, // ring buffer for the Output tab
    pub(crate) debug_console: std::collections::VecDeque<String>, // debug adapter/debuggee output
    pub(crate) panel_tab: usize, // active bottom-panel tab (index into theme::PANEL_TABS)
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
            right_sidebar_visible: false,
            right_split: Splitter::new_from_end(
                theme::SIDEBAR_WIDTH(),
                theme::SIDEBAR_MIN_WIDTH(),
                theme::SIDEBAR_MAX_WIDTH(),
                widgets::Axis::Horizontal,
            ),
            outline: None,
            outline_open: false,
            chat: None,
            palette: PaletteState::new(),
            find: FindBarState::new(),
            ui_cache: UiCache::new(),
            hovered_tab: None,
            hovered_tab_close: None,
            hovered_tree: None,
            hover_tip: None,
            hover_pending: None,
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
            click_streak: 1,
            gap_drag: None,
            hovered_diff_block: None,
            block_tip: None,
            commit_tip: None,
            blame_pending: None,
            tab_tip: None,
            row_tip: None,
            sidebar_view: SidebarView::Explorer,
            search: None, // built in `resumed` once the font system exists
            source_control: None, // built in `resumed`
            git_user: None,       // resolved in `open_initial`
            fs_watcher: None,     // started in `open_initial`
            fs_dirty: std::collections::HashSet::new(),
            fs_flush_due: None,
            settings_editor: ui::settings_editor::SettingsEditor::default(),
            debug: None, // built in `resumed`
            dap: None,
            debug_thread: None,
            debug_config: None,
            debug_handshook: false,
            debug_pending_pause: false,
            extensions_panel: None, // built in `resumed`
            extensions: Vec::new(),
            text_drag: None,
            find_drag: None,
            completion: completion::Completion::default(),
            completion_req: None,
            tree_drag: None,
            tree_drop_target: None,
            tab_drag: None,
            palette_preview_return: None,
            palette_search_gen: 1 << 32,
            palette_file_cache: None,
            palette_preview_region: None,
            last_shift: None,
            ctx_menu: None,
            ctx_hover: None,
            sel_matches: Vec::new(),
            sel_hl_text: String::new(),
            sel_hl_version: -1,
            #[cfg(target_os = "macos")]
            macos_menu: None,
            image_drag_last: None,
            ext_remote: Vec::new(),
            worker_tx,
            worker_rx,
            lsp: lsp::LspManager::new(),
            installing: None,
            detail: ui::ext_detail_view::ExtDetailView::new(),
            pending_close: false,
            terminal: ui::terminal_panel::TerminalPanel::new(root.clone()),
            terminal_cell_w: theme::FONT_SIZE() * 0.6, // refined after first shape
            cursor_blink_on: true,
            last_blink: Instant::now(),
            term_blink_on: true,
            term_last_blink: Instant::now(),
            last_edit: Instant::now(),
            nav: nav::NavState::default(),
            zen_saved: None,
            pending_rename: None,
            pending_term_rename: None,
            file_clipboard: None,
            compare_select: None,
            lsp_log: std::collections::VecDeque::new(),
            debug_console: std::collections::VecDeque::new(),
            panel_tab: theme::PANEL_ACTIVE_TAB, // default to TERMINAL
            anim_start: Instant::now(),
            cursor_icon: CursorIcon::Default,
        }
    }

    /// Make the caret solid and restart its blink timer — call on any caret movement
    /// (keystroke, click) so it doesn't blink out mid-edit. Covers both the editor and
    /// terminal carets (they blink on separate phases).
    fn reset_blink(&mut self) {
        let now = Instant::now();
        self.cursor_blink_on = true;
        self.last_blink = now;
        self.term_blink_on = true;
        self.term_last_blink = now;
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

        // Command palette (modal) captures hover: pointer over a row, text over the
        // input, arrow elsewhere. No chrome hover behind it (it would show through
        // the dim and flicker as the pointer moves).
        if self.palette.active {
            let cursor = self
                .gpu
                .as_ref()
                .zip(layout.palette.as_ref())
                .map(|(g, pal)| {
                    if pal.input.contains(p) {
                        CursorIcon::Text
                    } else if g.ui.palette_list.row_at_scrolled(pal.list, self.palette.scroll, p, self.palette.filtered.len()).is_some() {
                        CursorIcon::Pointer
                    } else {
                        CursorIcon::Default
                    }
                })
                .unwrap_or(CursorIcon::Default);
            // Selecting the hovered row mirrors VSCode (mouse hover moves selection).
            if let Some((g, pal)) = self.gpu.as_ref().zip(layout.palette.as_ref()) {
                if let Some(i) = g.ui.palette_list.row_at_scrolled(pal.list, self.palette.scroll, p, self.palette.filtered.len()) {
                    if self.palette.selected != i {
                        self.palette.selected = i;
                        self.redraw();
                    }
                }
            }
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

        let new_tree = if self.sidebar_visible && self.sidebar_view == SidebarView::Explorer {
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
        // Explorer indent guides: revealed (eased) while hovering the tree.
        let in_tree = self.sidebar_visible
            && self.sidebar_view == SidebarView::Explorer
            && layout.tree_region().contains(p);
        if let Some(g) = self.gpu.as_mut() {
            if g.ui.sidebar.set_guides_hovered(in_tree) {
                changed = true;
            }
            if g.ui.sidebar.set_hover_row(if in_tree { self.hovered_tree } else { None }) {
                changed = true;
            }
        }

        let tab_rects = layout.tab_rects(self.tab_count());
        let new_tab = tab_rects.iter().position(|r| r.contains(p));
        let new_close =
            new_tab.filter(|&i| Layout::tab_close_rect(tab_rects[i]).contains(p));
        if new_tab != self.hovered_tab {
            self.hovered_tab = new_tab;
            changed = true;
        }
        // Full-name tooltip for the hovered document tab (names truncate to fit).
        let tab_tip = new_tab
            .filter(|&i| i < self.workspace.documents.len())
            .map(|i| {
                let r = tab_rects[i];
                (self.workspace.documents[i].name.clone(), r.x + theme::zpx(8.0), r.y + r.h)
            });
        if tab_tip != self.tab_tip {
            self.tab_tip = tab_tip;
            changed = true;
        }
        // Full-path tooltip for a hovered Explorer / Source Control row whose label
        // is ellipsized (#40). Anchored just below-right of the pointer.
        let anchor = (p.0 + theme::zpx(12.0), p.1 + theme::zpx(16.0));
        let row_tip = if !self.sidebar_visible {
            None
        } else if self.sidebar_view == SidebarView::SourceControl {
            self.source_control
                .as_ref()
                .and_then(|scp| scp.row_tip_at(p, layout.panel_region()))
                .map(|full| (full, anchor.0, anchor.1))
        } else if self.sidebar_view == SidebarView::Explorer {
            new_tree
                .filter(|&i| self.gpu.as_ref().map_or(false, |g| g.ui.sidebar.row_truncated(i)))
                .and_then(|i| self.workspace.tree.nodes.get(i))
                .map(|n| (n.name.clone(), anchor.0, anchor.1))
        } else {
            None
        };
        if row_tip != self.row_tip {
            self.row_tip = row_tip;
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

        let new_menu = if layout.palette.is_none() && !cfg!(target_os = "macos") {
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
        if let Some((anchor, _)) = self.ctx_menu {
            let hov = self.gpu.as_ref().and_then(|g| {
                let win = (g.config.width as f32, g.config.height as f32);
                let rect = g.ui.ctx.rect(anchor, win);
                g.ui.ctx.item_at(rect, p)
            });
            if hov != self.ctx_hover {
                self.ctx_hover = hov;
                self.redraw();
            }
        }
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
        // Breadcrumb dropdown row hover.
        if self.gpu.as_ref().map_or(false, |g| g.ui.breadcrumbs.is_open()) {
            let win = self.gpu.as_ref().map(|g| (g.config.width as f32, g.config.height as f32)).unwrap_or((1280.0, 800.0));
            let row = self.gpu.as_ref().and_then(|g| {
                g.ui.breadcrumbs.dropdown_rect(layout.breadcrumbs, win)
                    .and_then(|r| g.ui.breadcrumbs.dropdown_item_at(r, p))
            });
            if let Some(g) = self.gpu.as_mut() {
                if g.ui.breadcrumbs.hovered_row != row {
                    g.ui.breadcrumbs.hovered_row = row;
                    changed = true;
                }
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
            self.gpu.as_ref().map(|g| g.ui.ext_detail.hit_button(region, p)).unwrap_or(false)
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

        // Hovering a link row in an info page → pointer cursor.
        let over_md_link = if self.detail.open_extension.is_none() {
            let body = ui::info_page::InfoPage::body(render::editor_region(&layout));
            let info_hit = self.workspace.active_doc().map_or(false, |d| {
                d.info.as_ref().map_or(false, |page| page.links(body, d.scroll_y()).iter().any(|(r, _)| r.contains(p)))
            });
            let md_hit = self.gpu.as_ref().map_or(false, |g| {
                self.workspace.active_doc().map_or(false, |d| {
                    d.markdown_preview.as_ref().map_or(false, |md| {
                        md.link_geometry(body, d.scroll_y(), &|k| g.media.size(k)).iter().any(|(r, _)| r.contains(p))
                    })
                })
            });
            info_hit || md_hit
        } else {
            false
        };

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
        // Diagnostic hover: the card only appears after the cursor *rests* on a
        // squiggle (staged in `hover_pending`, promoted after a delay in about_to_wait).
        // This stops it flickering in/out as the pointer sweeps across diagnostics.
        let squiggle = if ed_inside && !over_scroll_thumb {
            self.workspace.active_doc().and_then(|d| {
                let bx = p.0 - (layout.editor_text.x + theme::EDITOR_PAD()) + d.scroll_x();
                let by = p.1 - (layout.editor_text.y + theme::EDITOR_PAD()) + d.scroll_y();
                d.diagnostic_at(bx, by).map(|info| (info, p.0, p.1))
            })
        } else {
            None
        };
        // Keep a shown card visible while the pointer is over it — or in the small gap
        // between the squiggle and the card (so moving toward it doesn't dismiss it).
        // Also resolve whether the pointer is over the clickable docs link.
        let (over_card, over_diag_link) = self
            .hover_tip
            .as_ref()
            .zip(self.gpu.as_ref())
            .map_or((false, false), |((_, ax, ay), g)| {
                let screen = crate::widgets::Rect { x: 0.0, y: 0.0, w: g.config.width as f32, h: g.config.height as f32 };
                let card = g.ui.diag_hover.rect((*ax, *ay), screen);
                let m = theme::zpx(20.0); // margin bridges the anchor↔card gap
                let bridge = crate::widgets::Rect { x: card.x - m, y: card.y - m, w: card.w + 2.0 * m, h: card.h + 2.0 * m };
                let on_link = g.ui.diag_hover.link_rect(card).map_or(false, |lr| lr.contains(p));
                (bridge.contains(p), on_link)
            });
        if over_card {
            self.hover_pending = None; // leave hover_tip as-is
        } else if let Some((info, cx, cy)) = squiggle {
            let showing_same = self.hover_tip.as_ref().map_or(false, |(i, ..)| *i == info);
            if showing_same {
                self.hover_pending = None; // already visible; don't restart the timer
            } else {
                let same_pending = self.hover_pending.as_ref().map_or(false, |(i, ..)| *i == info);
                if !same_pending {
                    self.hover_pending = Some((info, cx, cy, Instant::now()));
                }
                if self.hover_tip.take().is_some() {
                    changed = true; // hide a stale card from a previous squiggle
                }
            }
        } else {
            // Off any squiggle and not over the card → dismiss.
            self.hover_pending = None;
            if self.hover_tip.take().is_some() {
                changed = true;
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
                if sp.hover(p, layout.sidebar) {
                    changed = true;
                }
            }
        }
        if self.sidebar_visible && self.sidebar_view == SidebarView::SourceControl {
            if let Some(scp) = self.source_control.as_mut() {
                if scp.hover(p, layout.panel_region()) {
                    changed = true;
                }
            }
        }
        if self.sidebar_visible && self.sidebar_view == SidebarView::Debug {
            if let Some(dp) = self.debug.as_mut() {
                if dp.on_hover(p, layout.panel_region()) {
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
        let over_right_handle = self.right_sidebar_visible
            && layout.palette.is_none()
            && self.right_split.handle_rect(layout.right_sidebar).contains(p);
        // Explorer OUTLINE section hover (the panel owns its row geometry); the
        // header gets a pointer cursor too (it's a collapsible toggle).
        let mut over_outline_row = false;
        let mut outline_hover_changed = false;
        let outline_region = match (layout.outline_header_rect(), layout.outline_body_rect()) {
            (Some(h), Some(b)) if layout.palette.is_none() => {
                over_outline_row = h.contains(p);
                Some(widgets::Rect { x: h.x, y: h.y, w: h.w, h: h.h + b.h })
            }
            _ => None,
        };
        if let Some(region) = outline_region {
            if let Some(o) = self.outline.as_mut() {
                outline_hover_changed = o.on_move(p, region);
                over_outline_row |= o.row_at(p, region).is_some();
            }
        }
        if outline_hover_changed {
            self.redraw();
        }
        // Chat panel cursor (text over the input, default elsewhere in the panel).
        let chat_cursor = if self.right_sidebar_visible && layout.palette.is_none() {
            self.chat.as_ref().and_then(|c| c.cursor(p, layout.right_sidebar))
        } else {
            None
        };
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
        // Over the terminal's text area → I-beam (selectable text), like VS Code.
        let over_term_content = self.terminal.visible
            && layout.palette.is_none()
            && !over_term_btn
            && !over_term_handle
            && !over_scroll_thumb
            && layout.terminal_panel.map_or(false, |panel| {
                let content = terminal_content(panel);
                content.contains(p)
                    && terminal_tablist_rect(content, self.terminal.groups.len()).map_or(true, |tl| !tl.contains(p))
            });
        // Find/replace widget: update button-hover highlight + resolve its cursor.
        let find_cursor = if self.find.active {
            let er = render::editor_region(&layout);
            let fl = ui::find_widget::FindWidget::layout(er, self.find.replace_open);
            if let Some(g) = self.gpu.as_mut() {
                let h = g.ui.find.button_at(&fl, p);
                if h != g.ui.find.hover {
                    g.ui.find.hover = h;
                    changed = true;
                }
                g.ui.find.cursor(&fl, p)
            } else {
                None
            }
        } else {
            None
        };

        // Gutter fold chevron (foldable line) is clickable → pointer.
        let over_fold_chevron = layout.gutter.contains(p)
            && p.0 >= layout.gutter.x + layout.gutter.w - theme::zpx(18.0)
            && self.workspace.active_doc().map_or(false, |d| {
                if d.diff.is_some() {
                    return false;
                }
                let lh = theme::LINE_HEIGHT();
                let vy = p.1 - (layout.editor_text.y + theme::EDITOR_PAD()) + d.scroll_y();
                if vy < 0.0 {
                    return false;
                }
                let line = d.visible_index_to_line((vy / lh) as usize);
                d.is_foldable(line)
            });

        // Combined-diff file headers are clickable (collapse/expand) → pointer.
        let over_diff_header = layout.editor_text.contains(p)
            && self.workspace.active_doc().map_or(false, |d| {
                d.diff_full.is_some()
                    && ui::editor_view::EditorView::line_at(d, &layout, p.0, p.1)
                        .and_then(|line| d.diff_file_at_line(line))
                        .is_some()
            });
        // Collapsed-unchanged separator (band or gutter unfold button): click to
        // expand, drag to reveal → resize cursor.
        let diff_region = render::editor_region(&layout);
        let over_diff_gap = diff_region.contains(p)
            && self.workspace.active_doc().map_or(false, |d| d.diff_gap_at_y(diff_region, p.1).is_some());
        // Per-block Stage/Revert/Unstage button → pointer.
        let over_diff_block_btn = diff_region.contains(p)
            && self.workspace.active_doc().map_or(false, |d| {
                if d.diff_path.is_none() {
                    return false;
                }
                d.diff_block_at_y(diff_region, p.1)
                    .and_then(|(vbs, vbe)| {
                        let count = if d.diff_staged { 1 } else { 2 };
                        render::diff_block_btn_rects(diff_region, vbs, vbe, d.scroll_y(), count)
                    })
                    .map_or(false, |rects| rects.iter().any(|r| r.contains(p)))
            });
        // Floating overlays claim the cursor FIRST — otherwise whatever sits UNDER
        // a menu / palette / popup picks the icon (e.g. the editor's I-beam showing
        // over a context menu).
        let over_overlay = {
            let in_ctx = self
                .ctx_menu
                .as_ref()
                .and_then(|(a, _)| self.gpu.as_ref().map(|g| {
                    let win = (g.config.width as f32, g.config.height as f32);
                    g.ui.ctx.rect(*a, win).contains(p)
                }))
                .unwrap_or(false);
            let in_dd = self.open_menu.is_some() && self.menu_dd_rect().map_or(false, |r| r.contains(p));
            let in_palette_list = layout.palette.as_ref().map_or(false, |pal| pal.box_.contains(p));
            let in_palette_input = layout.palette.as_ref().map_or(false, |pal| pal.input.contains(p));
            let modal = self.dialog.is_some() || self.feedback_form.is_some();
            if in_palette_input {
                Some(CursorIcon::Text)
            } else if in_ctx || in_dd || in_palette_list || modal {
                Some(CursorIcon::Default)
            } else {
                None
            }
        };
        let new_cursor = if self.settings_editor.open {
            // The Settings modal owns the whole screen — resolve its cursor here so
            // background regions (editor I-beam, etc.) can't bleed through.
            self.settings_cursor(p)
        } else if let Some(c) = over_overlay {
            c
        } else if self.sidebar_split.is_dragging() || over_handle {
            self.sidebar_split.cursor()
        } else if self.right_split.is_dragging() || over_right_handle {
            self.right_split.cursor()
        } else if over_outline_row {
            CursorIcon::Pointer
        } else if self.sidebar_visible
            && self.sidebar_view == SidebarView::Debug
            && self.debug.as_ref().map_or(false, |dp| dp.over_row(p, layout.panel_region()))
        {
            CursorIcon::Pointer
        } else if let Some(c) = chat_cursor {
            c
        } else if let Some(c) = find_cursor {
            c
        } else if self.terminal.split.is_dragging() || over_term_handle {
            self.terminal.split.cursor()
        } else if over_term_btn {
            CursorIcon::Pointer
        } else if over_term_content
            && (self.mods.control_key() || (cfg!(target_os = "macos") && self.mods.super_key()))
            && self.terminal.url_at(p, &layout, self.terminal_cell_w).is_some()
        {
            // Ctrl/Cmd held over a terminal URL → it's clickable.
            CursorIcon::Pointer
        } else if over_term_content {
            CursorIcon::Text
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
            .then(|| self.search.as_ref().and_then(|sp| sp.cursor(p, layout.sidebar)))
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
        } else if self.sidebar_visible
            && self.sidebar_view == SidebarView::SourceControl
            && self
                .source_control
                .as_ref()
                .map_or(false, |scp| scp.resizing() || scp.over_divider(p, layout.panel_region()))
        {
            CursorIcon::RowResize
        } else if self.sidebar_visible
            && self.sidebar_view == SidebarView::SourceControl
            && self
                .source_control
                .as_ref()
                .map_or(false, |scp| scp.over_message(p, layout.panel_region()))
        {
            CursorIcon::Text
        } else if self.sidebar_visible
            && self.sidebar_view == SidebarView::SourceControl
            && self
                .source_control
                .as_ref()
                .map_or(false, |scp| scp.over_row(p, layout.panel_region()))
        {
            // A changed-file row or collapsible folder header is clickable.
            CursorIcon::Pointer
        } else if self.focused_input_at(&layout, p).is_some() {
            CursorIcon::Text
        } else if new_page_install || new_detail_tab.is_some() || over_detail_link || over_diag_link {
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
            } else if over_diff_block_btn {
                // Per-block Stage/Revert/Unstage button: clickable.
                CursorIcon::Pointer
            } else if over_diff_gap {
                // Collapsed-unchanged separator: draggable to reveal lines.
                CursorIcon::RowResize
            } else if over_diff_header || over_fold_chevron {
                // Combined-diff file header / gutter fold chevron: clickable.
                CursorIcon::Pointer
            } else if over_md_link {
                CursorIcon::Pointer
            } else if self.workspace.active_doc().map_or(false, |d| d.binary)
                && g.ui.binary_placeholder.hit_button(render::editor_region(&layout), p)
            {
                // "Open Anyway" button in the binary-file placeholder: clickable.
                CursorIcon::Pointer
            } else if self.workspace.active_doc().is_some()
                && render::encoding_cell(layout.status_bar, g.ui.encoding.width()).contains(p)
            {
                // Status-bar encoding cell: clickable (opens the encoding picker).
                CursorIcon::Pointer
            } else if layout.status_bar.contains(p) && g.ui.branch.width() > 0.0 && {
                // Status-bar branch indicator: clickable (geometry matches render.rs).
                let icon_x = layout.status_bar.x + theme::zpx(10.0);
                let name_x = icon_x + g.ui.branch_icon.width() + theme::zpx(2.0);
                let block_w = (name_x + g.ui.branch.width() + theme::zpx(12.0)) - layout.status_bar.x;
                p.0 < layout.status_bar.x + block_w
            } {
                CursorIcon::Pointer
            } else if layout.editor_text.contains(p)
                && self.workspace.active_doc().map_or(false, |d| d.info.is_some())
            {
                // Designed info page: regular arrow, not an I-beam.
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
        // Load user settings.
        let s = settings::reload();
        self.sidebar_visible = s.workbench_sidebar_visible;

        // Resolve the commit author once so blame can render "You" for the user's
        // own lines instead of their full name.
        self.git_user = git::user_name(&git::repo_root(&self.cwd));

        // Watch the workspace so Source Control + open files react to external
        // changes (terminal commands, other windows, `claude code`, another editor).
        self.start_fs_watcher();

        // Scan installed extensions up front so "Installed" status is accurate from
        // the start and grammar extensions (rainbow-csv, …) activate on launch.
        self.extensions = extensions::scan();
        self.activate_installed_grammars();
        // The panel was created with empty rows before this scan ran; push the
        // installed list into it now so the Extensions view isn't blank on launch.
        self.rebuild_ext_rows();

        // Apply the saved color theme AFTER scanning — its label lives in an installed
        // theme extension, so applying before the scan would never find it (and the UI
        // would silently fall back to the built-in dark theme on every launch).
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
        // No welcome/Untitled doc on launch — if nothing was opened, show an empty
        // editor. The user opens files from the sidebar or command palette.
    }

    /// Map the active sidebar view to/from its persisted string tag.
    fn sidebar_view_tag(view: SidebarView) -> &'static str {
        match view {
            SidebarView::Explorer => "explorer",
            SidebarView::Search => "search",
            SidebarView::SourceControl => "scm",
            SidebarView::Debug => "debug",
            SidebarView::Extensions => "extensions",
        }
    }

    /// Snapshot the current window/layout into the per-workspace session store, so
    /// the next launch in this folder restores open files + panels + sizes.
    fn save_session(&self) {
        // Only plain file tabs are restorable (diff/image/graph/info/preview views
        // are derived and can't be reopened by path).
        let files: Vec<PathBuf> = self
            .workspace
            .documents
            .iter()
            .filter(|d| {
                d.path.is_some()
                    && d.diff.is_none()
                    && d.image.is_none()
                    && d.graph.is_none()
                    && d.info.is_none()
                    && d.markdown_preview.is_none()
            })
            .filter_map(|d| d.path.clone())
            .collect();
        let active = self
            .workspace
            .active_doc()
            .and_then(|d| d.path.clone())
            .and_then(|ap| files.iter().position(|f| *f == ap));
        let session = state::Session {
            files,
            active,
            sidebar_visible: self.sidebar_visible,
            sidebar_view: Self::sidebar_view_tag(self.sidebar_view).to_string(),
            sidebar_width: self.sidebar_split.size(),
            terminal_visible: self.terminal.visible,
            terminal_maximized: self.terminal.maximized,
            terminal_height: self.terminal.split.size(),
            panel_tab: self.panel_tab,
            right_visible: self.right_sidebar_visible,
            right_width: self.right_split.size(),
            window: self.gpu.as_ref().map(|g| (g.config.width, g.config.height)),
        };
        state::State::save_session(&self.cwd, session);
    }

    /// Reopen the files + restore the panel layout saved for this workspace. Called
    /// once at startup after `open_initial` and the panels are built. Window size is
    /// restored separately at window-creation time.
    fn restore_session(&mut self) {
        let Some(sess) = state::State::load().session_for(&self.cwd).cloned() else {
            return;
        };
        if let Some(gpu) = self.gpu.as_mut() {
            for f in &sess.files {
                if f.exists() && !self.workspace.documents.iter().any(|d| d.path.as_ref() == Some(f)) {
                    let _ = self.workspace.open_file(f, &mut gpu.font_system);
                }
            }
        }
        // Restore the active tab by path (indices shift if a file went missing).
        if let Some(target) = sess.active.and_then(|i| sess.files.get(i)) {
            if let Some(idx) = self.workspace.documents.iter().position(|d| d.path.as_ref() == Some(target)) {
                self.workspace.active = Some(idx);
            }
        }
        // Sidebar: width + visibility + which view. The panels were all built in
        // `resumed`, so setting the field directly is safe (no lazy init needed).
        self.sidebar_split.set_size(sess.sidebar_width);
        self.sidebar_visible = sess.sidebar_visible;
        self.sidebar_view = match sess.sidebar_view.as_str() {
            "search" => SidebarView::Search,
            "scm" => SidebarView::SourceControl,
            "debug" => SidebarView::Debug,
            "extensions" => SidebarView::Extensions,
            _ => SidebarView::Explorer,
        };
        // Right (AI chat) sidebar.
        self.right_split.set_size(sess.right_width);
        self.right_sidebar_visible = sess.right_visible;
        // Bottom panel: height + active tab, then spawn the terminal if it was open.
        self.terminal.split.set_size(sess.terminal_height);
        self.panel_tab = sess.panel_tab;
        if sess.terminal_visible && !self.terminal.visible {
            self.terminal.maximized = sess.terminal_maximized;
            self.toggle_terminal();
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
            if self.right_sidebar_visible { self.right_split.size() } else { 0.0 },
            (self.sidebar_visible && self.sidebar_view == SidebarView::Explorer)
                .then_some(self.outline_open),
            Layout::breadcrumbs_visible(self.workspace.active_doc()),
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
        // Indent-guide hover fades (explorer tree + source-control groups).
        consider(self.gpu.as_ref().and_then(|g| g.ui.sidebar.guides_next_wake(now)));
        if let Some(scp) = self.source_control.as_ref() {
            consider(scp.guides_next_wake(now));
            // Keep ticking ~30fps while an AI commit message is generating so the
            // pulsing commit-box animation runs.
            if scp.generating {
                consider(Some(now + Duration::from_millis(33)));
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
        self.invalidate_file_index(); // a new file/folder may now exist on disk
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
        // Terminal tab list: per-tab management menu (rename / split / kill).
        if let Some(i) = self.terminal.tab_at((x, y), &layout) {
            let items = vec![
                CtxEntry::new("Rename…", CtxAction::TermRename(i)),
                CtxEntry::sep(),
                CtxEntry::new("New Terminal", CtxAction::TermNew),
                CtxEntry::new("Split Terminal", CtxAction::TermSplit(i)),
                CtxEntry::sep(),
                CtxEntry::new("Kill Terminal", CtxAction::TermKill(i)),
            ];
            self.open_ctx_menu((x, y), items);
            return;
        }
        // Windows-style terminal right-click: copy the selection if there is one,
        // otherwise paste the clipboard into the shell.
        if self.terminal.visible {
            if let Some(panel) = layout.terminal_panel {
                if crate::terminal_content(panel).contains((x, y)) {
                    if let Some(text) = self.terminal.selection_text() {
                        if let Some(cb) = self.clipboard.as_mut() {
                            let _ = cb.set_text(text);
                        }
                        self.terminal.clear_focused_selection();
                    } else if let Some(text) = self.clipboard.as_mut().and_then(|cb| cb.get_text().ok()) {
                        self.terminal.paste_focused(&text);
                    }
                    self.redraw();
                    return;
                }
            }
        }
        // Editor tabs: per-tab management menu.
        if layout.tab_strip.contains((x, y)) {
            let tab_rects = layout.tab_rects(self.tab_count());
            if let Some(idx) = tab_rects.iter().position(|r| r.contains((x, y))) {
                if Some(idx) != self.ext_tab_index() && idx < self.workspace.documents.len() {
                    let path = self.workspace.documents[idx].path.clone();
                    let mut items = vec![
                        CtxEntry::key("Close", CtxAction::CloseTab(idx), "Ctrl+W"),
                        CtxEntry::new("Close Others", CtxAction::CloseOtherTabs(idx)),
                        CtxEntry::new("Close to the Right", CtxAction::CloseTabsRight(idx)),
                        CtxEntry::new("Close Saved", CtxAction::CloseSavedTabs),
                        CtxEntry::new("Close All", CtxAction::CloseAllTabs),
                        CtxEntry::sep(),
                    ];
                    if let Some(p) = path {
                        let rel = p.strip_prefix(&self.cwd).unwrap_or(&p).to_string_lossy().to_string();
                        if p.extension().map_or(false, |e| e.eq_ignore_ascii_case("md")) {
                            items.push(CtxEntry::key("Open Preview", CtxAction::MarkdownPreviewTab(idx), "Ctrl+Shift+V"));
                            items.push(CtxEntry::sep());
                        }
                        items.push(CtxEntry::new("Copy Path", CtxAction::CopyText(p.to_string_lossy().to_string())));
                        items.push(CtxEntry::new("Copy Relative Path", CtxAction::CopyText(rel)));
                        items.push(CtxEntry::sep());
                        items.push(CtxEntry::new("Reveal in Finder", CtxAction::RevealInOs(p.clone())));
                        items.push(CtxEntry::new("Reveal in Explorer View", CtxAction::RevealInTree(p)));
                        items.push(CtxEntry::stub("Reopen Editor With…"));
                        items.push(CtxEntry::sep());
                    }
                    items.push(CtxEntry::stub("Keep Open"));
                    items.push(CtxEntry::stub("Split Up"));
                    items.push(CtxEntry::stub("Split Down"));
                    items.push(CtxEntry::stub("Split Left"));
                    items.push(CtxEntry::stub("Split Right"));
                    items.push(CtxEntry::new("Move into New Window", CtxAction::MoveToNewWindow(idx)));
                    self.open_ctx_menu((x, y), items);
                }
            }
            return;
        }

        // Editor body: the standard editor menu.
        if self.detail.open_extension.is_none()
            && self.workspace.active_doc().is_some()
            && render::editor_region(&layout).contains((x, y))
        {
            let is_md = self
                .workspace
                .active_doc()
                .and_then(|d| d.path.as_ref())
                .map_or(false, |p| p.extension().map_or(false, |e| e.eq_ignore_ascii_case("md")));
            let mut items = Vec::new();
            if is_md {
                items.push(CtxEntry::key("Open Preview", CtxAction::Command(Command::MarkdownPreview), "Ctrl+Shift+V"));
                items.push(CtxEntry::sep());
            }
            items.extend([
                CtxEntry { label: "Go to Definition".into(), hint: "F12", action: CtxAction::Command(Command::GotoDefinition) },
                CtxEntry { label: "Go to Declaration".into(), hint: "", action: CtxAction::Command(Command::GotoDeclaration) },
                CtxEntry { label: "Go to Type Definition".into(), hint: "", action: CtxAction::Command(Command::GotoTypeDefinition) },
                CtxEntry { label: "Go to Implementations".into(), hint: "", action: CtxAction::Command(Command::GotoImplementations) },
                CtxEntry { label: "Go to References".into(), hint: "Shift+F12", action: CtxAction::Command(Command::GotoReferences) },
                CtxEntry::sep(),
                CtxEntry::stub("Peek Definition"),
                CtxEntry { label: "Find All References".into(), hint: "", action: CtxAction::Command(Command::GotoReferences) },
                CtxEntry::stub("Show Call Hierarchy"),
                CtxEntry::sep(),
                CtxEntry { label: "Rename Symbol".into(), hint: "F2", action: CtxAction::Command(Command::RenameSymbol) },
                CtxEntry::stub("Change All Occurrences"),
                CtxEntry { label: "Format Document".into(), hint: "Shift+Alt+F", action: CtxAction::Command(Command::FormatDocument) },
                CtxEntry { label: "Format Selection".into(), hint: "", action: CtxAction::Command(Command::FormatSelection) },
                CtxEntry::stub("Refactor…"),
                CtxEntry::stub("Source Action…"),
                CtxEntry::sep(),
                CtxEntry::key("Cut", CtxAction::Cut, "Ctrl+X"),
                CtxEntry::key("Copy", CtxAction::Copy, "Ctrl+C"),
                CtxEntry::key("Paste", CtxAction::Paste, "Ctrl+V"),
                CtxEntry::sep(),
                CtxEntry::key("Command Palette…", CtxAction::Palette, "Ctrl+Shift+P"),
            ]);
            self.open_ctx_menu((x, y), items);
            return;
        }

        if !self.sidebar_visible || !layout.sidebar.contains((x, y)) {
            return;
        }
        // Source Control rows: stage/unstage/discard/open for the file under the cursor.
        if self.sidebar_view == SidebarView::SourceControl {
            let region = layout.panel_region();
            if let Some((path, staged, untracked)) =
                self.source_control.as_ref().and_then(|scp| scp.row_at_point((x, y), region))
            {
                let abs = self.cwd.join(&path);
                let mut items = vec![
                    CtxEntry::new("Open File", CtxAction::ScmIntent(ui::Intent::OpenFile { path: abs.clone(), line: 1, col: 0 })),
                    CtxEntry::new("Open Changes", CtxAction::ScmIntent(ui::Intent::OpenDiff { path: path.clone(), staged, untracked })),
                    CtxEntry::new("Open File (HEAD)", CtxAction::OpenAtHead(path.clone())),
                    CtxEntry::sep(),
                ];
                if staged {
                    items.push(CtxEntry::new("Unstage Changes", CtxAction::ScmIntent(ui::Intent::GitUnstage(path.clone()))));
                } else {
                    items.push(CtxEntry::new("Stage Changes", CtxAction::ScmIntent(ui::Intent::GitStage(path.clone()))));
                    items.push(CtxEntry::new("Discard Changes", CtxAction::ScmIntent(ui::Intent::GitDiscard { path: path.clone(), untracked })));
                }
                items.push(CtxEntry::new("Add to .gitignore", CtxAction::GitIgnore(path.clone())));
                items.push(CtxEntry::sep());
                items.push(CtxEntry::new("Copy Path", CtxAction::CopyText(abs.to_string_lossy().to_string())));
                items.push(CtxEntry::new("Copy Relative Path", CtxAction::CopyText(path.clone())));
                items.push(CtxEntry::sep());
                items.push(CtxEntry::new("Reveal in Finder", CtxAction::RevealInOs(abs)));
                self.open_ctx_menu((x, y), items);
            }
            return;
        }
        if self.sidebar_view != SidebarView::Explorer {
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
        let node = target.and_then(|t| self.workspace.tree.nodes.get(t).map(|n| (t, n.path.clone(), n.is_dir)));
        let mut items = vec![
            CtxEntry::new("New File…", CtxAction::TreeNewFile),
            CtxEntry::new("New Folder…", CtxAction::TreeNewFolder),
            CtxEntry::sep(),
        ];
        if let Some((t, path, is_dir)) = node {
            let rel = path.strip_prefix(&self.cwd).unwrap_or(&path).to_string_lossy().to_string();
            let dir = if is_dir { path.clone() } else { path.parent().map(|p| p.to_path_buf()).unwrap_or_else(|| self.cwd.clone()) };
            items.push(CtxEntry::stub("Open to the Side"));
            items.push(CtxEntry::stub("Open With…"));
            items.push(CtxEntry::sep());
            items.push(CtxEntry::new("Reveal in Finder", CtxAction::RevealInOs(path.clone())));
            items.push(CtxEntry::new("Open in Integrated Terminal", CtxAction::OpenTerminalAt(dir.clone())));
            items.push(CtxEntry::sep());
            if !is_dir {
                if path.extension().map_or(false, |e| e.eq_ignore_ascii_case("md")) {
                    items.push(CtxEntry::key("Open Preview", CtxAction::MarkdownPreviewPath(path.clone()), "Ctrl+Shift+V"));
                    items.push(CtxEntry::sep());
                }
                items.push(CtxEntry::new("Select for Compare", CtxAction::SelectForCompare(path.clone())));
                if let Some(sel) = self.compare_select.clone().filter(|s| s != &path) {
                    let name = sel.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
                    items.push(CtxEntry::new(&format!("Compare with '{name}'"), CtxAction::CompareWith(path.clone())));
                }
                items.push(CtxEntry::sep());
            }
            items.push(CtxEntry::new("Cut", CtxAction::FileCut(path.clone())));
            items.push(CtxEntry::new("Copy", CtxAction::FileCopy(path.clone())));
            if self.file_clipboard.is_some() {
                items.push(CtxEntry::new("Paste", CtxAction::FilePaste(dir)));
            }
            items.push(CtxEntry::sep());
            items.push(CtxEntry::new("Copy Path", CtxAction::CopyText(path.to_string_lossy().to_string())));
            items.push(CtxEntry::new("Copy Relative Path", CtxAction::CopyText(rel)));
            items.push(CtxEntry::sep());
            items.push(CtxEntry::key("Rename…", CtxAction::TreeRename(t), "F2"));
            items.push(CtxEntry::new("Delete", CtxAction::TreeDelete(t)));
        } else {
            items.push(CtxEntry::new("Reveal in Finder", CtxAction::RevealInOs(self.cwd.clone())));
            items.push(CtxEntry::new("Open in Integrated Terminal", CtxAction::OpenTerminalAt(self.cwd.clone())));
            if self.file_clipboard.is_some() {
                items.push(CtxEntry::sep());
                items.push(CtxEntry::new("Paste", CtxAction::FilePaste(self.cwd.clone())));
            }
        }
        self.open_ctx_menu((x, y), items);
    }

    /// Open the generic context menu at `anchor` with `entries`.
    fn open_ctx_menu(&mut self, anchor: (f32, f32), entries: Vec<CtxEntry>) {
        let rows: Vec<(&str, &str, bool)> = entries
            .iter()
            .map(|e| (e.label.as_str(), e.hint, matches!(e.action, CtxAction::Separator)))
            .collect();
        if let Some(g) = self.gpu.as_mut() {
            g.ui.ctx.set_entries(&mut g.font_system, &rows);
        }
        self.close_app_menu();
        self.ctx_menu = Some((anchor, entries));
        self.ctx_hover = None;
        self.redraw();
    }

    fn close_ctx_menu(&mut self) {
        if self.ctx_menu.take().is_some() {
            self.ctx_hover = None;
            self.redraw();
        }
    }

    fn exec_ctx_action(&mut self, action: CtxAction) {
        match action {
            CtxAction::Separator => {}
            CtxAction::Stub(name) => {
                self.show_info_dialog(&format!("“{name}” isn’t implemented yet — it's on the roadmap."));
            }
            CtxAction::Command(c) => self.exec_command(c),
            CtxAction::MenuCmd(m) => self.exec_menu_cmd(m),
            CtxAction::SetSetting(key, value_json) => {
                settings::set_user_value(key, &value_json);
                self.apply_settings();
                self.redraw();
            }
            CtxAction::MarkdownPreviewTab(idx) => {
                if idx < self.workspace.documents.len() {
                    self.workspace.active = Some(idx);
                    self.open_markdown_preview();
                }
            }
            CtxAction::MarkdownPreviewPath(path) => {
                if let Some(g) = self.gpu.as_mut() {
                    if self.workspace.open_file(&path, &mut g.font_system).is_ok() {
                        self.detail.open_extension = None;
                        self.open_markdown_preview();
                    }
                }
            }
            CtxAction::Cut => self.cut(),
            CtxAction::Copy => self.copy(),
            CtxAction::Paste => self.paste(),
            CtxAction::Palette => self.open_palette(),
            CtxAction::CloseTab(i) => self.request_close(i),
            CtxAction::CloseOtherTabs(keep) => {
                // Close every clean doc except `keep` (dirty ones stay, VSCode prompts
                // per-file; we keep them open instead of risking data).
                let paths: Vec<usize> = (0..self.workspace.documents.len()).rev().filter(|&i| i != keep).collect();
                for i in paths {
                    if !self.workspace.documents[i].dirty {
                        self.workspace.close_idx(i);
                    }
                }
                self.redraw();
            }
            CtxAction::CloseTabsRight(from) => {
                for i in (from + 1..self.workspace.documents.len()).rev() {
                    if !self.workspace.documents[i].dirty {
                        self.workspace.close_idx(i);
                    }
                }
                self.redraw();
            }
            CtxAction::CloseAllTabs => {
                for i in (0..self.workspace.documents.len()).rev() {
                    if !self.workspace.documents[i].dirty {
                        self.workspace.close_idx(i);
                    }
                }
                self.redraw();
            }
            CtxAction::CopyDocPath(i) => {
                if let Some(p) = self.workspace.documents.get(i).and_then(|d| d.path.clone()) {
                    if let Some(cb) = self.clipboard.as_mut() {
                        let _ = cb.set_text(p.to_string_lossy().to_string());
                    }
                }
            }
            CtxAction::RevealInOs(path) => reveal_in_os(&path),
            CtxAction::ScmIntent(intent) => self.apply_intent(intent),
            CtxAction::CopyText(text) => {
                if let Some(cb) = self.clipboard.as_mut() {
                    let _ = cb.set_text(text);
                }
            }
            CtxAction::CloseSavedTabs => {
                for i in (0..self.workspace.documents.len()).rev() {
                    if !self.workspace.documents[i].dirty {
                        self.workspace.close_idx(i);
                    }
                }
                self.redraw();
            }
            CtxAction::TreeNewFile => self.begin_create(false),
            CtxAction::TreeNewFolder => self.begin_create(true),
            CtxAction::TreeRename(t) => self.begin_rename(t),
            CtxAction::TreeDelete(t) => self.request_delete(t),
            CtxAction::OpenTerminalAt(dir) => {
                // Spawn a tab whose shell starts in `dir`, then restore the workspace
                // cwd for future tabs.
                let old = self.cwd.clone();
                self.terminal.set_cwd(dir);
                if !self.terminal.visible {
                    self.toggle_terminal();
                } else {
                    let panel = self.layout().terminal_panel;
                    self.terminal.new_terminal_tab(panel, self.terminal_cell_w);
                }
                self.terminal.set_cwd(old);
                self.redraw();
            }
            CtxAction::GitIgnore(rel) => {
                use std::io::Write as _;
                let gi = self.cwd.join(".gitignore");
                if let Ok(mut fh) = std::fs::OpenOptions::new().create(true).append(true).open(&gi) {
                    let _ = writeln!(fh, "{rel}");
                }
                self.refresh_source_control();
            }
            CtxAction::FileCut(p) => self.file_clipboard = Some((p, true)),
            CtxAction::FileCopy(p) => self.file_clipboard = Some((p, false)),
            CtxAction::FilePaste(dir) => self.paste_file_into(dir),
            CtxAction::SelectForCompare(p) => self.compare_select = Some(p),
            CtxAction::CompareWith(p) => {
                if let Some(a) = self.compare_select.clone() {
                    let d = diff::compute_files(&a, &p);
                    if let Some(gpu) = self.gpu.as_mut() {
                        self.workspace.open_diff(d, &mut gpu.font_system);
                    }
                }
            }
            CtxAction::OpenAtHead(rel) => self.open_file_at_head(rel),
            CtxAction::RevealInTree(p) => self.reveal_in_tree(p),
            CtxAction::TermRename(i) => self.open_term_rename(i),
            CtxAction::TermSplit(i) => {
                self.terminal.switch_tab(i);
                let panel = self.layout().terminal_panel;
                self.terminal.split_terminal(panel, self.terminal_cell_w);
                self.redraw();
            }
            CtxAction::TermKill(i) => {
                self.terminal.kill_tab(i);
                self.redraw();
            }
            CtxAction::TermNew => {
                let panel = self.layout().terminal_panel;
                self.terminal.new_terminal_tab(panel, self.terminal_cell_w);
                self.redraw();
            }
            CtxAction::MoveToNewWindow(i) => {
                let path = self.workspace.documents.get(i).and_then(|d| d.path.clone());
                if let (Some(path), Ok(exe)) = (path, std::env::current_exe()) {
                    let mut cmd = std::process::Command::new(exe);
                    cmd.arg(path);
                    #[cfg(unix)]
                    {
                        use std::os::unix::process::CommandExt;
                        cmd.process_group(0); // detach so it doesn't die with this window
                    }
                    if cmd.spawn().is_ok() {
                        self.workspace.close_idx(i);
                    }
                }
            }
        }
    }

    /// Paste the explorer clipboard entry into `dir` (Cut moves, Copy duplicates;
    /// directories copy recursively; name collisions get a " copy" suffix).
    fn paste_file_into(&mut self, dir: PathBuf) {
        let Some((src, is_cut)) = self.file_clipboard.clone() else { return };
        if !src.exists() || !dir.is_dir() {
            return;
        }
        // Refuse pasting a folder into itself/descendant — fs::rename would loop.
        if dir.starts_with(&src) {
            return self.show_info_dialog("Can't paste a folder into itself.");
        }
        let name = src.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
        let mut dest = dir.join(&name);
        if dest.exists() && dest != src {
            dest = unique_sibling(&dest);
        }
        let result = if is_cut {
            if dest == src {
                Ok(()) // cut + paste in place: nothing to do
            } else {
                std::fs::rename(&src, &dest)
            }
        } else {
            if dest == src {
                dest = unique_sibling(&dest); // copy beside the original
            }
            copy_recursive(&src, &dest)
        };
        match result {
            Ok(()) => {
                if is_cut {
                    self.file_clipboard = None;
                }
                self.workspace.tree.rebuild();
                self.invalidate_file_index();
                self.refresh_source_control();
            }
            Err(e) => self.show_info_dialog(&format!("Paste failed: {e}")),
        }
        self.redraw();
    }

    /// SCM "Open File (HEAD)": the committed version in a read-only tab.
    fn open_file_at_head(&mut self, rel: String) {
        let out = std::process::Command::new("git")
            .arg("-C")
            .arg(&self.cwd)
            .args(["show", &format!("HEAD:{rel}")])
            .output();
        let text = match out {
            Ok(o) if o.status.success() => String::from_utf8_lossy(&o.stdout).into_owned(),
            _ => return self.show_info_dialog("No committed version of this file (new file?)."),
        };
        let name = format!("{} (HEAD)", rel.rsplit('/').next().unwrap_or(&rel));
        if let Some(i) = self.workspace.documents.iter().position(|d| d.read_only && d.name == name) {
            self.workspace.active = Some(i);
        } else if let Some(gpu) = self.gpu.as_mut() {
            let mut d = Document::new(Some(self.cwd.join(&rel)), text, &mut gpu.font_system);
            d.name = name;
            d.read_only = true;
            d.path = None; // not the working file — don't let Save/LSP touch it
            self.workspace.documents.push(d);
            self.workspace.active = Some(self.workspace.documents.len() - 1);
        }
        self.redraw();
    }

    /// Tab "Reveal in Explorer View": show the explorer, expand the tree to the
    /// file, select it, and scroll it into view.
    fn reveal_in_tree(&mut self, path: PathBuf) {
        self.show_sidebar_view(SidebarView::Explorer);
        if let Some(parent) = path.parent() {
            self.workspace.tree.expand(parent);
        }
        self.workspace.tree.rebuild();
        self.invalidate_file_index();
        if let Some(idx) = self.workspace.tree.nodes.iter().position(|n| n.path == path) {
            self.selected_tree = Some(idx);
            let y = idx as f32 * theme::TREE_ROW_HEIGHT();
            self.explorer.scroll.scroll_to_y((y - theme::zpx(80.0)).max(0.0));
        }
        self.redraw();
    }

    // ---- Top menu-bar dropdowns (File / Edit / …) ----

    /// Open the dropdown for top-level menu `idx`, loading its entries into the
    /// shared dropdown widget. Closes any open file-explorer context menu.
    fn open_app_menu(&mut self, idx: usize) {
        // Toggle rows show a live checkmark for their current state.
        let labels: Vec<String> = menus::entries(idx)
            .iter()
            .map(|e| {
                let on = match e.cmd {
                    menus::MenuCmd::AutoSave => settings::auto_save(),
                    menus::MenuCmd::ZenMode => layout::zen(),
                    menus::MenuCmd::CenteredLayout => layout::centered(),
                    menus::MenuCmd::FullScreen => {
                        self.gpu.as_ref().map_or(false, |g| g.window.fullscreen().is_some())
                    }
                    _ => false,
                };
                if on { format!("✓ {}", e.label) } else { e.label.to_string() }
            })
            .collect();
        let rows: Vec<(&str, &str, bool)> = menus::entries(idx)
            .iter()
            .zip(&labels)
            .map(|(e, l)| (l.as_str(), e.hint, matches!(e.cmd, menus::MenuCmd::Separator)))
            .collect();
        if let Some(g) = self.gpu.as_mut() {
            g.ui.menu_dropdown.set_entries(&mut g.font_system, &rows);
        }
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
                    "Aether v{} · {} ({})",
                    update::current_version(),
                    std::env::consts::OS,
                    std::env::consts::ARCH
                ));
            }
            menus::MenuCmd::NewWindow => self.open_new_window(),
            menus::MenuCmd::OpenRecent => self.open_recent_picker(),
            menus::MenuCmd::AutoSave => {
                settings::set_auto_save(!settings::auto_save());
            }
            menus::MenuCmd::RevertFile => self.revert_file(),
            menus::MenuCmd::CloseFolder => self.close_folder(),
            menus::MenuCmd::ZenMode => self.toggle_zen(),
            menus::MenuCmd::CenteredLayout => {
                layout::set_centered(!layout::centered());
            }
            menus::MenuCmd::Problems => self.open_problems_picker(),
            menus::MenuCmd::OutputLog => self.open_output_tab(),
            menus::MenuCmd::Welcome => {
                let page = ui::info_page::welcome(&state::State::load().recent);
                self.open_info_tab(page);
            }
            menus::MenuCmd::ShortcutsRef => self.open_info_tab(ui::info_page::shortcuts()),
            menus::MenuCmd::Tips => self.open_info_tab(ui::info_page::tips()),
            menus::MenuCmd::RunActiveFile => self.run_active_file(),
            menus::MenuCmd::RunSelectedText => self.run_selected_text(),
            menus::MenuCmd::ReplaceInFiles => {
                self.show_sidebar_view(SidebarView::Search);
                if let Some(sp) = self.search.as_mut() {
                    sp.show_replace();
                }
            }
            menus::MenuCmd::QuickOpen => self.open_quick_open(),
            menus::MenuCmd::GotoSymbol => self.open_palette_with("@"),
            menus::MenuCmd::GotoWsSymbol => self.open_palette_with("#"),
            menus::MenuCmd::GotoLine => self.open_palette_with(":"),
            menus::MenuCmd::OpenFileDlg => {
                let start = self.workspace.tree.root.clone();
                if let Some(path) = rfd::FileDialog::new().set_directory(&start).pick_file() {
                    self.open_file_at(path, 1, 0);
                }
            }
            menus::MenuCmd::SaveAs => {
                let start = self.workspace.tree.root.clone();
                if let Some(path) = rfd::FileDialog::new().set_directory(&start).save_file() {
                    if let (Some(g), Some(d)) = (self.gpu.as_mut(), self.workspace.active_doc_mut()) {
                        d.set_path(path, &mut g.font_system);
                        let _ = d.save();
                    }
                    self.refresh_source_control();
                }
            }
            menus::MenuCmd::SaveAll => {
                for d in self.workspace.documents.iter_mut() {
                    if d.dirty && d.path.is_some() {
                        let _ = d.save();
                    }
                }
                self.refresh_source_control();
            }
            menus::MenuCmd::Cut => self.cut(),
            menus::MenuCmd::Copy => self.copy(),
            menus::MenuCmd::Paste => self.paste(),
            menus::MenuCmd::Replace => {
                self.find.active = true;
                self.find.focused = true;
                self.find.replace_open = true;
                self.redraw();
            }
            menus::MenuCmd::FindInFiles => {
                self.sidebar_visible = true;
                self.sidebar_view = SidebarView::Search;
                self.redraw();
            }
            menus::MenuCmd::ShowExplorer => self.show_sidebar_view(SidebarView::Explorer),
            menus::MenuCmd::ShowSearch => self.show_sidebar_view(SidebarView::Search),
            menus::MenuCmd::ShowScm => self.show_sidebar_view(SidebarView::SourceControl),
            menus::MenuCmd::ShowExtensions => self.show_sidebar_view(SidebarView::Extensions),
            menus::MenuCmd::FullScreen => {
                if let Some(g) = self.gpu.as_ref() {
                    let on = g.window.fullscreen().is_some();
                    g.window.set_fullscreen(if on {
                        None
                    } else {
                        Some(winit::window::Fullscreen::Borderless(None))
                    });
                }
            }
            menus::MenuCmd::ZoomIn => self.zoom_step(0.1),
            menus::MenuCmd::ZoomOut => self.zoom_step(-0.1),
            menus::MenuCmd::ZoomReset => self.set_zoom(1.0),
            menus::MenuCmd::NewTerminal => {
                if !self.terminal.visible {
                    self.toggle_terminal(); // spawns the first tab when none exist
                } else {
                    let panel = self.layout().terminal_panel;
                    self.terminal.new_terminal_tab(panel, self.terminal_cell_w);
                }
                self.redraw();
            }
            menus::MenuCmd::SplitTerminal => {
                if !self.terminal.visible {
                    self.toggle_terminal();
                } else {
                    let panel = self.layout().terminal_panel;
                    self.terminal.split_terminal(panel, self.terminal_cell_w);
                }
                self.redraw();
            }
            menus::MenuCmd::KillTerminal => {
                self.terminal.kill_terminal();
                self.redraw();
            }
            menus::MenuCmd::OpenDocs => open_url("https://github.com/actuallyroy/aether-editor#readme"),
            menus::MenuCmd::OpenReleases => open_url("https://github.com/actuallyroy/aether-editor/releases"),
            menus::MenuCmd::Stub(name) => {
                self.show_info_dialog(&format!("“{name}” isn’t implemented yet — it's on the roadmap."));
            }
            menus::MenuCmd::Separator => {} // not clickable; here for exhaustiveness
            menus::MenuCmd::Exit => {
                if self.confirm_close_window() {
                    self.pending_close = true;
                }
            }
        }
    }

    /// Show (and switch to) a sidebar view from the View menu, mirroring the
    /// activity-bar behavior (SCM refreshes; Extensions scans on first open).
    fn show_sidebar_view(&mut self, view: SidebarView) {
        if view == SidebarView::Extensions {
            if self.extensions.is_empty() {
                self.extensions = extensions::scan();
            }
            self.rebuild_ext_rows();
        }
        if view == SidebarView::SourceControl {
            if let (Some(scp), Some(g)) = (self.source_control.as_mut(), self.gpu.as_mut()) {
                scp.refresh(&mut g.font_system);
            }
        }
        self.sidebar_view = view;
        self.sidebar_visible = true;
        self.redraw();
    }

    /// Launch another app window: a fresh, folder-less `aether` instance (like
    /// VSCode's File → New Window). The user opens a folder from there; if they pick
    /// one that's already open in a live window, that window is focused instead.
    fn open_new_window(&mut self) {
        if let Ok(exe) = std::env::current_exe() {
            let mut cmd = std::process::Command::new(exe);
            cmd.arg("--new-window");
            #[cfg(unix)]
            {
                use std::os::unix::process::CommandExt;
                cmd.process_group(0); // detach so it doesn't die with this process
            }
            let _ = cmd.spawn();
        }
    }

    /// Activate already-installed extensions' declarative contributions on startup
    /// (and after a scan): register each grammar extension's TextMate grammar so it
    /// works without re-clicking Install every session (e.g. rainbow-csv colors CSV).
    fn activate_installed_grammars(&mut self) {
        for e in &self.extensions {
            if e.kind == ExtKind::Grammar {
                for gp in &e.grammar_paths {
                    if let Some(g) = textmate::Grammar::load(gp) {
                        textmate::register(g, &[]);
                    }
                }
            }
        }
    }

    /// "Install" a supported extension into Aether. For color themes this loads and
    /// applies the theme immediately; other supported kinds just mark installed
    /// (their declarative contributions aren't loaded yet).
    fn install_extension(&mut self, i: usize) {
        let (kind, themes, grammar_paths) = match self.extensions.get(i) {
            Some(e) => (e.kind, e.themes.clone(), e.grammar_paths.clone()),
            None => return,
        };
        match kind {
            ExtKind::Theme => {
                // Apply the extension's first theme as a preview; the picker (Set Color
                // Theme) lets the user choose among all of them and persists the choice.
                if let Some(first) = themes.first() {
                    if let Some(t) = theme::load_vscode(&first.path) {
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


    /// Fetch inline git blame for `path` off the UI thread; the result posts back
    /// as `WorkerMsg::Blame` and attaches to the matching open document.
    fn request_blame(&self, path: PathBuf) {
        let root = git::repo_root(&self.cwd);
        let tx = self.worker_tx.clone();
        std::thread::spawn(move || {
            let lines = git::blame(&root, &path);
            if !lines.is_empty() {
                let _ = tx.send(WorkerMsg::Blame { path, lines });
            }
        });
    }

    /// The full-commit hover card for the inline blame annotation: if `(x, y)` is
    /// over the caret line's annotation (committed lines only), returns the card
    /// text + anchor. Geometry mirrors the draw in `render.rs`.
    fn blame_card_at(&self, x: f32, y: f32) -> Option<(String, f32, f32)> {
        let d = self.workspace.active_doc()?;
        if d.read_only
            || d.diff.is_some()
            || d.image.is_some()
            || d.graph.is_some()
            || d.info.is_some()
            || d.markdown_preview.is_some()
        {
            return None;
        }
        let cur = d.head_line_col().0;
        let bl = d.blame.get(cur)?;
        if bl.uncommitted {
            return None;
        }
        let layout = self.layout();
        let et = layout.editor_text;
        let line_w = d.buffer.layout_runs().find(|r| r.line_i == cur).map(|r| r.line_w).unwrap_or(0.0);
        let (ltop, lh) = d.line_visual_bounds(cur);
        let off = theme::LINE_HEIGHT() * d.hidden_above(cur) as f32;
        let line_y = et.y + theme::EDITOR_PAD() + ltop - d.scroll_y() - off;
        let bx = et.x + theme::EDITOR_PAD() - d.scroll_x() + line_w + theme::zpx(28.0);
        let bw = self.gpu.as_ref().map_or(0.0, |g| g.ui.blame.width());
        let rect = widgets::Rect { x: bx, y: line_y, w: bw, h: lh };
        if !rect.contains((x, y)) {
            return None;
        }
        Some((render::blame_card_text(bl, render::now_unix_secs()), bx, line_y + lh))
    }

    /// Kick off a blame fetch for the active document if it's a plain tracked file
    /// and one hasn't been requested for the current content yet. Called every tick
    /// so it covers opens, tab switches, and session restore uniformly.
    fn maybe_request_blame(&mut self) {
        let path = match self.workspace.active_doc() {
            Some(d)
                if !d.blame_requested
                    && d.path.is_some()
                    && !d.read_only
                    && d.diff.is_none()
                    && d.image.is_none()
                    && d.graph.is_none()
                    && d.info.is_none()
                    && d.markdown_preview.is_none() =>
            {
                d.path.clone()
            }
            _ => return,
        };
        if let Some(d) = self.workspace.active_doc_mut() {
            d.blame_requested = true;
        }
        if let Some(p) = path {
            self.request_blame(p);
        }
    }

    /// Start (or restart) the workspace filesystem watcher. Debounced fs events
    /// post `WorkerMsg::FsChanged`; the UI flushes them into a Source Control
    /// refresh + external-edit reload of open documents.
    fn start_fs_watcher(&mut self) {
        use notify::{RecursiveMode, Watcher};
        let tx = self.worker_tx.clone();
        let mut watcher = match notify::recommended_watcher(move |res: notify::Result<notify::Event>| {
            if let Ok(ev) = res {
                if !ev.paths.is_empty() {
                    let _ = tx.send(WorkerMsg::FsChanged { paths: ev.paths });
                }
            }
        }) {
            Ok(w) => w,
            Err(_) => return,
        };
        let root = git::repo_root(&self.cwd);
        if watcher.watch(&root, RecursiveMode::Recursive).is_ok() {
            self.fs_watcher = Some(watcher);
        }
    }

    /// Flush a debounced batch of filesystem changes: refresh Source Control, and
    /// reload any open document whose file changed on disk and has no unsaved edits
    /// (so an external editor / `claude code` / another window is reflected live).
    fn flush_fs_changes(&mut self) {
        let paths = std::mem::take(&mut self.fs_dirty);
        if paths.is_empty() {
            return;
        }
        // Reload externally-changed, non-dirty docs from disk (skip dirty ones so we
        // never clobber unsaved local edits).
        let mut reloaded = false;
        if let Some(g) = self.gpu.as_mut() {
            for d in self.workspace.documents.iter_mut() {
                let Some(path) = d.path.clone() else { continue };
                if d.dirty || !paths.contains(&path) {
                    continue;
                }
                if let Ok(text) = std::fs::read_to_string(&path) {
                    if text != d.text() {
                        d.set_text_external(&text, &mut g.font_system);
                        d.blame_requested = false; // re-blame the new content
                        reloaded = true;
                    }
                }
            }
        }
        // Any change (working tree or .git) can move the git status.
        self.refresh_source_control();
        if reloaded {
            self.redraw();
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
                    // Record the source so per-block Stage/Unstage/Revert can patch it
                    // (untracked files have no HEAD side to stage hunks against).
                    if !untracked {
                        if let Some(doc) = self.workspace.active_doc_mut() {
                            doc.diff_path = Some(path.clone());
                            doc.diff_staged = staged;
                        }
                    }
                }
                self.ensure_cursor_visible();
                self.redraw();
            }
            ui::Intent::OpenAllDiffs { staged } => {
                // Collect this group's changed files, then open one combined diff tab.
                let entries: Vec<(String, bool)> = git::status(&self.cwd)
                    .into_iter()
                    .filter_map(|c| {
                        let included = if staged { c.staged != ' ' && c.staged != '?' } else { c.worktree != ' ' };
                        included.then(|| (c.path, !staged && c.worktree == '?'))
                    })
                    .collect();
                if !entries.is_empty() {
                    if let Some(g) = self.gpu.as_mut() {
                        let d = diff::compute_all(&self.cwd, &entries, staged);
                        self.workspace.open_diff(d, &mut g.font_system);
                        self.detail.open_extension = None;
                    }
                    self.ensure_cursor_visible();
                    self.redraw();
                }
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
                self.confirm_discard(path, untracked);
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
                self.confirm_discard_all();
            }
            ui::Intent::GitStash => {
                self.confirm_stash();
            }
            ui::Intent::GitGenerateCommitMessage => {
                // Summarize the staged (or, if nothing staged, working-tree) diff
                // into a commit message via Azure OpenAI, off the UI thread.
                let diff = git::commit_diff(&self.cwd).unwrap_or_default();
                if diff.trim().is_empty() {
                    self.show_info_dialog("No changes to summarize — stage or edit some files first.");
                } else if let Some(scp) = self.source_control.as_mut() {
                    scp.begin_generating();
                    ai::generate_commit_async(diff, self.worker_tx.clone());
                }
                self.redraw();
            }
            ui::Intent::GitCommitGraph => {
                let now = std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .map(|d| d.as_secs() as i64)
                    .unwrap_or(0);
                let entries = git::commit_log(&self.cwd, 2000);
                let g = graph::build("Commit Graph".to_string(), entries, now);
                if let Some(i) = self.workspace.documents.iter().position(|d| d.graph.is_some()) {
                    if let Some(gpu) = self.gpu.as_mut() {
                        self.workspace.documents[i] = Document::new_graph(g, &mut gpu.font_system);
                        self.workspace.active = Some(i);
                    }
                } else if let Some(gpu) = self.gpu.as_mut() {
                    let d = Document::new_graph(g, &mut gpu.font_system);
                    self.workspace.documents.push(d);
                    self.workspace.active = Some(self.workspace.documents.len() - 1);
                }
                self.redraw();
            }
            ui::Intent::OpenCommitDiff { hash, path } => {
                if let Some(g) = self.gpu.as_mut() {
                    let d = diff::compute_commit(&self.cwd, &path, &hash);
                    self.workspace.open_diff(d, &mut g.font_system);
                    self.detail.open_extension = None;
                }
                self.ensure_cursor_visible();
                self.redraw();
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
            ui::Intent::OpenCommitMenu { anchor, msg, stage_all } => {
                let items = vec![
                    CtxEntry::new("Commit", CtxAction::ScmIntent(ui::Intent::GitCommit { msg: msg.clone(), stage_all })),
                    CtxEntry::new("Commit & Push", CtxAction::ScmIntent(ui::Intent::GitCommitPush { msg, stage_all })),
                ];
                self.open_ctx_menu(anchor, items);
            }
            ui::Intent::OpenMoreMenu { anchor, tree_mode } => {
                let view_label = if tree_mode { "View as List" } else { "View as Tree" };
                let items = vec![
                    CtxEntry::new(view_label, CtxAction::ScmIntent(ui::Intent::GitToggleView)),
                    CtxEntry::sep(),
                    CtxEntry::new("Checkout to…", CtxAction::ScmIntent(ui::Intent::GitOpenCheckout)),
                    CtxEntry::new("Create Branch…", CtxAction::ScmIntent(ui::Intent::GitOpenCreateBranch)),
                    CtxEntry::new("Rename Branch…", CtxAction::ScmIntent(ui::Intent::GitOpenRenameBranch)),
                    CtxEntry::new("Delete Branch…", CtxAction::ScmIntent(ui::Intent::GitOpenDeleteBranch)),
                    CtxEntry::sep(),
                    CtxEntry::new("Pull", CtxAction::ScmIntent(ui::Intent::GitPull)),
                    CtxEntry::new("Push", CtxAction::ScmIntent(ui::Intent::GitPush)),
                    CtxEntry::new("Fetch", CtxAction::ScmIntent(ui::Intent::GitFetch)),
                    CtxEntry::sep(),
                    CtxEntry::new("Stage All Changes", CtxAction::ScmIntent(ui::Intent::GitStageAll)),
                    CtxEntry::new("Unstage All Changes", CtxAction::ScmIntent(ui::Intent::GitUnstageAll)),
                    CtxEntry::new("Discard All Changes", CtxAction::ScmIntent(ui::Intent::GitDiscardAll)),
                    CtxEntry::sep(),
                    CtxEntry::new("Stash Changes", CtxAction::ScmIntent(ui::Intent::GitStash)),
                    CtxEntry::new("Pop Latest Stash", CtxAction::ScmIntent(ui::Intent::GitStashPop)),
                    CtxEntry::new("Apply Latest Stash", CtxAction::ScmIntent(ui::Intent::GitStashApply)),
                    CtxEntry::sep(),
                    CtxEntry::new("View Commit Graph", CtxAction::ScmIntent(ui::Intent::GitCommitGraph)),
                    CtxEntry::new("Refresh", CtxAction::ScmIntent(ui::Intent::GitRefresh)),
                ];
                self.open_ctx_menu(anchor, items);
            }
            ui::Intent::GitPush => {
                git::push(&self.cwd);
                self.refresh_source_control();
            }
            ui::Intent::GitPull => {
                git::pull(&self.cwd);
                self.refresh_source_control();
                self.apply_intent(ui::Intent::ReloadOpenDocs);
            }
            ui::Intent::GitFetch => {
                git::fetch(&self.cwd);
                self.refresh_source_control();
            }
            ui::Intent::GitToggleView => {
                if let Some(scp) = self.source_control.as_mut() {
                    scp.toggle_view();
                    // Persist the tree/list choice so it survives a restart.
                    let mut st = state::State::load();
                    st.scm_tree_view = scp.tree_mode();
                    st.save();
                }
                self.redraw();
            }
            ui::Intent::GitStashPop => {
                git::stash_pop(&self.cwd);
                self.refresh_source_control();
                self.apply_intent(ui::Intent::ReloadOpenDocs);
            }
            ui::Intent::GitStashApply => {
                git::stash_apply(&self.cwd);
                self.refresh_source_control();
                self.apply_intent(ui::Intent::ReloadOpenDocs);
            }
            ui::Intent::GitOpenCheckout => self.open_branch_pick(commands::PickKind::Checkout),
            ui::Intent::GitOpenDeleteBranch => self.open_branch_pick(commands::PickKind::DeleteBranch),
            ui::Intent::GitOpenCreateBranch => {
                self.open_branch_input(commands::PickKind::CreateBranch, "", "Type a name, Enter to create the branch, Esc to cancel");
            }
            ui::Intent::GitOpenRenameBranch => {
                let cur = git::branch(&self.cwd).unwrap_or_default();
                self.open_branch_input(commands::PickKind::RenameBranch, &cur, "Edit the branch name, Enter to rename, Esc to cancel");
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
            ui::Intent::OpenSearchEditor { text } => {
                if let Some(gpu) = self.gpu.as_mut() {
                    let d = Document::new(None, text, &mut gpu.font_system);
                    self.workspace.documents.push(d);
                    self.workspace.active = Some(self.workspace.documents.len() - 1);
                    self.terminal.maximized = false;
                }
            }
            ui::Intent::DebugStart { config_idx } => self.debug_start(config_idx),
            ui::Intent::DebugStop => self.debug_stop(),
            ui::Intent::DebugContinue => {
                if let (Some(d), Some(t)) = (self.dap.as_mut(), self.debug_thread) {
                    d.continue_(t);
                    self.clear_execution_lines();
                    self.redraw();
                }
            }
            ui::Intent::DebugStepOver => {
                if let (Some(d), Some(t)) = (self.dap.as_mut(), self.debug_thread) { d.next(t); }
            }
            ui::Intent::DebugStepIn => {
                if let (Some(d), Some(t)) = (self.dap.as_mut(), self.debug_thread) { d.step_in(t); }
            }
            ui::Intent::DebugStepOut => {
                if let (Some(d), Some(t)) = (self.dap.as_mut(), self.debug_thread) { d.step_out(t); }
            }
            ui::Intent::DebugSelectConfig(i) => {
                if let Some(p) = self.debug.as_mut() { p.selected = i; }
                self.redraw();
            }
            ui::Intent::DebugSelectFrame(_id) => { /* Phase 2: navigate to a non-top frame */ }
            ui::Intent::DebugToggleBreakpoint { path, line } => self.debug_toggle_breakpoint(path, line),
            ui::Intent::DebugExpandVar(var_ref) => {
                if let Some(d) = self.dap.as_mut() {
                    d.variables(var_ref);
                }
            }
            ui::Intent::DebugAttachProcess => self.open_attach_picker(),
            ui::Intent::DebugPause => {
                // Suspend the running process. We don't have a thread id yet, so ask
                // for the thread list and pause the first one when it arrives.
                if let Some(d) = self.dap.as_mut() {
                    self.debug_pending_pause = true;
                    d.threads();
                }
            }
        }
    }

    /// List running debuggable (Python) processes and open a quick-pick to attach.
    fn open_attach_picker(&mut self) {
        let procs = list_debuggable_processes();
        if procs.is_empty() {
            self.show_info_dialog("No attachable Python processes found.\n\nStart one with debugpy, e.g.:\n  python -m debugpy --listen 5678 your_app.py");
            return;
        }
        let items: Vec<commands::PickItem> = procs
            .into_iter()
            .map(|(pid, cmd)| commands::PickItem::at_line(cmd, format!("pid {pid}"), pid as usize))
            .collect();
        self.palette.open_quick_pick(commands::PickKind::AttachProcess, items);
        if let Some(g) = self.gpu.as_mut() {
            g.ui.palette_input.set_text(&mut g.font_system, "");
            g.ui.palette_input.focus(true);
        }
        self.redraw();
    }

    /// Attach the debugger to a running process by PID (debugpy injection).
    fn debug_attach_pid(&mut self, pid: i64) {
        let adapter = debug_config::python_adapter();
        let client = dap::DapClient::start(&adapter.program, &adapter.args, &self.cwd, self.worker_tx.clone());
        match client {
            Some(mut c) => {
                c.initialize();
                self.dap = Some(c);
                self.debug_handshook = false;
                self.debug_console.clear();
                self.open_debug_console();
                self.debug_config = Some(debug_config::LaunchConfig {
                    name: format!("Attach to pid {pid}"),
                    request: debug_config::Request::Attach,
                    adapter,
                    args: serde_json::json!({ "processId": pid }),
                });
                if let (Some(p), Some(g)) = (self.debug.as_mut(), self.gpu.as_mut()) {
                    p.set_session(&mut g.font_system, ui::debug_panel::Session::Running);
                }
                self.redraw();
            }
            None => self.show_info_dialog(adapter.install_hint),
        }
    }

    /// Reload launch configs for the workspace + active file into the panel.
    fn refresh_debug_configs(&mut self) {
        let active = self.workspace.active_doc().and_then(|d| d.path.clone());
        let configs = debug_config::load(&self.cwd, active.as_deref());
        if let (Some(p), Some(g)) = (self.debug.as_mut(), self.gpu.as_mut()) {
            p.set_configs(&mut g.font_system, configs);
        }
        self.redraw();
    }

    /// All (abs-path, sorted 1-based lines) for files that currently have breakpoints.
    fn debug_breakpoint_map(&self) -> Vec<(String, Vec<i64>)> {
        self.workspace
            .documents
            .iter()
            .filter_map(|d| {
                let path = d.path.as_ref()?;
                if d.breakpoints.is_empty() {
                    return None;
                }
                let mut lines: Vec<i64> = d.breakpoints.iter().map(|&l| l as i64 + 1).collect();
                lines.sort_unstable();
                Some((path.to_string_lossy().into_owned(), lines))
            })
            .collect()
    }

    fn clear_execution_lines(&mut self) {
        for d in self.workspace.documents.iter_mut() {
            d.execution_line = None;
        }
    }

    /// Append debug-adapter / debuggee output to the Debug Console buffer (the bottom
    /// panel's DEBUG CONSOLE tab renders it live from here).
    fn run_in_terminal_output(&mut self, text: &str) {
        for line in text.split_inclusive('\n') {
            let line = line.trim_end_matches('\n');
            if !line.is_empty() {
                self.debug_console.push_back(line.to_string());
            }
        }
        while self.debug_console.len() > 5000 {
            self.debug_console.pop_front();
        }
        self.redraw();
    }

    /// Show the bottom panel with the DEBUG CONSOLE tab active.
    fn open_debug_console(&mut self) {
        self.terminal.visible = true;
        self.terminal.maximized = false;
        self.panel_tab = theme::PANEL_DEBUG_CONSOLE_TAB;
        self.redraw();
    }

    fn debug_start(&mut self, config_idx: usize) {
        self.refresh_debug_configs();
        let cfg = self.debug.as_ref().and_then(|p| p.configs.get(config_idx).cloned());
        let Some(cfg) = cfg else {
            self.show_info_dialog("No debug configuration available. Open a file or add a .vscode/launch.json.");
            return;
        };
        let client = dap::DapClient::start(&cfg.adapter.program, &cfg.adapter.args, &self.cwd, self.worker_tx.clone());
        match client {
            Some(mut c) => {
                c.initialize();
                self.dap = Some(c);
                self.debug_config = Some(cfg);
                self.debug_handshook = false;
                self.debug_console.clear();
                if let (Some(p), Some(g)) = (self.debug.as_mut(), self.gpu.as_mut()) {
                    p.set_session(&mut g.font_system, ui::debug_panel::Session::Running);
                }
                self.open_debug_console();
                self.redraw();
            }
            None => {
                self.show_info_dialog(cfg.adapter.install_hint);
            }
        }
    }

    fn debug_stop(&mut self) {
        if let Some(d) = self.dap.as_mut() {
            d.disconnect();
        }
        self.dap = None;
        self.debug_thread = None;
        self.debug_config = None;
        self.clear_execution_lines();
        if let (Some(p), Some(g)) = (self.debug.as_mut(), self.gpu.as_mut()) {
            p.set_session(&mut g.font_system, ui::debug_panel::Session::Idle);
        }
        self.redraw();
    }

    fn debug_toggle_breakpoint(&mut self, path: PathBuf, line: usize) {
        // Toggle on the matching open document.
        let mut abs: Option<String> = None;
        for d in self.workspace.documents.iter_mut() {
            if d.path.as_deref() == Some(path.as_path()) {
                d.toggle_breakpoint(line);
                abs = Some(path.to_string_lossy().into_owned());
                break;
            }
        }
        // If a session is live, resend this file's breakpoints.
        if let (Some(abs), Some(d)) = (abs, self.dap.as_mut()) {
            let lines: Vec<i64> = self
                .workspace
                .documents
                .iter()
                .find(|doc| doc.path.as_deref() == Some(path.as_path()))
                .map(|doc| {
                    let mut v: Vec<i64> = doc.breakpoints.iter().map(|&l| l as i64 + 1).collect();
                    v.sort_unstable();
                    v
                })
                .unwrap_or_default();
            d.set_breakpoints(&abs, &lines);
        }
        crate::state::save_breakpoints(&self.debug_breakpoint_map());
        self.redraw();
    }

    /// Open `path` and place the caret at (1-based `line`, byte `col`).
    /// Resolve a literal path the user typed/pasted into the quick-open box: `~`
    /// expands to $HOME, relative paths join the workspace root, and the result
    /// must exist on disk. Returns None for fuzzy queries (no path separators).
    fn resolve_literal_path(&self, raw: &str) -> Option<PathBuf> {
        let raw = raw.trim();
        if raw.is_empty() {
            return None;
        }
        let expanded: PathBuf = if let Some(rest) = raw.strip_prefix("~/") {
            std::env::var_os("HOME").map(PathBuf::from)?.join(rest)
        } else if raw == "~" {
            std::env::var_os("HOME").map(PathBuf::from)?
        } else {
            PathBuf::from(raw)
        };
        let abs = if expanded.is_absolute() { expanded } else { self.cwd.join(expanded) };
        abs.exists().then_some(abs)
    }

    fn open_file_at(&mut self, path: PathBuf, line: usize, col: usize) {
        // Opening a file reveals the editor: if the terminal is maximized (filling
        // the editor area), hide the whole bottom panel so the file content shows.
        // The shells keep running — this only collapses the panel, like VSCode's
        // toggle; reopen it with the terminal toggle and the sessions are intact.
        if self.terminal.maximized {
            self.terminal.maximized = false;
            self.terminal.visible = false;
        }
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
        // Inline blame is fetched lazily for whichever doc is active (covers opens,
        // tab switches, and session restore uniformly — see `maybe_request_blame`).
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
        const REPO: &str = "actuallyroy/aether-editor";
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
            self.show_info_dialog("Couldn't locate the extensions folder (~/.aether/extensions).");
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

    /// Uninstall whatever the detail page currently shows: delete it from Aether's
    /// store, rescan, refresh the panel, and re-open the detail so it flips to
    /// "Install". A running language server keeps going until the next launch.
    fn uninstall_open(&mut self) {
        let slug = match self.detail.open_extension {
            Some(OpenExt::Local(i)) => self.extensions.get(i).map(|e| e.slug.clone()),
            Some(OpenExt::Remote(i)) => self.ext_remote.get(i).map(|e| e.id().to_lowercase()),
            None => None,
        };
        let Some(slug) = slug else { return };
        if let Err(e) = extensions::uninstall(&slug) {
            self.show_info_dialog(&format!("Couldn't uninstall: {e}"));
            return;
        }
        self.extensions = extensions::scan();
        // Stop any language server that came from the removed extension and clear its
        // diagnostics, so uninstalling takes effect without a restart.
        if let Some(ext_dir) = extensions::extensions_dir() {
            self.lsp.reconcile(&mut self.workspace.documents, &[ext_dir]);
        }
        if let Some(g) = self.gpu.as_mut() {
            for d in self.workspace.documents.iter_mut() {
                d.reshape(&mut g.font_system);
            }
        }
        // Close the detail page (the Local index it held may now be stale) and return
        // to the refreshed list, where the extension is gone.
        self.detail.open_extension = None;
        self.rebuild_ext_rows();
        self.show_info_dialog("Extension uninstalled.");
        self.redraw();
    }

    /// The currently focused element (single source of truth for key routing).
    /// Precedence matches modal nesting: inline rename > palette > find > the
    /// extensions filter > the editor.
    fn focus(&self) -> Focus {
        if self.explorer.creating.is_some() {
            Focus::Rename
        } else if self.palette.active {
            Focus::Palette
        } else if self.find.active && self.find.focused {
            Focus::Find
        } else if self.terminal.visible && self.terminal.focused && !self.terminal.groups.is_empty() {
            Focus::Terminal
        } else if self.extensions_panel.as_ref().map_or(false, |ep| ep.focused()) {
            Focus::ExtFilter
        } else if self.search.as_ref().map_or(false, |sp| sp.focused()) {
            Focus::Search
        } else if self.source_control.as_ref().map_or(false, |s| s.focused()) {
            Focus::SourceControl
        } else if self.right_sidebar_visible && self.chat.as_ref().map_or(false, |c| c.focused()) {
            Focus::Chat
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
        let same = now.duration_since(self.last_click) < Duration::from_millis(400)
            && (x - self.last_click_pos.0).abs() < 4.0
            && (y - self.last_click_pos.1).abs() < 4.0;
        self.click_streak = if same { self.click_streak + 1 } else { 1 };
        self.last_click = now;
        self.last_click_pos = (x, y);
        self.click_streak >= 2
    }

    /// Like `register_click`, but returns the full consecutive-click count (1, 2,
    /// 3, …) so callers can distinguish double (word) from triple (line/all).
    fn register_click_count(&mut self, x: f32, y: f32) -> u32 {
        self.register_click(x, y);
        self.click_streak as u32
    }

    /// (rect, left-pad) of a given input, if it's currently shown.
    fn input_rect_for(&self, id: InputId, layout: &Layout) -> Option<(Rect, f32)> {
        match id {
            InputId::Palette => layout.palette.as_ref().map(|p| (p.input, theme::zpx(14.0))),
        }
    }

    /// The focused input under point `p` (for click-to-position / drag-select).
    fn focused_input_at(&self, layout: &Layout, p: (f32, f32)) -> Option<(InputId, Rect, f32)> {
        for id in [InputId::Palette] {
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
                self.invalidate_file_index();
            }
        }
        self.redraw();
    }

    /// Confirm before discarding a single file's working-tree changes — this is
    /// irreversible (git checkout / delete of an untracked file), so always ask.
    fn confirm_discard(&mut self, path: String, untracked: bool) {
        let name = std::path::Path::new(&path)
            .file_name()
            .map(|f| f.to_string_lossy().into_owned())
            .unwrap_or_else(|| path.clone());
        let msg = if untracked {
            format!("Are you sure you want to DELETE '{}'? This is irreversible.", name)
        } else {
            format!(
                "Are you sure you want to discard changes in '{}'? This is irreversible.",
                name
            )
        };
        if let Some(g) = self.gpu.as_mut() {
            g.ui
                .dialog
                .set(&mut g.font_system, &msg, &["Discard Changes", "Cancel"], None);
        }
        self.dialog = Some(DialogState {
            action: DialogAction::GitDiscard { path, untracked },
            has_check: false,
            checked: false,
            hovered: None,
        });
        self.redraw();
    }

    /// Confirm before discarding ALL working-tree changes — irreversible.
    fn confirm_discard_all(&mut self) {
        let msg = "Are you sure you want to discard ALL changes? This is irreversible and cannot be undone.";
        if let Some(g) = self.gpu.as_mut() {
            g.ui
                .dialog
                .set(&mut g.font_system, msg, &["Discard All Changes", "Cancel"], None);
        }
        self.dialog = Some(DialogState {
            action: DialogAction::GitDiscardAll,
            has_check: false,
            checked: false,
            hovered: None,
        });
        self.redraw();
    }

    /// Apply a per-block diff action. `staged` diffs offer Unstage (idx 0); working-
    /// tree diffs offer Revert (idx 0, destructive → confirm) and Stage (idx 1).
    fn apply_diff_block(&mut self, vbs: usize, staged: bool, idx: usize) {
        let Some(patch) = self.workspace.active_doc().and_then(|d| d.diff_block_patch(vbs)) else {
            return;
        };
        if staged {
            git::apply_patch(&self.cwd, &patch, true, true); // unstage
            self.after_block_action();
        } else if idx == 1 {
            git::apply_patch(&self.cwd, &patch, true, false); // stage
            self.after_block_action();
        } else {
            // Revert discards working-tree changes — confirm first.
            let msg = "Revert this change block? Your edits in this block will be discarded from the working tree (not recoverable).";
            if let Some(g) = self.gpu.as_mut() {
                g.ui.dialog.set(&mut g.font_system, msg, &["Revert Block", "Cancel"], None);
            }
            self.dialog = Some(DialogState {
                action: DialogAction::RevertDiffBlock { patch },
                has_check: false,
                checked: false,
                hovered: None,
            });
            self.redraw();
        }
    }

    /// After staging/unstaging/reverting a block: refresh SCM and rebuild the diff
    /// in place so the view reflects the new state.
    fn after_block_action(&mut self) {
        self.refresh_source_control();
        let Some((path, staged)) = self
            .workspace
            .active_doc()
            .and_then(|d| d.diff_path.clone().map(|p| (p, d.diff_staged)))
        else {
            self.redraw();
            return;
        };
        let nd = diff::compute(&self.cwd, &path, staged, false);
        if let Some(g) = self.gpu.as_mut() {
            let mut doc = Document::new_diff(nd, &mut g.font_system);
            doc.diff_path = Some(path);
            doc.diff_staged = staged;
            if let Some(slot) = self.workspace.active_doc_mut() {
                *slot = doc;
            }
        }
        self.hovered_diff_block = None;
        self.redraw();
    }

    fn confirm_stash(&mut self) {
        let msg = "Stash all changes? This moves your uncommitted changes onto the stash (recoverable with `git stash pop`).";
        if let Some(g) = self.gpu.as_mut() {
            g.ui
                .dialog
                .set(&mut g.font_system, msg, &["Stash Changes", "Cancel"], None);
        }
        self.dialog = Some(DialogState {
            action: DialogAction::GitStash,
            has_check: false,
            checked: false,
            hovered: None,
        });
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

    /// Gate window close on busy terminals: true ⇒ nothing running, close freely.
    /// Otherwise shows the Close Processes / Keep Running / Cancel dialog and the
    /// close is deferred to the user's choice.
    fn confirm_close_window(&mut self) -> bool {
        let busy = self.terminal.busy_terminal_count();
        if busy == 0 {
            return true;
        }
        let msg = if busy == 1 {
            "A terminal has a running process. Close it, or keep it running in the background?".to_string()
        } else {
            format!("{busy} terminals have running processes. Close them, or keep them running in the background?")
        };
        if let Some(g) = self.gpu.as_mut() {
            g.ui
                .dialog
                .set(&mut g.font_system, &msg, &["Close Processes", "Keep Running", "Cancel"], None);
        }
        self.dialog = Some(DialogState {
            action: DialogAction::CloseWindowBusy,
            has_check: false,
            checked: false,
            hovered: None,
        });
        self.redraw();
        false
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
            DialogAction::GitDiscard { path, untracked } => {
                // 0 = Discard Changes, 1 = Cancel
                if i == 0 {
                    git::discard(&self.cwd, &path, untracked);
                    self.refresh_source_control();
                }
            }
            DialogAction::RevertDiffBlock { patch } => {
                // 0 = Revert Block, 1 = Cancel
                if i == 0 {
                    git::apply_patch(&self.cwd, &patch, false, true); // reverse-apply to worktree
                    self.after_block_action();
                }
            }
            DialogAction::GitDiscardAll => {
                // 0 = Discard All Changes, 1 = Cancel
                if i == 0 {
                    git::discard_all(&self.cwd);
                    self.refresh_source_control();
                }
            }
            DialogAction::GitStash => {
                // 0 = Stash Changes, 1 = Cancel
                if i == 0 {
                    git::stash(&self.cwd);
                    self.refresh_source_control();
                }
            }
            DialogAction::InstallUpdate => {
                // 0 = Install & Restart, 1 = Later
                if i == 0 {
                    update::install_async(self.worker_tx.clone());
                    self.show_info_dialog("Downloading update… the app will restart when it's ready.");
                }
            }
            DialogAction::CloseWindowBusy => {
                // 0 = Close Processes, 1 = Keep Running, 2 = Cancel
                match i {
                    0 => {
                        self.terminal.close_all_terminals(); // kill the shells
                        self.pending_close = true;
                    }
                    // Just disconnect: the daemon orphans them, still running, and
                    // reopening this folder reclaims them.
                    1 => self.pending_close = true,
                    _ => {}
                }
            }
            DialogAction::Dismiss => {}
        }
        self.redraw();
    }

    /// Prompt to install an available update.
    fn show_update_prompt(&mut self, version: &str) {
        let msg = format!(
            "Aether v{version} is available (you have v{}). Install and restart?",
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
    /// Open the command palette (Ctrl+Shift+P) — prefilled with `>` so it starts in
    /// command mode, VSCode-style.
    fn open_palette(&mut self) {
        self.open_palette_with(">");
    }

    /// Open quick-open (Ctrl+P) — empty input, so it starts in go-to-file mode.
    fn open_quick_open(&mut self) {
        self.open_palette_with("");
    }

    /// Open the palette with `prefill` in the input; the prefix drives the mode.
    fn open_palette_with(&mut self, prefill: &str) {
        self.palette.open();
        // Clear any chrome hover so nothing lingers (dimmed) behind the modal.
        self.hovered_tab = None;
        self.hovered_tree = None;
        self.hovered_activity = None;
        self.hovered_explorer = None;
        if let Some(g) = self.gpu.as_mut() {
            g.ui.palette_input.set_text(&mut g.font_system, prefill);
            g.ui.palette_input.focus(true);
        }
        self.refilter_palette(); // derive the mode from the prefill + build its source
        self.redraw();
    }

    /// Live-preview the selected palette item: in `@` symbols mode the editor jumps
    /// to each symbol as you move through the list (VSCode behavior); the original
    /// position is restored if the palette is dismissed without committing.
    fn palette_preview(&mut self) {
        if self.palette.mode != commands::PaletteMode::Symbols {
            return;
        }
        let Some((line, label)) = self
            .palette
            .selected_item()
            .and_then(|it| it.line.map(|l| (l, it.label.clone())))
        else {
            return;
        };
        if self.palette_preview_return.is_none() {
            self.palette_preview_return = self.workspace.active_doc().map(|d| d.caret_byte());
        }
        let _ = label;
        self.goto_line(line);
        // Highlight the REGION: the symbol's whole block (declaration line through the
        // end of its indented body, via the fold engine), drawn as a tinted band.
        if let Some(d) = self.workspace.active_doc() {
            let l = line.saturating_sub(1).min(d.rope.len_lines().saturating_sub(1));
            let end = d.fold_range(l).unwrap_or(l);
            self.palette_preview_region = Some((l, end));
        }
    }

    /// Undo a symbol preview (palette dismissed without committing).
    fn palette_restore_preview(&mut self) {
        self.palette_preview_region = None;
        if let Some(byte) = self.palette_preview_return.take() {
            if let Some(d) = self.workspace.active_doc_mut() {
                let b = byte.min(d.rope.len_bytes());
                d.place(b, false);
            }
            self.ensure_cursor_visible();
        }
    }

    /// Build the quick-pick item list of available color themes: the built-in plus
    /// every theme contributed by installed extensions. `only_ext` scopes it to one
    /// extension (the detail page's "Set Color Theme" button).
    fn theme_items(&self, only_ext: Option<usize>) -> Vec<commands::PickItem> {
        let mut items = Vec::new();
        if only_ext.is_none() {
            items.push(commands::PickItem::new("Aether Dark", "dark · built-in"));
        }
        for (idx, e) in self.extensions.iter().enumerate() {
            if only_ext.map_or(false, |o| o != idx) {
                continue;
            }
            for t in &e.themes {
                items.push(commands::PickItem::new(t.label.clone(), if t.dark { "dark" } else { "light" }));
            }
        }
        items
    }

    /// Open the command palette as a color-theme quick-pick (whole registry, or one
    /// extension's themes when `only_ext` is set).
    fn open_theme_picker(&mut self, only_ext: Option<usize>) {
        let items = self.theme_items(only_ext);
        if items.is_empty() {
            return;
        }
        self.palette.open_quick_pick(commands::PickKind::SetColorTheme, items);
        if let Some(g) = self.gpu.as_mut() {
            g.ui.palette_input.clear(&mut g.font_system);
            g.ui.palette_input.focus(true);
        }
        self.redraw();
    }

    /// Commit a quick-pick selection.
    fn exec_pick(&mut self, kind: commands::PickKind, label: &str) {
        match kind {
            // Handled at the commit site (needs the item's PID), not by label.
            commands::PickKind::AttachProcess => {}
            commands::PickKind::OpenRecent => {
                let p = PathBuf::from(label);
                if p.is_dir() {
                    self.open_folder(p);
                } else {
                    self.show_info_dialog("That folder no longer exists.");
                }
            }
            commands::PickKind::Problem
            | commands::PickKind::Location
            | commands::PickKind::RenameSymbol
            | commands::PickKind::RenameTerminal
            | commands::PickKind::CreateBranch
            | commands::PickKind::RenameBranch => {} // handled at the commit site (need item/input)
            commands::PickKind::Checkout => {
                if git::checkout(&self.cwd, label) {
                    self.refresh_source_control();
                    self.apply_intent(ui::Intent::ReloadOpenDocs);
                } else {
                    self.show_info_dialog("Couldn't switch branch — commit or stash your changes first.");
                }
            }
            commands::PickKind::ReopenEncoding => {
                let enc = encoding::static_label(label);
                if let (Some(g), Some(d)) = (self.gpu.as_mut(), self.workspace.active_doc_mut()) {
                    d.reopen_with_encoding(enc, &mut g.font_system);
                }
                self.redraw();
            }
            commands::PickKind::DeleteBranch => {
                if git::branch(&self.cwd).as_deref() == Some(label) {
                    self.show_info_dialog("Can't delete the branch you're on — switch first.");
                } else if !git::delete_branch(&self.cwd, label) {
                    self.show_info_dialog("Couldn't delete the branch.");
                }
            }
            commands::PickKind::SetColorTheme => {
                self.apply_theme_by_name(label);
                settings::set_color_theme(label); // persist across restarts
                if let Some(g) = self.gpu.as_mut() {
                    g.ui.line_numbers.invalidate();
                    g.ui.line_numbers2.invalidate();
                    for d in self.workspace.documents.iter_mut() {
                        d.reshape(&mut g.font_system);
                    }
                }
                self.redraw();
            }
        }
    }

    /// All files under the workspace root (skipping VCS/build dirs), as Files-mode
    /// items. `label` is the repo-relative path (filtered + opened on commit).
    /// Go-to-file index, memoized. The first call walks the workspace; later calls
    /// (e.g. deleting the `>` prefix to drop into Files mode) clone the cached list,
    /// which keeps the mode switch off the slow filesystem path. Cleared by
    /// `invalidate_file_index` on any tree mutation.
    fn file_index(&mut self) -> Vec<commands::PickItem> {
        if self.palette_file_cache.is_none() {
            self.palette_file_cache = Some(self.build_file_items());
        }
        self.palette_file_cache.clone().unwrap_or_default()
    }

    /// Drop the cached go-to-file index so the next palette open rebuilds it. Call
    /// after any change to the set of files on disk (create/delete/rename/move) or a
    /// workspace-folder switch.
    fn invalidate_file_index(&mut self) {
        self.palette_file_cache = None;
    }

    fn build_file_items(&self) -> Vec<commands::PickItem> {
        const SKIP: &[&str] = &[
            ".git", "target", "node_modules", ".aether", "dist", "build", "out", ".next", ".venv",
            "bin", "obj", "Pods", ".expo", "__pycache__", ".gradle", "DerivedData", "coverage",
        ];
        let root = self.cwd.clone();
        let mut out = Vec::new();
        let mut stack = vec![root.clone()];
        while let Some(dir) = stack.pop() {
            let Ok(rd) = std::fs::read_dir(&dir) else { continue };
            for ent in rd.flatten() {
                let p = ent.path();
                let name = ent.file_name().to_string_lossy().to_string();
                if p.is_dir() {
                    if !SKIP.contains(&name.as_str()) {
                        stack.push(p);
                    }
                } else {
                    let rel = p.strip_prefix(&root).unwrap_or(&p).to_string_lossy().replace('\\', "/");
                    out.push(commands::PickItem::new(rel, ""));
                    // Generous cap: a truncated index silently "loses" files from
                    // search (rendering is separately capped at 500 matches).
                    if out.len() >= 100_000 {
                        return out;
                    }
                }
            }
        }
        out.sort_by(|a, b| a.label.cmp(&b.label));
        out
    }

    /// Symbols of the active document, as Symbols-mode items (`@`).
    fn build_symbol_items(&self) -> Vec<commands::PickItem> {
        let Some(d) = self.workspace.active_doc() else { return Vec::new() };
        let text = d.rope.to_string();
        extract_symbols(&text, d.ext())
            .into_iter()
            .map(|(name, kind, line)| commands::PickItem::at_line(name, kind, line))
            .collect()
    }

    /// Move the caret to a 1-based line in the active doc, reveal any fold over it,
    /// and center it.
    fn goto_line(&mut self, line: usize) {
        let target = line.saturating_sub(1);
        let editor_h = self.layout().editor_text.h;
        if let Some(d) = self.workspace.active_doc_mut() {
            let l = target.min(d.rope.len_lines().saturating_sub(1));
            if d.is_line_hidden(l) {
                d.reveal_line(l);
            }
            let byte = d.rope.line_to_byte(l);
            d.place(byte, false);
            // Center the target (VSCode revealInCenter) — a minimal "keep visible"
            // scroll leaves the destination hugging the bottom edge, out of view.
            let y = l as f32 * theme::LINE_HEIGHT() - (editor_h - theme::LINE_HEIGHT()) * 0.5;
            d.scroll.scroll_to_y(y.max(0.0));
        }
        self.redraw();
    }

    /// Re-filter the palette from its input. The leading character selects the mode
    /// (`>` commands, `@` symbols, `:` line, none = files) — VSCode quick-open.
    fn refilter_palette(&mut self) {
        let raw = self.gpu.as_ref().map(|g| g.ui.palette_input.text().to_string()).unwrap_or_default();
        // Resolve (target mode, residual query) from the prefix.
        let (mode, sub): (commands::PaletteMode, String) = match raw.chars().next() {
            // Keep an active programmatic quick-pick (e.g. theme chooser) regardless.
            _ if matches!(self.palette.mode, commands::PaletteMode::QuickPick(_)) => (self.palette.mode, raw.clone()),
            Some('>') => (commands::PaletteMode::Commands, raw[1..].to_string()),
            Some('@') => (commands::PaletteMode::Symbols, raw.trim_start_matches(['@', ':']).to_string()),
            Some(':') => (commands::PaletteMode::GoToLine, raw[1..].to_string()),
            Some('%') => (commands::PaletteMode::TextSearch, raw[1..].to_string()),
            Some('#') => (commands::PaletteMode::WorkspaceSymbols, raw[1..].to_string()),
            _ => (commands::PaletteMode::Files, raw.clone()),
        };
        // `%` text search: live results stream into the palette list itself.
        if mode == commands::PaletteMode::TextSearch {
            // Tolerate SQL-LIKE habits: %query% — the wrapping %s aren't part of it.
            let q = sub.trim().trim_matches('%').trim().to_string();
            if q.len() < 3 {
                let hint = "Search in files… (type at least 3 characters)".to_string();
                self.palette.set_source(mode, vec![commands::PickItem::new(hint, "")]);
                return;
            }
            // New query → new generation; stale streams are dropped by the gen guard.
            self.palette_search_gen += 1;
            self.palette.set_source(mode, Vec::new());
            crate::search::search_async(
                self.worker_tx.clone(),
                self.palette_search_gen,
                self.cwd.clone(),
                q,
                crate::search::SearchOpts::default(),
                crate::search::Filters::default(),
            );
            return;
        }
        // `#` workspace symbols: each keystroke re-queries the language server; the
        // response replaces the list when it lands (stale ids are dropped).
        if mode == commands::PaletteMode::WorkspaceSymbols {
            let q = sub.trim().to_string();
            if self.palette.mode != mode {
                self.palette.set_source(mode, Vec::new());
            }
            if q.is_empty() {
                self.palette.set_source(mode, vec![commands::PickItem::new("Search workspace symbols…", "")]);
                return;
            }
            let lang = self.workspace.active_doc().and_then(|d| d.language_id());
            if !self.lsp.request_workspace_symbols(lang, &q) {
                self.palette.set_source(
                    mode,
                    vec![commands::PickItem::new("No language server running for workspace symbols.", "")],
                );
            }
            return;
        }
        // Go-to-line: show a single hint row reflecting the typed number.
        if mode == commands::PaletteMode::GoToLine {
            let total = self.workspace.active_doc().map_or(0, |d| d.rope.len_lines());
            let label = if sub.trim().is_empty() {
                format!("Go to line… (1–{total})")
            } else {
                format!("Go to line {}", sub.trim())
            };
            self.palette.set_source(mode, vec![commands::PickItem::new(label, "")]);
            return;
        }
        // Rebuild the source only when the mode changes (file/symbol scans are not free).
        if self.palette.mode != mode {
            match mode {
                commands::PaletteMode::Commands => self.palette.set_source(mode, Vec::new()),
                commands::PaletteMode::Files => {
                    let items = self.file_index();
                    self.palette.set_source(mode, items);
                }
                commands::PaletteMode::Symbols => {
                    let items = self.build_symbol_items();
                    self.palette.set_source(mode, items);
                }
                commands::PaletteMode::GoToLine => self.palette.set_source(mode, Vec::new()),
                commands::PaletteMode::TextSearch => {} // handled by the early return above
                commands::PaletteMode::WorkspaceSymbols => {} // handled by the early return above
                commands::PaletteMode::QuickPick(_) => {}
            }
        }
        self.palette.refilter(&sub);
    }

    /// Commit the current palette selection based on its mode. Returns true if the
    /// palette should close afterward.
    fn commit_palette(&mut self) -> bool {
        self.palette_preview_return = None; // a commit keeps the navigated position
        self.palette_preview_region = None;
        match self.palette.mode {
            commands::PaletteMode::Commands => {
                if let Some(cmd) = self.palette.selected_command() {
                    self.palette.close();
                    self.exec_command(cmd);
                }
                true
            }
            commands::PaletteMode::QuickPick(_) => {
                // Rename input: the TYPED text is the new name (no item selection).
                if matches!(self.palette.mode, commands::PaletteMode::QuickPick(commands::PickKind::RenameSymbol)) {
                    let new_name = self
                        .gpu
                        .as_ref()
                        .map(|g| g.ui.palette_input.text().trim().to_string())
                        .unwrap_or_default();
                    self.palette.close();
                    if let Some((uri, lang, line, col)) = self.pending_rename.take() {
                        if !new_name.is_empty() && !self.lsp.request_rename(lang, &uri, line, col, &new_name) {
                            self.show_info_dialog("The language server isn't running yet.");
                        }
                    }
                    return true;
                }
                // Terminal tab rename: same palette-as-input flow.
                if matches!(self.palette.mode, commands::PaletteMode::QuickPick(commands::PickKind::RenameTerminal)) {
                    let new_name = self
                        .gpu
                        .as_ref()
                        .map(|g| g.ui.palette_input.text().trim().to_string())
                        .unwrap_or_default();
                    self.palette.close();
                    if let Some(i) = self.pending_term_rename.take() {
                        if !new_name.is_empty() {
                            self.terminal.rename_tab(i, &new_name);
                        }
                    }
                    return true;
                }
                // Create / rename branch: typed text is the branch name.
                if let commands::PaletteMode::QuickPick(k @ (commands::PickKind::CreateBranch | commands::PickKind::RenameBranch)) = self.palette.mode {
                    let name = self
                        .gpu
                        .as_ref()
                        .map(|g| g.ui.palette_input.text().trim().to_string())
                        .unwrap_or_default();
                    self.palette.close();
                    if !name.is_empty() {
                        let ok = match k {
                            commands::PickKind::RenameBranch => git::rename_branch(&self.cwd, &name),
                            _ => git::create_branch(&self.cwd, &name),
                        };
                        if ok {
                            self.refresh_source_control();
                        } else {
                            self.show_info_dialog("Couldn't update the branch (name may already exist).");
                        }
                    }
                    return true;
                }
                if let Some((kind, label)) = self.palette.selected_pick() {
                    let item = self.palette.selected_item().cloned();
                    self.palette.close();
                    if kind == commands::PickKind::AttachProcess {
                        if let Some(pid) = item.and_then(|it| it.line) {
                            self.debug_attach_pid(pid as i64);
                        }
                    } else if matches!(kind, commands::PickKind::Problem | commands::PickKind::Location) {
                        // detail = "rel/path:line" — jump straight to the diagnostic.
                        if let Some(it) = item {
                            if let Some((rel, _)) = it.detail.rsplit_once(':') {
                                self.nav.mark(&self.workspace);
                                let path = self.cwd.join(rel);
                                self.open_file_at(path, it.line.unwrap_or(1), 0);
                            }
                        }
                    } else {
                        self.exec_pick(kind, &label);
                    }
                }
                true
            }
            commands::PaletteMode::Files => {
                // A literal path typed into the box wins over the fuzzy selection so
                // pasting/typing a full path (absolute, ~, or cwd-relative) opens it.
                let raw = self
                    .gpu
                    .as_ref()
                    .map(|g| g.ui.palette_input.text().trim().to_string())
                    .unwrap_or_default();
                if let Some(path) = self.resolve_literal_path(&raw) {
                    if path.is_file() {
                        self.palette.close();
                        self.nav.mark(&self.workspace);
                        self.open_file_at(path, 1, 0);
                        return true;
                    }
                }
                if let Some(rel) = self.palette.selected_item().map(|it| it.label.clone()) {
                    self.palette.close();
                    self.nav.mark(&self.workspace);
                    let path = self.cwd.join(&rel);
                    self.open_file_at(path, 1, 0);
                }
                true
            }
            commands::PaletteMode::Symbols => {
                if let Some(line) = self.palette.selected_item().and_then(|it| it.line) {
                    self.palette.close();
                    self.nav.mark(&self.workspace);
                    self.goto_line(line);
                }
                true
            }
            commands::PaletteMode::TextSearch => {
                // Each row is one match: detail = workspace-relative path, line = hit.
                if let Some((rel, line)) = self
                    .palette
                    .selected_item()
                    .filter(|it| it.line.is_some())
                    .map(|it| (it.detail.clone(), it.line.unwrap_or(1)))
                {
                    self.palette.close();
                    self.nav.mark(&self.workspace);
                    let path = self.cwd.join(rel);
                    self.open_file_at(path, line, 0);
                }
                true
            }
            commands::PaletteMode::WorkspaceSymbols => {
                // detail = "rel/path:line" (like Problems) — jump to the symbol.
                if let Some(it) = self.palette.selected_item().cloned().filter(|it| it.line.is_some()) {
                    self.palette.close();
                    if let Some((rel, _)) = it.detail.rsplit_once(':') {
                        self.nav.mark(&self.workspace);
                        let path = self.cwd.join(rel);
                        self.open_file_at(path, it.line.unwrap_or(1), 0);
                    }
                }
                true
            }
            commands::PaletteMode::GoToLine => {
                let n: usize = self
                    .gpu
                    .as_ref()
                    .and_then(|g| g.ui.palette_input.text().trim_start_matches(':').trim().parse().ok())
                    .unwrap_or(0);
                self.palette.close();
                if n > 0 {
                    self.nav.mark(&self.workspace);
                    self.goto_line(n);
                }
                true
            }
        }
    }

    fn exec_command(&mut self, cmd: Command) {
        match cmd {
            Command::Save => {
                // Untitled docs (no path) can't be written — prompt Save As first,
                // assign the chosen path, then fall through to the normal save.
                let needs_path = self.workspace.active_doc().map_or(false, |d| d.path.is_none());
                if needs_path {
                    let start_dir = self.workspace.tree.root.clone();
                    match rfd::FileDialog::new().set_directory(&start_dir).save_file() {
                        Some(path) => {
                            if let (Some(g), Some(d)) =
                                (self.gpu.as_mut(), self.workspace.active_doc_mut())
                            {
                                d.set_path(path, &mut g.font_system);
                            }
                        }
                        None => return, // user cancelled
                    }
                }
                let saved_path = self.workspace.active_doc().and_then(|d| d.path.clone());
                if let Some(d) = self.workspace.active_doc_mut() {
                    let _ = d.save();
                }
                // Tell the language servers (didSave triggers e.g. rust-analyzer's
                // cargo check — without it, full diagnostics never refresh).
                if let Some(d) = self.workspace.active_doc() {
                    if let Some(uri) = d.uri() {
                        let text = d.text();
                        for server in d.lsp_servers.clone() {
                            self.lsp.did_save(server, &uri, &text);
                        }
                    }
                }
                // Saving settings.json applies the new values immediately.
                if let Some(p) = saved_path {
                    if settings::is_user_settings(&p) {
                        self.apply_settings();
                    }
                }
                self.refresh_source_control(); // a save changes git status → update badge
                // Re-blame on next tick so edited lines flip to "Uncommitted".
                if let Some(d) = self.workspace.active_doc_mut() {
                    d.blame.clear();
                    d.blame_requested = false;
                }
            }
            Command::Close => {
                self.request_close_active();
            }
            Command::Find => {
                self.find.active = true;
                self.find.focused = true;
                self.find.on_replace = false;
                // Seed the query from the editor's current selection, if any.
                let seed = self
                    .workspace
                    .active_doc()
                    .filter(|d| !d.sel.is_empty())
                    .map(|d| {
                        let (lo, hi) = d.sel.range();
                        d.rope.byte_slice(lo..hi).to_string()
                    })
                    .filter(|s| !s.contains('\n') && !s.is_empty());
                if let Some(g) = self.gpu.as_mut() {
                    if let Some(s) = seed.as_ref() {
                        g.ui.find.query.set_text(&mut g.font_system, s);
                    }
                    g.ui.find.query.select_all();
                    g.ui.find.query.focus(true);
                    g.ui.find.replace.focus(false);
                }
                self.recompute_find();
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
            Command::OpenSettings => self.open_settings_editor(),
            Command::OpenDefaultSettings => self.open_settings_file(settings::default_settings_path()),
            Command::MarkdownPreview => self.open_markdown_preview(),
            Command::ToggleTerminal => self.toggle_terminal(),
            Command::OpenFolder => {
                if let Some(folder) = rfd::FileDialog::new().pick_folder() {
                    self.open_folder(folder);
                }
            }
            Command::ColorTheme => self.open_theme_picker(None),
            Command::ToggleLineComment => self.doc_edit(|d, fs| d.toggle_line_comment(fs)),
            Command::ToggleBlockComment => self.doc_edit(|d, fs| d.toggle_block_comment(fs)),
            Command::MoveLineUp => self.doc_edit(|d, fs| d.move_lines(false, fs)),
            Command::MoveLineDown => self.doc_edit(|d, fs| d.move_lines(true, fs)),
            Command::CopyLineUp => self.doc_edit(|d, fs| d.copy_lines(false, fs)),
            Command::CopyLineDown => self.doc_edit(|d, fs| d.copy_lines(true, fs)),
            Command::DuplicateSelection => self.doc_edit(|d, fs| d.duplicate_selection(fs)),
            Command::ExpandSelection => self.doc_edit(|d, _| d.expand_selection()),
            Command::ShrinkSelection => self.doc_edit(|d, _| d.shrink_selection()),
            Command::GotoBracket => self.doc_edit(|d, _| d.goto_bracket()),
            Command::NavBack => {
                if let Some(loc) = self.nav.back(&self.workspace) {
                    self.nav_go(loc);
                }
            }
            Command::NavForward => {
                if let Some(loc) = self.nav.forward(&self.workspace) {
                    self.nav_go(loc);
                }
            }
            Command::LastEditLocation => {
                if let Some(loc) = self.nav.last_edit.clone() {
                    self.nav.mark(&self.workspace);
                    self.nav_go(loc);
                }
            }
            Command::NextProblem => self.cycle_problem(true),
            Command::PrevProblem => self.cycle_problem(false),
            Command::NextEditor => self.cycle_editor(1),
            Command::PrevEditor => self.cycle_editor(-1),
            Command::GotoDefinition => self.lsp_goto(lsp::LocKind::Definition),
            Command::GotoDeclaration => self.lsp_goto(lsp::LocKind::Declaration),
            Command::GotoTypeDefinition => self.lsp_goto(lsp::LocKind::TypeDefinition),
            Command::GotoImplementations => self.lsp_goto(lsp::LocKind::Implementation),
            Command::GotoReferences => self.lsp_goto(lsp::LocKind::References),
            Command::FormatDocument => self.lsp_format(false),
            Command::FormatSelection => self.lsp_format(true),
            Command::RenameSymbol => self.open_rename_input(),
        }
        self.redraw();
    }

    /// Fire a Go-to / references request for the caret position. The response
    /// arrives as `WorkerMsg::LspLocations` and lands in `apply_locations`.
    fn lsp_goto(&mut self, kind: lsp::LocKind) {
        let Some(d) = self.workspace.active_doc() else { return };
        let (Some(uri), Some(lang)) = (d.uri(), d.language_id()) else {
            return self.show_info_dialog("No language server for this file type.");
        };
        let (line, col) = d.lsp_pos(d.caret_byte());
        if !self.lsp.request_locations(lang, &uri, line, col, kind) {
            self.show_info_dialog("The language server isn't running yet.");
        }
    }

    /// Format Document / Format Selection: the server's TextEdits arrive as
    /// `WorkerMsg::LspTextEdits` and apply as one undo step.
    fn lsp_format(&mut self, selection_only: bool) {
        self.flush_doc_to_lsp(); // format against the current text, not the debounce
        let Some(d) = self.workspace.active_doc() else { return };
        let (Some(uri), Some(lang)) = (d.uri(), d.language_id()) else {
            return self.show_info_dialog("No language server for this file type.");
        };
        let range = if selection_only {
            if d.sel.is_empty() {
                return self.show_info_dialog("Select some text to format first.");
            }
            let (lo, hi) = d.sel.range();
            let (sl, sc) = d.lsp_pos(lo);
            let (el, ec) = d.lsp_pos(hi);
            Some((sl, sc, el, ec))
        } else {
            None
        };
        if !self.lsp.request_formatting(lang, &uri, range) {
            self.show_info_dialog("The language server isn't running yet.");
        }
    }

    /// Rename Symbol (F2): open the palette as an input box seeded with the
    /// identifier at the caret; Enter fires `textDocument/rename`.
    fn open_rename_input(&mut self) {
        self.flush_doc_to_lsp();
        let Some(d) = self.workspace.active_doc() else { return };
        let (Some(uri), Some(lang)) = (d.uri(), d.language_id()) else {
            return self.show_info_dialog("No language server for this file type.");
        };
        let Some((lo, hi)) = d.word_at(d.caret_byte()) else {
            return self.show_info_dialog("Place the caret on a symbol to rename it.");
        };
        let current = d.rope.byte_slice(lo..hi).to_string();
        let (line, col) = d.lsp_pos(lo);
        self.pending_rename = Some((uri, lang, line, col));
        self.palette.open_quick_pick(
            commands::PickKind::RenameSymbol,
            vec![commands::PickItem::new("Press Enter to rename, Esc to cancel", "")],
        );
        if let Some(g) = self.gpu.as_mut() {
            g.ui.palette_input.set_text(&mut g.font_system, &current);
            g.ui.palette_input.select_all();
            g.ui.palette_input.focus(true);
        }
        self.redraw();
    }

    /// Apply a WorkspaceEdit (rename response) across files: open docs get the
    /// edits in place (one undo step each, left dirty); unopened files are opened
    /// first so the change is visible and undoable.
    fn apply_workspace_edit(&mut self, changes: Vec<(String, Vec<lsp::TextEdit>)>) {
        let mut files = 0usize;
        let mut total = 0usize;
        for (uri, edits) in changes {
            if edits.is_empty() {
                continue;
            }
            let Some(path) = lsp::uri_to_path(&uri) else { continue };
            let open_idx = self.workspace.documents.iter().position(|d| d.path.as_deref() == Some(path.as_path()));
            let idx = match open_idx {
                Some(i) => Some(i),
                None => {
                    let Some(gpu) = self.gpu.as_mut() else { continue };
                    if self.workspace.open_file(&path, &mut gpu.font_system).is_ok() {
                        self.workspace.active
                    } else {
                        None
                    }
                }
            };
            if let (Some(i), Some(gpu)) = (idx, self.gpu.as_mut()) {
                if let Some(d) = self.workspace.documents.get_mut(i) {
                    d.apply_text_edits(&edits, &mut gpu.font_system);
                    files += 1;
                    total += edits.len();
                }
            }
        }
        if files == 0 {
            self.show_info_dialog("Nothing to rename here.");
        } else if files > 1 {
            // Single-file renames are self-evident; summarize multi-file ones.
            self.show_info_dialog(&format!("Renamed {total} occurrence(s) across {files} files (unsaved)."));
        }
        self.redraw();
    }

    /// Open a branch quick-pick (Checkout / Delete) listing local branches.
    /// Open the "Reopen with Encoding" quick-pick (status-bar encoding click).
    fn open_encoding_pick(&mut self) {
        if self.workspace.active_doc().and_then(|d| d.path.as_ref()).is_none() {
            return;
        }
        let cur = self.workspace.active_doc().map(|d| d.encoding).unwrap_or("UTF-8");
        let items: Vec<commands::PickItem> = encoding::ENCODINGS
            .iter()
            .map(|(label, _)| {
                let detail = if *label == cur { "current" } else { "" };
                commands::PickItem::new(*label, detail)
            })
            .collect();
        self.palette.open_quick_pick(commands::PickKind::ReopenEncoding, items);
        if let Some(g) = self.gpu.as_mut() {
            g.ui.palette_input.set_text(&mut g.font_system, "");
            g.ui.palette_input.focus(true);
        }
        self.redraw();
    }

    fn open_branch_pick(&mut self, kind: commands::PickKind) {
        let cur = git::branch(&self.cwd);
        let items: Vec<commands::PickItem> = git::branches(&self.cwd)
            .into_iter()
            .map(|b| {
                let detail = if Some(&b) == cur.as_ref() { "current" } else { "" };
                commands::PickItem::new(b, detail)
            })
            .collect();
        if items.is_empty() {
            self.show_info_dialog("No branches found (is this a git repo?).");
            return;
        }
        self.palette.open_quick_pick(kind, items);
        if let Some(g) = self.gpu.as_mut() {
            g.ui.palette_input.set_text(&mut g.font_system, "");
            g.ui.palette_input.focus(true);
        }
        self.redraw();
    }

    /// Open the palette as a text input (create/rename branch), seeded with `prefill`.
    fn open_branch_input(&mut self, kind: commands::PickKind, prefill: &str, prompt: &str) {
        self.palette.open_quick_pick(kind, vec![commands::PickItem::new(prompt, "")]);
        if let Some(g) = self.gpu.as_mut() {
            g.ui.palette_input.set_text(&mut g.font_system, prefill);
            g.ui.palette_input.select_all();
            g.ui.palette_input.focus(true);
        }
        self.redraw();
    }

    /// Terminal tab rename: the palette becomes the input, seeded with the
    /// current title (same flow as Rename Symbol).
    fn open_term_rename(&mut self, i: usize) {
        let Some(current) = self.terminal.tab_title(i) else { return };
        self.pending_term_rename = Some(i);
        self.palette.open_quick_pick(
            commands::PickKind::RenameTerminal,
            vec![commands::PickItem::new("Press Enter to rename the terminal, Esc to cancel", "")],
        );
        if let Some(g) = self.gpu.as_mut() {
            g.ui.palette_input.set_text(&mut g.font_system, &current);
            g.ui.palette_input.select_all();
            g.ui.palette_input.focus(true);
        }
        self.redraw();
    }

    /// Send any unsent edits of the active doc to its language servers right now
    /// (rename/format must see the latest text, not the debounced version).
    fn flush_doc_to_lsp(&mut self) {
        let _probe = crate::perf::Probe::new("flush_doc_to_lsp", 8);
        let Some(d) = self.workspace.active_doc_mut() else { return };
        if !d.lsp_dirty {
            return;
        }
        if let Some(uri) = d.uri() {
            let (text, version) = (d.text(), d.version);
            for server in d.lsp_servers.clone() {
                self.lsp.did_change(server, &uri, version, &text);
            }
        }
    }

    /// Handle a definition/references/symbol response: jump straight to a single
    /// target, open a picker for several, and feed `#` palette queries.
    fn apply_locations(&mut self, kind: lsp::LocKind, locs: Vec<lsp::LspLocation>) {
        // Palette `#` mode: replace the list with the latest symbol results.
        if kind == lsp::LocKind::WorkspaceSymbol {
            if self.palette.active && self.palette.mode == commands::PaletteMode::WorkspaceSymbols {
                let items = locs.iter().map(|l| self.loc_item(l)).collect();
                self.palette.set_source(commands::PaletteMode::WorkspaceSymbols, items);
                self.redraw();
            }
            return;
        }
        match (locs.len(), kind) {
            (0, _) => self.show_info_dialog(&format!("No {} found.", kind.label())),
            (1, k) if k != lsp::LocKind::References => {
                let l = &locs[0];
                self.nav.mark(&self.workspace);
                if let Some(p) = lsp::uri_to_path(&l.uri) {
                    self.open_file_at(p, l.line as usize + 1, l.character as usize);
                }
            }
            _ => {
                // Several targets (or any references): pick from a palette list.
                let items: Vec<commands::PickItem> = locs.iter().map(|l| self.loc_item(l)).collect();
                self.palette.open_quick_pick(commands::PickKind::Location, items);
                if let Some(g) = self.gpu.as_mut() {
                    g.ui.palette_input.clear(&mut g.font_system);
                    g.ui.palette_input.focus(true);
                }
            }
        }
        self.redraw();
    }

    /// A palette row for one LSP location: `symbol-or-file  path:line`.
    fn loc_item(&self, l: &lsp::LspLocation) -> commands::PickItem {
        let path = lsp::uri_to_path(&l.uri).unwrap_or_default();
        let rel = path.strip_prefix(&self.cwd).unwrap_or(&path).to_string_lossy().into_owned();
        let label = l.name.clone().unwrap_or_else(|| {
            path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_else(|| rel.clone())
        });
        commands::PickItem {
            label,
            detail: format!("{rel}:{}", l.line + 1),
            line: Some(l.line as usize + 1),
        }
    }

    /// Jump to a recorded navigation location: re-activate its tab (re-opening the
    /// file if it was closed) and center its line.
    fn nav_go(&mut self, loc: nav::NavLoc) {
        let found = self.workspace.documents.iter().position(|d| {
            (loc.path.is_some() && d.path == loc.path) || (loc.path.is_none() && d.name == loc.name)
        });
        match (found, &loc.path) {
            (Some(i), _) => self.workspace.active = Some(i),
            (None, Some(p)) => {
                let p = p.clone();
                if let Some(gpu) = self.gpu.as_mut() {
                    if self.workspace.open_file(&p, &mut gpu.font_system).is_err() {
                        return; // file gone — drop the entry silently
                    }
                }
            }
            (None, None) => return, // pathless tab (diff/untitled) was closed
        }
        self.nav.note_switch(&self.workspace);
        self.goto_line(loc.line + 1);
    }

    /// Jump to the next/previous diagnostic in the active document (F8 / Shift+F8),
    /// wrapping around.
    fn cycle_problem(&mut self, next: bool) {
        let editor_h = self.layout().editor_text.h;
        let Some(d) = self.workspace.active_doc_mut() else { return };
        if d.diagnostics.is_empty() {
            return;
        }
        let mut starts: Vec<usize> = d.diagnostics.iter().map(|g| d.lsp_byte(g.start_line, g.start_char)).collect();
        starts.sort_unstable();
        starts.dedup();
        let caret = d.caret_byte();
        let target = if next {
            *starts.iter().find(|&&b| b > caret).unwrap_or(&starts[0])
        } else {
            *starts.iter().rev().find(|&&b| b < caret).unwrap_or(starts.last().unwrap())
        };
        let l = d.rope.byte_to_line(target);
        if d.is_line_hidden(l) {
            d.reveal_line(l);
        }
        d.place(target, false);
        let y = l as f32 * theme::LINE_HEIGHT() - (editor_h - theme::LINE_HEIGHT()) * 0.5;
        d.scroll.scroll_to_y(y.max(0.0));
    }

    /// Cycle the active editor tab (Ctrl+PageDown / Ctrl+PageUp).
    fn cycle_editor(&mut self, dir: i32) {
        let n = self.workspace.documents.len();
        if n == 0 {
            return;
        }
        let cur = self.workspace.active.unwrap_or(0) as i32;
        self.workspace.active = Some(((cur + dir).rem_euclid(n as i32)) as usize);
    }

    /// Run an editing op on the active document (palette / menu entry path) and
    /// keep the caret in view.
    fn doc_edit(&mut self, f: impl FnOnce(&mut Document, &mut glyphon::FontSystem)) {
        if let Some(gpu) = self.gpu.as_mut() {
            if let Some(d) = self.workspace.active_doc_mut() {
                f(d, &mut gpu.font_system);
            }
        }
        self.ensure_cursor_visible();
    }

    /// Switch the workspace to `folder`: re-root the file tree (and the find-in-files
    /// root), update the explorer header, and clear stale search state. Open editors
    /// are kept, like VSCode.
    fn open_folder(&mut self, folder: PathBuf) {
        // Single-window-per-folder (like VSCode): if another live window already has
        // this folder open, it raises itself instead of us opening a duplicate.
        if self.terminal.focus_other_window(&folder.to_string_lossy()) {
            return;
        }
        self.cwd = folder.clone();
        if let Some(g) = self.gpu.as_ref() {
            g.window.set_title(&window_title(&folder)); // Dock window-list / title bar
        }
        self.terminal.set_cwd(folder.clone()); // new shells start in the new root
        if let Some(scp) = self.source_control.as_mut() {
            // Git operates on the repo top-level so status paths align with diff/stage
            // pathspecs when the opened folder is a subdirectory of the repo.
            scp.set_root(git::repo_root(&folder));
        }
        self.workspace.tree = crate::workspace::FileTree::new(folder);
        self.invalidate_file_index(); // new root → rebuild go-to-file index on next open
        self.sidebar_view = SidebarView::Explorer;
        self.sidebar_visible = true;
        if let Some(sp) = self.search.as_mut() {
            sp.reset();
        }
        self.refresh_source_control(); // update the change-count badge for the new repo
        self.persist_state(); // remember this folder for the next launch
        self.redraw();
    }

    /// Switch this window to the folder-less state (File > Close Folder). Open
    /// editors are kept; the explorer empties out.
    fn open_folder_less(&mut self) {
        self.cwd = PathBuf::new();
        if let Some(g) = self.gpu.as_ref() {
            g.window.set_title("Aether");
        }
        self.terminal.set_cwd(PathBuf::new());
        if let Some(scp) = self.source_control.as_mut() {
            scp.set_root(PathBuf::new());
        }
        self.workspace.tree = crate::workspace::FileTree::new(PathBuf::new());
        self.sidebar_view = SidebarView::Explorer;
        if let Some(sp) = self.search.as_mut() {
            sp.reset();
        }
        self.refresh_source_control();
        self.persist_state(); // keeps the last real folder (guarded inside)
        self.redraw();
    }

    /// Run a shell command in the integrated terminal (Terminal > Run Active File /
    /// Run Selected Text): opens the panel if needed, pastes, and presses Enter.
    fn run_in_terminal(&mut self, text: &str) {
        if !self.terminal.visible {
            self.toggle_terminal(); // also spawns the first shell when none exists
        }
        self.terminal.paste_focused(text);
        self.terminal.write_focused(b"\r"); // execute (paste may be bracketed)
        self.redraw();
    }

    /// Terminal > Run Active File: run the file with its language's interpreter.
    fn run_active_file(&mut self) {
        let Some(path) = self.workspace.active_doc().and_then(|d| d.path.clone()) else {
            return self.show_info_dialog("The active tab has no file on disk.");
        };
        let ext = path.extension().map(|e| e.to_string_lossy().to_lowercase()).unwrap_or_default();
        let quoted = shell_quoted(&path);
        let cmd = match ext.as_str() {
            "py" => format!("python3 {quoted}"),
            "js" | "mjs" | "cjs" => format!("node {quoted}"),
            "ts" => format!("npx tsx {quoted}"),
            "sh" | "bash" => format!("bash {quoted}"),
            "zsh" => format!("zsh {quoted}"),
            "rb" => format!("ruby {quoted}"),
            "pl" => format!("perl {quoted}"),
            "lua" => format!("lua {quoted}"),
            "php" => format!("php {quoted}"),
            "swift" => format!("swift {quoted}"),
            "r" => format!("Rscript {quoted}"),
            "go" => format!("go run {quoted}"),
            "rs" => "cargo run".to_string(), // workspace-aware; rustc one-offs are rare
            _ => {
                return self.show_info_dialog(&format!(
                    "Don't know how to run .{ext} files in the terminal."
                ));
            }
        };
        self.run_in_terminal(&cmd);
    }

    /// Terminal > Run Selected Text: send the editor selection to the shell.
    fn run_selected_text(&mut self) {
        let Some(text) = self.workspace.active_doc().and_then(|d| d.selected_text()) else {
            return self.show_info_dialog("Select some text in the editor first.");
        };
        self.run_in_terminal(text.trim_end());
    }

    /// View > Zen Mode: fullscreen with all chrome hidden; restores the previous
    /// sidebar/terminal visibility on exit.
    fn toggle_zen(&mut self) {
        let on = !layout::zen();
        layout::set_zen(on);
        if on {
            self.zen_saved = Some((self.sidebar_visible, self.terminal.visible));
            self.sidebar_visible = false;
            if self.terminal.visible {
                self.toggle_terminal();
            }
            if let Some(g) = self.gpu.as_ref() {
                g.window.set_fullscreen(Some(winit::window::Fullscreen::Borderless(None)));
            }
        } else {
            if let Some((sb, term)) = self.zen_saved.take() {
                self.sidebar_visible = sb;
                if term && !self.terminal.visible {
                    self.toggle_terminal();
                }
            }
            if let Some(g) = self.gpu.as_ref() {
                g.window.set_fullscreen(None);
            }
        }
        self.redraw();
    }

    /// View > Problems: quick-pick of every diagnostic across open documents.
    fn open_problems_picker(&mut self) {
        let mut items: Vec<commands::PickItem> = Vec::new();
        for d in &self.workspace.documents {
            let rel = match d.path.as_ref() {
                Some(p) => p.strip_prefix(&self.cwd).unwrap_or(p).to_string_lossy().into_owned(),
                None => continue,
            };
            for g in &d.diagnostics {
                let sev = match g.severity {
                    crate::lsp::Severity::Error => "error",
                    crate::lsp::Severity::Warning => "warning",
                    _ => "info",
                };
                let msg = g.message.lines().next().unwrap_or("").trim();
                let mut label = format!("{sev}: {msg}");
                label.truncate(label.char_indices().map(|(i, _)| i).nth(100).unwrap_or(label.len()));
                items.push(commands::PickItem {
                    label,
                    detail: format!("{rel}:{}", g.start_line + 1),
                    line: Some(g.start_line as usize + 1),
                });
            }
        }
        if items.is_empty() {
            return self.show_info_dialog("No problems detected in open files.");
        }
        self.palette.open_quick_pick(commands::PickKind::Problem, items);
        if let Some(g) = self.gpu.as_mut() {
            g.ui.palette_input.clear(&mut g.font_system);
            g.ui.palette_input.focus(true);
        }
        self.redraw();
    }

    /// View > Output: a read-only tab streaming the language servers' log lines.
    fn open_output_tab(&mut self) {
        let text = self.output_text();
        if let Some(i) = self.workspace.documents.iter().position(|d| d.read_only && d.name == "Output") {
            self.workspace.active = Some(i);
            if let Some(gpu) = self.gpu.as_mut() {
                if let Some(d) = self.workspace.documents.get_mut(i) {
                    d.set_text_external(&text, &mut gpu.font_system);
                }
            }
        } else if let Some(gpu) = self.gpu.as_mut() {
            let mut d = Document::new(None, text, &mut gpu.font_system);
            d.name = "Output".into();
            d.read_only = true;
            self.workspace.documents.push(d);
            self.workspace.active = Some(self.workspace.documents.len() - 1);
        }
        self.redraw();
    }

    /// Open a rendered Markdown preview of the active document (a read-only tab).
    /// Re-runs against the current text if the preview tab is already open.
    fn open_markdown_preview(&mut self) {
        // Source = the active editable document's text + name. Skip if the active tab
        // is itself a preview/info/diff/etc.
        let Some((src, base)) = self.workspace.active_doc().and_then(|d| {
            if d.markdown_preview.is_some() || d.info.is_some() || d.diff.is_some() || d.image.is_some() {
                return None;
            }
            Some((d.text(), d.name.clone()))
        }) else {
            self.show_info_dialog("Open a Markdown (.md) file first, then run Markdown: Open Preview.");
            return;
        };
        let title = format!("Preview {base}");
        if let Some(i) = self.workspace.documents.iter().position(|d| d.markdown_preview.is_some() && d.name == title) {
            self.workspace.active = Some(i);
            // Refresh the source so the preview reflects the latest edits.
            if let Some(gpu) = self.gpu.as_mut() {
                if let Some(d) = self.workspace.documents.get_mut(i) {
                    d.set_text_external(&src, &mut gpu.font_system);
                }
            }
        } else if let Some(gpu) = self.gpu.as_mut() {
            let d = Document::new_markdown_preview(title, src, &mut gpu.font_system);
            self.workspace.documents.push(d);
            self.workspace.active = Some(self.workspace.documents.len() - 1);
            self.terminal.maximized = false;
        }
        self.redraw();
    }

    /// Open (or refocus) a designed informational tab (Welcome / Shortcuts / Tips).
    fn open_info_tab(&mut self, page: ui::info_page::InfoPage) {
        if let Some(i) = self.workspace.documents.iter().position(|d| d.read_only && d.name == page.title) {
            self.workspace.active = Some(i);
        } else if let Some(gpu) = self.gpu.as_mut() {
            let d = Document::new_info(page, &mut gpu.font_system);
            self.workspace.documents.push(d);
            self.workspace.active = Some(self.workspace.documents.len() - 1);
        }
        self.redraw();
    }

    fn output_text(&self) -> String {
        if self.lsp_log.is_empty() {
            "(no language-server output yet)".to_string()
        } else {
            self.lsp_log.iter().cloned().collect::<Vec<_>>().join("\n")
        }
    }

    /// Persist machine-managed session state (zoom + last workspace) to
    /// `~/.aether/state.json` so the next launch restores it.
    fn persist_state(&self) {
        // A folder-less window (File → New Window) must not clobber the remembered
        // workspace — keep whatever the last real folder was.
        let mut st = state::State::load();
        if !self.cwd.as_os_str().is_empty() {
            st.last_workspace = Some(self.cwd.clone());
            st.touch_recent(&self.cwd);
        }
        st.zoom = Some(theme::ui_zoom());
        st.save();
    }

    /// File > Open Recent — quick-pick of recently-opened folders.
    fn open_recent_picker(&mut self) {
        let recents = state::State::load().recent;
        let items: Vec<commands::PickItem> = recents
            .iter()
            .filter(|p| p.is_dir())
            .map(|p| {
                let name = p.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
                commands::PickItem::new(p.to_string_lossy().into_owned(), name)
            })
            .collect();
        if items.is_empty() {
            return self.show_info_dialog("No recent folders yet.");
        }
        self.palette.open_quick_pick(commands::PickKind::OpenRecent, items);
        if let Some(g) = self.gpu.as_mut() {
            g.ui.palette_input.clear(&mut g.font_system);
            g.ui.palette_input.focus(true);
        }
        self.redraw();
    }

    /// File > Revert File — reload the active document from disk, discarding
    /// unsaved changes (one undoable step, like VSCode).
    fn revert_file(&mut self) {
        let Some(path) = self.workspace.active_doc().and_then(|d| d.path.clone()) else {
            return;
        };
        let Ok(text) = std::fs::read_to_string(&path) else {
            return self.show_info_dialog("Couldn't read the file from disk.");
        };
        if let Some(gpu) = self.gpu.as_mut() {
            if let Some(d) = self.workspace.active_doc_mut() {
                d.set_text_external(&text, &mut gpu.font_system);
            }
        }
        self.redraw();
    }

    /// File > Close Folder — back to a folder-less window (open editors are kept,
    /// like VSCode; terminals are released to the daemon as orphans).
    fn close_folder(&mut self) {
        if self.cwd.as_os_str().is_empty() {
            return;
        }
        self.open_folder_less();
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
        // Rescale the draggable panels so they keep their proportion at the new zoom
        // (the sidebar/terminal splitters store raw pixels set at zoom 1).
        let prev = theme::ui_zoom();
        if prev > 0.0 {
            self.sidebar_split.scale(zoom / prev);
            self.right_split.scale(zoom / prev);
        }
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
            g.ui.find.reshape(&mut g.font_system);
            g.ui.diff_chev_down.reshape(&mut g.font_system); // fold + combined-diff chevrons
            g.ui.diff_chev_right.reshape(&mut g.font_system);
            g.ui.diff_unfold.reshape(&mut g.font_system); // diff gap expand-all button
            g.ui.diff_stage.reshape(&mut g.font_system); // per-block stage/unstage/revert
            g.ui.diff_unstage.reshape(&mut g.font_system);
            g.ui.diff_revert.reshape(&mut g.font_system);
            g.ui.block_tip.reshape(&mut g.font_system);
            for ib in g.ui.tab_icons.values_mut() {
                ib.reshape(&mut g.font_system);
            }
            g.ui.sidebar.reshape_icons(&mut g.font_system); // explorer tree icons
            g.create_input.rezoom(&mut g.font_system);
            for b in g.create_icons.iter_mut() {
                b.reshape(&mut g.font_system); // inline new-file/folder row icons
            }
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
        if let (Some(dp), Some(g)) = (self.debug.as_mut(), self.gpu.as_mut()) {
            dp.reshape(&mut g.font_system);
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

    /// Drive language-server document sync from the idle tick (delegated to the
    /// manager, which owns the open/change/pull logic).
    fn sync_lsp(&mut self) {
        let _probe = crate::perf::Probe::new("sync_lsp", 8);
        // Language servers come only from Aether's own store (+ PATH for standalone
        // binaries). We deliberately do NOT scan the user's VS Code extensions.
        let Some(ext_dir) = crate::extensions::extensions_dir() else { return };
        self.lsp.sync(&mut self.workspace.documents, &self.cwd, &[ext_dir], &self.worker_tx);
    }

    fn apply_settings(&mut self) {
        let s = settings::reload();
        self.sidebar_visible = s.workbench_sidebar_visible;
        self.apply_theme_by_name(&s.workbench_color_theme);
        // files.exclude / explorer.excludeGitIgnore changes affect tree visibility.
        self.workspace.tree.refresh();
        if let Some(gpu) = self.gpu.as_mut() {
            gpu.ui.line_numbers.invalidate();
            for d in self.workspace.documents.iter_mut() {
                d.reshape(&mut gpu.font_system);
            }
        }
        self.redraw();
    }

    /// Apply a color theme by its `workbench.colorTheme` name. "Aether Dark" is the
    /// built-in default; other names match against installed theme extensions.
    fn apply_theme_by_name(&self, name: &str) {
        if name.eq_ignore_ascii_case("Aether Dark") || name.is_empty() {
            theme::set(theme::Theme::dark());
            return;
        }
        // Match the saved name against any contributed theme's label (VS Code keys
        // `workbench.colorTheme` by the theme label, not the extension name).
        for e in &self.extensions {
            for t in &e.themes {
                if t.label.eq_ignore_ascii_case(name) {
                    if let Some(theme) = theme::load_vscode(&t.path) {
                        theme::set(theme);
                        return;
                    }
                }
            }
        }
        // Unknown theme name — keep the current theme.
    }

    /// Open the Settings editor modal.
    fn open_settings_editor(&mut self) {
        self.settings_editor.open = true;
        self.settings_editor.edit_key = None;
        if let Some(g) = self.gpu.as_mut() {
            g.ui.settings_search.set_text(&mut g.font_system, &self.settings_editor.query);
            g.ui.settings_search.focus(true);
        }
        self.redraw();
    }

    /// Close the Settings editor modal.
    fn close_settings_editor(&mut self) {
        self.settings_editor.open = false;
        self.settings_editor.edit_key = None;
        if let Some(g) = self.gpu.as_mut() {
            g.ui.settings_search.focus(false);
            g.ui.settings_input.focus(false);
        }
        self.redraw();
    }

    /// Commit an inline number/text edit to settings.json and re-apply.
    fn commit_settings_input(&mut self) {
        let Some(key) = self.settings_editor.edit_key.take() else { return };
        let Some(def) = ui::settings_editor::SCHEMA.iter().find(|d| d.key == key) else { return };
        let raw = self.gpu.as_ref().map(|g| g.ui.settings_input.text().trim().to_string()).unwrap_or_default();
        let value_json = match def.control {
            ui::settings_editor::Control::Number => {
                // Accept integers; ignore garbage so a stray keystroke can't corrupt the file.
                match raw.parse::<f64>() {
                    Ok(n) => format!("{}", n as i64),
                    Err(_) => return,
                }
            }
            _ => format!("{:?}", raw), // quoted JSON string
        };
        settings::set_user_value(key, &value_json);
        self.apply_settings();
        if let Some(g) = self.gpu.as_mut() {
            g.ui.settings_input.focus(false);
        }
        self.redraw();
    }

    /// Resolve the mouse cursor over the open Settings modal.
    fn settings_cursor(&self, p: (f32, f32)) -> CursorIcon {
        use ui::settings_editor as se;
        let (sw, sh) = self.gpu.as_ref().map_or((1280.0, 800.0), |g| (g.config.width as f32, g.config.height as f32));
        let lay = se::layout(Rect { x: 0.0, y: 0.0, w: sw, h: sh });
        if !lay.card.contains(p) {
            return CursorIcon::Default; // scrim
        }
        if lay.search.contains(p) {
            return CursorIcon::Text;
        }
        if lay.close.contains(p) {
            return CursorIcon::Pointer;
        }
        if self.settings_editor.nav_cache.iter().any(|(r, _)| r.contains(p)) {
            return CursorIcon::Pointer;
        }
        for h in &self.settings_editor.rows_cache {
            if h.control.contains(p) {
                return match se::SCHEMA[h.idx].control {
                    se::Control::Number | se::Control::Text => CursorIcon::Text,
                    _ => CursorIcon::Pointer,
                };
            }
        }
        CursorIcon::Default
    }

    /// Apply an `Action` returned by the Settings editor's click handler.
    fn apply_settings_action(&mut self, action: ui::settings_editor::Action) {
        use ui::settings_editor::Action;
        match action {
            Action::Close => self.close_settings_editor(),
            Action::Navigate => self.redraw(),
            Action::Toggle(key) => {
                let cur = settings::value_json(key);
                let next = if cur == "true" { "false" } else { "true" };
                settings::set_user_value(key, next);
                self.apply_settings();
                self.redraw();
            }
            Action::OpenEnum(key, rect) => {
                let Some(def) = ui::settings_editor::SCHEMA.iter().find(|d| d.key == key) else { return };
                if let ui::settings_editor::Control::Enum(opts) = def.control {
                    let items: Vec<CtxEntry> = opts
                        .iter()
                        .map(|(val, label)| {
                            CtxEntry::new(*label, CtxAction::SetSetting(key, format!("{:?}", val)))
                        })
                        .collect();
                    self.open_ctx_menu((rect.x, rect.y + rect.h), items);
                }
            }
            Action::EditText(key) => {
                // Seed the inline input with the current value (strip surrounding quotes).
                let cur = settings::value_json(key);
                let seed = cur.trim_matches('"').to_string();
                self.settings_editor.edit_key = Some(key);
                if let Some(g) = self.gpu.as_mut() {
                    g.ui.settings_search.focus(false);
                    g.ui.settings_input.set_text(&mut g.font_system, &seed);
                    g.ui.settings_input.select_all();
                    g.ui.settings_input.focus(true);
                }
                self.redraw();
            }
            Action::OpenTheme(rect) => {
                // Open the theme list as an in-modal dropdown (the menu draws above the
                // settings card now) so picking a theme applies in place — no eject to
                // the command palette.
                let mut labels: Vec<String> = vec!["Aether Dark".to_string()];
                for e in &self.extensions {
                    for t in &e.themes {
                        if !labels.iter().any(|l| l.eq_ignore_ascii_case(&t.label)) {
                            labels.push(t.label.clone());
                        }
                    }
                }
                let items: Vec<CtxEntry> = labels
                    .into_iter()
                    .map(|label| {
                        let value = format!("{label:?}");
                        CtxEntry::new(label, CtxAction::SetSetting("workbench.colorTheme", value))
                    })
                    .collect();
                self.open_ctx_menu((rect.x, rect.y + rect.h), items);
            }
        }
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
                // Paste is its own undo step (stops before and after, like VSCode).
                d.break_undo_group();
                d.insert_str(&text, &mut gpu.font_system);
                d.break_undo_group();
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

    /// VSCode-style selection highlight: find every occurrence of the current
    /// word-like selection so the renderer can box them + mark the scrollbar.
    /// Cached on (selection text, doc version); cleared while the find widget owns
    /// the highlights.
    pub(crate) fn recompute_selection_highlight(&mut self) {
        if self.find.active {
            self.sel_matches.clear();
            self.sel_hl_text.clear();
            self.sel_hl_version = -1;
            return;
        }
        let (text, version) = match self.workspace.active_doc() {
            Some(d) if !d.sel.is_empty() && d.diff.is_none() => {
                let (lo, hi) = d.sel.range();
                let s = d.rope.byte_slice(lo..hi).to_string();
                let ok = !s.contains('\n') && !s.trim().is_empty() && s.trim() == s && s.chars().count() <= 200;
                (if ok { s } else { String::new() }, d.version)
            }
            _ => (String::new(), -1),
        };
        if text == self.sel_hl_text && version == self.sel_hl_version {
            return; // unchanged since last frame
        }
        self.sel_hl_text = text.clone();
        self.sel_hl_version = version;
        self.sel_matches.clear();
        if text.is_empty() {
            return;
        }
        if let Some(hay) = self.workspace.active_doc().map(|d| d.rope.to_string()) {
            let mut start = 0;
            while let Some(rel) = hay[start..].find(&text) {
                let abs = start + rel;
                self.sel_matches.push((abs, abs + text.len()));
                start = abs + text.len();
            }
            // A lone occurrence (just the selection itself) isn't worth highlighting.
            if self.sel_matches.len() < 2 {
                self.sel_matches.clear();
            }
        }
    }

    /// Rebuild the match list for the current query + options against the active
    /// doc, refresh the count, and select the match at/after the caret.
    fn recompute_find(&mut self) {
        let query = self.gpu.as_ref().map(|g| g.ui.find.query.text().to_string()).unwrap_or_default();
        self.find.matches.clear();
        self.find.index = None;
        let (text, caret) = match self.workspace.active_doc() {
            Some(d) => (d.rope.to_string(), d.sel.head),
            None => {
                self.update_find_count();
                return;
            }
        };
        if !query.is_empty() {
            if let Some(re) = crate::search::build_regex(&query, self.find.opts) {
                let mut start = 0usize;
                while start <= text.len() {
                    match re.find_from_pos(&text, start) {
                        Ok(Some(m)) => {
                            self.find.matches.push((m.start(), m.end()));
                            start = if m.end() > m.start() { m.end() } else { m.end() + 1 };
                        }
                        _ => break,
                    }
                }
            }
            if !self.find.matches.is_empty() {
                let idx = self.find.matches.iter().position(|&(s, _)| s >= caret).unwrap_or(0);
                self.find.index = Some(idx);
                self.select_find_match(idx);
            }
        }
        self.update_find_count();
    }

    fn update_find_count(&mut self) {
        let empty_query = self.gpu.as_ref().map(|g| g.ui.find.query.text().is_empty()).unwrap_or(true);
        let text = if !self.find.matches.is_empty() {
            let i = self.find.index.map(|i| i + 1).unwrap_or(0);
            format!("{} of {}", i, self.find.matches.len())
        } else if empty_query {
            String::new()
        } else {
            "No results".to_string()
        };
        if let Some(g) = self.gpu.as_mut() {
            g.ui.find.set_count(&mut g.font_system, &text);
        }
    }

    fn select_find_match(&mut self, i: usize) {
        let Some(&(s, e)) = self.find.matches.get(i) else { return };
        if let Some(d) = self.workspace.active_doc_mut() {
            d.sel.anchor = s;
            d.sel.head = e;
            d.sel.desired_col = None;
            // If the match sits inside a collapsed fold, expand it so it's visible.
            let line = d.rope.byte_to_line(s.min(d.rope.len_bytes()));
            if d.is_line_hidden(line) {
                d.reveal_line(line);
            }
        }
        self.ensure_cursor_visible();
    }

    fn find_step(&mut self, forward: bool) {
        if self.find.matches.is_empty() {
            self.recompute_find();
        }
        let n = self.find.matches.len();
        if n == 0 {
            return;
        }
        let cur = self.find.index.unwrap_or(0);
        let next = if forward { (cur + 1) % n } else { (cur + n - 1) % n };
        self.find.index = Some(next);
        self.select_find_match(next);
        self.update_find_count();
        self.redraw();
    }

    /// Replace the current match with the replace field's text, then advance.
    fn replace_current(&mut self) {
        let Some(i) = self.find.index else { return };
        let Some(&(s, e)) = self.find.matches.get(i) else { return };
        let repl = self.gpu.as_ref().map(|g| g.ui.find.replace.text().to_string()).unwrap_or_default();
        if let (Some(g), Some(d)) = (self.gpu.as_mut(), self.workspace.active_doc_mut()) {
            d.sel.anchor = s;
            d.sel.head = e;
            d.sel.desired_col = None;
            d.insert_str(&repl, &mut g.font_system);
        }
        self.recompute_find();
        self.refresh_source_control();
        self.redraw();
    }

    /// Replace every match (back-to-front so earlier byte offsets stay valid).
    fn replace_all(&mut self) {
        let repl = self.gpu.as_ref().map(|g| g.ui.find.replace.text().to_string()).unwrap_or_default();
        let matches = self.find.matches.clone();
        if matches.is_empty() {
            return;
        }
        if let (Some(g), Some(d)) = (self.gpu.as_mut(), self.workspace.active_doc_mut()) {
            for &(s, e) in matches.iter().rev() {
                d.sel.anchor = s;
                d.sel.head = e;
                d.sel.desired_col = None;
                d.insert_str(&repl, &mut g.font_system);
            }
        }
        self.recompute_find();
        self.refresh_source_control();
        self.redraw();
    }

    /// Close the find/replace widget and return focus to the editor.
    fn close_find(&mut self) {
        self.find.active = false;
        self.find.focused = false;
        if let Some(g) = self.gpu.as_mut() {
            g.ui.find.query.focus(false);
            g.ui.find.replace.focus(false);
        }
        self.redraw();
    }

    /// Handle a mouse press inside the find/replace widget panel.
    fn on_find_press(&mut self, p: (f32, f32), fl: &ui::find_widget::FindLayout, double: bool) {
        use ui::find_widget::FindBtn;
        if let Some(btn) = self.gpu.as_ref().and_then(|g| g.ui.find.button_at(fl, p)) {
            match btn {
                FindBtn::Expand => {
                    self.find.replace_open = !self.find.replace_open;
                    if self.find.replace_open {
                        self.find.focused = true;
                        self.find.on_replace = true;
                        if let Some(g) = self.gpu.as_mut() {
                            g.ui.find.replace.focus(true);
                            g.ui.find.query.focus(false);
                        }
                    }
                }
                FindBtn::Case => {
                    self.find.opts.case_sensitive = !self.find.opts.case_sensitive;
                    self.recompute_find();
                }
                FindBtn::Word => {
                    self.find.opts.whole_word = !self.find.opts.whole_word;
                    self.recompute_find();
                }
                FindBtn::Regex => {
                    self.find.opts.regex = !self.find.opts.regex;
                    self.recompute_find();
                }
                FindBtn::Prev => self.find_step(false),
                FindBtn::Next => self.find_step(true),
                FindBtn::Close => self.close_find(),
                FindBtn::Replace => self.replace_current(),
                FindBtn::ReplaceAll => self.replace_all(),
            }
            return;
        }
        // Click in one of the inputs: focus it + place caret + start drag-select.
        let (rect, on_replace) = if fl.replace_input.map_or(false, |r| r.contains(p)) {
            (fl.replace_input.unwrap(), true)
        } else if fl.find_text.contains(p) {
            (fl.find_text, false)
        } else {
            return; // panel chrome — keep current focus
        };
        self.find.active = true;
        self.find.focused = true;
        self.find.on_replace = on_replace;
        let pad = theme::zpx(6.0);
        if let Some(g) = self.gpu.as_mut() {
            let inp = if on_replace { &mut g.ui.find.replace } else { &mut g.ui.find.query };
            if double {
                inp.select_word_at(rect, pad, p.0);
            } else {
                inp.set_caret_from_x(rect, pad, p.0);
            }
            inp.focus(true);
            let other = if on_replace { &mut g.ui.find.query } else { &mut g.ui.find.replace };
            other.focus(false);
        }
        self.find_drag = Some(on_replace);
    }

    // ---- Input dispatch ----

    fn title_btn_at(&self, x: f32, y: f32, layout: &Layout) -> Option<usize> {
        if cfg!(target_os = "macos") {
            return None; // native traffic lights handle window controls on macOS
        }
        layout.title_btn_rects().iter().position(|r| r.contains((x, y)))
    }

    fn on_mouse_press(&mut self, x: f32, y: f32) {
        let layout = self.layout();

        // Any click dismisses the completion popup (VSCode behavior) — the click
        // itself still lands wherever it was aimed.
        if self.completion.active {
            self.completion.close();
            self.redraw();
        }

        // Generic context menu: a click selects its item or dismisses it.
        if let Some((anchor, entries)) = self.ctx_menu.clone() {
            let hit = self.gpu.as_ref().and_then(|g| {
                let win = (g.config.width as f32, g.config.height as f32);
                let rect = g.ui.ctx.rect(anchor, win);
                g.ui.ctx.item_at(rect, (x, y))
            });
            self.close_ctx_menu();
            if let Some(i) = hit {
                if let Some(e) = entries.get(i) {
                    self.exec_ctx_action(e.action.clone());
                }
            }
            self.redraw();
            return;
        }

        // Breadcrumb dropdown (folder-contents popup) is a transient overlay: a click
        // inside it selects an entry (file → open, folder → drill in); a click outside
        // dismisses it. A click on a breadcrumb segment opens/toggles its dropdown.
        if self.gpu.as_ref().map_or(false, |g| g.ui.breadcrumbs.is_open()) {
            let win = self.gpu.as_ref().map(|g| (g.config.width as f32, g.config.height as f32)).unwrap_or((1280.0, 800.0));
            let hit = self.gpu.as_ref().and_then(|g| {
                g.ui.breadcrumbs.dropdown_rect(layout.breadcrumbs, win)
                    .filter(|r| r.contains((x, y)))
                    .and_then(|r| g.ui.breadcrumbs.dropdown_item_at(r, (x, y)))
            });
            if let Some(i) = hit {
                if let Some((path, is_dir)) = self.gpu.as_ref().and_then(|g| g.ui.breadcrumbs.entry(i)) {
                    if is_dir {
                        if let Some(g) = self.gpu.as_mut() {
                            g.ui.breadcrumbs.drill(&mut g.font_system, path);
                        }
                    } else {
                        if let Some(g) = self.gpu.as_mut() { g.ui.breadcrumbs.close(); }
                        self.open_file_at(path, 1, 0);
                    }
                }
                self.redraw();
                return;
            }
            // Click outside the dropdown closes it (then falls through so a click on
            // another segment can re-open).
            if let Some(g) = self.gpu.as_mut() { g.ui.breadcrumbs.close(); }
            self.redraw();
        }
        if layout.breadcrumbs.h > 0.0 && layout.breadcrumbs.contains((x, y)) {
            let seg = self.gpu.as_ref().and_then(|g| g.ui.breadcrumbs.segment_at(layout.breadcrumbs, (x, y)));
            if let Some(i) = seg {
                if let Some(g) = self.gpu.as_mut() {
                    g.ui.breadcrumbs.toggle(&mut g.font_system, i);
                }
                self.redraw();
            }
            return;
        }

        // The Settings editor is modal: handle it before any region handler so a
        // click outside the card dismisses it (rather than landing underneath).
        if self.settings_editor.open {
            let (sw, sh) = self.gpu.as_ref().map_or((1280.0, 800.0), |g| (g.config.width as f32, g.config.height as f32));
            let lay = ui::settings_editor::layout(Rect { x: 0.0, y: 0.0, w: sw, h: sh });
            let p = (x, y);
            if !lay.card.contains(p) {
                self.close_settings_editor();
                return;
            }
            let clicks = self.register_click_count(x, y);
            // Search box: focus + place caret.
            if lay.search.contains(p) {
                self.settings_editor.edit_key = None;
                if let Some(g) = self.gpu.as_mut() {
                    g.ui.settings_input.focus(false);
                    g.ui.settings_search.focus(true);
                    g.ui.settings_search.on_click(lay.search, theme::zpx(10.0), x, y, clicks);
                }
                self.redraw();
                return;
            }
            // An inline number/text edit in progress: clicks inside its box edit it,
            // clicks elsewhere commit it first.
            if let Some(key) = self.settings_editor.edit_key {
                let hit = self
                    .settings_editor
                    .rows_cache
                    .iter()
                    .find(|h| ui::settings_editor::SCHEMA[h.idx].key == key)
                    .copied();
                if let Some(h) = hit {
                    if h.control.contains(p) {
                        if let Some(g) = self.gpu.as_mut() {
                            g.ui.settings_input.on_click(h.control, theme::zpx(8.0), x, y, clicks);
                        }
                        self.redraw();
                        return;
                    }
                }
                self.commit_settings_input();
            }
            if let Some(act) = self.settings_editor.on_click(&lay, p) {
                self.apply_settings_action(act);
            } else if let Some(g) = self.gpu.as_mut() {
                g.ui.settings_search.focus(false);
            }
            return;
        }

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

        // Clicking the docs link in the diagnostic hover card opens the rule's page.
        if self.hover_tip.is_some() {
            if let Some((url, hit)) = self.gpu.as_ref().and_then(|g| {
                let (_, ax, ay) = self.hover_tip.as_ref().unwrap();
                let screen = crate::widgets::Rect { x: 0.0, y: 0.0, w: g.config.width as f32, h: g.config.height as f32 };
                let card = g.ui.diag_hover.rect((*ax, *ay), screen);
                g.ui.diag_hover.link_rect(card).map(|lr| (g.ui.diag_hover.href().map(str::to_string), lr.contains((x, y))))
            }) {
                if hit {
                    if let Some(url) = url {
                        open_url(&url);
                    }
                    return;
                }
            }
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

        // Command palette is modal: handle it before any region handler (terminal,
        // editor, sidebar) so a click outside it dismisses it instead of being
        // swallowed by whatever is underneath (e.g. the terminal panel).
        if let Some(pal) = layout.palette.as_ref() {
            // The input is the title-bar pill now — clicking it keeps the palette open.
            if !pal.box_.contains((x, y)) && !pal.input.contains((x, y)) {
                self.palette_restore_preview();
                self.palette.close();
                self.redraw();
                return;
            }
            let row = self
                .gpu
                .as_ref()
                .and_then(|gpu| gpu.ui.palette_list.row_at_scrolled(pal.list, self.palette.scroll, (x, y), self.palette.filtered.len()));
            if let Some(idx) = row {
                self.palette.selected = idx;
                self.commit_palette();
                self.redraw();
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
            } else if self.workspace.active_doc().map_or(false, |d| d.diff.is_some()) {
                // Diff: per-pane horizontal scrollbar drag, then the shared vertical bar.
                let (_, lt, _, rt) = render::diff_pane_rects(render::editor_region(&layout));
                if let Some(d) = self.workspace.active_doc_mut() {
                    if d.diff_hbar_press((x, y), 1, crate::document::Document::diff_htrack(rt), rt.w)
                        || d.diff_hbar_press((x, y), 0, crate::document::Document::diff_htrack(lt), lt.w)
                        || d.scroll.press((x, y))
                    {
                        self.redraw();
                        return;
                    }
                }
            } else if let Some(d) = self.workspace.active_doc_mut() {
                if d.scroll.press((x, y)) {
                    self.redraw();
                    return;
                }
            }
        }

        // Info tab (Welcome / Tips / …): a click in the page fires the link row
        // under it; everything else is inert (no caret in a designed page).
        if self.detail.open_extension.is_none()
            && self.workspace.active_doc().map_or(false, |d| d.info.is_some())
        {
            let region = render::editor_region(&layout);
            if region.contains((x, y)) {
                let body = ui::info_page::InfoPage::body(region);
                let action = self.workspace.active_doc().and_then(|d| {
                    d.info.as_ref().and_then(|page| {
                        page.links(body, d.scroll_y())
                            .into_iter()
                            .find(|(r, _)| r.contains((x, y)))
                            .map(|(_, a)| a)
                    })
                });
                match action {
                    Some(ui::info_page::Action::Url(url)) => open_url(&url),
                    Some(ui::info_page::Action::OpenFolder(p)) => self.open_folder(p),
                    None => {}
                }
                return;
            }
        }

        // Markdown preview: a click on a link opens it (http → browser); otherwise
        // the page is inert (read-only, no caret).
        if self.detail.open_extension.is_none()
            && self.workspace.active_doc().map_or(false, |d| d.markdown_preview.is_some())
        {
            let region = render::editor_region(&layout);
            if region.contains((x, y)) {
                let body = ui::info_page::InfoPage::body(region);
                let url = self.gpu.as_ref().and_then(|g| {
                    self.workspace.active_doc().and_then(|d| {
                        d.markdown_preview.as_ref().and_then(|md| {
                            md.link_geometry(body, d.scroll_y(), &|k| g.media.size(k))
                                .into_iter()
                                .find(|(r, _)| r.contains((x, y)))
                                .map(|(_, u)| u)
                        })
                    })
                });
                if let Some(url) = url {
                    if url.starts_with("http://") || url.starts_with("https://") {
                        open_url(&url);
                    }
                }
                return;
            }
        }

        // Binary / unsupported-file placeholder: "Open Anyway" reloads the file as
        // lossy UTF-8 text; clicks elsewhere in the overlay are inert.
        if self.detail.open_extension.is_none()
            && self.workspace.active_doc().map_or(false, |d| d.binary)
        {
            let region = render::editor_region(&layout);
            if region.contains((x, y)) {
                let hit = self
                    .gpu
                    .as_ref()
                    .map_or(false, |g| g.ui.binary_placeholder.hit_button(region, (x, y)));
                if hit {
                    if let (Some(g), Some(d)) = (self.gpu.as_mut(), self.workspace.active_doc_mut()) {
                        d.open_anyway(&mut g.font_system);
                    }
                    self.redraw();
                }
                return;
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

        // Bottom-panel tab bar (PROBLEMS / OUTPUT / DEBUG CONSOLE / TERMINAL / PORTS):
        // clicking a tab switches the active view.
        if self.terminal.visible {
            if let Some(panel) = layout.terminal_panel {
                let header = Rect { x: panel.x, y: panel.y, w: panel.w, h: theme::TERMINAL_HEADER_H() };
                if header.contains((x, y)) {
                    let hit = self.gpu.as_ref().and_then(|g| {
                        render::panel_tab_rects(header, &g.terminal_tabs).iter().position(|r| r.contains((x, y)))
                    });
                    if let Some(i) = hit {
                        self.panel_tab = i;
                        self.redraw();
                        return;
                    }
                }
            }
        }

        // Terminal panel: header buttons, tab list, and pane-focus — the panel owns
        // its region's press handling. Clicking elsewhere while visible drops focus
        // (handled inside) without consuming the click.
        // Count consecutive clicks for word/line selection, but only when the press
        // lands in the terminal panel (so it consumes the click — no double-counting
        // with the input/title handlers below).
        // Ctrl/Cmd+click a URL in the terminal → open it in the browser (don't start
        // a selection).
        let link_mod = self.mods.control_key() || (cfg!(target_os = "macos") && self.mods.super_key());
        if link_mod {
            if let Some(target) = self.terminal.url_at((x, y), &layout, self.terminal_cell_w) {
                let low = target.to_lowercase();
                if low.starts_with("http://") || low.starts_with("https://") || low.starts_with("file://") {
                    open_url(&target);
                } else {
                    // A filesystem path: open files in the editor; for a folder,
                    // drop the path into the quick-open palette so the user can drill in.
                    let path = PathBuf::from(&target);
                    if path.is_dir() {
                        self.open_palette_with(&target);
                    } else {
                        self.nav.mark(&self.workspace);
                        self.open_file_at(path, 1, 0);
                    }
                }
                return;
            }
        }
        let term_clicks = if layout.terminal_panel.map_or(false, |p| p.contains((x, y))) {
            self.register_click(x, y);
            self.click_streak
        } else {
            1
        };
        if self.terminal.content_press((x, y), &layout, self.terminal_cell_w, term_clicks) {
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
                };
                if double {
                    inp.select_word_at(rect, pad, x);
                } else {
                    inp.set_caret_from_x(rect, pad, x);
                }
                inp.focus(true);
            }
            self.text_drag = Some(id);
            self.redraw();
            return;
        }

        // Find/replace widget: buttons, input focus + caret, drag-select.
        if self.find.active {
            let er = crate::render::editor_region(&layout);
            let fl = ui::find_widget::FindWidget::layout(er, self.find.replace_open);
            if fl.panel.contains((x, y)) {
                let double = self.register_click(x, y);
                self.on_find_press((x, y), &fl, double);
                self.redraw();
                return;
            }
        }

        // Sidebar resize handle — let the Splitter claim the press.
        if self.sidebar_visible
            && layout.palette.is_none()
            && self.sidebar_split.press((x, y), layout.sidebar)
        {
            return;
        }
        // Right (AI chat) sidebar: resize handle, then the panel's own clicks
        // (input focus / scrollbar).
        if self.right_sidebar_visible && layout.palette.is_none() {
            if self.right_split.press((x, y), layout.right_sidebar) {
                return;
            }
            let handled = self.chat.as_mut().map_or(false, |c| c.on_press((x, y), layout.right_sidebar));
            if handled {
                self.redraw();
                return;
            }
        }
        // Explorer OUTLINE section: header click toggles, body clicks jump.
        if layout.palette.is_none() {
            if let Some(hdr) = layout.outline_header_rect() {
                if hdr.contains((x, y)) {
                    self.outline_open = !self.outline_open;
                    self.redraw();
                    return;
                }
                if let Some(body) = layout.outline_body_rect().filter(|b| b.contains((x, y))) {
                    let region = widgets::Rect { x: hdr.x, y: hdr.y, w: hdr.w, h: hdr.h + body.h };
                    let line = self.outline.as_mut().and_then(|o| o.on_press((x, y), region));
                    if let Some(line) = line {
                        self.nav.mark(&self.workspace);
                        self.goto_line(line);
                    }
                    self.redraw();
                    return;
                }
            }
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
                    2 => {
                        self.right_sidebar_visible = !self.right_sidebar_visible;
                        self.redraw();
                    }
                    _ => {}
                }
                return;
            }
            // Menu titles open their dropdown (custom menu bar is non-macOS only).
            if !cfg!(target_os = "macos") {
                if let Some(idx) = self
                    .gpu
                    .as_ref()
                    .and_then(|g| g.menubar.item_at(layout.menu_bar_rect(), (x, y)))
                {
                    self.open_app_menu(idx);
                    return;
                }
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
                    // Custom close button (non-macOS chrome): same busy-terminal gate
                    // as the native close.
                    if self.confirm_close_window() {
                        self.pending_close = true;
                    }
                }
                _ => {
                    if let Some(g) = self.gpu.as_ref() {
                        let _ = g.window.drag_window();
                    }
                }
            }
            return;
        }

        if layout.status_bar.contains((x, y)) {
            // Branch indicator (far left): open the checkout quick-pick. Geometry must
            // match render.rs's status-bar branch block.
            let branch_block_w = self
                .gpu
                .as_ref()
                .filter(|g| g.ui.branch.width() > 0.0)
                .map(|g| {
                    let icon_x = layout.status_bar.x + theme::zpx(10.0);
                    let name_x = icon_x + g.ui.branch_icon.width() + theme::zpx(2.0);
                    (name_x + g.ui.branch.width() + theme::zpx(12.0)) - layout.status_bar.x
                })
                .unwrap_or(0.0);
            if branch_block_w > 0.0 && x < layout.status_bar.x + branch_block_w {
                self.open_branch_pick(commands::PickKind::Checkout);
                return;
            }
            // Encoding cell (left of the zoom controls): open the encoding picker.
            let enc_hit = self.workspace.active_doc().is_some()
                && self.gpu.as_ref().map_or(false, |g| {
                    render::encoding_cell(layout.status_bar, g.ui.encoding.width()).contains((x, y))
                });
            if enc_hit {
                self.open_encoding_pick();
                return;
            }
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
                3 => Some(SidebarView::Debug),
                4 => Some(SidebarView::Extensions),
                _ => None,
            };
            if let Some(v) = view {
                if v == SidebarView::Extensions {
                    if self.extensions.is_empty() {
                        self.extensions = extensions::scan();
                    }
                    // Always rebuild: the list may have changed (install/uninstall)
                    // since the panel last drew, and the guard above can leave stale rows.
                    self.rebuild_ext_rows();
                }
                if v == SidebarView::SourceControl {
                    if let (Some(scp), Some(g)) = (self.source_control.as_mut(), self.gpu.as_mut()) {
                        scp.refresh(&mut g.font_system);
                    }
                }
                if v == SidebarView::Debug {
                    self.refresh_debug_configs();
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
            // Bottom gear (idx 6): the "Manage" menu, VSCode-style.
            if idx == 6 {
                use menus::MenuCmd;
                let r = layout.activity_rects()[6];
                let items = vec![
                    CtxEntry::key("Command Palette…", CtxAction::Palette, "Ctrl+Shift+P"),
                    CtxEntry::sep(),
                    CtxEntry::new("Profiles", CtxAction::Stub("Profiles")),
                    CtxEntry::new("Settings", CtxAction::Command(Command::OpenSettings)),
                    CtxEntry::key("Extensions", CtxAction::MenuCmd(MenuCmd::ShowExtensions), "Shift+Ctrl+X"),
                    CtxEntry::new("Keyboard Shortcuts", CtxAction::MenuCmd(MenuCmd::ShortcutsRef)),
                    CtxEntry::new("Snippets", CtxAction::Stub("Snippets")),
                    CtxEntry::new("Tasks", CtxAction::Stub("Tasks")),
                    CtxEntry::new("Color Theme", CtxAction::Command(Command::ColorTheme)),
                    CtxEntry::sep(),
                    CtxEntry::new("Backup and Sync Settings…", CtxAction::Stub("Backup and Sync Settings")),
                    CtxEntry::new("Check for Updates…", CtxAction::MenuCmd(MenuCmd::CheckUpdate)),
                ];
                // Anchor just right of the gear; the menu renderer clamps it on-screen.
                self.open_ctx_menu((r.x + r.w + theme::zpx(4.0), r.y), items);
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
            let clicks = self.register_click_count(x, y);
            let mut intents = Vec::new();
            let consumed = self
                .extensions_panel
                .as_mut()
                .map_or(false, |ep| ep.on_press((x, y), region, clicks, &mut intents));
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
            let region = layout.sidebar;
            let clicks = self.register_click_count(x, y);
            let root = self.cwd.clone();
            let mut intents = Vec::new();
            let mut consumed = false;
            if let (Some(sp), Some(g)) = (self.search.as_mut(), self.gpu.as_mut()) {
                consumed = sp.on_press(
                    (x, y),
                    region,
                    clicks,
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
            let region = layout.panel_region();
            // Count consecutive clicks so the commit box gets double/triple-click
            // word + select-all selection.
            self.register_click(x, y);
            let clicks = self.click_streak;
            let mut intents = Vec::new();
            let graph_before = self.source_control.as_ref().map(|scp| scp.graph_open());
            let consumed = self
                .source_control
                .as_mut()
                .map_or(false, |scp| scp.on_press((x, y), region, clicks, &mut intents));
            // The GRAPH accordion toggles inside on_press (no Intent); persist if it changed.
            if let (Some(before), Some(scp)) = (graph_before, self.source_control.as_ref()) {
                if before != scp.graph_open() {
                    let mut st = state::State::load();
                    st.scm_graph_open = scp.graph_open();
                    st.save();
                }
            }
            for i in intents {
                self.apply_intent(i);
            }
            if consumed {
                self.redraw();
                return;
            }
        }

        // Run & Debug: toolbar buttons / config selector / call-stack frames.
        if self.sidebar_visible
            && self.sidebar_view == SidebarView::Debug
            && layout.sidebar.contains((x, y))
        {
            let region = layout.panel_region();
            let mut intents = Vec::new();
            let consumed = if let (Some(dp), Some(g)) = (self.debug.as_mut(), self.gpu.as_mut()) {
                dp.on_press((x, y), region, &mut g.font_system, &mut intents)
            } else {
                false
            };
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
                    2 => {
                        self.workspace.tree.refresh();
                        self.invalidate_file_index();
                    }
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
                // Arm a drag-to-move; it activates only past a movement threshold,
                // so plain clicks behave exactly as before.
                self.tree_drag = Some((self.workspace.tree.nodes[idx].path.clone(), (x, y), false));
                let is_dir = self.workspace.tree.nodes[idx].is_dir;
                if is_dir {
                    self.workspace.tree.toggle(idx);
                }
                // Files open on release (a click that never became a drag), so a
                // drag-and-drop into the terminal isn't pre-empted by opening the file.
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
                    // Arm a drag-reorder (activates past a horizontal threshold).
                    self.tab_drag = Some((idx, (x, y), false));
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
                let (hit_install, hit_uninstall, hit_set_theme) = self
                    .gpu
                    .as_ref()
                    .map(|g| {
                        (
                            g.ui.ext_detail.hit_install(region, (x, y)),
                            g.ui.ext_detail.hit_uninstall(region, (x, y)),
                            g.ui.ext_detail.hit_set_theme(region, (x, y)),
                        )
                    })
                    .unwrap_or((false, false, false));
                if hit_install {
                    self.install_open();
                } else if hit_set_theme {
                    // Open the picker scoped to this extension's themes.
                    if let Some(OpenExt::Local(i)) = self.detail.open_extension {
                        self.open_theme_picker(Some(i));
                    }
                } else if hit_uninstall {
                    self.uninstall_open();
                }
                return;
            }
        }

        // Gutter click (left of the fold-chevron zone): toggle a breakpoint on that
        // line — fold-aware via the same visual-row→line mapping the chevron uses.
        if layout.gutter.contains((x, y))
            && x < layout.gutter.x + layout.gutter.w - theme::zpx(18.0)
            && self.workspace.active_doc().map_or(false, |d| d.diff.is_none() && d.info.is_none() && d.path.is_some())
        {
            let lh = theme::LINE_HEIGHT();
            let hit = self.workspace.active_doc_mut().and_then(|d| {
                let vy = y - (layout.editor_text.y + theme::EDITOR_PAD()) + d.scroll_y();
                let vidx = (vy / lh).max(0.0) as usize;
                let line = d.visible_index_to_line(vidx);
                d.path.clone().map(|p| (p, line))
            });
            if let Some((path, line)) = hit {
                self.debug_toggle_breakpoint(path, line);
                return;
            }
        }

        // Gutter fold chevron: clicking the right edge of the gutter on a foldable
        // line collapses/expands it.
        if layout.gutter.contains((x, y))
            && x >= layout.gutter.x + layout.gutter.w - theme::zpx(18.0)
            && self.workspace.active_doc().map_or(false, |d| d.diff.is_none())
        {
            let lh = theme::LINE_HEIGHT();
            if let Some(d) = self.workspace.active_doc_mut() {
                let vy = y - (layout.editor_text.y + theme::EDITOR_PAD()) + d.scroll_y();
                let vidx = (vy / lh).max(0.0) as usize;
                let line = d.visible_index_to_line(vidx);
                if d.is_foldable(line) {
                    d.toggle_fold(line);
                    self.redraw();
                    return;
                }
            }
        }

        // Single-file diff: a per-block Stage/Revert/Unstage button click.
        {
            let region = render::editor_region(&layout);
            if region.contains((x, y)) {
                if let Some((vbs, vbe)) = self.workspace.active_doc().and_then(|d| d.diff_block_at_y(region, y)) {
                    let staged = self.workspace.active_doc().map_or(false, |d| d.diff_staged);
                    let sy = self.workspace.active_doc().map_or(0.0, |d| d.scroll_y());
                    let count = if staged { 1 } else { 2 };
                    if let Some(rects) = render::diff_block_btn_rects(region, vbs, vbe, sy, count) {
                        if let Some(idx) = rects.iter().position(|r| r.contains((x, y))) {
                            self.apply_diff_block(vbs, staged, idx);
                            return;
                        }
                    }
                }
            }
        }

        // Single-file diff: pressing a collapsed-unchanged separator (its band OR the
        // unfold button in either gutter) arms a gap interaction — a plain click
        // expands the whole region, a drag reveals lines (down = top, up = bottom).
        // Diff rows are uniform-height so the row is derived arithmetically from y
        // (buffer.hit mis-maps the windowed diff buffer). Covers the full editor
        // region (gutters included), so it runs before the editor-text gate.
        {
            let region = render::editor_region(&layout);
            if region.contains((x, y)) {
                if let Some(gi) = self.workspace.active_doc().and_then(|d| d.diff_gap_at_y(region, y)) {
                    let (top, bot) = self
                        .workspace
                        .active_doc()
                        .and_then(|d| d.diff_gap_info(gi))
                        .map_or((0, 0), |(t, b, _)| (t, b));
                    self.gap_drag = Some((gi, y, top, bot, false));
                    return;
                }
            }
        }

        if layout.editor_text.contains((x, y)) {
            self.set_ext_filter_focus(false); // editor takes keyboard focus
            // Clicking the editor moves focus off the find widget (it stays open, but
            // typing now goes to the editor — like VSCode).
            if self.find.focused {
                self.find.focused = false;
                if let Some(g) = self.gpu.as_mut() {
                    g.ui.find.query.focus(false);
                    g.ui.find.replace.focus(false);
                }
            }
            if let Some(sp) = self.search.as_mut() {
                sp.set_unfocused();
            }
            if let Some(scp) = self.source_control.as_mut() {
                scp.set_unfocused();
            }
            let consecutive = self.register_click(x, y);
            let extend = self.mods.shift_key();
            // Empty editor (no files open): a double-click on the blank area opens a
            // new Untitled file, like VSCode's empty workbench.
            if consecutive && self.workspace.active_doc().is_none() && self.detail.open_extension.is_none() {
                self.exec_command(Command::NewFile);
                return;
            }
            // Combined diff: a click on a file header collapses/expands that file.
            let toggle = self.workspace.active_doc().and_then(|d| {
                d.diff_full.as_ref()?;
                let line = ui::editor_view::EditorView::line_at(d, &layout, x, y)?;
                d.diff_file_at_line(line)
            });
            if let Some(fidx) = toggle {
                if let (Some(d), Some(g)) = (self.workspace.active_doc_mut(), self.gpu.as_mut()) {
                    d.toggle_diff_file(fidx, &mut g.font_system);
                }
                self.redraw();
                return;
            }
            if let Some(d) = self.workspace.active_doc_mut() {
                self.editor.on_press(d, &layout, x, y, extend, consecutive);
            }
            self.redraw();
            return;
        }
    }

    fn on_mouse_move(&mut self, x: f32, y: f32) {
        // Settings editor: drag inside the focused field extends its selection. The
        // TextInput component applies its own drag dead-zone, so a double-click's
        // word selection survives the trailing move event.
        if self.settings_editor.open && self.mouse_pressed {
            if self.gpu.as_ref().map_or(false, |g| g.ui.settings_search.focused()) {
                let (sw, sh) = self.gpu.as_ref().map_or((1280.0, 800.0), |g| (g.config.width as f32, g.config.height as f32));
                let lay = ui::settings_editor::layout(Rect { x: 0.0, y: 0.0, w: sw, h: sh });
                if let Some(g) = self.gpu.as_mut() {
                    g.ui.settings_search.on_drag(lay.search, theme::zpx(10.0), x, y);
                }
                self.redraw();
                return;
            }
            if let Some(key) = self.settings_editor.edit_key {
                let ctrl = self.settings_editor.rows_cache.iter().find(|h| ui::settings_editor::SCHEMA[h.idx].key == key).map(|h| h.control);
                if let (Some(rect), Some(g)) = (ctrl, self.gpu.as_mut()) {
                    g.ui.settings_input.on_drag(rect, theme::zpx(8.0), x, y);
                    self.redraw();
                    return;
                }
            }
        }
        // Diff: drag a collapsed-unchanged separator to reveal lines progressively
        // (each line-height of travel reveals one hidden line from the top).
        if self.mouse_pressed {
            if let Some((gi, anchor_y, anchor_top, anchor_bot, _)) = self.gap_drag {
                let lh = theme::LINE_HEIGHT().max(1.0);
                let delta = ((y - anchor_y) / lh).round() as i64;
                if (y - anchor_y).abs() > 3.0 * theme::ui_zoom() {
                    self.gap_drag = Some((gi, anchor_y, anchor_top, anchor_bot, true));
                    // Drag down (delta > 0) reveals from the top of the run; drag up
                    // (delta < 0) reveals from the bottom. The other end resets to its
                    // press value so reversing direction collapses back symmetrically.
                    let (top, bot) = if delta >= 0 {
                        ((anchor_top as i64 + delta).max(0) as usize, anchor_bot)
                    } else {
                        (anchor_top, (anchor_bot as i64 - delta).max(0) as usize)
                    };
                    if let (Some(d), Some(g)) = (self.workspace.active_doc_mut(), self.gpu.as_mut()) {
                        d.set_diff_gap_reveal(gi, top, bot, &mut g.font_system);
                    }
                    self.redraw();
                }
                return;
            }
        }
        // Diff per-block hover: track the change block under the cursor so its
        // Stage/Revert buttons show. Only meaningful for a file diff.
        if !self.mouse_pressed {
            let region = render::editor_region(&self.layout());
            let (staged, sy, is_diff) = self
                .workspace
                .active_doc()
                .map_or((false, 0.0, false), |d| (d.diff_staged, d.scroll_y(), d.diff_path.is_some()));
            let count = if staged { 1 } else { 2 };
            let label = |idx: usize| -> String {
                if staged { "Unstage Block" } else if idx == 0 { "Revert Block" } else { "Stage Block" }.to_string()
            };
            // Keep the current block hovered if the cursor is over one of its buttons
            // (a 1-line block's stacked buttons extend past the row, so y-based block
            // detection alone would drop the hover the moment you reach the 2nd button).
            let on_cur_btn = is_diff
                .then(|| self.hovered_diff_block)
                .flatten()
                .and_then(|(vbs, vbe)| {
                    let rects = render::diff_block_btn_rects(region, vbs, vbe, sy, count)?;
                    let idx = rects.iter().position(|r| r.contains((x, y)))?;
                    Some(((vbs, vbe), idx, rects[idx]))
                });
            let (nb, tip) = if let Some((block, idx, r)) = on_cur_btn {
                (Some(block), Some((label(idx), r.x + r.w, r.y)))
            } else if is_diff && region.contains((x, y)) {
                match self.workspace.active_doc().and_then(|d| d.diff_block_at_y(region, y)) {
                    Some((vbs, vbe)) => {
                        let tip = render::diff_block_btn_rects(region, vbs, vbe, sy, count)
                            .and_then(|rects| rects.iter().position(|r| r.contains((x, y))).map(|i| (i, rects[i])))
                            .map(|(idx, r)| (label(idx), r.x + r.w, r.y));
                        (Some((vbs, vbe)), tip)
                    }
                    None => (None, None),
                }
            } else {
                (None, None)
            };
            if nb != self.hovered_diff_block || tip != self.block_tip {
                self.hovered_diff_block = nb;
                self.block_tip = tip;
                self.redraw();
            }
        }
        // Commit-message hover card. Two sources share `commit_tip` (mutually
        // exclusive by view): the GRAPH shows instantly; the inline BLAME annotation
        // waits ~2s of rest (GitLens-style) via `blame_pending`.
        if !self.mouse_pressed {
            let region = render::editor_region(&self.layout());
            let graph_tip = region
                .contains((x, y))
                .then(|| self.workspace.active_doc().and_then(|d| d.graph_message_at_y(region, y)).map(|m| (m.to_string(), x, y)))
                .flatten();
            if let Some(gt) = graph_tip {
                self.blame_pending = None;
                if self.commit_tip.as_ref() != Some(&gt) {
                    self.commit_tip = Some(gt);
                    self.redraw();
                }
            } else if let Some(card) = region.contains((x, y)).then(|| self.blame_card_at(x, y)).flatten() {
                // Over the blame annotation: stage it; promote after the delay.
                let showing = self.commit_tip.as_ref() == Some(&card);
                let pending = self.blame_pending.as_ref().map_or(false, |(c, ..)| *c == card.0);
                if !showing && !pending {
                    self.blame_pending = Some((card.0, card.1, card.2, Instant::now()));
                }
            } else {
                // Off both → dismiss any card + pending timer.
                self.blame_pending = None;
                if self.commit_tip.take().is_some() {
                    self.redraw();
                }
            }
        }
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
                        };
                        inp.extend_to_x(rect, pad, x);
                    }
                    self.redraw();
                }
                return;
            }
        }
        // Find/replace widget drag-select.
        if let Some(on_replace) = self.find_drag {
            if self.mouse_pressed {
                let layout = self.layout();
                let er = crate::render::editor_region(&layout);
                let fl = ui::find_widget::FindWidget::layout(er, self.find.replace_open);
                let (rect, pad) = if on_replace {
                    (fl.replace_input.unwrap_or(fl.find_text), theme::zpx(6.0))
                } else {
                    (fl.find_text, theme::zpx(6.0))
                };
                if let Some(g) = self.gpu.as_mut() {
                    let inp = if on_replace { &mut g.ui.find.replace } else { &mut g.ui.find.query };
                    inp.extend_to_x(rect, pad, x);
                }
                self.redraw();
                return;
            }
        }
        // Scrollbar thumb drags — one ScrollView is dragging at a time.
        if self.mouse_pressed && self.terminal.pane_scroll_drag((x, y)) {
            self.redraw();
            return;
        }
        // Terminal tab-list reorder drag (takes precedence over text selection).
        if self.mouse_pressed {
            let layout = self.layout();
            if self.terminal.tab_drag_to((x, y), &layout) {
                self.redraw();
                return;
            }
        }
        // Terminal text-selection drag.
        if self.mouse_pressed {
            let layout = self.layout();
            if self.terminal.selection_drag((x, y), &layout, self.terminal_cell_w) {
                self.redraw();
                return;
            }
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
            let region = self.layout().sidebar;
            if let Some(sp) = self.search.as_mut() {
                if sp.on_drag((x, y), region) {
                    self.redraw();
                    return;
                }
            }
        }
        if self.mouse_pressed && self.sidebar_view == SidebarView::SourceControl {
            let region = self.layout().panel_region();
            if let Some(scp) = self.source_control.as_mut() {
                if scp.on_drag((x, y), region) {
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
            // Diff per-pane horizontal scrollbar thumb drag.
            if self.workspace.active_doc().map_or(false, |d| d.diff_hbar_dragging()) {
                let (_, lt, _, rt) = render::diff_pane_rects(render::editor_region(&self.layout()));
                if let Some(d) = self.workspace.active_doc_mut() {
                    if d.diff_hbar_drag((x, y), [lt, rt]) {
                        self.redraw();
                    }
                }
                return;
            }
            if let Some(d) = self.workspace.active_doc_mut() {
                if d.scroll.is_dragging() {
                    if d.scroll.drag((x, y)) {
                        self.redraw();
                    }
                    return;
                }
            }
        }
        // Source Control scrollbar thumb drag.
        if self.mouse_pressed {
            if let Some(scp) = self.source_control.as_mut() {
                if (scp.scroll.is_dragging() && scp.scroll.drag((x, y)))
                    || (scp.graph_scroll.is_dragging() && scp.graph_scroll.drag((x, y)))
                {
                    self.redraw();
                    return;
                }
            }
        }
        // Outline / chat scrollbar thumb drags.
        if self.mouse_pressed {
            if let Some(o) = self.outline.as_mut() {
                if o.scroll.is_dragging() && o.scroll.drag((x, y)) {
                    self.redraw();
                    return;
                }
            }
            if let Some(c) = self.chat.as_mut() {
                if c.scroll.is_dragging() && c.scroll.drag((x, y)) {
                    self.redraw();
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
        if self.right_split.is_dragging() && self.mouse_pressed {
            // Width is measured back from the window's right edge.
            let origin = self.gpu.as_ref().map_or(0.0, |g| g.config.width as f32);
            if self.right_split.drag(x, origin) {
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
        if (self.editor.dragging || self.editor.text_move.is_some()) && self.mouse_pressed {
            let layout = self.layout();
            if let Some(d) = self.workspace.active_doc_mut() {
                if self.editor.on_drag(d, &layout, x, y) {
                    self.redraw();
                }
            }
        }
        // Explorer drag-to-move: activate past a threshold, then track the folder
        // under the pointer (a file row targets its parent; empty space → the root).
        if self.mouse_pressed {
            if let Some((path, at, active)) = self.tree_drag.clone() {
                let dist = ((x - at.0).powi(2) + (y - at.1).powi(2)).sqrt();
                let now_active = active || dist > 5.0 * theme::ui_zoom();
                if now_active != active {
                    self.tree_drag = Some((path.clone(), at, true));
                }
                if now_active {
                    let layout = self.layout();
                    let tr = layout.tree_region();
                    let target = if tr.contains((x, y)) {
                        let row = self.gpu.as_ref().and_then(|g| {
                            g.ui.sidebar.row_at_scrolled(tr, self.explorer.scroll.offset().1, (x, y), self.workspace.tree.nodes.len())
                        });
                        match row.and_then(|i| self.workspace.tree.nodes.get(i)) {
                            Some(n) if n.is_dir => Some(n.path.clone()),
                            Some(n) => n.path.parent().map(|p| p.to_path_buf()),
                            None => Some(self.workspace.tree.root.clone()),
                        }
                    } else {
                        None
                    };
                    if target != self.tree_drop_target {
                        self.tree_drop_target = target;
                    }
                    self.redraw(); // the drag ghost follows the cursor
                }
            }
            // Tab drag-reorder: live-swap once the pointer crosses another tab.
            if let Some((idx, at, active)) = self.tab_drag {
                // Activate on movement along EITHER axis — dragging a tab straight
                // down into the terminal is a vertical gesture (x barely changes).
                let thresh = 6.0 * theme::ui_zoom();
                let now_active = active || (x - at.0).abs() > thresh || (y - at.1).abs() > thresh;
                if now_active != active {
                    self.tab_drag = Some((idx, at, true));
                    self.redraw();
                }
                if now_active {
                    let layout = self.layout();
                    let rects = layout.tab_rects(self.tab_count());
                    if let Some(j) = rects.iter().position(|r| r.contains((x, y))) {
                        let ndocs = self.workspace.documents.len();
                        if j != idx && j < ndocs && idx < ndocs {
                            self.workspace.move_tab(idx, j);
                            self.tab_drag = Some((j, at, true));
                            self.redraw();
                        }
                    }
                }
            }
        }
    }

    fn on_mouse_release(&mut self) {
        // Text drag-move: drop the selection at the target, or — if the press never
        // became a drag — place the caret now (deferred from press).
        if let Some(tm) = self.editor.text_move.take() {
            let layout = self.layout();
            let p = (self.mouse_pos.x as f32, self.mouse_pos.y as f32);
            if let (Some(gpu), Some(d)) = (self.gpu.as_mut(), self.workspace.active_doc_mut()) {
                if tm.active {
                    if let Some(drop) = tm.drop {
                        d.move_selection_to(drop, &mut gpu.font_system);
                    }
                } else {
                    ui::editor_view::EditorView::place_caret(d, &layout, p.0, p.1, false);
                }
            }
            self.redraw();
        }
        // Explorer drag-to-move: dropping on the terminal pastes the quoted path
        // (VSCode behavior); dropping on a folder moves the entry there.
        if let Some((src, _, active)) = self.tree_drag.take() {
            let target = self.tree_drop_target.take();
            if active {
                let p = (self.mouse_pos.x as f32, self.mouse_pos.y as f32);
                let over_terminal = self.terminal.visible
                    && self.layout().terminal_panel.map_or(false, |tp| tp.contains(p));
                if over_terminal {
                    self.terminal.write_focused(shell_quoted(&src).as_bytes());
                    self.terminal.focused = true; // typing continues in the shell (#31)
                } else if let Some(dir) = target {
                    self.move_tree_entry(&src, &dir);
                }
                self.redraw();
            } else if src.is_file() {
                // A plain click on a file (no drag) → open it now, deferred from press
                // so a drag-and-drop into the terminal isn't pre-empted.
                self.open_file_at(src, 1, 0);
                self.redraw();
            }
        }
        // Editor tab dropped onto the terminal → paste its file path (like the
        // explorer drag-to-terminal). Reordering already happened live during drag.
        if let Some((idx, _, active)) = self.tab_drag.take() {
            if active {
                let p = (self.mouse_pos.x as f32, self.mouse_pos.y as f32);
                let over_terminal = self.terminal.visible
                    && self.layout().terminal_panel.map_or(false, |tp| tp.contains(p));
                if over_terminal {
                    if let Some(path) = self.workspace.documents.get(idx).and_then(|d| d.path.clone()) {
                        self.terminal.write_focused(shell_quoted(&path).as_bytes());
                        self.terminal.focused = true;
                        self.redraw();
                    }
                }
            }
        }
        self.editor.on_release();
        self.text_drag = None;
        self.find_drag = None;
        self.image_drag_last = None;
        if let Some(f) = self.feedback_form.as_mut() {
            f.end_drag();
        }
        self.sidebar_split.release();
        self.right_split.release();
        if let Some(o) = self.outline.as_mut() {
            o.scroll.release();
        }
        if let Some(c) = self.chat.as_mut() {
            c.scroll.release();
        }
        self.terminal.split.release();
        self.terminal.release_scrolls();
        self.terminal.selection_release();
        self.terminal.end_tab_drag();
        self.detail.ext_detail_scroll.release();
        self.explorer.scroll.release();
        if let Some(scp) = self.source_control.as_mut() {
            scp.scroll.release();
            scp.graph_scroll.release();
            scp.on_release();
        }
        if let Some(ep) = self.extensions_panel.as_mut() {
            ep.on_release();
        }
        if let Some(sp) = self.search.as_mut() {
            sp.on_release();
        }
        if let Some(d) = self.workspace.active_doc_mut() {
            d.scroll.release();
            d.diff_release_hbars();
        }
        // A diff gap that was pressed but never dragged is a click → expand it fully.
        if let Some((gi, _, _, _, dragged)) = self.gap_drag.take() {
            if !dragged {
                if let (Some(d), Some(g)) = (self.workspace.active_doc_mut(), self.gpu.as_mut()) {
                    d.expand_diff_gap(gi, usize::MAX, true, &mut g.font_system);
                }
                self.redraw();
            }
        }
    }

    /// Move a file/folder into `dir` (explorer drag-and-drop). Refuses no-op,
    /// into-own-subtree, and would-overwrite moves; re-points any open documents
    /// living under the moved entry and refreshes the tree + git badge.
    fn move_tree_entry(&mut self, src: &Path, dir: &Path) {
        let Some(name) = src.file_name() else { return };
        let dest = dir.join(name);
        if dest == *src || dir.starts_with(src) || dest.exists() {
            return;
        }
        if std::fs::rename(src, &dest).is_err() {
            return;
        }
        if let Some(gpu) = self.gpu.as_mut() {
            for d in self.workspace.documents.iter_mut() {
                if let Some(p) = d.path.clone() {
                    if let Ok(rest) = p.strip_prefix(src) {
                        let np = if rest.as_os_str().is_empty() { dest.clone() } else { dest.join(rest) };
                        d.set_path(np, &mut gpu.font_system);
                    }
                }
            }
        }
        self.workspace.tree.refresh();
        self.invalidate_file_index();
        self.refresh_source_control();
    }

    fn on_scroll(&mut self, dy: f32) {
        let layout = self.layout();
        let p = (self.mouse_pos.x as f32, self.mouse_pos.y as f32);
        // Settings editor: modal. Wheel over the search box or the focused field
        // scrolls that text horizontally; elsewhere it scrolls the settings viewport.
        if self.settings_editor.open {
            let (sw, sh) = self.gpu.as_ref().map_or((1280.0, 800.0), |g| (g.config.width as f32, g.config.height as f32));
            let lay = ui::settings_editor::layout(Rect { x: 0.0, y: 0.0, w: sw, h: sh });
            let edit_rect = self.settings_editor.edit_key.and_then(|key| {
                self.settings_editor.rows_cache.iter().find(|h| ui::settings_editor::SCHEMA[h.idx].key == key).map(|h| h.control)
            });
            if let Some(g) = self.gpu.as_ref() {
                if lay.search.contains(p) {
                    g.ui.settings_search.scroll_h(-dy);
                    self.redraw();
                    return;
                }
                if edit_rect.map_or(false, |r| r.contains(p)) {
                    g.ui.settings_input.scroll_h(-dy);
                    self.redraw();
                    return;
                }
            }
            let max = (self.settings_editor.content_h - lay.right.h).max(0.0);
            self.settings_editor.scroll = (self.settings_editor.scroll - dy).clamp(0.0, max);
            self.redraw();
            return;
        }
        // Command palette: the wheel scrolls its list only when the pointer is over
        // the card; elsewhere the editor scrolls underneath (useful while previewing).
        if self.palette.active {
            if layout.palette.as_ref().map_or(false, |pal| pal.box_.contains(p)) {
                self.palette.scroll = (self.palette.scroll - dy).max(0.0);
                self.redraw();
                return;
            }
        }
        // Terminal scrollback: the panel owns its pane ScrollViews; consumes the
        // event (when over the content) so the editor doesn't scroll underneath.
        if self.terminal.on_scroll(p, &layout, dy) {
            self.redraw();
            return;
        }
        // AI chat (right sidebar) scrolls when the cursor is over it.
        if self.right_sidebar_visible {
            if let Some(c) = self.chat.as_mut() {
                if c.on_wheel(p, layout.right_sidebar, dy) {
                    self.redraw();
                    return;
                }
            }
        }
        // Explorer OUTLINE section scrolls when the cursor is over its body.
        if let Some(body) = layout.outline_body_rect().filter(|b| b.contains(p)) {
            if let Some(o) = self.outline.as_mut() {
                let _ = body;
                o.scroll.on_wheel(0.0, dy);
                self.redraw();
                return;
            }
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
            let region = layout.sidebar;
            if let Some(sp) = self.search.as_mut() {
                if sp.on_wheel(p, region, dy) {
                    self.redraw();
                    return;
                }
            }
        }
        // Source Control: the groups area (headers + file lists) scrolls.
        if self.sidebar_visible && self.sidebar_view == SidebarView::SourceControl {
            let region = layout.panel_region();
            if let Some(scp) = self.source_control.as_mut() {
                if scp.on_wheel(p, region, dy) {
                    self.redraw();
                    return;
                }
            }
        }
        // Run & Debug: the call-stack/variables body scrolls.
        if self.sidebar_visible && self.sidebar_view == SidebarView::Debug {
            let region = layout.panel_region();
            if let Some(dp) = self.debug.as_mut() {
                if dp.on_wheel(p, region, dy) {
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

    /// Scroll the single-line input under `p` horizontally by `dx` px, if any. Used
    /// by horizontal-wheel routing so inputs scroll the same as on a vertical wheel.
    fn scroll_input_h(&mut self, p: (f32, f32), layout: &Layout, dx: f32) -> bool {
        if self.settings_editor.open {
            let (sw, sh) = self.gpu.as_ref().map_or((1280.0, 800.0), |g| (g.config.width as f32, g.config.height as f32));
            let lay = ui::settings_editor::layout(Rect { x: 0.0, y: 0.0, w: sw, h: sh });
            let edit_rect = self.settings_editor.edit_key.and_then(|key| {
                self.settings_editor.rows_cache.iter().find(|h| ui::settings_editor::SCHEMA[h.idx].key == key).map(|h| h.control)
            });
            if let Some(g) = self.gpu.as_ref() {
                if lay.search.contains(p) {
                    g.ui.settings_search.scroll_h(-dx);
                    return true;
                }
                if edit_rect.map_or(false, |r| r.contains(p)) {
                    g.ui.settings_input.scroll_h(-dx);
                    return true;
                }
            }
            return false;
        }
        if self.sidebar_visible && self.sidebar_view == SidebarView::Search {
            if let Some(sp) = self.search.as_ref() {
                if sp.hwheel(p, layout.sidebar, dx) {
                    return true;
                }
            }
        }
        if self.sidebar_visible && self.sidebar_view == SidebarView::Extensions {
            if let Some(ep) = self.extensions_panel.as_ref() {
                if ep.hwheel(p, layout.tree_region(), dx) {
                    return true;
                }
            }
        }
        false
    }

    fn on_scroll_h(&mut self, dx: f32) {
        let p = (self.mouse_pos.x as f32, self.mouse_pos.y as f32);
        let layout = self.layout();
        // Horizontal wheel (incl. Ctrl/Shift+wheel and trackpad) over an input box
        // scrolls its text — same as a vertical wheel there.
        if self.scroll_input_h(p, &layout, dx) {
            self.redraw();
            return;
        }
        if let Some(d) = self.workspace.active_doc_mut() {
            // Diff: scroll only the pane under the cursor (independent panes).
            if d.diff.is_some() {
                let (_, lt, _, rt) = render::diff_pane_rects(render::editor_region(&layout));
                let pane = if rt.contains(p) { Some((1, rt.w)) } else if lt.contains(p) { Some((0, lt.w)) } else { None };
                if let Some((i, vw)) = pane {
                    if d.diff_hwheel(i, dx, vw) {
                        self.redraw();
                    }
                }
                return;
            }
            if d.scroll.on_wheel(dx, 0.0) {
                self.redraw();
            }
        }
    }

    fn on_key(&mut self, event: winit::event::KeyEvent) {
        if event.state != ElementState::Pressed {
            return;
        }
        // Double-Shift opens the command palette (IntelliJ-style). A lone Shift tap
        // twice within the window — any other key in between resets the chain.
        if matches!(event.logical_key.as_ref(), Key::Named(NamedKey::Shift)) {
            if !self.mods.control_key() && !self.mods.super_key() && !self.mods.alt_key() {
                let now = Instant::now();
                if self.last_shift.map_or(false, |t| now.duration_since(t) < Duration::from_millis(350)) {
                    self.last_shift = None;
                    if !self.palette.active {
                        // Double-Shift opens quick-open (go-to-file, empty input) —
                        // no `>` command prefix. Ctrl+Shift+P is the command palette.
                        self.open_quick_open();
                    }
                    return;
                }
                self.last_shift = Some(now);
            }
            return; // a bare Shift press does nothing else
        } else {
            self.last_shift = None;
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

        // The Settings editor is modal: route keys to its focused input.
        if self.settings_editor.open && self.ctx_menu.is_none() {
            let key = event.logical_key.as_ref();
            if matches!(key, Key::Named(NamedKey::Escape)) {
                if self.settings_editor.edit_key.is_some() {
                    // Esc cancels an inline edit but keeps the modal open.
                    self.settings_editor.edit_key = None;
                    if let Some(g) = self.gpu.as_mut() {
                        g.ui.settings_input.focus(false);
                        g.ui.settings_search.focus(true);
                    }
                    self.redraw();
                } else {
                    self.close_settings_editor();
                }
                return;
            }
            // Inline number/text editor focused: Enter commits, else edit the input.
            if self.settings_editor.edit_key.is_some() {
                if matches!(key, Key::Named(NamedKey::Enter)) {
                    self.commit_settings_input();
                    return;
                }
                if let Some(g) = self.gpu.as_mut() {
                    edit_input(&mut g.ui.settings_input, &mut g.font_system, self.clipboard.as_mut(), &event, ctrl, extend);
                }
                self.redraw();
                return;
            }
            // Otherwise the search box has focus.
            if let Some(g) = self.gpu.as_mut() {
                edit_input(&mut g.ui.settings_search, &mut g.font_system, self.clipboard.as_mut(), &event, ctrl, extend);
                self.settings_editor.query = g.ui.settings_search.text().to_string();
            }
            self.settings_editor.scroll = 0.0;
            self.redraw();
            return;
        }

        // Escape closes an open context menu first.
        if self.ctx_menu.is_some() && matches!(event.logical_key.as_ref(), Key::Named(NamedKey::Escape)) {
            self.close_ctx_menu();
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
                // Copy: Ctrl+Shift+C always, or Ctrl+C when there's a selection
                // (otherwise Ctrl+C falls through to send SIGINT, like VS Code).
                let is_c = matches!(
                    event.physical_key,
                    winit::keyboard::PhysicalKey::Code(winit::keyboard::KeyCode::KeyC)
                );
                // Ctrl+Shift+A selects everything (Ctrl+A alone still goes to the shell
                // as beginning-of-line).
                let is_a = matches!(
                    event.physical_key,
                    winit::keyboard::PhysicalKey::Code(winit::keyboard::KeyCode::KeyA)
                );
                if ctrl && extend && is_a {
                    if self.terminal.select_all() {
                        self.redraw();
                    }
                    return;
                }
                if ctrl && is_c && (extend || self.terminal.selection_text().is_some()) {
                    if let Some(text) = self.terminal.selection_text() {
                        if let Some(cb) = self.clipboard.as_mut() {
                            let _ = cb.set_text(text);
                        }
                    }
                    self.terminal.clear_focused_selection();
                    self.redraw();
                    return;
                }
                // Paste: Cmd+V on macOS, Ctrl(+Shift)+V elsewhere — sends the clipboard
                // to the shell as input (newlines become CR, like pressing Enter).
                // On macOS plain Ctrl+V is deliberately NOT paste: it falls through to
                // the shell as a literal ^V (0x16), which TUIs that read the clipboard
                // themselves bind — claude code's Ctrl+V image paste — matching
                // Terminal.app/iTerm behavior (#39).
                let is_v = matches!(
                    event.physical_key,
                    winit::keyboard::PhysicalKey::Code(winit::keyboard::KeyCode::KeyV)
                );
                let paste_mod = if cfg!(target_os = "macos") {
                    self.mods.super_key()
                } else {
                    self.mods.control_key()
                };
                if paste_mod && is_v {
                    if let Some(text) = self.clipboard.as_mut().and_then(|cb| cb.get_text().ok()) {
                        self.terminal.clear_focused_selection();
                        self.terminal.paste_focused(&text);
                        self.redraw();
                    }
                    return;
                }
                let alt = self.mods.alt_key();
                if let Some(mut bytes) = translate_terminal_key(&event, ctrl, extend, alt) {
                    // Escape at an IDLE shell prompt clears the typed input (kill-line,
                    // Ctrl+U). Any running foreground process — alt-screen TUIs (vim)
                    // AND inline ones (claude code) — gets the real ESC, so interrupt
                    // and mode keys work. Busy-ness is asked of the pty-host, which
                    // checks the shell for child processes.
                    if bytes == [0x1b] {
                        let in_alt = self
                            .terminal
                            .groups
                            .get(self.terminal.active)
                            .and_then(|g| g.panes.get(g.focused))
                            .map_or(false, |p| p.term.is_alt());
                        // Windows can't detect a running foreground process
                        // (`shell_busy` is Unix-only), so the idle check is always
                        // false there — rewriting would eat Esc inside inline TUIs
                        // (claude code). Only apply the kill-line convenience where
                        // busy-detection is reliable; otherwise send the real Esc.
                        if cfg!(not(windows)) && !in_alt && !self.terminal.focused_term_busy() {
                            bytes = vec![0x15]; // ^U — kill the whole input line
                        }
                    }
                    self.terminal.clear_focused_selection(); // input dismisses the selection
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
                        self.palette_restore_preview();
                        self.palette.close();
                        self.redraw();
                        return;
                    }
                    Key::Named(NamedKey::ArrowDown) => {
                        self.palette.select_next();
                        self.palette_preview();
                        self.redraw();
                        return;
                    }
                    Key::Named(NamedKey::ArrowUp) => {
                        self.palette.select_prev();
                        self.palette_preview();
                        self.redraw();
                        return;
                    }
                    Key::Named(NamedKey::Enter) => {
                        self.commit_palette();
                        self.redraw();
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
                        self.palette_preview();
                    }
                    self.redraw();
                }
                return;
            }
            Focus::Find => {
                let on_replace = self.find.on_replace;
                match event.logical_key.as_ref() {
                    Key::Named(NamedKey::Escape) => {
                        self.close_find();
                        return;
                    }
                    Key::Named(NamedKey::Enter) => {
                        // Enter in the replace field replaces the current match; in the
                        // find field it steps to the next (Shift+Enter = previous).
                        if on_replace {
                            self.replace_current();
                        } else {
                            self.find_step(!extend);
                        }
                        return;
                    }
                    // Tab / Ctrl+H jump to (and open) the replace field.
                    Key::Named(NamedKey::Tab) => {
                        self.find.replace_open = true;
                        self.find.on_replace = !on_replace;
                        if let Some(g) = self.gpu.as_mut() {
                            g.ui.find.query.focus(self.find.on_replace == false);
                            g.ui.find.replace.focus(self.find.on_replace);
                        }
                        self.redraw();
                        return;
                    }
                    _ => {}
                }
                let consumed = self.gpu.as_mut().and_then(|g| {
                    let inp = if on_replace { &mut g.ui.find.replace } else { &mut g.ui.find.query };
                    edit_input(inp, &mut g.font_system, self.clipboard.as_mut(), &event, ctrl, extend)
                });
                if let Some(changed) = consumed {
                    // Editing the find query re-runs the search incrementally.
                    if changed && !on_replace {
                        self.recompute_find();
                    }
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
            Focus::Chat => {
                // Route to the chat input; Enter sends, Esc unfocuses.
                let mut handled = false;
                if let (Some(c), Some(g)) = (self.chat.as_mut(), self.gpu.as_mut()) {
                    handled = c.on_key(&event, ctrl, extend, &mut g.font_system, self.clipboard.as_mut());
                }
                if handled {
                    self.redraw();
                    return;
                }
            }
            Focus::Editor => {
                // Escape closes the find widget when the editor has focus (VSCode parity).
                if self.find.active && matches!(event.logical_key.as_ref(), Key::Named(NamedKey::Escape)) {
                    self.close_find();
                    return;
                }
                // F12 / Shift+F12: go to definition / references.
                if matches!(event.logical_key.as_ref(), Key::Named(NamedKey::F12)) {
                    self.exec_command(if self.mods.shift_key() {
                        Command::GotoReferences
                    } else {
                        Command::GotoDefinition
                    });
                    return;
                }
                // F2: rename the symbol at the caret.
                if matches!(event.logical_key.as_ref(), Key::Named(NamedKey::F2)) {
                    self.exec_command(Command::RenameSymbol);
                    return;
                }
                // Shift+Alt+F: format document (physical key — Alt remaps logicals).
                if self.mods.alt_key() && self.mods.shift_key() {
                    if let winit::keyboard::PhysicalKey::Code(winit::keyboard::KeyCode::KeyF) =
                        event.physical_key
                    {
                        self.exec_command(Command::FormatDocument);
                        return;
                    }
                }
                // Ctrl/Cmd+Shift+V: open a rendered Markdown preview of the active file.
                if ctrl && self.mods.shift_key() {
                    if let winit::keyboard::PhysicalKey::Code(winit::keyboard::KeyCode::KeyV) =
                        event.physical_key
                    {
                        self.exec_command(Command::MarkdownPreview);
                        return;
                    }
                }
                // F8 / Shift+F8: cycle through the document's problems.
                if matches!(event.logical_key.as_ref(), Key::Named(NamedKey::F8)) {
                    self.exec_command(if self.mods.shift_key() {
                        Command::PrevProblem
                    } else {
                        Command::NextProblem
                    });
                    return;
                }
                // Alt+Left / Alt+Right (no shift): navigate Back / Forward.
                if self.mods.alt_key() && !self.mods.shift_key() {
                    if let Key::Named(k @ (NamedKey::ArrowLeft | NamedKey::ArrowRight)) =
                        event.logical_key.as_ref()
                    {
                        self.exec_command(if k == NamedKey::ArrowLeft {
                            Command::NavBack
                        } else {
                            Command::NavForward
                        });
                        return;
                    }
                }
            }
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
                    KeyCode::KeyP => {
                        self.open_quick_open(); // Ctrl/Cmd+P → go to file
                        return;
                    }
                    KeyCode::KeyA => {
                        self.exec_command(Command::SelectAll);
                        return;
                    }
                    KeyCode::Slash => {
                        self.exec_command(Command::ToggleLineComment);
                        return;
                    }
                    KeyCode::KeyM if shift => {
                        self.open_problems_picker();
                        return;
                    }
                    KeyCode::KeyT => {
                        self.open_palette_with("#"); // workspace symbols
                        return;
                    }
                    KeyCode::PageDown => {
                        self.exec_command(Command::NextEditor);
                        return;
                    }
                    KeyCode::PageUp => {
                        self.exec_command(Command::PrevEditor);
                        return;
                    }
                    KeyCode::Backslash if shift => {
                        self.exec_command(Command::GotoBracket);
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

        // Code-completion popup intercepts navigation/accept/dismiss before the editor
        // sees the key (so ↑/↓/Enter/Tab/Esc drive the popup, not the document).
        if self.completion.active {
            match event.logical_key.as_ref() {
                Key::Named(NamedKey::Escape) => {
                    self.completion.close();
                    self.redraw();
                    return;
                }
                Key::Named(NamedKey::ArrowDown) => {
                    self.completion.move_sel(1);
                    self.redraw();
                    return;
                }
                Key::Named(NamedKey::ArrowUp) => {
                    self.completion.move_sel(-1);
                    self.redraw();
                    return;
                }
                Key::Named(NamedKey::Enter) | Key::Named(NamedKey::Tab) => {
                    if let Some(item) = self.completion.selected_item() {
                        let insert = item.insert.clone();
                        let start = self.completion.prefix_start;
                        d.replace_prefix(start, &insert, &mut gpu.font_system);
                        self.completion.close();
                        self.last_edit = Instant::now();
                        self.ensure_cursor_visible();
                        self.redraw();
                        return;
                    }
                }
                _ => {}
            }
        }

        // Alt-modified editor ops (VSCode): Alt+Up/Down move lines, Shift+Alt copies
        // them, Shift+Alt+A toggles a block comment, Shift+Alt+Right/Left expand and
        // shrink the selection. Physical keys — Alt remaps logical characters.
        if self.mods.alt_key() {
            use winit::keyboard::{KeyCode, PhysicalKey};
            if let PhysicalKey::Code(code) = event.physical_key {
                let done = match code {
                    KeyCode::ArrowUp | KeyCode::ArrowDown => {
                        let down = code == KeyCode::ArrowDown;
                        if extend {
                            d.copy_lines(down, &mut gpu.font_system);
                        } else {
                            d.move_lines(down, &mut gpu.font_system);
                        }
                        true
                    }
                    KeyCode::KeyA if extend => {
                        d.toggle_block_comment(&mut gpu.font_system);
                        true
                    }
                    KeyCode::ArrowRight if extend => {
                        d.expand_selection();
                        true
                    }
                    KeyCode::ArrowLeft if extend => {
                        d.shrink_selection();
                        true
                    }
                    _ => false,
                };
                if done {
                    self.completion.close();
                    self.last_edit = Instant::now();
                    self.ensure_cursor_visible();
                    self.redraw();
                    return;
                }
            }
        }

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
                // Auto-indent: carry the current line's leading whitespace onto the
                // new line so the cursor lands at the same indentation level.
                let (line, _) = d.head_line_col();
                let indent: String = d
                    .rope
                    .line(line)
                    .chars()
                    .take_while(|&c| c == ' ' || c == '\t')
                    .collect();
                d.insert_str(&format!("\n{indent}"), &mut gpu.font_system);
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
        // Update the completion popup: typing/deleting recomputes suggestions; any
        // other navigation key dismisses it (VSCode behavior).
        match event.logical_key.as_ref() {
            Key::Character(_) | Key::Named(NamedKey::Backspace) | Key::Named(NamedKey::Delete) => {
                self.recompute_completion();
            }
            Key::Named(_) => self.completion.close(),
            _ => {}
        }
        self.last_edit = Instant::now(); // for files.autoSave idle timer
        self.ensure_cursor_visible();
        self.redraw();
    }

    /// Recompute the completion popup: fill it instantly from words in the document,
    /// and fire an async LSP request (if a server serves this language) whose richer
    /// results replace the word-based ones when they arrive.
    fn recompute_completion(&mut self) {
        let _probe = crate::perf::Probe::new("recompute_completion", 8);
        let Some(d) = self.workspace.active_doc() else {
            self.completion.close();
            self.completion_req = None;
            return;
        };
        if d.large {
            // Large-file mode: scanning a multi-MB doc for word candidates on every
            // keystroke isn't worth it (and there's no LSP for it either).
            self.completion.close();
            self.completion_req = None;
            return;
        }
        let text = d.text();
        let caret = d.caret_byte();
        let (line, col) = d.head_line_col();
        let lang = d.language_id();
        let uri = d.uri();
        let version = d.version;
        // Fill instantly from words in the document.
        self.completion.update_words(&text, caret);
        // Fire a language-server request alongside (scoped to the doc's language) when
        // there's an identifier prefix; its richer results replace the word-based ones.
        self.completion_req = None;
        let Some(prefix_start) = completion::word_prefix(&text, caret) else { return };
        let (Some(lang), Some(uri)) = (lang, uri) else { return };
        // Only worth a request once a completion server is running with this doc open.
        let Some(server) = self.lsp.completion_server(lang) else { return };
        if !d.lsp_servers.contains(&server) {
            return; // not opened to the server yet; the sync tick will, next keystroke works
        }
        // Flush the just-typed text so the server completes against current contents
        // (the debounced didChange would otherwise lag a keystroke behind). The doc
        // stays lsp_dirty so the sync tick still runs its semantic/diagnostic pulls —
        // its duplicate same-version didChange is benign.
        self.lsp.did_change(server, &uri, version, &text);
        if let Some(id) = self.lsp.request_completion(lang, &uri, line as u32, col as u32) {
            self.completion_req = Some((id, prefix_start));
        }
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
    Chat,      // AI chat input (right sidebar)
}

/// Open a URL in the OS default browser. Best-effort, http(s) only (so README
/// link text can't launch arbitrary commands).
/// Resolve the `gh` executable. macOS GUI apps don't inherit the shell PATH, so a
/// bare "gh" often isn't found (it lives in /opt/homebrew/bin etc.) — check the
/// common locations and fall back to a login-shell lookup before giving up.
/// A diagnostic snapshot for bug reports (feedback form): the environment bits that
/// most often explain "extension X isn't working" — node, the ESLint/TS servers, and
/// the installed-extension count. Runs a couple of quick probes; only on submit.
pub(crate) fn diagnostics_report() -> String {
    let node = lsp::resolve_node().unwrap_or_else(|| "NOT FOUND".to_string());
    let ts = if lsp::typescript_ls_cli().is_some() { "found" } else { "not found" };
    let ext_dir = extensions::dir();
    let eslint = match &ext_dir {
        Some(d) if lsp::eslint_server_path(std::slice::from_ref(d)).is_some() => "found",
        _ => "not found",
    };
    let (dir_str, count) = match &ext_dir {
        Some(d) => {
            let n = std::fs::read_dir(d)
                .map(|r| r.filter_map(|e| e.ok()).filter(|e| e.path().is_dir()).count())
                .unwrap_or(0);
            (d.display().to_string(), n)
        }
        None => ("not found".to_string(), 0),
    };
    format!(
        "node: {node}\neslint server: {eslint}\ntypescript-language-server: {ts}\nextensions: {count} installed in {dir_str}"
    )
}

/// Lightweight, language-aware symbol extractor for go-to-symbol (`@`). Scans each
/// line for a declaration keyword and takes the following identifier — no LSP needed,
/// so it works offline and instantly. Returns (name, kind, 1-based line).
pub(crate) fn extract_symbols(text: &str, ext: &str) -> Vec<(String, String, usize)> {
    let kws: &[&str] = match ext {
        "rs" => &["fn", "struct", "enum", "trait", "impl", "mod", "const", "static", "type", "macro_rules"],
        "ts" | "tsx" | "js" | "jsx" | "mjs" | "cjs" => &["function", "class", "interface", "enum", "type", "namespace", "const", "let", "var"],
        "py" => &["def", "class"],
        "go" => &["func", "type", "const", "var"],
        "rb" => &["def", "class", "module"],
        "cs" | "java" | "kt" | "swift" => &["class", "interface", "enum", "struct", "func", "void", "public", "private", "protected", "fn"],
        "c" | "h" | "cpp" | "hpp" | "cc" | "cxx" => &["struct", "class", "enum", "namespace", "typedef"],
        _ => &[],
    };
    if kws.is_empty() {
        // Markdown / plain: use headings as the symbol outline.
        if matches!(ext, "md" | "markdown") {
            return text
                .lines()
                .enumerate()
                .filter_map(|(i, l)| {
                    let t = l.trim_start();
                    t.starts_with('#').then(|| {
                        let level = t.chars().take_while(|&c| c == '#').count();
                        (t.trim_start_matches('#').trim().to_string(), format!("h{level}"), i + 1)
                    })
                })
                .filter(|(n, _, _)| !n.is_empty())
                .collect();
        }
        return Vec::new();
    }
    let ident = |s: &str| -> String {
        s.trim_start_matches(['&', '*', '!'])
            .chars()
            .take_while(|&c| c.is_alphanumeric() || c == '_')
            .collect()
    };
    let mut out = Vec::new();
    for (i, line) in text.lines().enumerate() {
        let trimmed = line.trim_start();
        if trimmed.is_empty() || trimmed.starts_with("//") || trimmed.starts_with('#') && ext != "py" {
            continue;
        }
        let words: Vec<&str> = trimmed.split(|c: char| c.is_whitespace() || c == '(').filter(|w| !w.is_empty()).collect();
        if let Some(pos) = words.iter().position(|w| kws.contains(&w.trim_end_matches('!'))) {
            let kw = words[pos].trim_end_matches('!');
            if let Some(next) = words.get(pos + 1) {
                let name = ident(next);
                if name.len() >= 2 && !kws.contains(&name.as_str()) {
                    out.push((name, kw.to_string(), i + 1));
                }
            }
        }
    }
    out
}

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
    /// The event loop is shutting down (Cmd/Ctrl+Q, menu Quit, or a `CloseRequested`
    /// that reached `el.exit()`): persist the window/layout one last time. Idempotent
    /// with the `CloseRequested` save above.
    fn exiting(&mut self, _el: &ActiveEventLoop) {
        if self.gpu.is_some() {
            self.save_session();
        }
    }

    fn about_to_wait(&mut self, el: &ActiveEventLoop) {
        // Lazily fetch inline git blame for the active document (open / tab switch /
        // session restore all land here).
        self.maybe_request_blame();
        // Native macOS menu clicks → commands.
        #[cfg(target_os = "macos")]
        {
            let cmds = self.macos_menu.as_ref().map(|(_, map)| macos_menu::poll(map)).unwrap_or_default();
            for c in cmds {
                self.exec_menu_cmd(c);
            }
        }
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
                WorkerMsg::ExtIcon { gen, id, bytes } => {
                    // A lazily-fetched search-result icon arrived: cache it in the atlas
                    // and rebuild rows so it appears. Gen-gated to drop stale searches' icons.
                    if self.extensions_panel.as_ref().map_or(false, |ep| ep.search_gen() == gen) {
                        if let Some(g) = self.gpu.as_mut() {
                            g.icon_atlas.load_bytes(&g.queue, &id, &bytes);
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
                            self.activate_installed_grammars(); // register grammars now (no reload needed)
                            self.rebuild_ext_rows();
                            // Re-highlight open docs so a newly-installed grammar lights up.
                            if let Some(g) = self.gpu.as_mut() {
                                for d in self.workspace.documents.iter_mut() {
                                    d.invalidate_highlight();
                                    d.reshape(&mut g.font_system);
                                }
                            }
                            self.show_info_dialog("Extension installed.");
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
                WorkerMsg::SearchHits { gen, files } if gen == self.palette_search_gen => {
                    // The palette's `%` live search: append rows (file:line — snippet).
                    if self.palette.active && self.palette.mode == commands::PaletteMode::TextSearch {
                        for f in files {
                            for lm in &f.lines {
                                if self.palette.items.len() >= 500 {
                                    break;
                                }
                                let label = format!("{}:{}  {}", f.rel, lm.line, lm.text.trim());
                                self.palette.items.push(commands::PickItem {
                                    label,
                                    detail: f.rel.clone(),
                                    line: Some(lm.line),
                                });
                            }
                        }
                        self.palette.refilter("");
                        self.redraw();
                    }
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
                WorkerMsg::LspInitialized { sem_token_types } => {
                    self.lsp.on_initialized_legend(sem_token_types);
                }
                WorkerMsg::LspSemanticTokens { id, data } => {
                    if let Some(g) = self.gpu.as_mut() {
                        if self.lsp.apply_semantic(&mut self.workspace.documents, &mut g.font_system, id, data) {
                            self.redraw();
                        }
                    }
                }
                WorkerMsg::LspDiagnostics { server, uri, diags } => {
                    // Only servers that own push diagnostics (e.g. rust-analyzer) get applied;
                    // the TS server's empty push must not clobber ESLint's pulled squiggles.
                    if crate::lsp::server_accepts_push(server) {
                        self.lsp.apply_diagnostics_push(&mut self.workspace.documents, &uri, diags);
                        self.redraw();
                    }
                }
                WorkerMsg::LspDiagnosticReport { id, diags } => {
                    self.lsp.apply_diagnostic_report(&mut self.workspace.documents, id, diags);
                    self.redraw();
                }
                WorkerMsg::LspLocations { id, locs } => {
                    if let Some(kind) = self.lsp.take_locations(id) {
                        self.apply_locations(kind, locs);
                    }
                }
                WorkerMsg::LspTextEdits { id, edits } => {
                    // Formatting result: apply to the doc it was requested for.
                    if let Some(uri) = self.lsp.take_formatting(id) {
                        if edits.is_empty() {
                            self.show_info_dialog("Already formatted.");
                        } else if let Some(gpu) = self.gpu.as_mut() {
                            if let Some(d) = self.workspace.documents.iter_mut().find(|d| d.uri().as_deref() == Some(uri.as_str())) {
                                d.apply_text_edits(&edits, &mut gpu.font_system);
                            }
                        }
                        self.redraw();
                    }
                }
                WorkerMsg::LspWorkspaceEdit { id, changes } => {
                    // Rename result: apply each file's edits (opening files as needed,
                    // leaving them dirty for the user to save — VSCode behavior).
                    if self.lsp.take_rename(id) {
                        self.apply_workspace_edit(changes);
                    }
                }
                WorkerMsg::LspSemanticRefresh { server } => {
                    self.lsp.refresh_semantic(&self.workspace.documents, server);
                }
                WorkerMsg::LspCompletion { id, items } => {
                    // Apply only the newest request's results, and only if the popup is
                    // still open at the prefix we requested for (the user may have moved).
                    if self.lsp.is_current_completion(id) && !items.is_empty() {
                        if let Some((req_id, prefix_start)) = self.completion_req {
                            if req_id == id {
                                self.completion.set_items(completion::from_lsp(items), prefix_start);
                                self.redraw();
                            }
                        }
                    }
                }
                WorkerMsg::LspExited { server } => self.lsp.drop_server(server),
                WorkerMsg::LspLog { server, message } => {
                    eprintln!("[lsp:{server}] {message}");
                    self.lsp_log.push_back(format!("[{server}] {message}"));
                    while self.lsp_log.len() > 1000 {
                        self.lsp_log.pop_front();
                    }
                    // Live-update an open Output tab.
                    if let Some(i) = self.workspace.documents.iter().position(|d| d.read_only && d.name == "Output") {
                        let text = self.output_text();
                        if let Some(gpu) = self.gpu.as_mut() {
                            if let Some(d) = self.workspace.documents.get_mut(i) {
                                d.set_text_external(&text, &mut gpu.font_system);
                            }
                        }
                        self.redraw();
                    }
                }
                WorkerMsg::FeedbackDone { result } => match result {
                    Ok(url) if url.starts_with("http") => open_url(&url),
                    Ok(_) => self.show_info_dialog("Thanks! Your feedback was submitted."),
                    Err(_) => self.show_info_dialog(
                        "Couldn't submit feedback. Check that GitHub CLI (gh) is installed and you're logged in.",
                    ),
                },
                WorkerMsg::CommitMessage { result } => {
                    match result {
                        Ok(msg) => {
                            if let (Some(scp), Some(g)) =
                                (self.source_control.as_mut(), self.gpu.as_mut())
                            {
                                scp.set_generated_message(&mut g.font_system, Some(&msg));
                            }
                        }
                        Err(e) => {
                            if let (Some(scp), Some(g)) =
                                (self.source_control.as_mut(), self.gpu.as_mut())
                            {
                                scp.set_generated_message(&mut g.font_system, None);
                            }
                            self.show_info_dialog(&format!("Couldn't generate a commit message.\n\n{e}"));
                        }
                    }
                    self.redraw();
                }
                WorkerMsg::FsChanged { paths } => {
                    // Coalesce bursts: accumulate paths, flush ~500ms after the first
                    // event of a quiet-ish window (npm install etc. won't thrash git).
                    self.fs_dirty.extend(paths);
                    if self.fs_flush_due.is_none() {
                        self.fs_flush_due = Some(Instant::now() + Duration::from_millis(500));
                    }
                }
                WorkerMsg::Blame { path, lines } => {
                    // Attach to the matching open document (by path); ignore if it
                    // closed or its content changed enough that blame is stale-on-arrival.
                    if let Some(d) = self.workspace.documents.iter_mut().find(|d| d.path.as_deref() == Some(path.as_path())) {
                        d.blame = lines;
                        self.redraw();
                    }
                }
                // ---- Debug adapter events ----
                WorkerMsg::DebugInitialized => {
                    self.debug_handshook = true;
                    // Adapter is up: send the launch/attach request now (per DAP order;
                    // setBreakpoints + configurationDone wait for the `initialized` event).
                    if let (Some(d), Some(cfg)) = (self.dap.as_mut(), self.debug_config.as_ref()) {
                        match cfg.request {
                            debug_config::Request::Launch => d.launch(cfg.args.clone()),
                            debug_config::Request::Attach => d.attach(cfg.args.clone()),
                        }
                    }
                }
                WorkerMsg::DebugConfigured => {
                    // `initialized` event: register breakpoints, then configurationDone.
                    let bps = self.debug_breakpoint_map();
                    if let Some(d) = self.dap.as_mut() {
                        for (path, lines) in &bps {
                            d.set_breakpoints(path, lines);
                        }
                        d.configuration_done();
                    }
                }
                WorkerMsg::DebugStopped { thread_id, .. } => {
                    self.debug_thread = Some(thread_id);
                    if let Some(d) = self.dap.as_mut() {
                        d.stack_trace(thread_id);
                    }
                }
                WorkerMsg::DebugThreads { ids } => {
                    // Pause was requested: suspend the first thread (debugpy stops the
                    // whole process and reports its current frame's source).
                    if self.debug_pending_pause {
                        self.debug_pending_pause = false;
                        if let (Some(d), Some(&tid)) = (self.dap.as_mut(), ids.first()) {
                            d.pause(tid);
                        }
                    }
                }
                WorkerMsg::DebugContinued => {
                    self.clear_execution_lines();
                    self.redraw();
                }
                WorkerMsg::DebugStackTrace { frames } => {
                    // Highlight + open the top frame's source.
                    self.clear_execution_lines();
                    if let Some(top) = frames.first() {
                        if let Some(path) = top.path.clone() {
                            self.open_file_at(PathBuf::from(&path), top.line.max(1) as usize, 0);
                            if let Some(doc) = self.workspace.active_doc_mut() {
                                doc.execution_line = Some((top.line.max(1) as usize).saturating_sub(1));
                            }
                        }
                        // Pull variables for the top frame.
                        if let Some(d) = self.dap.as_mut() {
                            d.scopes(top.id);
                        }
                    }
                    if let (Some(p), Some(g)) = (self.debug.as_mut(), self.gpu.as_mut()) {
                        p.set_stopped(&mut g.font_system, frames);
                    }
                    self.redraw();
                }
                WorkerMsg::DebugScopes { scopes } => {
                    // Hand the scopes to the panel, then fetch the variables of any it
                    // wants open by default (Locals).
                    let refs = if let (Some(p), Some(g)) = (self.debug.as_mut(), self.gpu.as_mut()) {
                        p.set_scopes(&mut g.font_system, scopes);
                        p.pending_scope_refs()
                    } else {
                        Vec::new()
                    };
                    if let Some(d) = self.dap.as_mut() {
                        for r in refs {
                            d.variables(r);
                        }
                    }
                    self.redraw();
                }
                WorkerMsg::DebugVariables { var_ref, vars } => {
                    if let (Some(p), Some(g)) = (self.debug.as_mut(), self.gpu.as_mut()) {
                        p.set_children(&mut g.font_system, var_ref, vars);
                    }
                    self.redraw();
                }
                WorkerMsg::DebugBreakpointsVerified { .. } => {}
                WorkerMsg::DebugOutput { text, .. } | WorkerMsg::DebugLog { text } => {
                    self.run_in_terminal_output(&text);
                }
                WorkerMsg::DebugRunInTerminal { seq, cwd, args } => {
                    // Adapter asked us to run the debuggee in a terminal.
                    let cmd = args.join(" ");
                    let _ = cwd;
                    self.run_in_terminal(&cmd);
                    if let Some(d) = self.dap.as_mut() {
                        d.reply(seq, "runInTerminal", true, serde_json::json!({ "processId": std::process::id() }));
                    }
                }
                WorkerMsg::DebugTerminated | WorkerMsg::DebugExited => {
                    // If the adapter died before completing the handshake, it almost
                    // certainly isn't installed — surface its install hint.
                    let hint = if !self.debug_handshook {
                        self.debug_config.as_ref().map(|c| c.adapter.install_hint)
                    } else {
                        None
                    };
                    self.dap = None;
                    self.debug_thread = None;
                    self.debug_config = None;
                    self.clear_execution_lines();
                    if let (Some(p), Some(g)) = (self.debug.as_mut(), self.gpu.as_mut()) {
                        p.set_session(&mut g.font_system, ui::debug_panel::Session::Idle);
                    }
                    if let Some(h) = hint {
                        self.show_info_dialog(h);
                    }
                    self.redraw();
                }
            }
        }

        let now = Instant::now();

        // Promote a rested diagnostic hover into a visible card (VS Code-style delay).
        if let Some((info, hx, hy, t0)) = self.hover_pending.clone() {
            if now.duration_since(t0) >= HOVER_DELAY {
                self.hover_pending = None;
                self.hover_tip = Some((info, hx, hy));
                self.redraw();
            }
        }
        let hover_wake = self.hover_pending.as_ref().map(|(.., t0)| *t0 + HOVER_DELAY);

        // Promote a rested inline-blame annotation into its full-commit card.
        if let Some((txt, bx, by, t0)) = self.blame_pending.clone() {
            if now.duration_since(t0) >= BLAME_DELAY {
                self.blame_pending = None;
                self.commit_tip = Some((txt, bx, by));
                self.redraw();
            }
        }
        let blame_wake = self.blame_pending.as_ref().map(|(.., t0)| *t0 + BLAME_DELAY);

        // Flush a debounced batch of filesystem changes (Source Control + reload).
        if let Some(due) = self.fs_flush_due {
            if now >= due {
                self.fs_flush_due = None;
                self.flush_fs_changes();
            }
        }
        let fs_wake = self.fs_flush_due;

        // Debounced marketplace search: fire once the user pauses typing. While a search
        // is staged or in flight, keep ticking ~110ms so the loader spinner animates.
        let (debounce_wake, searching) = self
            .extensions_panel
            .as_mut()
            .map(|ep| (ep.poll_search(&self.worker_tx), ep.is_searching()))
            .unwrap_or((None, false));
        if searching {
            self.redraw();
        }
        let search_wake = if searching {
            min_instant(debounce_wake, Some(now + Duration::from_millis(110)))
        } else {
            debounce_wake
        };

        // Language-server document sync (open + debounced didChange).
        self.sync_lsp();

        // Navigation history: record tab switches / edits for Go > Back / Last Edit.
        self.nav.tick(&self.workspace);

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

        // Drain the pty-host link even with the panel hidden: the daemon may ask this
        // window to raise itself (another instance opened our workspace).
        let polled = self.terminal.poll();
        if self.terminal.focus_requested {
            self.terminal.focus_requested = false;
            if let Some(g) = self.gpu.as_ref() {
                g.window.focus_window();
            }
        }
        // A workspace switch swapped the panel's contents out — rebuild it now with
        // the new folder's restored shells (or a fresh one) so it changes in place.
        if self.terminal.needs_initial() {
            let panel = self.layout().terminal_panel;
            self.terminal.spawn_initial(panel, self.terminal_cell_w);
            self.redraw();
        }

        // Integrated terminal: keep ticking while open so new output appears promptly.
        if self.terminal.visible {
            let mut changed = polled;
            // Blink the terminal block cursor on the standard cadence (PowerShell-style).
            if now.duration_since(self.term_last_blink) >= Duration::from_millis(theme::BLINK_MS) {
                self.term_blink_on = !self.term_blink_on;
                self.term_last_blink = now;
                changed = true;
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
            // While connected to the pty-host, keep a slow heartbeat so cross-window
            // focus requests are drained even when this window is otherwise idle.
            let daemon_wake = self.terminal.connected().then(|| now + Duration::from_millis(500));
            let wake = min_instant(
                min_instant(
                    min_instant(
                        min_instant(min_instant(min_instant(scroll_wake, autosave_wake), hover_wake), search_wake),
                        blame_wake,
                    ),
                    fs_wake,
                ),
                daemon_wake,
            );
            el.set_control_flow(match wake {
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
        let mut wake = scroll_wake.map_or(blink_wake, |s| s.min(blink_wake));
        if let Some(hw) = hover_wake {
            wake = wake.min(hw);
        }
        if let Some(sw) = search_wake {
            wake = wake.min(sw);
        }
        el.set_control_flow(ControlFlow::WaitUntil(wake));
    }

    fn resumed(&mut self, el: &ActiveEventLoop) {
        if self.gpu.is_some() {
            return;
        }
        // Restore the saved window size for this workspace, else the default.
        let saved_window = state::State::load().session_for(&self.cwd).and_then(|s| s.window);
        let inner: winit::dpi::Size = match saved_window {
            Some((w, h)) if w >= 480 && h >= 360 => winit::dpi::PhysicalSize::new(w, h).into(),
            _ => LogicalSize::new(1400.0, 900.0).into(),
        };
        let mut attrs = Window::default_attributes()
            .with_title(window_title(&self.cwd))
            .with_window_icon(app_icon())
            .with_inner_size(inner);
        // macOS: keep the native traffic lights (top-left) but let our content fill
        // the window behind a transparent titlebar — so we render our own header but
        // the OS draws min/zoom/close. Other platforms: fully borderless (we draw the
        // window controls ourselves).
        #[cfg(target_os = "macos")]
        {
            use winit::platform::macos::WindowAttributesExtMacOS;
            attrs = attrs
                .with_titlebar_transparent(true)
                .with_fullsize_content_view(true)
                .with_title_hidden(true);
        }
        #[cfg(not(target_os = "macos"))]
        {
            attrs = attrs.with_decorations(false);
        }
        let window = Arc::new(el.create_window(attrs).expect("create window"));
        // Install the native macOS menu bar (system menu). Kept alive on `self`.
        #[cfg(target_os = "macos")]
        {
            self.macos_menu = Some(macos_menu::install());
        }
        match pollster::block_on(GpuState::new(window)) {
            Ok(gpu) => {
                self.gpu = Some(gpu);
                if let Some(g) = self.gpu.as_mut() {
                    self.search = Some(ui::search_panel::SearchPanel::new(&mut g.font_system));
                    self.debug = Some(ui::debug_panel::DebugPanel::new(&mut g.font_system));
                    self.extensions_panel =
                        Some(ui::extensions_panel::ExtensionsPanel::new(&mut g.font_system));
                    // Use the git top-level (not the opened cwd) so status paths and
                    // diff/stage pathspecs align when a subdirectory of a repo is open.
                    let mut scp = ui::source_control_panel::SourceControlPanel::new(
                        &mut g.font_system,
                        git::repo_root(&self.cwd),
                    );
                    // Restore the persisted tree/list view choice + GRAPH state.
                    let st = state::State::load();
                    scp.set_tree_mode(st.scm_tree_view);
                    scp.set_graph_open(st.scm_graph_open);
                    self.source_control = Some(scp);
                }
                self.open_initial();
                // Restore the prior window/layout for this workspace (open files,
                // panels, terminal height) — after panels are built, before first draw.
                self.restore_session();
                // Populate the Source Control change-count badge at startup.
                self.refresh_source_control();
                // Check GitHub for a newer release now, then re-check every 6 hours so
                // a long-running window still learns about new releases.
                update::check_async(self.worker_tx.clone(), false);
                update::check_periodic(self.worker_tx.clone(), std::time::Duration::from_secs(6 * 3600));
                // Register this window with the pty-host (single-window-per-folder):
                // if another live window already has this workspace open, it raises
                // itself and this duplicate instance closes.
                if self.terminal.register_window() {
                    el.exit();
                }
            }
            Err(e) => {
                eprintln!("init failed: {e:?}");
                el.exit();
            }
        }
    }

    fn window_event(&mut self, el: &ActiveEventLoop, _id: WindowId, event: WindowEvent) {
        match event {
            // Warn first when terminals have running processes; the dialog's choice
            // then closes via `pending_close`.
            WindowEvent::CloseRequested => {
                if self.confirm_close_window() {
                    self.save_session();
                    el.exit();
                }
            }
            // Persist the layout when the window loses focus, so a later force-quit
            // (no clean exit) still restores the most recent arrangement.
            WindowEvent::Focused(false) => {
                if self.gpu.is_some() {
                    self.save_session();
                }
            }
            WindowEvent::ModifiersChanged(m) => {
                self.mods = m.state();
                // Ctrl/Cmd toggles the terminal link highlight + cursor under a
                // stationary pointer — refresh so it appears/clears without moving.
                if self.terminal.visible {
                    self.redraw();
                }
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
                // While dragging (tab / explorer entry / terminal tab), the floating
                // ghost must track the cursor every frame — skip the heavy hover
                // hit-testing and just redraw so the ghost stays glued to the pointer.
                let dragging = matches!(self.tab_drag, Some((_, _, true)))
                    || matches!(self.tree_drag, Some((_, _, true)))
                    || self.terminal.dragging_tab().is_some();
                if dragging {
                    self.redraw();
                } else {
                    self.recompute_hover();
                }
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
            // Files dragged in from the OS: over the terminal the quoted path is
            // pasted; elsewhere a folder becomes the workspace and a file opens.
            WindowEvent::DroppedFile(path) => {
                let p = (self.mouse_pos.x as f32, self.mouse_pos.y as f32);
                let over_terminal = self.terminal.visible
                    && self.layout().terminal_panel.map_or(false, |tp| tp.contains(p));
                if over_terminal {
                    self.terminal.write_focused(shell_quoted(&path).as_bytes());
                    self.terminal.focused = true; // typing continues in the shell (#31)
                } else if path.is_dir() {
                    self.open_folder(path);
                } else {
                    self.open_file_at(path, 1, 0);
                }
                self.redraw();
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

/// Reveal a path in the OS file manager (Finder / Explorer / file manager).
fn reveal_in_os(path: &Path) {
    #[cfg(target_os = "macos")]
    let _ = std::process::Command::new("open").arg("-R").arg(path).spawn();
    #[cfg(target_os = "windows")]
    let _ = std::process::Command::new("explorer").arg("/select,").arg(path).spawn();
    #[cfg(all(unix, not(target_os = "macos")))]
    let _ = std::process::Command::new("xdg-open").arg(path.parent().unwrap_or(path)).spawn();
}

/// Shell-single-quote a path for pasting into a terminal (a trailing space lets
/// repeated drops form an argument list).
fn shell_quoted(path: &Path) -> String {
    format!("'{}' ", path.to_string_lossy().replace('\'', r"'\''"))
}

/// Running processes that can be attached with debugpy (Python interpreters).
/// Returns (pid, command-line) pairs, newest-listed first. Unix-only `ps`; empty
/// elsewhere (the picker then shows a hint).
fn list_debuggable_processes() -> Vec<(i64, String)> {
    let out = match std::process::Command::new("ps").args(["-axww", "-o", "pid=,command="]).output() {
        Ok(o) if o.status.success() => o.stdout,
        _ => return Vec::new(),
    };
    let me = std::process::id() as i64;
    let text = String::from_utf8_lossy(&out);
    let mut v: Vec<(i64, String)> = Vec::new();
    for line in text.lines() {
        let line = line.trim_start();
        let Some((pid_str, cmd)) = line.split_once(char::is_whitespace) else { continue };
        let Ok(pid) = pid_str.trim().parse::<i64>() else { continue };
        let cmd = cmd.trim();
        // Only CPython processes are debugpy-attachable; skip ourselves and the picker's ps.
        let low = cmd.to_lowercase();
        let is_python = low.contains("python") && !low.contains("debugpy.adapter");
        if pid == me || !is_python {
            continue;
        }
        let label = if cmd.len() > 90 { format!("{}…", &cmd[..90]) } else { cmd.to_string() };
        v.push((pid, label));
    }
    v
}

/// First non-existing "name copy[ N]" sibling of `dest` (Finder-style collision
/// names for explorer paste).
fn unique_sibling(dest: &Path) -> PathBuf {
    let dir = dest.parent().unwrap_or_else(|| Path::new("."));
    let stem = dest.file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default();
    let ext = dest.extension().map(|e| format!(".{}", e.to_string_lossy())).unwrap_or_default();
    for n in 0..1000 {
        let name = if n == 0 {
            format!("{stem} copy{ext}")
        } else {
            format!("{stem} copy {}{ext}", n + 1)
        };
        let cand = dir.join(name);
        if !cand.exists() {
            return cand;
        }
    }
    dest.to_path_buf()
}

/// Copy a file or directory tree (explorer Copy/Paste).
fn copy_recursive(src: &Path, dest: &Path) -> std::io::Result<()> {
    if src.is_dir() {
        std::fs::create_dir_all(dest)?;
        for entry in std::fs::read_dir(src)? {
            let entry = entry?;
            copy_recursive(&entry.path(), &dest.join(entry.file_name()))?;
        }
        Ok(())
    } else {
        std::fs::copy(src, dest).map(|_| ())
    }
}

fn main() -> Result<()> {
    env_logger::init();
    // Pty-host mode: this process is the detached daemon that owns terminal PTYs so
    // they survive GUI restarts. It never opens a window — just runs the event loop.
    if std::env::args().any(|a| a == "--pty-host") {
        ptyhost::client::run_daemon()?;
        return Ok(());
    }
    // Move a legacy ~/.nova config dir to ~/.aether before anything reads config.
    settings::migrate_legacy_config_dir();
    // Optional path arg: a directory becomes the workspace root; a file is opened
    // (and its parent becomes the root). Falls back to the current directory.
    // `--new-window` opens a folder-less window (File → New Window, like VSCode) —
    // no last-workspace restore; the user picks a folder via Open Folder.
    let new_window = std::env::args().any(|a| a == "--new-window");
    let arg = std::env::args().nth(1).filter(|a| a != "--new-window").map(PathBuf::from);
    let (root, initial_file) = match arg {
        _ if new_window => (PathBuf::new(), None),
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
