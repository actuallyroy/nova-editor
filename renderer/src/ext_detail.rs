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
    installed_lbl: TextLabel,
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
}

impl ExtensionDetail {
    const PAD: f32 = 24.0;
    const ICON: f32 = 72.0;
    const HEADER_TOP: f32 = 24.0;
    const TABBAR_H: f32 = 34.0;
    const SIDEBAR_W: f32 = 270.0;
    const CONTENT_GAP: f32 = 24.0;
    const BTN_W: f32 = 130.0;
    const BTN_H: f32 = 34.0;

    pub fn new(fs: &mut FontSystem) -> Self {
        let mk = |fs: &mut FontSystem, w: f32, h: f32| {
            let mut l = TextLabel::new(fs, w, h);
            l.align = VAlign::Top;
            l
        };
        let mut install = TextLabel::new(fs, 140.0, Self::BTN_H);
        install.set(fs, "Install", theme::UI_FAMILY);
        let mut installed_lbl = TextLabel::new(fs, 140.0, Self::BTN_H);
        installed_lbl.set(fs, "Installed", theme::UI_FAMILY);
        let tabs = [
            { let mut l = TextLabel::new(fs, 140.0, Self::TABBAR_H); l.set(fs, "DETAILS", theme::UI_FAMILY); l },
            { let mut l = TextLabel::new(fs, 140.0, Self::TABBAR_H); l.set(fs, "FEATURES", theme::UI_FAMILY); l },
            { let mut l = TextLabel::new(fs, 140.0, Self::TABBAR_H); l.set(fs, "CHANGELOG", theme::UI_FAMILY); l },
        ];
        Self {
            name: mk(fs, 1000.0, 36.0),
            meta: mk(fs, 1000.0, 24.0),
            desc: mk(fs, 1000.0, 48.0),
            install,
            installed_lbl,
            tabs,
            details: Markdown::new(fs),
            features: Markdown::new(fs),
            changelog: Markdown::new(fs),
            sidebar: mk(fs, Self::SIDEBAR_W, 600.0),
            tab: DetailTab::Details,
            icon_uv: None,
            icon_color: [0.3, 0.3, 0.3, 1.0],
            supported: false,
            installed: false,
        }
    }

    pub fn tab(&self) -> DetailTab {
        self.tab
    }
    pub fn set_tab(&mut self, tab: DetailTab) {
        self.tab = tab;
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
        icon_uv: Option<[f32; 4]>,
        readme: Option<&str>,
        changelog: Option<&str>,
        features_md: &str,
    ) {
        let ui = |c: Color| Attrs::new().family(Family::Name(theme::UI_FAMILY)).color(c);

        // Name (large).
        let name_span = [(name.to_string(), ui(theme::FG_ACTIVE()).metrics(Metrics::new(24.0, 32.0)))];
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
        self.desc.set(fs, d, theme::UI_FAMILY);

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
        section(&mut side, "STATUS", if supported { "Supported in Nova".into() } else { "Needs runtime".into() });
        let skey = format!("{publisher}{name}{version}{downloads}{supported}");
        self.sidebar.set_rich(fs, &skey, &side, ui(theme::FG_TEXT()));

        self.icon_uv = icon_uv;
        self.icon_color = icon_color(name);
        self.supported = supported;
        self.installed = installed;
    }

    // ---- geometry ----

