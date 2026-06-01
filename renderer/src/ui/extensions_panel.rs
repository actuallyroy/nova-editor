// Extensions sidebar view — a self-contained panel: owns the filter box, the
// scrollable extension-row list, the list scrollbar, and all of its state, plus
// its own draw + input. Marketplace search results (`ext_remote`) and the local
// `extensions` list stay on `App` (the detail view reads them too); the panel
// borrows them when it rebuilds its rows. Opening a detail page is returned as an
// `Intent`.

use std::sync::mpsc::Sender;
use std::time::Instant;

use glyphon::{FontSystem, TextArea};
use winit::window::CursorIcon;
use arboard::Clipboard;

use crate::extensions::{Extension, OpenExt};
use crate::gpu::GpuState;
use crate::icon::IconInstance;
use crate::marketplace::{self, RemoteExt, WorkerMsg};
use crate::quad::Quad;
use crate::theme;
use crate::ui::Intent;
use crate::widgets::{ExtSpec, ExtensionList, Rect, ScrollOpts, ScrollView, TextInput, TextLabel};
use crate::{ext_filter_rect, ext_list_region};

pub struct ExtensionsPanel {
    filter: TextInput,
    rows: ExtensionList,
    scroll: ScrollView,
    visible: Vec<usize>, // displayed row index -> index into the active source
    showing_remote: bool, // true while a (non-empty) marketplace query is active
    filter_active: bool,
    hovered: Option<usize>,
    dragging: bool, // filter-box text drag-select
    search_gen: u64, // discards stale background search results
    searching: bool, // a marketplace query is in flight (drives the "Searching…" line)
    l_status: TextLabel, // "Searching…" / "No extensions found"
}

impl ExtensionsPanel {
    pub fn new(fs: &mut FontSystem) -> Self {
        let mut filter = TextInput::new(fs, theme::SIDEBAR_WIDTH(), 30.0);
        filter.set_placeholder(fs, " Search Extensions in Marketplace");
        Self {
            filter,
            rows: ExtensionList::new(),
            scroll: ScrollView::new(ScrollOpts::vertical()),
            visible: Vec::new(),
            showing_remote: false,
            filter_active: false,
            hovered: None,
            dragging: false,
            search_gen: 0,
            searching: false,
            l_status: TextLabel::new(fs, theme::SIDEBAR_WIDTH(), theme::UI_LINE_HEIGHT()),
        }
    }

    /// Called when a marketplace search response (matching the current gen) arrives.
    pub fn finish_search(&mut self) {
        self.searching = false;
    }

    pub fn focused(&self) -> bool {
        self.filter_active
    }
    pub fn set_focus(&mut self, on: bool) {
        self.filter_active = on;
        self.filter.focus(on);
    }
    pub fn search_gen(&self) -> u64 {
        self.search_gen
    }

