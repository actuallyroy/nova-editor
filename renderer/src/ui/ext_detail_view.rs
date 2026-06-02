// Extension detail page (the README/CHANGELOG/Features view shown in the editor
// area). This struct groups all of the detail page's state in one place; the
// `ExtensionDetail` widget (markdown layout + buffers) lives in `gpu.ui` and the
// image/media GPU pass stays in `render.rs`, since both need direct `gpu` access.
//
// NOTE (refactor staging): the open/build/install methods + draw still live on
// `App`/`render.rs` and read these fields via `self.detail.*` / `app.detail.*`.
// Moving that logic into this file is a follow-up (it touches the GPU media pass,
// which needs visual verification to change safely).

use std::collections::HashSet;
use std::path::PathBuf;
use std::sync::mpsc::Sender;

use crate::ext_detail::DetailTab;
use crate::extensions::{ExtKind, Extension, OpenExt};
use crate::gpu::GpuState;
use crate::marketplace::{self, RemoteExt, WorkerMsg};
use crate::widgets::{ScrollOpts, ScrollView};

pub struct ExtDetailView {
    /// The extension whose detail page is open in the editor area (None = editor).
    pub open_extension: Option<OpenExt>,
    pub ext_readme: Option<String>,    // README text for the open detail page
    pub ext_changelog: Option<String>, // CHANGELOG text for the open detail page
    pub ext_features: String,          // generated Features-tab markdown
    pub ext_doc_gen: u64,              // discards stale async README/changelog fetches
    pub ext_img_dir: Option<PathBuf>,  // base dir for relative README images (local)
    pub ext_img_base: Option<String>,  // base URL for relative README images (remote)
    pub requested_images: HashSet<String>, // README image keys already fetched/loading
    pub ext_detail_scroll: ScrollView, // detail-page body scroll
    pub hovered_detail_tab: Option<DetailTab>,
    pub hovered_page_install: bool,
}

impl ExtDetailView {
    pub fn new() -> Self {
        Self {
            open_extension: None,
            ext_readme: None,
            ext_changelog: None,
            ext_features: String::new(),
            ext_doc_gen: 0,
            ext_img_dir: None,
            ext_img_base: None,
            requested_images: HashSet::new(),
            ext_detail_scroll: ScrollView::new(ScrollOpts::vertical()),
            hovered_detail_tab: None,
            hovered_page_install: false,
        }
    }

    /// Open the detail page for an extension and load its README (local read /
    /// remote fetch), resetting the page scroll. Caller redraws.
    pub fn open(
        &mut self,
        which: OpenExt,
        gpu: &mut GpuState,
        extensions: &[Extension],
        ext_remote: &[RemoteExt],
        worker_tx: &Sender<WorkerMsg>,
    ) {
        self.open_extension = Some(which);
        self.ext_detail_scroll.scroll_to_y(0.0);
        self.ext_readme = None;
        self.ext_changelog = None;
        self.ext_img_dir = None;
        self.ext_img_base = None;
        self.requested_images.clear();
        gpu.ui.ext_detail.set_tab(DetailTab::Details);
        self.ext_features = Self::build_features_md(which, extensions);
        self.ext_doc_gen += 1;
        let gen = self.ext_doc_gen;
        match which {
            OpenExt::Local(i) => {
                let readme = extensions.get(i).and_then(|e| e.readme_path.clone());
                self.ext_img_dir = readme.as_ref().and_then(|p| p.parent().map(|d| d.to_path_buf()));
                self.ext_readme = readme.and_then(|p| std::fs::read_to_string(&p).ok());
                self.ext_changelog = extensions
                    .get(i)
                    .and_then(|e| e.changelog_path.clone())
                    .and_then(|p| std::fs::read_to_string(&p).ok());
            }
            OpenExt::Remote(i) => {
                if let Some(e) = ext_remote.get(i) {
                    // Relative README images resolve against the readme URL's dir.
                    self.ext_img_base = e.readme_url.clone();
                    if let Some(url) = e.readme_url.clone() {
                        marketplace::readme_async(worker_tx.clone(), url, gen);
                    }
                    if let Some(url) = e.changelog_url.clone() {
                        marketplace::changelog_async(worker_tx.clone(), url, gen);
                    }
                }
            }
        }
    }

    /// Build the Features-tab markdown from what Nova knows about the extension.
    fn build_features_md(which: OpenExt, extensions: &[Extension]) -> String {
        match which {
            OpenExt::Local(i) => {
                let Some(e) = extensions.get(i) else { return String::new() };
                let mut s = String::new();
                match e.kind {
                    ExtKind::Theme => s.push_str("### Color Theme\nContributes a color theme Nova can apply natively.\n\n"),
                    ExtKind::Grammar => s.push_str("### Syntax Highlighting\nShips TextMate grammars Nova runs natively for syntax coloring.\n\n"),
                    ExtKind::Declarative => s.push_str("### Language Support\nContributes snippets / language configuration.\n\n"),
                    ExtKind::Code => s.push_str("### Code Extension\nNeeds the JavaScript extension runtime (not yet supported in Nova).\n\n"),
                }
                if !e.grammar_paths.is_empty() {
                    s.push_str(&format!("- {} grammar file(s)\n", e.grammar_paths.len()));
                }
                if !e.themes.is_empty() {
                    s.push_str(&format!("- {} color theme(s)\n", e.themes.len()));
                }
                s
            }
            OpenExt::Remote(_) => "Feature details are available after install.".to_string(),
        }
    }
}
