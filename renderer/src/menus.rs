// Top menu-bar dropdown contents. Each top-level title (File / Edit / … indexed
// the same as `widgets::MENU_ITEMS`) maps to a flat list of actionable entries.
// Most reuse the command-palette `Command`s; a couple are menu-only (`Palette`,
// `Exit`). `App` opens a dropdown for the clicked title and runs the chosen entry.

use crate::commands::Command;

#[derive(Clone, Copy)]
pub enum MenuCmd {
    Cmd(Command),
    Palette,  // open the command palette
    Feedback, // open the feedback / report-issue tab
    Exit,     // quit the app
}

pub struct Entry {
    pub label: &'static str,
    pub cmd: MenuCmd,
}

const fn e(label: &'static str, cmd: MenuCmd) -> Entry {
    Entry { label, cmd }
}

use Command::*;
use MenuCmd::{Cmd, Exit, Feedback, Palette};

const FILE: &[Entry] = &[
    e("New File", Cmd(NewFile)),
    e("Open Folder…", Cmd(OpenFolder)),
    e("Save", Cmd(Save)),
    e("Close Editor", Cmd(Close)),
    e("Settings", Cmd(OpenSettings)),
    e("Exit", Exit),
];

const EDIT: &[Entry] = &[
    e("Undo", Cmd(Undo)),
    e("Redo", Cmd(Redo)),
    e("Find", Cmd(Find)),
    e("Select All", Cmd(SelectAll)),
];

const SELECTION: &[Entry] = &[e("Select All", Cmd(SelectAll))];

const VIEW: &[Entry] = &[
    e("Command Palette…", Palette),
    e("Toggle Sidebar", Cmd(ToggleSidebar)),
    e("Toggle Terminal", Cmd(ToggleTerminal)),
];

const GO: &[Entry] = &[e("Go to File…", Palette)];

const RUN: &[Entry] = &[e("Toggle Terminal", Cmd(ToggleTerminal))];

const TERMINAL: &[Entry] = &[e("New Terminal", Cmd(ToggleTerminal))];

const HELP: &[Entry] = &[
    e("Send Feedback / Report Issue…", Feedback),
    e("Show All Commands", Palette),
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