    /// Rebuild the row widgets from current extension data. Called when the data
    /// changes (scan / install / query change), not per frame.
    pub fn rebuild(&mut self, gpu: &mut GpuState, extensions: &[Extension], ext_remote: &[RemoteExt]) {
        let query = self.filter.text().trim().to_lowercase();
        self.showing_remote = !query.is_empty();
        let mut visible = Vec::new();
        let mut specs: Vec<ExtSpec> = Vec::new();
        if self.showing_remote {
            // Marketplace results (already filtered by the remote query).
            for (idx, e) in ext_remote.iter().enumerate() {
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
            for (idx, e) in extensions.iter().enumerate() {
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
        self.rows.rebuild(&mut gpu.font_system, &specs);
        self.visible = visible;
    }

    /// Kick off a background marketplace search for the current filter text.
    pub fn trigger_search(&mut self, tx: &Sender<WorkerMsg>) {
        let query = self.filter.text().trim().to_string();
        if query.is_empty() {
            return;
        }
        self.search_gen += 1;
        self.searching = true;
        marketplace::search_async(tx.clone(), query, self.search_gen);
    }

    /// Re-shape the filter input + every row after a zoom change.
    pub fn rezoom(&mut self, fs: &mut FontSystem) {
        self.filter.rezoom(fs);
        self.rows.rezoom(fs);
    }

    // ---- Shaping: keep the scroll metrics in sync with the row content height. ----
    pub fn update(&mut self, fs: &mut FontSystem, region: Rect) {
        let list = ext_list_region(region);
        self.scroll.set_metrics(list, (list.w, self.rows.content_height()));
        // Keep the status label's text current (shaped here; pushed in draw_text).
        let status = if self.searching {
            "Searching…"
        } else if self.showing_remote && self.rows.len() == 0 {
            "No extensions found"
        } else {
            ""
        };
        self.l_status.set(fs, status, theme::UI_FAMILY());
    }

    // ---- Main-pass drawing (filter chrome + selection + caret). The scrollable
    // rows render in their own clipped pass via `list_*`. ----
    pub fn draw_quads(&self, region: Rect, blink: bool, bg: &mut Vec<Quad>, fg: &mut Vec<Quad>) {
        let fr = ext_filter_rect(region);
        let border = Rect { x: fr.x - 1.0, y: fr.y - 1.0, w: fr.w + 2.0, h: fr.h + 2.0 };
        bg.push(border.rounded_quad(theme::SEARCH_BORDER(), 3.0));
        bg.push(fr.rounded_quad(theme::SEARCH_BG(), 2.0));
        if self.filter_active {
            self.filter.selection_quads(fr, theme::zpx(6.0), bg);
            if blink {
                fg.push(self.filter.caret_quad(fr, theme::zpx(6.0)));
            }
        }
    }

    pub fn draw_text<'b>(&'b self, region: Rect, areas: &mut Vec<TextArea<'b>>) {
        let fr = ext_filter_rect(region);
        let fc = if self.filter.text().is_empty() { theme::FG_DIM() } else { theme::FG_TEXT() };
        self.filter.draw(fr, theme::zpx(6.0), fc, areas);
        // Status line under the filter (text set in `update`): "Searching…" / empty hint.
        if self.show_status() {
            let list = ext_list_region(region);
            let row = Rect { x: list.x + theme::zpx(12.0), y: list.y + theme::zpx(8.0), w: list.w, h: theme::UI_LINE_HEIGHT() };
            self.l_status.draw_left(row, 0.0, theme::FG_DIM(), areas);
        }
    }

    fn show_status(&self) -> bool {
        self.searching || (self.showing_remote && self.rows.len() == 0)
    }

    // ---- Clipped list pass data (drawn by the renderer into a scissored pass). ----
    pub fn list_pass_data(
        &self,
        region: Rect,
        now: Instant,
        quads: &mut Vec<Quad>,
        fg: &mut Vec<Quad>,
        icons: &mut Vec<IconInstance>,
    ) {
        let list = ext_list_region(region);
        let scroll = self.scroll.offset().1;
        self.rows.draw_quads(list, scroll, self.hovered, quads);
        self.rows.icon_instances(list, scroll, icons);
        self.scroll.draw(now, fg); // scrollbar thumb on top (clipped by the pass)
    }
    pub fn list_areas<'b>(&'b self, region: Rect, areas: &mut Vec<TextArea<'b>>) {
        let list = ext_list_region(region);
        self.rows.draw_text(list, self.scroll.offset().1, areas);
    }

    // ---- Hover / cursor ----
    pub fn hover(&mut self, p: (f32, f32), region: Rect) -> bool {
        let list = ext_list_region(region);
        let inside = list.contains(p);
        let new_hover = if inside { self.rows.hit(list, self.scroll.offset().1, p) } else { None };
        let mut changed = false;
        if new_hover != self.hovered {
            self.hovered = new_hover;
            changed = true;
        }
        if self.scroll.hover(inside) {
            changed = true;
        }
        changed
    }

    /// Cursor over the panel: text in the filter box, pointer over a row, arrow over
    /// the scrollbar / empty list. `None` when the point isn't over the panel.
    pub fn cursor(&self, p: (f32, f32), region: Rect) -> Option<CursorIcon> {
        if ext_filter_rect(region).contains(p) {
            return Some(CursorIcon::Text);
        }
        let list = ext_list_region(region);
        if list.contains(p) {
            if self.scroll.cursor(p).is_some() {
                return Some(CursorIcon::Default);
            }
            if self.rows.hit(list, self.scroll.offset().1, p).is_some() {
                return Some(CursorIcon::Pointer);
            }
            return Some(CursorIcon::Default);
        }
        None
    }

    pub fn next_wake(&self, now: Instant) -> Option<Instant> {
        self.scroll.next_wake(now)
    }

    // ---- Input ----
    pub fn on_wheel(&mut self, p: (f32, f32), region: Rect, dy: f32) -> bool {
        if ext_list_region(region).contains(p) {
            self.scroll.on_wheel(0.0, dy);
            return true;
        }
        false
    }

    /// Mouse press inside the sidebar while the Extensions view is active.
    pub fn on_press(&mut self, pt: (f32, f32), region: Rect, double: bool, out: &mut Vec<Intent>) -> bool {
        let fr = ext_filter_rect(region);
        if fr.contains(pt) {
            self.set_focus(true);
            if double {
                self.filter.select_word_at(fr, 6.0, pt.0);
            } else {
                self.filter.set_caret_from_x(fr, 6.0, pt.0);
            }
            self.dragging = true;
            return true;
        }
        if self.scroll.press(pt) {
            return true;
        }
        let list = ext_list_region(region);
        if list.contains(pt) {
            if let Some(i) = self.rows.hit(list, self.scroll.offset().1, pt) {
                if let Some(&src) = self.visible.get(i) {
                    let which = if self.showing_remote { OpenExt::Remote(src) } else { OpenExt::Local(src) };
                    out.push(Intent::OpenExtDetail(which));
                }
            }
            return true;
        }
        // Any other click in the sidebar defocuses the filter box.
        self.set_focus(false);
        true
    }

    pub fn on_drag(&mut self, pt: (f32, f32), region: Rect) -> bool {
        if self.dragging {
            self.filter.extend_to_x(ext_filter_rect(region), 6.0, pt.0);
            return true;
        }
        if self.scroll.is_dragging() {
            self.scroll.drag(pt);
            return true;
        }
        false
    }

    pub fn on_release(&mut self) {
        self.dragging = false;
        self.scroll.release();
    }

    /// Keyboard while the filter box is focused. Returns true if consumed.
    #[allow(clippy::too_many_arguments)]
    pub fn on_key(
        &mut self,
        event: &winit::event::KeyEvent,
        ctrl: bool,
        shift: bool,
        gpu: &mut GpuState,
        extensions: &[Extension],
        ext_remote: &[RemoteExt],
        tx: &Sender<WorkerMsg>,
        clip: Option<&mut Clipboard>,
    ) -> bool {
        use winit::keyboard::{Key, NamedKey};
        if !self.filter_active {
            return false;
        }
        if matches!(event.logical_key.as_ref(), Key::Named(NamedKey::Escape)) {
            self.filter.clear(&mut gpu.font_system);
            self.scroll.scroll_to_y(0.0);
            self.rebuild(gpu, extensions, ext_remote);
            return true;
        }
        match crate::edit_input(&mut self.filter, &mut gpu.font_system, clip, event, ctrl, shift) {
            Some(changed) => {
                if changed {
                    self.scroll.scroll_to_y(0.0);
                    self.trigger_search(tx);
                    self.rebuild(gpu, extensions, ext_remote);
                }
                true
            }
            None => !ctrl,
        }
    }
}
