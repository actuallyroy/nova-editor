// The VSCode-style extension detail page: a header (icon, name, meta, Install),
// a Details / Features / Changelog tab bar, the active tab's content on the left,
// and a metadata sidebar on the right. Owns its sub-widgets, layout rects, tab
// selection, and hit-tests — the renderer and input handling just hand it a
// region and forward clicks. Rendered into whatever region it's given (editor
// overlay or a dedicated tab), so it's agnostic to how it's hosted.

use glyphon::{Attrs, Color, Family, FontSystem, Metrics, TextArea};

use crate::icon::IconInstance;
use crate::markdown::Markdown;
use crate::quad::Quad;
use crate::theme;
use crate::widgets::{Rect, TextLabel, VAlign};

const GOLD: Color = Color::rgb(0xD7, 0xB5, 0x06); // rating stars

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum DetailTab {
    Details,
    Features,
    Changelog,
}

impl DetailTab {
    pub const ALL: [DetailTab; 3] = [DetailTab::Details, DetailTab::Features, DetailTab::Changelog];
    fn label(self) -> &'static str {
        match self {
            DetailTab::Details => "DETAILS",
            DetailTab::Features => "FEATURES",
            DetailTab::Changelog => "CHANGELOG",
        }
    }
    fn index(self) -> usize {
        match self {
            DetailTab::Details => 0,
            DetailTab::Features => 1,
            DetailTab::Changelog => 2,
        }
    }
}

pub struct ExtensionDetail {
    name: TextLabel,
    meta: TextLabel,
    desc: TextLabel,
    install: TextLabel,
    uninstall: TextLabel,
    set_theme: TextLabel,
    tabs: [TextLabel; 3],
    details: Markdown,
    features: Markdown,
    changelog: Markdown,
    sidebar: TextLabel,
    tab: DetailTab,
    icon_uv: Option<[f32; 4]>,
    icon_color: [f32; 4],
    supported: bool,
    installed: bool,
    is_theme: bool,
}

impl ExtensionDetail {
    // All layout dimensions scale with the UI zoom (so the header/tabs/body don't
    // overlap at high zoom while the text scales).
    fn pad() -> f32 { 24.0 * theme::ui_zoom() }
    fn icon() -> f32 { 72.0 * theme::ui_zoom() }
    fn header_top() -> f32 { 24.0 * theme::ui_zoom() }
    fn tabbar_h() -> f32 { 34.0 * theme::ui_zoom() }
    fn sidebar_w() -> f32 { 270.0 * theme::ui_zoom() }
    fn content_gap() -> f32 { 24.0 * theme::ui_zoom() }
    fn btn_w() -> f32 { 130.0 * theme::ui_zoom() }
    fn btn_h() -> f32 { 34.0 * theme::ui_zoom() }

    pub fn new(fs: &mut FontSystem) -> Self {
        let mk = |fs: &mut FontSystem, w: f32, h: f32| {
            let mut l = TextLabel::new(fs, w, h);
            l.align = VAlign::Top;
            l
        };
        let mut install = TextLabel::new(fs, 140.0, Self::btn_h());
        install.set(fs, "Install", theme::UI_FAMILY());
        let mut uninstall = TextLabel::new(fs, 140.0, Self::btn_h());
        uninstall.set(fs, "Uninstall", theme::UI_FAMILY());
        let mut set_theme = TextLabel::new(fs, 180.0, Self::btn_h());
        set_theme.set(fs, "Set Color Theme", theme::UI_FAMILY());
        let tabs = [
            { let mut l = TextLabel::new(fs, 140.0, Self::tabbar_h()); l.set(fs, "DETAILS", theme::UI_FAMILY()); l },
            { let mut l = TextLabel::new(fs, 140.0, Self::tabbar_h()); l.set(fs, "FEATURES", theme::UI_FAMILY()); l },
            { let mut l = TextLabel::new(fs, 140.0, Self::tabbar_h()); l.set(fs, "CHANGELOG", theme::UI_FAMILY()); l },
        ];
        Self {
            name: mk(fs, 1000.0, 36.0),
            meta: mk(fs, 1000.0, 24.0),
            desc: mk(fs, 1000.0, 48.0),
            install,
            uninstall,
            set_theme,
            tabs,
            details: Markdown::new(fs),
            features: Markdown::new(fs),
            changelog: Markdown::new(fs),
            sidebar: mk(fs, Self::sidebar_w(), 600.0),
            tab: DetailTab::Details,
            icon_uv: None,
            icon_color: [0.3, 0.3, 0.3, 1.0],
            supported: false,
            installed: false,
            is_theme: false,
        }
    }

