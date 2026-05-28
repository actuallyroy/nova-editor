// VSCode dark+ palette and layout constants.

use glyphon::Color;

pub const BG_EDITOR: wgpu::Color = wgpu::Color {
    r: 0.076,
    g: 0.078,
    b: 0.078,
    a: 1.0,
}; // ~#131314 (VSCode Dark Modern editor)

pub const FG_TEXT: Color = Color::rgb(0xD4, 0xD4, 0xD4);
// Markdown token colours (VSCode Dark+ -ish).
pub const MD_HEADING: Color = Color::rgb(0x56, 0x9C, 0xD6);
pub const MD_QUOTE: Color = Color::rgb(0x6A, 0x99, 0x55);
pub const MD_CODE: Color = Color::rgb(0xCE, 0x91, 0x78);
pub const MD_RULE: Color = Color::rgb(0x80, 0x80, 0x80);
pub const MD_LIST: Color = Color::rgb(0x64, 0x9B, 0xD6);
pub const FG_DIM: Color = Color::rgb(0x85, 0x85, 0x85);
pub const FG_ACTIVE: Color = Color::rgb(0xFF, 0xFF, 0xFF);
pub const FG_GUTTER: Color = Color::rgb(0x6E, 0x73, 0x81);
pub const FG_GUTTER_ACTIVE: Color = Color::rgb(0xC6, 0xC6, 0xC6);

pub const CURSOR: [f32; 4] = [0.82, 0.82, 0.82, 1.0];
pub const SELECTION: [f32; 4] = [0.16, 0.32, 0.55, 0.45];
pub const LINE_HIGHLIGHT: [f32; 4] = [1.0, 1.0, 1.0, 0.04];
pub const FIND_MATCH: [f32; 4] = [0.6, 0.5, 0.0, 0.45];

pub const ACTIVITY_BAR_BG: [f32; 4] = [0.129, 0.137, 0.141, 1.0]; // #212324
pub const ACTIVITY_BAR_ACTIVE: [f32; 4] = [1.0, 1.0, 1.0, 0.08];
pub const SIDEBAR_BG: [f32; 4] = [0.102, 0.110, 0.114, 1.0]; // #1A1C1D
pub const TAB_BAR_BG: [f32; 4] = [0.098, 0.102, 0.106, 1.0]; // #191A1B
pub const TAB_INACTIVE: [f32; 4] = [0.098, 0.102, 0.106, 1.0]; // #191A1B
pub const TAB_ACTIVE: [f32; 4] = [0.076, 0.078, 0.078, 1.0]; // = editor bg
pub const TAB_HOVER: [f32; 4] = [0.21, 0.21, 0.21, 1.0];
pub const TAB_FG_ACTIVE: Color = Color::rgb(0xFF, 0xFF, 0xFF);
pub const TAB_FG_INACTIVE: Color = Color::rgb(0xB0, 0xB0, 0xB0);
pub const CLOSE_FG: Color = Color::rgb(0x9A, 0x9A, 0x9A);
pub const CLOSE_FG_HOVER: Color = Color::rgb(0xFF, 0xFF, 0xFF);
pub const STATUS_BAR_BG: [f32; 4] = [0.129, 0.129, 0.125, 1.0]; // #212120 (Dark Modern)
pub const STATUS_BAR_FG: Color = Color::rgb(0xFF, 0xFF, 0xFF);
pub const BORDER: [f32; 4] = [0.0, 0.0, 0.0, 1.0];
pub const TREE_HOVER: [f32; 4] = [1.0, 1.0, 1.0, 0.04];
pub const TREE_SELECTED: [f32; 4] = [0.15, 0.30, 0.45, 0.6];

pub const PALETTE_BG: [f32; 4] = [0.176, 0.176, 0.176, 1.0];
pub const PALETTE_BORDER: [f32; 4] = [0.30, 0.30, 0.30, 1.0];
pub const PALETTE_INPUT_BG: [f32; 4] = [0.20, 0.20, 0.20, 1.0];
pub const PALETTE_SELECTED: [f32; 4] = [0.07, 0.36, 0.61, 0.85];

pub const FONT_SIZE: f32 = 14.0;
pub const LINE_HEIGHT: f32 = 20.0;
pub const UI_FONT_SIZE: f32 = 13.0;
pub const UI_LINE_HEIGHT: f32 = 22.0;
pub const ICON_SIZE: f32 = 16.0;
pub const ACTIVITY_ICON_SIZE: f32 = 22.0;
pub const ACTIVITY_CELL: f32 = 48.0;
pub const ACTIVITY_ICON_FG: Color = Color::rgb(0xC8, 0xC8, 0xC8); // visible inactive icon
pub const ACTIVITY_ICON_ACTIVE: Color = Color::rgb(0xFF, 0xFF, 0xFF);

pub const MONO_FAMILY: &str = "Consolas";
pub const UI_FAMILY: &str = "Segoe UI";
// VSCode's own icon font (Codicon, MIT). Bundled at renderer/assets/codicon.ttf
// and loaded into the FontSystem at startup; its internal family name is "codicon".
pub const ICON_FAMILY: &str = "codicon";

