// Top menu-bar dropdown contents — the complete VSCode menu set. Each top-level
// title (File / Edit / … indexed the same as `widgets::MENU_ITEMS`) maps to a flat
// list of entries with separators and right-aligned shortcut hints. Entries the
// editor can't perform yet are `Stub`s: they show a "not implemented yet" notice,
// so the full menu surface exists ahead of the features (and doubles as a roadmap).

use crate::commands::Command;

#[derive(Clone, Copy)]
pub enum MenuCmd {
    Cmd(Command),
    Palette,      // open the command palette (commands mode)
    QuickOpen,    // go to file (no-prefix quick open)
    GotoSymbol,   // palette in `@` mode
    GotoLine,     // palette in `:` mode
    Feedback,     // open the feedback / report-issue tab
    CheckUpdate,  // check GitHub for a newer release
    About,        // show version info
    NewWindow,    // open a new app window
    OpenRecent,   // quick-pick of recent folders
    AutoSave,     // toggle files.autoSave (afterDelay)
    RevertFile,   // reload the active doc from disk
    CloseFolder,  // back to a folder-less window
    ZenMode,      // distraction-free: fullscreen, no chrome
    CenteredLayout,
    Problems,     // quick-pick of all diagnostics
    OutputLog,    // read-only tab with language-server logs
    ReplaceInFiles,
    GotoWsSymbol, // palette in `#` mode
    RunActiveFile,
    RunSelectedText,
    Welcome,        // Help > Welcome tab
    ShortcutsRef,   // keyboard-shortcuts reference tab
    Tips,           // tips & tricks tab
    OpenFileDlg,  // OS file-picker → open the chosen file
    SaveAs,       // save the active doc under a new path
    SaveAll,      // save every dirty doc
    Cut,          // editor clipboard ops (route through App's cut/copy/paste)
    Copy,
    Paste,
    Replace,      // open find with the replace row expanded
    FindInFiles,  // show the Search sidebar
    ShowExplorer, // sidebar views
    ShowSearch,
    ShowScm,
    ShowExtensions,
    FullScreen,   // toggle OS fullscreen
    ZoomIn,
    ZoomOut,
    ZoomReset,
    NewTerminal,   // terminal: new tab / split / kill
    SplitTerminal,
    KillTerminal,
    OpenDocs,      // browser: repository README
    OpenReleases,  // browser: releases page
    Exit,          // quit the app
    /// A menu row that exists for completeness but isn't implemented yet — shows
    /// an informational dialog naming the feature.
    Stub(&'static str),
    /// Visual divider (not clickable).
    Separator,
}

pub struct Entry {
    pub label: &'static str,
    pub cmd: MenuCmd,
    /// Right-aligned shortcut hint (display only).
    pub hint: &'static str,
}

const fn e(label: &'static str, cmd: MenuCmd) -> Entry {
    Entry { label, cmd, hint: "" }
}
const fn k(label: &'static str, cmd: MenuCmd, hint: &'static str) -> Entry {
    Entry { label, cmd, hint }
}
const SEP: Entry = Entry { label: "", cmd: MenuCmd::Separator, hint: "" };
const fn stub(label: &'static str) -> Entry {
    Entry { label, cmd: MenuCmd::Stub(label), hint: "" }
}
const fn stubk(label: &'static str, hint: &'static str) -> Entry {
    Entry { label, cmd: MenuCmd::Stub(label), hint }
}

use Command::*;
use MenuCmd::*;

const FILE: &[Entry] = &[
    k("New File", Cmd(NewFile), "Ctrl+N"),
    k("New Window", NewWindow, ""),
    SEP,
    e("Open File…", OpenFileDlg),
    k("Open Folder…", Cmd(OpenFolder), "Ctrl+O"),
    e("Open Recent", OpenRecent),
    SEP,
    stub("Add Folder to Workspace…"),
    stub("Save Workspace As…"),
    SEP,
    k("Save", Cmd(Save), "Ctrl+S"),
    e("Save As…", SaveAs),
    e("Save All", SaveAll),
    SEP,
    e("Auto Save", AutoSave),
    e("Settings", Cmd(OpenSettings)),
    e("Color Theme", Cmd(ColorTheme)),
    e("Keyboard Shortcuts", ShortcutsRef),
    SEP,
    e("Revert File", RevertFile),
    k("Close Editor", Cmd(Close), "Ctrl+W"),
    e("Close Folder", CloseFolder),
    SEP,
    e("Exit", Exit),
];

const EDIT: &[Entry] = &[
    k("Undo", Cmd(Undo), "Ctrl+Z"),
    k("Redo", Cmd(Redo), "Ctrl+Y"),
    SEP,
    k("Cut", Cut, "Ctrl+X"),
    k("Copy", Copy, "Ctrl+C"),
    k("Paste", Paste, "Ctrl+V"),
    SEP,
    k("Find", Cmd(Find), "Ctrl+F"),
    e("Replace", Replace),
    SEP,
    e("Find in Files", FindInFiles),
    e("Replace in Files", ReplaceInFiles),
    SEP,
    k("Toggle Line Comment", Cmd(ToggleLineComment), "Ctrl+/"),
    k("Toggle Block Comment", Cmd(ToggleBlockComment), "Shift+Alt+A"),
];

const SELECTION: &[Entry] = &[
    k("Select All", Cmd(SelectAll), "Ctrl+A"),
    k("Expand Selection", Cmd(ExpandSelection), "Shift+Alt+Right"),
    k("Shrink Selection", Cmd(ShrinkSelection), "Shift+Alt+Left"),
    SEP,
    k("Copy Line Up", Cmd(CopyLineUp), "Shift+Alt+Up"),
    k("Copy Line Down", Cmd(CopyLineDown), "Shift+Alt+Down"),
    k("Move Line Up", Cmd(MoveLineUp), "Alt+Up"),
    k("Move Line Down", Cmd(MoveLineDown), "Alt+Down"),
    e("Duplicate Selection", Cmd(DuplicateSelection)),
    SEP,
    stub("Add Cursor Above"),
    stub("Add Cursor Below"),
    stub("Add Cursors to Line Ends"),
    stub("Add Next Occurrence"),
    stub("Select All Occurrences"),
];

const VIEW: &[Entry] = &[
    k("Command Palette…", Palette, "Ctrl+Shift+P"),
    SEP,
    e("Full Screen", FullScreen),
    e("Zen Mode", ZenMode),
    e("Centered Layout", CenteredLayout),
    SEP,
    e("Explorer", ShowExplorer),
    e("Search", ShowSearch),
    e("Source Control", ShowScm),
    stub("Run and Debug"),
    e("Extensions", ShowExtensions),
    SEP,
    k("Problems", Problems, "Ctrl+Shift+M"),
    e("Output", OutputLog),
    stub("Debug Console"),
    k("Terminal", Cmd(ToggleTerminal), "Ctrl+`"),
    SEP,
    k("Toggle Sidebar", Cmd(ToggleSidebar), "Ctrl+B"),
    stub("Word Wrap"),
    stub("Minimap"),
    SEP,
    k("Zoom In", ZoomIn, "Ctrl+="),
    k("Zoom Out", ZoomOut, "Ctrl+-"),
    k("Reset Zoom", ZoomReset, "Ctrl+0"),
];

const GO: &[Entry] = &[
    k("Back", Cmd(NavBack), "Alt+Left"),
    k("Forward", Cmd(NavForward), "Alt+Right"),
    e("Last Edit Location", Cmd(LastEditLocation)),
    SEP,
    k("Next Editor", Cmd(NextEditor), "Ctrl+PgDn"),
    k("Previous Editor", Cmd(PrevEditor), "Ctrl+PgUp"),
    stub("Switch Group"),
    SEP,
    k("Go to File…", QuickOpen, "Ctrl+P"),
    k("Go to Symbol in Workspace…", GotoWsSymbol, "Ctrl+T"),
    e("Go to Symbol in Editor…", GotoSymbol),
    k("Go to Definition", Cmd(GotoDefinition), "F12"),
    e("Go to Declaration", Cmd(GotoDeclaration)),
    e("Go to Type Definition", Cmd(GotoTypeDefinition)),
    e("Go to Implementations", Cmd(GotoImplementations)),
    k("Go to References", Cmd(GotoReferences), "Shift+F12"),
    SEP,
    e("Go to Line/Column…", GotoLine),
    k("Go to Bracket", Cmd(GotoBracket), "Ctrl+Shift+\\"),
    SEP,
    k("Next Problem", Cmd(NextProblem), "F8"),
    k("Previous Problem", Cmd(PrevProblem), "Shift+F8"),
];

const RUN: &[Entry] = &[
    stubk("Start Debugging", "F5"),
    stub("Run Without Debugging"),
    stub("Stop Debugging"),
    stub("Restart Debugging"),
    SEP,
    stub("Open Configurations"),
    stub("Add Configuration…"),
    SEP,
    stubk("Step Over", "F10"),
    stubk("Step Into", "F11"),
    stub("Step Out"),
    stub("Continue"),
    SEP,
    stubk("Toggle Breakpoint", "F9"),
    stub("New Breakpoint"),
    SEP,
    stub("Enable All Breakpoints"),
    stub("Disable All Breakpoints"),
    stub("Remove All Breakpoints"),
];

const TERMINAL: &[Entry] = &[
    e("New Terminal", NewTerminal),
    e("Split Terminal", SplitTerminal),
    e("Kill Terminal", KillTerminal),
    SEP,
    stub("Run Task…"),
    stub("Run Build Task…"),
    e("Run Active File", RunActiveFile),
    e("Run Selected Text", RunSelectedText),
    SEP,
    stub("Show Running Tasks"),
    stub("Restart Running Task"),
    stub("Terminate Task"),
    SEP,
    stub("Configure Tasks…"),
];

const HELP: &[Entry] = &[
    e("Welcome", Welcome),
    k("Show All Commands", Palette, "Ctrl+Shift+P"),
    e("Documentation", OpenDocs),
    e("Release Notes", OpenReleases),
    SEP,
    e("Keyboard Shortcuts Reference", ShortcutsRef),
    e("Tips and Tricks", Tips),
    SEP,
    e("Report Issue / Feedback…", Feedback),
    SEP,
    e("Check for Updates…", CheckUpdate),
    SEP,
    e("About Aether", About),
];

/// Entries for the top-level menu at `idx` (matches `widgets::MENU_ITEMS` order).
pub fn entries(idx: usize) -> &'static [Entry] {
    match idx {
        0 => FILE,
        1 => EDIT,
        2 => SELECTION,
        3 => VIEW,
        4 => GO,
        5 => RUN,
        6 => TERMINAL,
        7 => HELP,
        _ => &[],
    }
}