    pub fn tab(&self) -> DetailTab {
        self.tab
    }
    pub fn set_tab(&mut self, tab: DetailTab) {
        self.tab = tab;
    }

    /// Re-shape the static labels (Install / tab names) after a zoom change. The
    /// name/meta/desc/sidebar/body re-shape on their own since `set` runs each frame.
    pub fn reshape(&mut self, fs: &mut FontSystem) {
        self.install.reshape(fs);
        self.uninstall.reshape(fs);
        self.set_theme.reshape(fs);
        for t in &mut self.tabs {
            t.reshape(fs);
        }
    }

    /// Update all content from extension data + the loaded docs.
    #[allow(clippy::too_many_arguments)]
    pub fn set(
        &mut self,
        fs: &mut FontSystem,
        region: Rect,
        name: &str,
        publisher: &str,
        category: &str,
        description: &str,
        version: &str,
        downloads: u64,
        rating: f32,
        supported: bool,
        installed: bool,
        is_theme: bool,
        icon_uv: Option<[f32; 4]>,
        readme: Option<&str>,
        changelog: Option<&str>,
        features_md: &str,
    ) {
        let ui = |c: Color| Attrs::new().family(Family::Name(theme::UI_FAMILY())).color(c);

        // Name (large).
        let z = theme::ui_zoom();
        let name_span = [(name.to_string(), ui(theme::FG_ACTIVE()).metrics(Metrics::new(24.0 * z, 32.0 * z)))];
        self.name.set_rich(fs, name, &name_span, ui(theme::FG_ACTIVE()));

        // Meta: publisher · vX · ★rating · N installs.
        let mut meta = vec![(publisher.to_string(), ui(theme::FG_TEXT()))];
        if !version.is_empty() {
            meta.push((format!("   v{version}"), ui(theme::FG_DIM())));
        }
        if rating > 0.0 {
            meta.push((format!("   ★ {rating:.1}"), ui(GOLD)));
        }
        if downloads > 0 {
            meta.push((format!("   {} installs", fmt_count(downloads)), ui(theme::FG_DIM())));
        }
        let mkey = format!("{publisher}{version}{rating}{downloads}");
        self.meta.set_rich(fs, &mkey, &meta, ui(theme::FG_TEXT()));

        // Description.
        let d = if description.is_empty() { "" } else { description };
        self.desc.set(fs, d, theme::UI_FAMILY());

        // Tab bodies.
        let bw = Self::body_rect(region).w;
        let details_src = readme.filter(|r| !r.trim().is_empty()).unwrap_or(description);
        self.details.set(fs, &format!("d{name}{}", details_src.len()), details_src, bw);
        self.features.set(fs, &format!("f{name}{}", features_md.len()), features_md, bw);
        let cl = changelog.filter(|c| !c.trim().is_empty()).unwrap_or("_No changelog provided._");
        self.changelog.set(fs, &format!("c{name}{}", cl.len()), cl, bw);

        // Sidebar metadata.
        let hdr = |c: Color| ui(c);
        let mut side: Vec<(String, Attrs<'static>)> = Vec::new();
        let mut section = |side: &mut Vec<(String, Attrs<'static>)>, title: &str, value: String| {
            side.push((format!("{title}\n"), hdr(theme::FG_DIM())));
            side.push((format!("{value}\n\n"), hdr(theme::FG_TEXT())));
        };
        if downloads > 0 {
            section(&mut side, "INSTALLS", fmt_count(downloads));
        }
        section(&mut side, "VERSION", if version.is_empty() { "—".into() } else { version.to_string() });
        section(&mut side, "IDENTIFIER", format!("{publisher}.{name}"));
        section(&mut side, "CATEGORY", category.to_string());
        let status = if installed {
            "Installed"
        } else if supported {
            "Supported in Nova"
        } else {
            "Needs runtime"
        };
        section(&mut side, "STATUS", status.into());
        let skey = format!("{publisher}{name}{version}{downloads}{supported}");
        self.sidebar.set_rich(fs, &skey, &side, ui(theme::FG_TEXT()));

        self.icon_uv = icon_uv;
        self.icon_color = icon_color(name);
        self.supported = supported;
        self.installed = installed;
        self.is_theme = is_theme;
    }

    // ---- geometry ----

    fn icon_rect(r: Rect) -> Rect {
        Rect { x: r.x + Self::pad(), y: r.y + Self::header_top(), w: Self::icon(), h: Self::icon() }
    }
    fn sidebar_x(r: Rect) -> f32 {
        r.x + r.w - Self::sidebar_w()
    }
    fn header_text_x(r: Rect) -> f32 {
        r.x + Self::pad() + Self::icon() + theme::zpx(20.0)
    }
    fn install_rect(r: Rect) -> Rect {
        Rect { x: Self::sidebar_x(r) - Self::content_gap() - Self::btn_w(), y: r.y + Self::header_top() + theme::zpx(4.0), w: Self::btn_w(), h: Self::btn_h() }
    }
    /// "Set Color Theme" button, sized to its label, sitting just left of the
    /// Install/Uninstall button. Only meaningful for an installed theme extension.
    fn set_theme_rect(&self, r: Rect) -> Rect {
        let w = self.set_theme.width() + theme::zpx(24.0);
        let ir = Self::install_rect(r);
        Rect { x: ir.x - theme::zpx(10.0) - w, y: ir.y, w, h: ir.h }
    }
    fn tabbar_y(r: Rect) -> f32 {
        r.y + Self::header_top() + Self::icon() + theme::zpx(18.0)
    }
    fn content_y(r: Rect) -> f32 {
        Self::tabbar_y(r) + Self::tabbar_h() + theme::zpx(14.0)
    }
    fn main_x(r: Rect) -> f32 {
        r.x + Self::pad()
    }
    fn body_rect(r: Rect) -> Rect {
        let x = Self::main_x(r);
        let y = Self::content_y(r);
        let w = (Self::sidebar_x(r) - Self::content_gap() - x).max(80.0);
        Rect { x, y, w, h: (r.y + r.h - y - Self::pad()).max(0.0) }
    }
    fn sidebar_rect(r: Rect) -> Rect {
        Rect { x: Self::sidebar_x(r) + theme::zpx(16.0), y: Self::content_y(r), w: Self::sidebar_w() - theme::zpx(28.0), h: r.h - Self::content_y(r) }
    }
    /// Tab hit/draw rects (label width + padding), laid left-to-right.
    fn tab_rects(&self, r: Rect) -> [Rect; 3] {
        let y = Self::tabbar_y(r);
        let mut x = Self::main_x(r);
        let mut out = [Rect { x: 0.0, y, w: 0.0, h: Self::tabbar_h() }; 3];
        for (i, t) in self.tabs.iter().enumerate() {
            let w = t.width() + theme::zpx(28.0);
            out[i] = Rect { x, y, w, h: Self::tabbar_h() };
            x += w;
        }
        out
    }

    fn active_body(&self) -> &Markdown {
        match self.tab {
            DetailTab::Details => &self.details,
            DetailTab::Features => &self.features,
            DetailTab::Changelog => &self.changelog,
        }
    }

    pub fn body_content_height(&self, size_of: &dyn Fn(&str) -> Option<(f32, f32)>) -> f32 {
        self.active_body().content_height(size_of)
    }

    /// Image URLs referenced by the active tab's body (for prefetching).
    pub fn image_urls(&self) -> &[String] {
        self.active_body().image_urls()
    }

    /// Screen-space (rect, url) for every visible link in the active body — used
    /// for underline drawing and click hit-testing.
    pub fn link_rects(&self, region: Rect, scroll: f32, size_of: &dyn Fn(&str) -> Option<(f32, f32)>) -> Vec<(Rect, String)> {
        self.active_body().link_geometry(Self::body_rect(region), scroll, size_of)
    }
    pub fn body_viewport_height(r: Rect) -> f32 {
        Self::body_rect(r).h
    }
    /// The body viewport rect (used to clip the README images when scrolling).
    pub fn body_viewport(r: Rect) -> Rect {
        Self::body_rect(r)
    }

    // ---- interaction ----

    pub fn hit_install(&self, r: Rect, p: (f32, f32)) -> bool {
        self.supported && !self.installed && Self::install_rect(r).contains(p)
    }
    /// The same button rect, when the extension is installed, is the Uninstall action.
    pub fn hit_uninstall(&self, r: Rect, p: (f32, f32)) -> bool {
        self.installed && Self::install_rect(r).contains(p)
    }
    /// "Set Color Theme" button (installed theme extensions only).
    pub fn hit_set_theme(&self, r: Rect, p: (f32, f32)) -> bool {
        self.installed && self.is_theme && self.set_theme_rect(r).contains(p)
    }
    /// True when any header button is interactive — drives hover.
    pub fn hit_button(&self, r: Rect, p: (f32, f32)) -> bool {
        self.hit_install(r, p) || self.hit_uninstall(r, p) || self.hit_set_theme(r, p)
    }
    pub fn hit_tab(&self, r: Rect, p: (f32, f32)) -> Option<DetailTab> {
        self.tab_rects(r)
            .iter()
            .position(|rect| rect.contains(p))
            .map(|i| DetailTab::ALL[i])
    }

    pub fn icon_instance(&self, r: Rect) -> Option<IconInstance> {
        let ir = Self::icon_rect(r);
        // Hide the icon entirely if it would spill below the editor region (e.g. a
        // tall terminal leaves no room) — an atlas quad can't be partially clipped here.
        if ir.y + ir.h > r.y + r.h {
            return None;
        }
        self.icon_uv.map(|uv| IconInstance { rect: [ir.x, ir.y, ir.w, ir.h], uv })
    }

    // ---- drawing ----

    pub fn draw_quads(&self, r: Rect, hovered_install: bool, hovered_tab: Option<DetailTab>, quads: &mut Vec<Quad>) {
        // Clip chrome to the editor region (a tall terminal shortens it).
        let bottom = r.y + r.h;
        let clip = |rect: Rect| -> Option<Rect> {
            (rect.y < bottom).then_some(Rect { h: rect.h.min(bottom - rect.y).max(0.0), ..rect })
        };
        // Icon placeholder (real icon drawn via the atlas).
        if self.icon_uv.is_none() {
            if let Some(rr) = clip(Self::icon_rect(r)) {
                quads.push(rr.quad(self.icon_color));
            }
        }
        // Header button: Install when installable, Uninstall once installed.
        if self.installed || (self.supported && !self.installed) {
            let c = if hovered_install { theme::DIALOG_BTN_HOVER() } else { theme::DIALOG_BTN() };
            if let Some(rr) = clip(Self::install_rect(r)) {
                quads.push(rr.quad(c));
            }
        }
        // "Set Color Theme" button for an installed theme extension.
        if self.installed && self.is_theme {
            if let Some(rr) = clip(self.set_theme_rect(r)) {
                quads.push(rr.quad(theme::DIALOG_BTN()));
            }
        }
        // Tab bar bottom border across the main column.
        let tabs = self.tab_rects(r);
        let bar_y = Self::tabbar_y(r) + Self::tabbar_h();
        let main_x = Self::main_x(r);
        let bar_w = Self::sidebar_x(r) - Self::content_gap() - main_x;
        if bar_y <= bottom {
            quads.push(Quad::new(main_x, bar_y - 1.0, bar_w, 1.0, theme::BORDER()));
            // Hovered tab subtle bg + active tab accent underline.
            for (i, t) in DetailTab::ALL.iter().enumerate() {
                if hovered_tab == Some(*t) && *t != self.tab {
                    if let Some(rr) = clip(tabs[i]) {
                        quads.push(rr.quad(theme::TREE_HOVER()));
                    }
                }
            }
            let a = tabs[self.tab.index()];
            quads.push(Quad::new(a.x, bar_y - 2.0, a.w, 2.0, color_quad(theme::FG_ACTIVE())));
        }
        // Vertical separator before the sidebar.
        let sx = Self::sidebar_x(r) - Self::content_gap() * 0.5;
        let sep_top = Self::tabbar_y(r);
        if sep_top < bottom {
            quads.push(Quad::new(sx, sep_top, 1.0, bottom - sep_top, theme::BORDER()));
        }
    }

    pub fn draw_text<'a>(
        &'a self,
        r: Rect,
        scroll: f32,
        size_of: &dyn Fn(&str) -> Option<(f32, f32)>,
        areas: &mut Vec<TextArea<'a>>,
        img_rects: &mut Vec<(String, Rect)>,
    ) {
        // Clip everything to the editor region: when the terminal panel is tall the
        // region is short, and the detail content must not spill over the terminal.
        let bottom = r.y + r.h;
        let clip = |rect: Rect| -> Option<Rect> {
            (rect.y < bottom).then_some(Rect { h: rect.h.min(bottom - rect.y).max(0.0), ..rect })
        };
        let htext_x = Self::header_text_x(r);
        // Stop the header text before the left-most header button (the Set Color Theme
        // button sits left of Install/Uninstall) so the long title can't overlap it.
        let buttons_left = if self.installed && self.is_theme {
            self.set_theme_rect(r).x
        } else {
            Self::install_rect(r).x
        };
        let avail = buttons_left - htext_x - theme::zpx(16.0);
        let top = r.y + Self::header_top();
        if let Some(rr) = clip(Rect { x: htext_x, y: top, w: avail, h: theme::zpx(34.0) }) {
            self.name.push(htext_x, rr, theme::FG_ACTIVE(), areas);
        }
        if let Some(rr) = clip(Rect { x: htext_x, y: top + theme::zpx(36.0), w: avail, h: theme::zpx(22.0) }) {
            self.meta.push(htext_x, rr, theme::FG_DIM(), areas);
        }
        if let Some(rr) = clip(Rect { x: htext_x, y: top + theme::zpx(58.0), w: avail, h: theme::zpx(40.0) }) {
            self.desc.push(htext_x, rr, theme::FG_TEXT(), areas);
        }

        // Tab labels.
        let tabs = self.tab_rects(r);
        for (i, t) in DetailTab::ALL.iter().enumerate() {
            if let Some(rr) = clip(tabs[i]) {
                let color = if *t == self.tab { theme::FG_ACTIVE() } else { theme::FG_DIM() };
                self.tabs[i].draw_center(rr, color, areas);
            }
        }

        // Active body (scrolled + clipped); collects image rects for the media layer.
        self.active_body().draw(Self::body_rect(r), scroll, size_of, areas, img_rects);

        // Sidebar metadata.
        if let Some(sr) = clip(Self::sidebar_rect(r)) {
            self.sidebar.push(sr.x, sr, theme::FG_TEXT(), areas);
        }

        // Header button label: Uninstall once installed, else Install (when supported).
        if let Some(pill) = clip(Self::install_rect(r)) {
            if self.installed {
                self.uninstall.draw_center(pill, theme::FG_ACTIVE(), areas);
            } else if self.supported {
                self.install.draw_center(pill, theme::FG_ACTIVE(), areas);
            }
        }
        if self.installed && self.is_theme {
            if let Some(pill) = clip(self.set_theme_rect(r)) {
                self.set_theme.draw_center(pill, theme::FG_ACTIVE(), areas);
            }
        }
    }
}

