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
    (Command::ToggleTerminal, "View: Toggle Terminal", "Ctrl+`"),
];

pub struct PaletteState {
    pub active: bool,
    pub selected: usize,
    pub filtered: Vec<usize>,
}

impl PaletteState {
    pub fn new() -> Self {
        let filtered: Vec<usize> = (0..COMMANDS.len()).collect();
        Self {
            active: false,
            selected: 0,
            filtered,
        }
    }
    pub fn refilter(&mut self, query: &str) {
        let q = query.to_lowercase();
        self.filtered = (0..COMMANDS.len())
            .filter(|&i| q.is_empty() || COMMANDS[i].1.to_lowercase().contains(&q))
            .collect();
        if self.selected >= self.filtered.len() {
            self.selected = self.filtered.len().saturating_sub(1);
        }
    }
    pub fn open(&mut self) {
        self.active = true;
        self.selected = 0;
        self.refilter("");
    }
    pub fn close(&mut self) {
        self.active = false;
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
