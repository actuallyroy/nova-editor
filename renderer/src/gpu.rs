// GPU/rendering state: surface, device, text renderer, and all the persistent
// widget instances. Constructed once at startup.

use std::sync::Arc;

use anyhow::{Context, Result};
use glyphon::{
    Buffer, Cache, FontSystem, SwashCache, TextAtlas, TextRenderer, Viewport,
};
use wgpu::{
    Backends, CompositeAlphaMode, DeviceDescriptor, Instance, InstanceDescriptor,
    MultisampleState, PresentMode, RequestAdapterOptions, SurfaceConfiguration, TextureFormat,
    TextureUsages,
};
use winit::window::Window;

use crate::icon::IconAtlas;
use crate::media::Media;
use crate::quad::QuadRenderer;
use crate::theme;
use crate::ext_detail::ExtensionDetail;
use crate::widgets::{
    make_ui_buffer, Dialog, Gutter, IconButton, ListView, Menu,
    MenuBar, SearchField, TextInput, TextLabel,
};

pub struct UiBuffers {
    pub sidebar_header: TextLabel,
    pub root_label: TextLabel,
    pub sidebar: ListView,
    pub tabs: Buffer,
    pub status: TextLabel,
    pub status_right: TextLabel,
    pub line_numbers: Gutter,
    pub line_numbers2: Gutter, // right pane's gutter in a side-by-side diff
    pub palette_input: TextInput,
    pub palette_list: ListView,
    pub find_input: TextInput,
    pub menu: Menu,
    pub dialog: Dialog,
    pub ext_detail: ExtensionDetail,
    pub terminal_panes: Vec<Buffer>, // one monospace grid buffer per visible split pane
    pub term_tablist: ListView,      // right-side terminal tab switcher (multi-tab only)

}

pub struct GpuState {
    pub window: Arc<Window>,
    pub surface: wgpu::Surface<'static>,
    pub device: wgpu::Device,
    pub queue: wgpu::Queue,
    pub config: SurfaceConfiguration,
    pub font_system: FontSystem,
    pub swash_cache: SwashCache,
    pub viewport: Viewport,
    pub atlas: TextAtlas,
    pub text_renderer: TextRenderer,
    pub quad_renderer: QuadRenderer,
    pub icon_atlas: IconAtlas,
    pub media: Media,
    pub ui: UiBuffers,
    pub activity_btns: Vec<IconButton>,
    pub titlebar_btns: Vec<IconButton>,
    pub tab_close_btn: IconButton,
    pub search: SearchField,
    pub menubar: MenuBar,
    pub layout_btns: Vec<IconButton>,
    pub explorer_btns: Vec<IconButton>,
    pub terminal_tabs: Vec<TextLabel>,   // panel header tab labels (stub)
    pub terminal_btns: Vec<IconButton>,  // panel header right-side icons (stub)
    pub create_icons: [IconButton; 2],
    pub create_input: TextInput,
}