/// glyphon sRGB Color → quad [f32;4] (matches how its text renders).
fn color_quad(c: Color) -> [f32; 4] {
    [c.r() as f32 / 255.0, c.g() as f32 / 255.0, c.b() as f32 / 255.0, c.a() as f32 / 255.0]
}

/// Compact count formatting: 1234 -> "1.2K", 3_400_000 -> "3.4M".
fn fmt_count(n: u64) -> String {
    if n >= 1_000_000 {
        format!("{:.1}M", n as f64 / 1_000_000.0)
    } else if n >= 1_000 {
        format!("{:.1}K", n as f64 / 1_000.0)
    } else {
        n.to_string()
    }
}

/// Stable placeholder tile color from the name (when no real icon).
fn icon_color(name: &str) -> [f32; 4] {
    const PALETTE: [[f32; 4]; 8] = [
        [0.20, 0.45, 0.78, 1.0],
        [0.55, 0.36, 0.72, 1.0],
        [0.20, 0.62, 0.50, 1.0],
        [0.78, 0.45, 0.22, 1.0],
        [0.70, 0.27, 0.40, 1.0],
        [0.36, 0.58, 0.28, 1.0],
        [0.30, 0.52, 0.66, 1.0],
        [0.62, 0.55, 0.25, 1.0],
    ];
    let h = name.bytes().fold(0u32, |a, b| a.wrapping_mul(31).wrapping_add(b as u32));
    PALETTE[(h as usize) % PALETTE.len()]
}