// Activity-bar / chrome glyphs — exact Codicon codepoints (match VSCode 1:1).
pub const ICON_FILES: char = '\u{eaf0}'; // files (Explorer)
pub const ICON_SEARCH: char = '\u{ea6d}'; // search
pub const ICON_SOURCE_CONTROL: char = '\u{ea68}'; // source-control
pub const ICON_RUN: char = '\u{eb91}'; // debug-alt (Run & Debug)
pub const ICON_EXTENSIONS: char = '\u{eae6}'; // extensions
pub const ICON_CLOSE: char = '\u{ea76}'; // close (tab ×)
pub const ICON_ACCOUNT: char = '\u{eb99}'; // account
pub const ICON_SETTINGS: char = '\u{eb51}'; // settings-gear
pub const ICON_CHEVRON_DOWN: char = '\u{eab4}';
pub const ICON_CHEVRON_RIGHT: char = '\u{eab6}';
// Explorer header actions.
pub const ICON_NEW_FILE: char = '\u{ea7f}'; // new-file
pub const ICON_NEW_FOLDER: char = '\u{ea80}'; // new-folder
pub const ICON_REFRESH: char = '\u{eb37}'; // refresh
pub const ICON_COLLAPSE_ALL: char = '\u{eac5}'; // collapse-all
// Title-bar layout toggles (right group, like VSCode).
pub const ICON_LAYOUT_SIDEBAR_LEFT: char = '\u{ebf3}'; // layout-sidebar-left
pub const ICON_LAYOUT_PANEL: char = '\u{ebf2}'; // layout-panel
pub const ICON_LAYOUT_SIDEBAR_RIGHT: char = '\u{ebf4}'; // layout-sidebar-right

// File-type icon colours (Seti-ish), keyed by extension.
pub fn file_icon_color(name: &str) -> Color {
    let ext = name.rsplit('.').next().unwrap_or("");
    match ext {
        "rs" => Color::rgb(0xDE, 0xA5, 0x84),
        "md" => Color::rgb(0x51, 0x9A, 0xBA),
        "toml" | "lock" => Color::rgb(0x9C, 0x9C, 0x9C),
        "json" | "mcp" => Color::rgb(0xCB, 0xCB, 0x41),
        "wgsl" => Color::rgb(0x8F, 0xBC, 0x8F),
        "txt" => Color::rgb(0xB0, 0xB4, 0xBA),
        "png" | "jpg" | "ico" => Color::rgb(0xA0, 0x74, 0xC4),
        _ => Color::rgb(0xC5, 0xC5, 0xC5),
    }
}

// File-tree glyphs — Codicon codepoints.
pub const ICON_FOLDER_CLOSED: char = '\u{ea83}'; // folder
pub const ICON_FOLDER_OPEN: char = '\u{eaf7}'; // folder-opened
pub const ICON_FILE: char = '\u{ea7b}'; // file
pub const ICON_FOLDER_COLOR: Color = Color::rgb(0x8A, 0xB4, 0xE8);
pub const ICON_FILE_COLOR: Color = Color::rgb(0xC5, 0xC5, 0xC5);

pub const BLINK_MS: u64 = 530;

pub const TITLE_BAR_H: f32 = 30.0;
pub const TITLE_BAR_BG: [f32; 4] = [0.145, 0.149, 0.152, 1.0]; // #252627
pub const TITLE_FG: Color = Color::rgb(0xCC, 0xCC, 0xCC);
// Header command-center search field (VSCode-style centered box).
pub const SEARCH_BG: [f32; 4] = [0.18, 0.18, 0.19, 1.0];
pub const SEARCH_BG_HOVER: [f32; 4] = [0.22, 0.22, 0.23, 1.0];
pub const SEARCH_BORDER: [f32; 4] = [0.27, 0.27, 0.28, 1.0];
pub const TITLE_CLOSE_HOVER: [f32; 4] = [0.78, 0.16, 0.16, 1.0];
pub const TITLE_BTN_HOVER: [f32; 4] = [1.0, 1.0, 1.0, 0.08];
pub const MENU_HOVER: [f32; 4] = [1.0, 1.0, 1.0, 0.08];
// Window controls — Codicon chrome-* glyphs (rendered in ICON_FAMILY).
pub const ICON_MIN: char = '\u{eaba}'; // chrome-minimize
pub const ICON_MAX: char = '\u{eab9}'; // chrome-maximize
pub const ICON_RESTORE: char = '\u{eabb}'; // chrome-restore
pub const ICON_WIN_CLOSE: char = '\u{eab8}'; // chrome-close
pub const TITLE_BTN_W: f32 = 46.0;

pub const ACTIVITY_BAR_WIDTH: f32 = 48.0;
pub const SIDEBAR_WIDTH: f32 = 240.0;
pub const SIDEBAR_MIN_WIDTH: f32 = 120.0;
pub const SIDEBAR_MAX_WIDTH: f32 = 600.0;
pub const SIDEBAR_RESIZE_HANDLE: f32 = 6.0;
pub const TAB_HEIGHT: f32 = 34.0;
pub const STATUS_BAR_HEIGHT: f32 = 22.0;
pub const GUTTER_WIDTH: f32 = 56.0;
pub const EDITOR_PAD: f32 = 12.0;
pub const CURSOR_WIDTH: f32 = 2.0;
pub const TREE_INDENT: f32 = 16.0;
pub const TREE_ROW_HEIGHT: f32 = 22.0;
pub const SIDEBAR_HEADER_H: f32 = 30.0;
pub const TAB_MIN_WIDTH: f32 = 120.0;
pub const TAB_MAX_WIDTH: f32 = 220.0;
pub const FIND_BAR_HEIGHT: f32 = 36.0;
pub const PALETTE_WIDTH: f32 = 560.0;
pub const PALETTE_ROW_HEIGHT: f32 = 24.0;
pub const PALETTE_INPUT_HEIGHT: f32 = 30.0;
