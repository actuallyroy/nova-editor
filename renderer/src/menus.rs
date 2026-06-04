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
    stub("Open Recent"),
    SEP,
    stub("Add Folder to Workspace…"),
    stub("Save Workspace As…"),
    SEP,
    k("Save", Cmd(Save), "Ctrl+S"),
    e("Save As…", SaveAs),
    e("Save All", SaveAll),
    SEP,
    stub("Auto Save"),
    e("Settings", Cmd(OpenSettings)),
    e("Color Theme", Cmd(ColorTheme)),
    stub("Keyboard Shortcuts"),
    SEP,
    stub("Revert File"),
    k("Close Editor", Cmd(Close), "Ctrl+W"),
    stub("Close Folder"),
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
    stub("Replace in Files"),
    SEP,
    stubk("Toggle Line Comment", "Ctrl+/"),
    stub("Toggle Block Comment"),
];

const SELECTION: &[Entry] = &[
    k("Select All", Cmd(SelectAll), "Ctrl+A"),
    stub("Expand Selection"),
    stub("Shrink Selection"),
    SEP,
    stub("Copy Line Up"),
    stub("Copy Line Down"),
    stub("Move Line Up"),
    stub("Move Line Down"),
    stub("Duplicate Selection"),
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
    stub("Zen Mode"),
    stub("Centered Layout"),
    SEP,
    e("Explorer", ShowExplorer),
    e("Search", ShowSearch),
    e("Source Control", ShowScm),
    stub("Run and Debug"),
    e("Extensions", ShowExtensions),
    SEP,
    stub("Problems"),
    stub("Output"),
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
    stub("Back"),
    stub("Forward"),
    stub("Last Edit Location"),
    SEP,
    stub("Switch Editor"),
    stub("Switch Group"),
    SEP,
    k("Go to File…", QuickOpen, "Ctrl+P"),
    stub("Go to Symbol in Workspace…"),
    e("Go to Symbol in Editor…", GotoSymbol),
    stubk("Go to Definition", "F12"),
    stub("Go to Declaration"),
    stub("Go to Type Definition"),
    stub("Go to Implementations"),
    stub("Go to References"),
    SEP,
    e("Go to Line/Column…", GotoLine),
    stub("Go to Bracket"),
    SEP,
    stubk("Next Problem", "F8"),
    stub("Previous Problem"),
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
    stub("Run Active File"),
    stub("Run Selected Text"),
    SEP,
    stub("Show Running Tasks"),
    stub("Restart Running Task"),
    stub("Terminate Task"),
    SEP,
    stub("Configure Tasks…"),
];

const HELP: &[Entry] = &[
    stub("Welcome"),
    k("Show All Commands", Palette, "Ctrl+Shift+P"),
    e("Documentation", OpenDocs),
    e("Release Notes", OpenReleases),
    SEP,
    stub("Keyboard Shortcuts Reference"),
    stub("Tips and Tricks"),
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
