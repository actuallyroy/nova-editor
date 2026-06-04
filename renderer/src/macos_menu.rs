//! Native macOS menu bar (the system menu at the top of the screen), built from
//! the same `menus` structure as the custom in-window menu used on other platforms.
//! Clicking an item posts a `muda::MenuEvent`; we map its id back to a `MenuCmd`.

use std::collections::HashMap;

use muda::{Menu, MenuItem, PredefinedMenuItem, Submenu};

use crate::menus::{self, MenuCmd};

const TITLES: [&str; 8] = ["File", "Edit", "Selection", "View", "Go", "Run", "Terminal", "Help"];

/// Build + install the macOS menu bar. The returned `Menu` must be kept alive for
/// the menu to stay in the bar; the map resolves a clicked item id → `MenuCmd`.
pub fn install() -> (Menu, HashMap<String, MenuCmd>) {
    let menu = Menu::new();
    let mut map: HashMap<String, MenuCmd> = HashMap::new();

    // The first submenu becomes the application menu on macOS (About / Quit / …).
    let app_menu = Submenu::new("Aether", true);
    let _ = app_menu.append_items(&[
        &PredefinedMenuItem::about(Some("About Aether"), None),
        &PredefinedMenuItem::separator(),
        &PredefinedMenuItem::hide(None),
        &PredefinedMenuItem::hide_others(None),
        &PredefinedMenuItem::show_all(None),
        &PredefinedMenuItem::separator(),
        &PredefinedMenuItem::quit(None),
    ]);
    let _ = menu.append(&app_menu);

    for (i, title) in TITLES.iter().enumerate() {
        let entries = menus::entries(i);
        if entries.is_empty() {
            continue;
        }
        let sub = Submenu::new(*title, true);
        for (j, entry) in entries.iter().enumerate() {
            // "Exit" lives in the app menu as Quit on macOS — skip the duplicate.
            if matches!(entry.cmd, MenuCmd::Exit) {
                continue;
            }
            if matches!(entry.cmd, MenuCmd::Separator) {
                let _ = sub.append(&PredefinedMenuItem::separator());
                continue;
            }
            let id = format!("m{i}_{j}");
            let item = MenuItem::with_id(id.clone(), entry.label, true, None);
            let _ = sub.append(&item);
            map.insert(id, entry.cmd);
        }
        let _ = menu.append(&sub);
    }
    menu.init_for_nsapp();
    (menu, map)
}

/// Drain queued menu clicks into their `MenuCmd`s (called each event-loop tick).
pub fn poll(map: &HashMap<String, MenuCmd>) -> Vec<MenuCmd> {
    let mut out = Vec::new();
    while let Ok(ev) = muda::MenuEvent::receiver().try_recv() {
        if let Some(&cmd) = map.get(ev.id.0.as_str()) {
            out.push(cmd);
        }
    }
    out
}
