// Reads VSCode extensions installed under ~/.vscode/extensions and classifies
// which ones Nova can support. Tier-1 declarative contributions (color themes,
// grammars, snippets, languages) are "supported"; extensions that need a JS host
// or webviews are not (yet).

use std::path::PathBuf;

use serde_json::Value;

use crate::marketplace::RemoteExt;

/// Which extension the detail page is showing: a locally-installed one (index
/// into the scanned list) or a marketplace search result (index into results).
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum OpenExt {
    Local(usize),
    Remote(usize),
}

/// Display data for the detail page, unified across local + remote sources.
pub struct OpenExtView {
    pub name: String,
    pub publisher: String,
    pub description: String,
    pub category: String,
    pub version: String,
    pub downloads: u64,
    pub rating: f32,
    pub supported: bool,
    pub installed: bool,
    pub remote: bool,
    /// Installed theme extension → show a "Set Color Theme" button (opens the picker
    /// scoped to this extension's themes).
    pub is_theme: bool,
    pub key: String, // icon-atlas key (name for local, "publisher.name" for remote)
}

/// Build unified detail-page data for whatever extension is open. A free function
/// over the specific slices (not a `&App` method) so callers can use it while
/// `app.gpu` is mutably borrowed — the field borrows stay disjoint.
pub fn open_ext_view(
    open: Option<OpenExt>,
    extensions: &[Extension],
    remote: &[RemoteExt],
) -> Option<OpenExtView> {
    match open? {
        OpenExt::Local(i) => {
            let e = extensions.get(i)?;
            Some(OpenExtView {
                name: e.name.clone(),
                publisher: e.publisher.clone(),
                description: e.description.clone(),
                category: e.category().to_string(),
                version: e.version.clone(),
                downloads: 0,
                rating: 0.0,
                supported: e.supported(),
                installed: true, // it came from the on-disk scan, so it's installed
                remote: false,
                is_theme: e.kind == ExtKind::Theme && !e.themes.is_empty(),
                key: e.name.clone(),
            })
        }
        OpenExt::Remote(i) => {
            let e = remote.get(i)?;
            let name = if e.display.is_empty() { e.name.clone() } else { e.display.clone() };
            // Installed iff Nova's own store has this exact marketplace id. Nova wrote
            // the folder name as `namespace.name`, so the slug matches `e.id()` exactly —
            // no fragile display-name comparison across two registries.
            let installed = extensions.iter().any(|x| x.slug.eq_ignore_ascii_case(&e.id()));
            Some(OpenExtView {
                name,
                publisher: e.namespace.clone(),
                description: e.description.clone(),
                category: "Marketplace".to_string(),
                version: e.version.clone(),
                downloads: e.downloads,
                rating: e.rating,
                supported: true,
                installed,
                remote: true,
                is_theme: false, // remote: can't set a theme until it's installed
                key: e.id(),
            })
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum ExtKind {
    Theme,       // contributes color themes — installable (applies the theme)
    Grammar,     // ships TextMate grammars — installable (runs them natively)
    Declarative, // snippets / languages only — supported class, not yet applied
    Code,        // needs a JS host / webview — unsupported
}

/// One color theme contributed by an extension (a `contributes.themes` entry).
/// `label` is the user-facing name and the key used by `workbench.colorTheme`.
#[derive(Clone)]
pub struct ThemeDef {
    pub label: String,
    pub dark: bool, // uiTheme: vs-dark / hc-black ⇒ dark; vs / hc-light ⇒ light
    pub path: PathBuf,
}

pub struct Extension {
    pub name: String,
    /// Marketplace id (`namespace.name`, lowercased) derived from the install
    /// folder name Nova wrote — the stable key for matching against search results.
    pub slug: String,
    pub publisher: String,
    pub description: String,
    pub version: String,
    pub kind: ExtKind,
    /// Every color theme this extension contributes (`contributes.themes`). A theme
    /// extension can ship several (e.g. Dracula → "Dracula", "Dracula Soft", …).
    pub themes: Vec<ThemeDef>,
    pub grammar_paths: Vec<PathBuf>,
    pub icon_path: Option<PathBuf>, // raster icon shipped by the extension, if any
    pub readme_path: Option<PathBuf>, // README.md shipped by the extension, if any
    pub changelog_path: Option<PathBuf>, // CHANGELOG.md shipped by the extension, if any
    pub installed: bool, // user clicked Install in Nova this session
}

impl Extension {
    pub fn supported(&self) -> bool {
        self.kind != ExtKind::Code
    }
    pub fn category(&self) -> &'static str {
        match self.kind {
            ExtKind::Theme => "Color Theme",
            ExtKind::Grammar => "Syntax",
            ExtKind::Declarative => "Language",
            ExtKind::Code => "Code (needs runtime)",
        }
    }
}

/// Nova's own extension store, `~/.nova/extensions`. Distinct from VS Code's so
/// the Extensions view + Installed/Install state reflect only what Nova installed,
/// not whatever the user's separate VS Code install happens to have on disk.
/// `None` until something is installed (used by the sidebar scan).
pub(crate) fn extensions_dir() -> Option<PathBuf> {
    let home = std::env::var_os("USERPROFILE").or_else(|| std::env::var_os("HOME"))?;
    let dir = PathBuf::from(home).join(".nova").join("extensions");
    dir.is_dir().then_some(dir)
}

/// The Nova extensions directory, creating it if needed (for installs).
pub fn dir() -> Option<PathBuf> {
    let home = std::env::var_os("USERPROFILE").or_else(|| std::env::var_os("HOME"))?;
    let dir = PathBuf::from(home).join(".nova").join("extensions");
    std::fs::create_dir_all(&dir).ok()?;
    Some(dir)
}

/// Remove an installed extension from Nova's store by its marketplace `slug`
/// (`namespace.name`). Deletes every `<slug>-<version>/` folder for it. A running
/// language server already loaded from the folder keeps going until the next launch.
pub fn uninstall(slug: &str) -> std::io::Result<()> {
    let Some(dir) = extensions_dir() else { return Ok(()) };
    let slug = slug.to_lowercase();
    for entry in std::fs::read_dir(&dir)?.flatten() {
        let p = entry.path();
        if !p.is_dir() {
            continue;
        }
        if let Some(name) = p.file_name().and_then(|n| n.to_str()) {
            let lower = name.to_lowercase();
            if lower == slug || lower.starts_with(&format!("{slug}-")) {
                std::fs::remove_dir_all(&p)?;
            }
        }
    }
    Ok(())
}

/// Scan installed VSCode extensions; supported ones sorted first, then by name.
/// Parse a version like "1.10.2" into comparable numeric components (non-numeric
/// parts → 0), so "1.10.0" > "1.9.0".
fn version_tuple(v: &str) -> Vec<u32> {
    v.split('.').map(|p| p.trim().parse().unwrap_or(0)).collect()
}

pub fn scan() -> Vec<Extension> {
    let Some(dir) = extensions_dir() else {
        return Vec::new();
    };
    let mut out = Vec::new();
    let Ok(entries) = std::fs::read_dir(&dir) else {
        return out;
    };
    for entry in entries.flatten() {
        let ext_dir = entry.path();
        if !ext_dir.is_dir() {
            continue;
        }
        let pkg = ext_dir.join("package.json");
        let Ok(txt) = std::fs::read_to_string(&pkg) else {
            continue;
        };
        let Ok(v) = serde_json::from_str::<Value>(&txt) else {
            continue;
        };
        if let Some(e) = parse(&v, &ext_dir) {
            out.push(e);
        }
    }
    // VS Code keeps every installed *version* of an extension in its own folder
    // (e.g. claude-code-1.0.1, claude-code-1.0.2); collapse to the highest version
    // per (publisher, name) so each extension shows once.
    let mut best: std::collections::HashMap<(String, String), Extension> = std::collections::HashMap::new();
    for e in out {
        let key = (e.publisher.to_lowercase(), e.name.to_lowercase());
        match best.get(&key) {
            Some(prev) if version_tuple(&prev.version) >= version_tuple(&e.version) => {}
            _ => {
                best.insert(key, e);
            }
        }
    }
    let mut out: Vec<Extension> = best.into_values().collect();
    out.sort_by(|a, b| {
        b.supported()
            .cmp(&a.supported())
            .then_with(|| a.name.to_lowercase().cmp(&b.name.to_lowercase()))
    });
    out
}

fn parse(v: &Value, ext_dir: &std::path::Path) -> Option<Extension> {
    let raw_display = v["displayName"].as_str().unwrap_or("");
    let raw_name = v["name"].as_str()?;
    // displayName can be an nls placeholder like "%ext.name%"; fall back to name.
    let name = if raw_display.is_empty() || raw_display.starts_with('%') {
        raw_name.to_string()
    } else {
        raw_display.to_string()
    };
    let publisher = v["publisher"].as_str().unwrap_or("").to_string();
    // Only raster icons are renderable (we have no SVG rasterizer yet).
    let icon_path = v["icon"].as_str().and_then(|p| {
        let lower = p.to_lowercase();
        (lower.ends_with(".png") || lower.ends_with(".jpg") || lower.ends_with(".jpeg"))
            .then(|| ext_dir.join(p.trim_start_matches("./")))
    });
    let description = {
        let d = v["description"].as_str().unwrap_or("");
        if d.starts_with('%') { String::new() } else { d.to_string() }
    };
    let version = v["version"].as_str().unwrap_or("").to_string();
    // Nova installs to `<namespace>.<name>-<version>/`; recover the marketplace id
    // (`namespace.name`) by stripping the trailing version from the folder name.
    let slug = ext_dir
        .file_name()
        .and_then(|n| n.to_str())
        .map(|n| n.trim_end_matches(&format!("-{version}")).to_lowercase())
        .unwrap_or_else(|| format!("{publisher}.{raw_name}").to_lowercase());
    // README shipped alongside package.json (case-insensitive common names).
    let readme_path = ["README.md", "readme.md", "README.MD", "Readme.md"]
        .iter()
        .map(|n| ext_dir.join(n))
        .find(|p| p.is_file());
    let changelog_path = ["CHANGELOG.md", "changelog.md", "CHANGELOG.MD", "Changelog.md"]
        .iter()
        .map(|n| ext_dir.join(n))
        .find(|p| p.is_file());

    let contributes = &v["contributes"];
    let themes = contributes.get("themes").and_then(|t| t.as_array());
    let has_snippets = contributes.get("snippets").is_some();
    let has_languages = contributes.get("languages").is_some();
    let has_main = v.get("main").is_some() || v.get("browser").is_some();
    let grammar_paths: Vec<PathBuf> = contributes
        .get("grammars")
        .and_then(|g| g.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|x| x.get("path").and_then(|p| p.as_str()))
                .map(|p| ext_dir.join(p.trim_start_matches("./")))
                .collect()
        })
        .unwrap_or_default();

    // All contributed color themes (label + light/dark + JSON path). A theme entry's
    // `label` falls back to the file stem; `uiTheme` decides light vs dark.
    let theme_defs: Vec<ThemeDef> = themes
        .map(|arr| {
            arr.iter()
                .filter_map(|t| {
                    let rel = t.get("path").and_then(|p| p.as_str())?;
                    let path = ext_dir.join(rel.trim_start_matches("./"));
                    let label = t
                        .get("label")
                        .and_then(|l| l.as_str())
                        .map(|s| s.to_string())
                        .unwrap_or_else(|| {
                            path.file_stem().and_then(|s| s.to_str()).unwrap_or("Theme").to_string()
                        });
                    let ui = t.get("uiTheme").and_then(|u| u.as_str()).unwrap_or("vs-dark");
                    let dark = !(ui == "vs" || ui == "hc-light");
                    Some(ThemeDef { label, dark, path })
                })
                .collect()
        })
        .unwrap_or_default();

    // Themes and TextMate grammars are usable without JS (we parse/run them
    // natively), so they're installable even if the package also ships code.
    // Snippet/language-only packs are a supported class; anything else with a JS
    // entry needs the (not-yet-built) extension runtime, so it's unsupported.
    let kind = if !theme_defs.is_empty() {
        ExtKind::Theme
    } else if !grammar_paths.is_empty() {
        ExtKind::Grammar
    } else if has_main {
        ExtKind::Code
    } else if has_snippets || has_languages {
        ExtKind::Declarative
    } else {
        ExtKind::Code
    };

    Some(Extension {
        name,
        slug,
        publisher,
        description,
        version,
        kind,
        themes: theme_defs,
        grammar_paths,
        icon_path,
        readme_path,
        changelog_path,
        installed: false,
    })
}