impl GpuState {
    pub async fn new(window: Arc<Window>) -> Result<Self> {
        let size = window.inner_size();
        let instance = Instance::new(InstanceDescriptor {
            backends: Backends::PRIMARY,
            ..Default::default()
        });
        let surface = instance.create_surface(window.clone())?;
        let adapter = instance
            .request_adapter(&RequestAdapterOptions {
                power_preference: wgpu::PowerPreference::HighPerformance,
                compatible_surface: Some(&surface),
                force_fallback_adapter: false,
            })
            .await
            .context("no compatible GPU adapter")?;
        let (device, queue) = adapter
            .request_device(
                &DeviceDescriptor {
                    label: Some("nova-device"),
                    ..Default::default()
                },
                None,
            )
            .await
            .context("failed to acquire wgpu device")?;

        // Render to a non-sRGB surface so our sRGB-authored palette (and glyphon's
        // sRGB glyph colors) display at their true values. An sRGB surface would
        // re-encode the colors we write, washing every dark gray toward mid-gray.
        let caps = surface.get_capabilities(&adapter);
        let format = caps
            .formats
            .iter()
            .copied()
            .find(|f| *f == TextureFormat::Bgra8Unorm)
            .or_else(|| caps.formats.iter().copied().find(|f| !f.is_srgb()))
            .unwrap_or(caps.formats[0]);
        let config = SurfaceConfiguration {
            usage: TextureUsages::RENDER_ATTACHMENT,
            format,
            width: size.width.max(1),
            height: size.height.max(1),
            present_mode: PresentMode::Fifo,
            alpha_mode: CompositeAlphaMode::Auto,
            view_formats: vec![],
            desired_maximum_frame_latency: 2,
        };
        surface.configure(&device, &config);

        let mut font_system = FontSystem::new();
        // Load VSCode's Codicon icon font so our glyphs match VSCode exactly.
        font_system
            .db_mut()
            .load_font_data(include_bytes!("../assets/codicon.ttf").to_vec());
        let swash_cache = SwashCache::new();
        let cache = Cache::new(&device);
        let viewport = Viewport::new(&device, &cache);
        let mut atlas = TextAtlas::new(&device, &queue, &cache, config.format);
        let text_renderer =
            TextRenderer::new(&mut atlas, &device, MultisampleState::default(), None);
        let quad_renderer = QuadRenderer::new(&device, config.format);
        let icon_atlas = IconAtlas::new(&device, config.format);
        let media = Media::new(&device, config.format);

        let ic = theme::ICON_FAMILY;
        let activity_btns = vec![
            IconButton::new(&mut font_system, theme::ICON_FILES, ic, 20.0),
            IconButton::new(&mut font_system, theme::ICON_SEARCH, ic, 20.0),
            IconButton::new(&mut font_system, theme::ICON_SOURCE_CONTROL, ic, 20.0),
            IconButton::new(&mut font_system, theme::ICON_RUN, ic, 20.0),
            IconButton::new(&mut font_system, theme::ICON_EXTENSIONS, ic, 20.0),
            IconButton::new(&mut font_system, theme::ICON_ACCOUNT, ic, 20.0),
            IconButton::new(&mut font_system, theme::ICON_SETTINGS, ic, 20.0),
        ];
        let titlebar_btns = vec![
            IconButton::new(&mut font_system, theme::ICON_MIN, ic, 12.0),
            IconButton::new(&mut font_system, theme::ICON_MAX, ic, 12.0),
            IconButton::new(&mut font_system, theme::ICON_WIN_CLOSE, ic, 12.0),
        ];
        let tab_close_btn = IconButton::new(&mut font_system, theme::ICON_CLOSE, ic, 12.0);
        let search = SearchField::new(&mut font_system);
        let menubar = MenuBar::new(&mut font_system);
        let layout_btns = vec![
            IconButton::new(&mut font_system, theme::ICON_LAYOUT_SIDEBAR_LEFT, ic, 16.0),
            IconButton::new(&mut font_system, theme::ICON_LAYOUT_PANEL, ic, 16.0),
            IconButton::new(&mut font_system, theme::ICON_LAYOUT_SIDEBAR_RIGHT, ic, 16.0),
        ];
        let explorer_btns = vec![
            IconButton::new(&mut font_system, theme::ICON_NEW_FILE, ic, 16.0),
            IconButton::new(&mut font_system, theme::ICON_NEW_FOLDER, ic, 16.0),
            IconButton::new(&mut font_system, theme::ICON_REFRESH, ic, 16.0),
            IconButton::new(&mut font_system, theme::ICON_COLLAPSE_ALL, ic, 16.0),
        ];
        // Panel-header stub widgets: tab labels + right-side icon buttons (VSCode).
        let terminal_tabs = theme::PANEL_TABS
            .iter()
            .map(|label| {
                let mut l = TextLabel::new(&mut font_system, 200.0, theme::TERMINAL_HEADER_H);
                l.set(&mut font_system, label, theme::UI_FAMILY());
                l
            })
            .collect();
        let terminal_btns = vec![
            IconButton::new(&mut font_system, theme::ICON_ADD, ic, 16.0),
            IconButton::new(&mut font_system, theme::ICON_SPLIT_HORIZONTAL, ic, 16.0),
            IconButton::new(&mut font_system, theme::ICON_TRASH, ic, 16.0),
            IconButton::new(&mut font_system, theme::ICON_ELLIPSIS, ic, 16.0),
            IconButton::new(&mut font_system, theme::ICON_CHEVRON_UP, ic, 16.0),
            IconButton::new(&mut font_system, theme::ICON_CLOSE, ic, 16.0),
        ];
        let create_icons = [
            IconButton::new(&mut font_system, theme::ICON_FILE, ic, 16.0),
            IconButton::new(&mut font_system, theme::ICON_FOLDER_CLOSED, ic, 16.0),
        ];
        let create_input = TextInput::new(&mut font_system, theme::SIDEBAR_WIDTH, theme::TREE_ROW_HEIGHT);

        let ui = UiBuffers {
            sidebar_header: TextLabel::new(&mut font_system, theme::SIDEBAR_WIDTH, 60.0),
            root_label: TextLabel::new(&mut font_system, theme::SIDEBAR_WIDTH, theme::TREE_ROW_HEIGHT),
            sidebar: ListView::new(
                &mut font_system,
                theme::SIDEBAR_WIDTH,
                800.0,
                theme::TREE_ROW_HEIGHT,
                12.0,
            ),
            tabs: make_ui_buffer(&mut font_system, 4000.0, theme::TAB_HEIGHT),
            status: TextLabel::new(&mut font_system, 4000.0, theme::STATUS_BAR_HEIGHT),
            status_right: TextLabel::new(&mut font_system, 4000.0, theme::STATUS_BAR_HEIGHT),
            line_numbers: Gutter::new(&mut font_system, theme::GUTTER_WIDTH),
            line_numbers2: Gutter::new(&mut font_system, theme::GUTTER_WIDTH),
            palette_input: TextInput::new(&mut font_system, 600.0, theme::PALETTE_INPUT_HEIGHT),
            palette_list: ListView::new(
                &mut font_system,
                600.0,
                800.0,
                theme::PALETTE_ROW_HEIGHT,
                6.0,
            ),
            find_input: TextInput::new(&mut font_system, 800.0, theme::FIND_BAR_HEIGHT),
            menu: Menu::new(&mut font_system, 200.0),
            dialog: Dialog::new(&mut font_system),
            ext_detail: ExtensionDetail::new(&mut font_system),
            terminal_panes: Vec::new(), // grown on demand as panes are split
            term_tablist: ListView::new(
                &mut font_system,
                crate::TERMINAL_TABLIST_W,
                800.0,
                theme::TREE_ROW_HEIGHT,
                10.0,
            ),
        };

        Ok(Self {
            window,
            surface,
            device,
            queue,
            config,
            font_system,
            swash_cache,
            viewport,
            atlas,
            text_renderer,
            quad_renderer,
            icon_atlas,
            media,
            ui,
            activity_btns,
            titlebar_btns,
            tab_close_btn,
            search,
            menubar,
            layout_btns,
            explorer_btns,
            terminal_tabs,
            terminal_btns,
            create_icons,
            create_input,
        })
    }

    pub fn resize(&mut self, w: u32, h: u32) {
        if w == 0 || h == 0 {
            return;
        }
        self.config.width = w;
        self.config.height = h;
        self.surface.configure(&self.device, &self.config);
    }
}