    fn icon_rect(r: Rect) -> Rect {
        Rect { x: r.x + Self::PAD, y: r.y + Self::HEADER_TOP, w: Self::ICON, h: Self::ICON }
    }
    fn sidebar_x(r: Rect) -> f32 {
        r.x + r.w - Self::SIDEBAR_W
    }
    fn header_text_x(r: Rect) -> f32 {
        r.x + Self::PAD + Self::ICON + 20.0
    }
    fn install_rect(r: Rect) -> Rect {
        Rect { x: Self::sidebar_x(r) - Self::CONTENT_GAP - Self::BTN_W, y: r.y + Self::HEADER_TOP + 4.0, w: Self::BTN_W, h: Self::BTN_H }
    }
    fn tabbar_y(r: Rect) -> f32 {
        r.y + Self::HEADER_TOP + Self::ICON + 18.0
    }
    fn content_y(r: Rect) -> f32 {
        Self::tabbar_y(r) + Self::TABBAR_H + 14.0
    }
    fn main_x(r: Rect) -> f32 {
        r.x + Self::PAD
    }
    fn body_rect(r: Rect) -> Rect {
        let x = Self::main_x(r);
        let y = Self::content_y(r);
        let w = (Self::sidebar_x(r) - Self::CONTENT_GAP - x).max(80.0);
        Rect { x, y, w, h: (r.y + r.h - y - Self::PAD).max(40.0) }
    }
    fn sidebar_rect(r: Rect) -> Rect {
        Rect { x: Self::sidebar_x(r) + 16.0, y: Self::content_y(r), w: Self::SIDEBAR_W - 28.0, h: r.h - Self::content_y(r) }
    }
    /// Tab hit/draw rects (label width + padding), laid left-to-right.
    fn tab_rects(&self, r: Rect) -> [Rect; 3] {
        let y = Self::tabbar_y(r);
        let mut x = Self::main_x(r);
        let mut out = [Rect { x: 0.0, y, w: 0.0, h: Self::TABBAR_H }; 3];
        for (i, t) in self.tabs.iter().enumerate() {
            let w = t.width() + 28.0;
            out[i] = Rect { x, y, w, h: Self::TABBAR_H };
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

    pub fn body_content_height(&self) -> f32 {
        self.active_body().content_height()
    }
    pub fn body_viewport_height(r: Rect) -> f32 {
        Self::body_rect(r).h
    }

    // ---- interaction ----

    pub fn hit_install(&self, r: Rect, p: (f32, f32)) -> bool {
        self.supported && !self.installed && Self::install_rect(r).contains(p)
    }
    pub fn hit_tab(&self, r: Rect, p: (f32, f32)) -> Option<DetailTab> {
        self.tab_rects(r)
            .iter()
            .position(|rect| rect.contains(p))
            .map(|i| DetailTab::ALL[i])
    }

    pub fn icon_instance(&self, r: Rect) -> Option<IconInstance> {
        let ir = Self::icon_rect(r);
        self.icon_uv.map(|uv| IconInstance { rect: [ir.x, ir.y, ir.w, ir.h], uv })
    }

    // ---- drawing ----

    pub fn draw_quads(&self, r: Rect, hovered_install: bool, hovered_tab: Option<DetailTab>, quads: &mut Vec<Quad>) {
        // Icon placeholder (real icon drawn via the atlas).
        if self.icon_uv.is_none() {
            quads.push(Self::icon_rect(r).quad(self.icon_color));
        }
        // Install button.
        if self.supported && !self.installed {
            let c = if hovered_install { theme::DIALOG_BTN_HOVER() } else { theme::DIALOG_BTN() };
            quads.push(Self::install_rect(r).quad(c));
        }
        // Tab bar bottom border across the main column.
        let tabs = self.tab_rects(r);
        let bar_y = Self::tabbar_y(r) + Self::TABBAR_H;
        let main_x = Self::main_x(r);
        let bar_w = Self::sidebar_x(r) - Self::CONTENT_GAP - main_x;
        quads.push(Quad::new(main_x, bar_y - 1.0, bar_w, 1.0, theme::BORDER()));
        // Hovered tab subtle bg + active tab accent underline.
        for (i, t) in DetailTab::ALL.iter().enumerate() {
            if hovered_tab == Some(*t) && *t != self.tab {
                quads.push(tabs[i].quad(theme::TREE_HOVER()));
            }
        }
        let a = tabs[self.tab.index()];
        quads.push(Quad::new(a.x, bar_y - 2.0, a.w, 2.0, color_quad(theme::FG_ACTIVE())));
        // Vertical separator before the sidebar.
        let sx = Self::sidebar_x(r) - Self::CONTENT_GAP * 0.5;
        quads.push(Quad::new(sx, Self::tabbar_y(r), 1.0, r.y + r.h - Self::tabbar_y(r), theme::BORDER()));
    }

    pub fn draw_text<'a>(&'a self, r: Rect, scroll: f32, areas: &mut Vec<TextArea<'a>>) {
        let htext_x = Self::header_text_x(r);
        let avail = Self::install_rect(r).x - htext_x - 16.0;
        self.name.push(htext_x, Rect { x: htext_x, y: r.y + Self::HEADER_TOP, w: avail, h: 34.0 }, theme::FG_ACTIVE(), areas);
        self.meta.push(htext_x, Rect { x: htext_x, y: r.y + Self::HEADER_TOP + 36.0, w: avail, h: 22.0 }, theme::FG_DIM(), areas);
        self.desc.push(htext_x, Rect { x: htext_x, y: r.y + Self::HEADER_TOP + 58.0, w: avail, h: 40.0 }, theme::FG_TEXT(), areas);

        // Tab labels.
        let tabs = self.tab_rects(r);
        for (i, t) in DetailTab::ALL.iter().enumerate() {
            let color = if *t == self.tab { theme::FG_ACTIVE() } else { theme::FG_DIM() };
            self.tabs[i].draw_center(tabs[i], color, areas);
        }

        // Active body (scrolled + clipped).
        self.active_body().draw(Self::body_rect(r), scroll, areas);

        // Sidebar metadata.
        let sr = Self::sidebar_rect(r);
        self.sidebar.push(sr.x, sr, theme::FG_TEXT(), areas);

        // Install / Installed label.
        if self.supported {
            let pill = Self::install_rect(r);
            if self.installed {
                self.installed_lbl.draw_center(pill, theme::FG_DIM(), areas);
            } else {
                self.install.draw_center(pill, theme::FG_ACTIVE(), areas);
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
