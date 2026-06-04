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
    ToggleLineComment,
    ToggleBlockComment,
    MoveLineUp,
    MoveLineDown,
    CopyLineUp,
    CopyLineDown,
    DuplicateSelection,
    ExpandSelection,
    ShrinkSelection,
    GotoBracket,
    NavBack,
    NavForward,
    LastEditLocation,
    NextProblem,
    PrevProblem,
    NextEditor,
    PrevEditor,
    GotoDefinition,
    GotoDeclaration,
    GotoTypeDefinition,
    GotoImplementations,
    GotoReferences,
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
    (Command::ToggleLineComment, "Edit: Toggle Line Comment", "Ctrl+/"),
    (Command::ToggleBlockComment, "Edit: Toggle Block Comment", "Shift+Alt+A"),
    (Command::MoveLineUp, "Edit: Move Line Up", "Alt+Up"),
    (Command::MoveLineDown, "Edit: Move Line Down", "Alt+Down"),
    (Command::CopyLineUp, "Edit: Copy Line Up", "Shift+Alt+Up"),
    (Command::CopyLineDown, "Edit: Copy Line Down", "Shift+Alt+Down"),
    (Command::DuplicateSelection, "Edit: Duplicate Selection", ""),
    (Command::ExpandSelection, "Edit: Expand Selection", "Shift+Alt+Right"),
    (Command::ShrinkSelection, "Edit: Shrink Selection", "Shift+Alt+Left"),
    (Command::GotoBracket, "Go: Go to Bracket", "Ctrl+Shift+\\"),
    (Command::NavBack, "Go: Back", "Alt+Left"),
    (Command::NavForward, "Go: Forward", "Alt+Right"),
    (Command::LastEditLocation, "Go: Last Edit Location", ""),
    (Command::NextProblem, "Go: Next Problem", "F8"),
    (Command::PrevProblem, "Go: Previous Problem", "Shift+F8"),
    (Command::NextEditor, "View: Next Editor", "Ctrl+PageDown"),
    (Command::PrevEditor, "View: Previous Editor", "Ctrl+PageUp"),
    (Command::GotoDefinition, "Go: Go to Definition", "F12"),
    (Command::GotoDeclaration, "Go: Go to Declaration", ""),
    (Command::GotoTypeDefinition, "Go: Go to Type Definition", ""),
    (Command::GotoImplementations, "Go: Go to Implementations", ""),
    (Command::GotoReferences, "Go: Go to References", "Shift+F12"),
];

/// What a quick-pick selection does. Each variant carries no data here; the chosen
/// item's `label` is read from `PaletteState` when the pick is committed.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum PickKind {
    SetColorTheme,
    OpenRecent,
    Problem,
    Location, // jump targets from an LSP definition/references response
}

/// One row in a quick-pick list (dynamic, unlike the fixed `COMMANDS`).
#[derive(Clone)]
pub struct PickItem {
    pub label: String,        // committed value / primary text
    pub detail: String,       // dim right-hand hint
    pub line: Option<usize>,  // 1-based target line (go-to-symbol)
}

impl PickItem {
    pub fn new(label: impl Into<String>, detail: impl Into<String>) -> Self {
        Self { label: label.into(), detail: detail.into(), line: None }
    }
    pub fn at_line(label: impl Into<String>, detail: impl Into<String>, line: usize) -> Self {
        Self { label: label.into(), detail: detail.into(), line: Some(line) }
    }
}

/// Quick-open modes, VSCode-style. Most are driven by the input's leading prefix
/// (`>` commands, `@` symbols, `:` line, none = files); `QuickPick` is a one-off
/// chooser (e.g. themes) opened programmatically.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum PaletteMode {
    Commands,         // `>` — run a command
    Files,            // (no prefix) — go to file
    Symbols,          // `@` — go to symbol in the active file
    GoToLine,         // `:` — go to line
    TextSearch,       // `%` — find text across the workspace (opens the Search panel)
    WorkspaceSymbols, // `#` — language-server workspace/symbol query
    QuickPick(PickKind),
}

