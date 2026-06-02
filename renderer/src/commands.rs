// Command-palette command set + the palette/find-bar UI state.

#[derive(Clone, Copy)]
pub enum Command {
    Save,
    Close,
    Find,
    Undo,
    Redo,
    SelectAll,
    ToggleSidebar,
    NewFile,
    OpenSettings,
    OpenDefaultSettings,
    ToggleTerminal,
    OpenFolder,
    ColorTheme,
}

pub const COMMANDS: &[(Command, &str, &str)] = &[
    (Command::Save, "File: Save", "Ctrl+S"),
    (Command::NewFile, "File: New Untitled", ""),
    (Command::OpenFolder, "File: Open Folder", "Ctrl+O"),
    (Command::Close, "File: Close Tab", "Ctrl+W"),
    (Command::Find, "Edit: Find", "Ctrl+F"),
    (Command::Undo, "Edit: Undo", "Ctrl+Z"),
    (Command::Redo, "Edit: Redo", "Ctrl+Y"),
    (Command::SelectAll, "Edit: Select All", "Ctrl+A"),
    (Command::ToggleSidebar, "View: Toggle Sidebar", ""),
    (Command::OpenSettings, "Preferences: Open Settings (JSON)", ""),
    (Command::OpenDefaultSettings, "Preferences: Open Default Settings (JSON)", ""),
    (Command::ColorTheme, "Preferences: Color Theme", ""),
    (Command::ToggleTerminal, "View: Toggle Terminal", "Ctrl+`"),
];

/// What a quick-pick selection does. Each variant carries no data here; the chosen
/// item's `label` is read from `PaletteState` when the pick is committed.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum PickKind {
    SetColorTheme,
}

/// One row in a quick-pick list (dynamic, unlike the fixed `COMMANDS`).
#[derive(Clone)]
pub struct PickItem {
    pub label: String,  // committed value (e.g. the theme label) + primary text
    pub detail: String, // dim right-hand hint (e.g. "dark" / source extension)
}

/// The palette is either the fixed command list or a dynamic quick-pick (theme
/// chooser, etc.) — the same widget, two data sources, so all list pickers reuse it.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum PaletteMode {
    Commands,
    QuickPick(PickKind),
}

pub struct PaletteState {
    pub active: bool,
    pub selected: usize,
    pub filtered: Vec<usize>,
    pub mode: PaletteMode,
    pub items: Vec<PickItem>, // the quick-pick source (empty in Commands mode)
}

impl PaletteState {
    pub fn new() -> Self {
        let filtered: Vec<usize> = (0..COMMANDS.len()).collect();
        Self {
            active: false,
            selected: 0,
            filtered,
            mode: PaletteMode::Commands,
            items: Vec::new(),
        }
    }
    /// Number of rows in the active source (commands or quick-pick items).
    fn source_len(&self) -> usize {
        match self.mode {
            PaletteMode::Commands => COMMANDS.len(),
            PaletteMode::QuickPick(_) => self.items.len(),
        }
    }
    /// The display text of row `i` in the active source (for filtering).
    fn row_text(&self, i: usize) -> String {
        match self.mode {
            PaletteMode::Commands => COMMANDS[i].1.to_lowercase(),
            PaletteMode::QuickPick(_) => self.items[i].label.to_lowercase(),
        }
    }
    pub fn refilter(&mut self, query: &str) {
        let q = query.to_lowercase();
        self.filtered = (0..self.source_len())
            .filter(|&i| q.is_empty() || self.row_text(i).contains(&q))
            .collect();
        if self.selected >= self.filtered.len() {
            self.selected = self.filtered.len().saturating_sub(1);
        }
    }
    pub fn open(&mut self) {
        self.mode = PaletteMode::Commands;
        self.items.clear();
        self.active = true;
        self.selected = 0;
        self.refilter("");
    }
    /// Open as a dynamic quick-pick over `items`, committing via `kind`.
    pub fn open_quick_pick(&mut self, kind: PickKind, items: Vec<PickItem>) {
        self.mode = PaletteMode::QuickPick(kind);
        self.items = items;
        self.active = true;
        self.selected = 0;
        self.refilter("");
    }
    /// The selected quick-pick `(kind, label)`, if in quick-pick mode.
    pub fn selected_pick(&self) -> Option<(PickKind, String)> {
        match self.mode {
            PaletteMode::QuickPick(kind) => self
                .filtered
                .get(self.selected)
                .and_then(|&i| self.items.get(i))
                .map(|it| (kind, it.label.clone())),
            PaletteMode::Commands => None,
        }
    }
    pub fn close(&mut self) {
        self.active = false;
    }
    /// Move the selection down one row (wraps to the top).
    pub fn select_next(&mut self) {
        if !self.filtered.is_empty() {
            self.selected = (self.selected + 1) % self.filtered.len();
        }
    }
    /// Move the selection up one row (wraps to the bottom).
    pub fn select_prev(&mut self) {
        if !self.filtered.is_empty() {
            self.selected = if self.selected == 0 {
                self.filtered.len() - 1
            } else {
                self.selected - 1
            };
        }
    }
    /// The command under the current selection, if any (Commands mode only).
    pub fn selected_command(&self) -> Option<Command> {
        if self.mode != PaletteMode::Commands {
            return None;
        }
        self.filtered.get(self.selected).map(|&i| COMMANDS[i].0)
    }
}

pub struct FindBarState {
    pub active: bool,
    pub last_match: Option<usize>,
}

impl FindBarState {
    pub fn new() -> Self {
        Self {
            active: false,
            last_match: None,
        }
    }
}