pub struct PaletteState {
    pub active: bool,
    pub selected: usize,
    pub filtered: Vec<usize>,
    pub mode: PaletteMode,
    pub items: Vec<PickItem>, // the quick-pick source (empty in Commands mode)
    pub scroll: f32,          // list scroll offset in px (clamped/followed in render)
    pub follow_selection: bool, // scroll to keep the selection visible next frame
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
            scroll: 0.0,
            follow_selection: true,
        }
    }
    /// Number of rows in the active source (commands or item list).
    fn source_len(&self) -> usize {
        match self.mode {
            PaletteMode::Commands => COMMANDS.len(),
            _ => self.items.len(),
        }
    }
    /// The display text of row `i` in the active source (for filtering).
    fn row_text(&self, i: usize) -> String {
        match self.mode {
            PaletteMode::Commands => COMMANDS[i].1.to_lowercase(),
            _ => self.items[i].label.to_lowercase(),
        }
    }
    pub fn refilter(&mut self, query: &str) {
        let q = query.trim().to_lowercase();
        if q.is_empty() {
            self.filtered = (0..self.source_len()).take(500).collect();
        } else {
            // Fuzzy (VSCode-style): query chars must appear in order (spaces in the
            // query ignored, so "login screen" hits LoginScreen.tsx); results rank by
            // contiguity, word boundaries, and filename-segment matches.
            let mut scored: Vec<(i32, usize)> = (0..self.source_len())
                .filter_map(|i| fuzzy_score(&self.row_text(i), &q).map(|s| (s, i)))
                .collect();
            scored.sort_by(|a, b| b.0.cmp(&a.0).then(a.1.cmp(&b.1)));
            self.filtered = scored.into_iter().map(|(_, i)| i).take(500).collect();
        }
        if self.selected >= self.filtered.len() {
            self.selected = self.filtered.len().saturating_sub(1);
        }
        self.scroll = 0.0; // new results → back to the top
        self.follow_selection = true;
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
    /// Switch the source for a prefix-driven mode (Files/Symbols), keeping the
    /// palette open. Resets the selection to the top.
    pub fn set_source(&mut self, mode: PaletteMode, items: Vec<PickItem>) {
        self.mode = mode;
        self.items = items;
        self.selected = 0;
        self.refilter("");
    }
    /// The selected item in any item-based mode (Files/Symbols/QuickPick).
    pub fn selected_item(&self) -> Option<&PickItem> {
        self.filtered.get(self.selected).and_then(|&i| self.items.get(i))
    }
    /// The selected quick-pick `(kind, label)`, if in quick-pick mode.
    pub fn selected_pick(&self) -> Option<(PickKind, String)> {
        match self.mode {
            PaletteMode::QuickPick(kind) => self
                .filtered
                .get(self.selected)
                .and_then(|&i| self.items.get(i))
                .map(|it| (kind, it.label.clone())),
            _ => None,
        }
    }
    pub fn close(&mut self) {
        self.active = false;
    }
    /// Move the selection down one row (wraps to the top).
    pub fn select_next(&mut self) {
        if !self.filtered.is_empty() {
            self.selected = (self.selected + 1) % self.filtered.len();
            self.follow_selection = true;
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
            self.follow_selection = true;
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
    pub active: bool,                       // the widget is open/visible
    pub focused: bool,                      // a find/replace input has keyboard focus
    pub on_replace: bool,                   // focus is on the replace input (vs find)
    pub replace_open: bool,                 // the replace row is expanded
    pub opts: crate::search::SearchOpts,    // case / whole-word / regex
    pub matches: Vec<(usize, usize)>,       // byte ranges of matches in the active doc
    pub index: Option<usize>,               // current match within `matches`
}

impl FindBarState {
    pub fn new() -> Self {
        Self {
            active: false,
            focused: false,
            on_replace: false,
            replace_open: false,
            opts: crate::search::SearchOpts::default(),
            matches: Vec::new(),
            index: None,
        }
    }
}

/// Fuzzy match `q` (lowercase, whitespace ignored) against `text` (lowercase) as an
/// in-order subsequence. None = no match; higher scores = better: contiguous runs,
/// matches right after a separator (word starts), and matches inside the last path
/// segment (the filename) all earn bonuses, so "login screen" ranks
/// `…/auth/LoginScreen.tsx` above incidental scattered-letter matches.
fn fuzzy_score(text: &str, q: &str) -> Option<i32> {
    let t: Vec<char> = text.chars().collect();
    let file_start = text.rfind('/').map(|i| text[..i].chars().count() + 1).unwrap_or(0);
    let mut score = 0i32;
    let mut ti = 0usize;
    let mut last: Option<usize> = None;
    let mut first: Option<usize> = None;
    for qc in q.chars().filter(|c| !c.is_whitespace()) {
        let mut found = None;
        while ti < t.len() {
            if t[ti] == qc {
                found = Some(ti);
                break;
            }
            ti += 1;
        }
        let i = found?;
        first.get_or_insert(i);
        score += 1;
        if last == Some(i.wrapping_sub(1)) {
            score += 8; // contiguous with the previous matched char
        }
        if i == 0 || matches!(t[i - 1], '/' | '_' | '-' | '.' | ' ') {
            score += 6; // word start
        }
        if i >= file_start {
            score += 2; // inside the filename segment
        }
        last = Some(i);
        ti = i + 1;
    }
    // Strong preference for matches that begin in the filename itself.
    if first.map_or(false, |f| f >= file_start) {
        score += 30;
    }
    Some(score)
}

#[cfg(test)]
mod fuzzy_tests {
    use super::fuzzy_score;

    #[test]
    fn fuzzy_matches_and_ranks() {
        // Space-separated words match camel-case filenames.
        assert!(fuzzy_score("mobile/src/screens/auth/loginscreen.tsx", "login screen").is_some());
        // Plain subsequence ("lgscr") matches too.
        assert!(fuzzy_score("mobile/src/screens/auth/loginscreen.tsx", "lgscr").is_some());
        // Non-subsequence does not.
        assert!(fuzzy_score("readme.md", "xyz").is_none());
        // A filename match outranks a scattered path match.
        let a = fuzzy_score("screens/auth/loginscreen.tsx", "loginscreen").unwrap();
        let b = fuzzy_score("logs/in_screen_dump/other.txt", "loginscreen").unwrap_or(0);
        assert!(a > b, "filename match should win: {a} vs {b}");
    }
}
