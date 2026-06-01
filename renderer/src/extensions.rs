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
                installed: e.installed,
                remote: false,
                key: e.name.clone(),
            })
        }
        OpenExt::Remote(i) => {
            let e = remote.get(i)?;
            let name = if e.display.is_empty() { e.name.clone() } else { e.display.clone() };
            Some(OpenExtView {
                name,
                publisher: e.namespace.clone(),
                description: e.description.clone(),
                category: "Marketplace".to_string(),
                version: e.version.clone(),
                downloads: e.downloads,
                rating: e.rating,
                supported: true,
                installed: false,
                remote: true,
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

pub struct Extension {
    pub name: String,
    pub publisher: String,
    pub description: String,
    pub version: String,
    pub kind: ExtKind,
    pub theme_path: Option<PathBuf>,
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

pub(crate) fn extensions_dir() -> Option<PathBuf> {
    let home = std::env::var_os("USERPROFILE").or_else(|| std::env::var_os("HOME"))?;
    let dir = PathBuf::from(home).join(".vscode").join("extensions");
    dir.is_dir().then_some(dir)
}

/// The extensions directory path, creating it if needed (for installs).
pub fn dir() -> Option<PathBuf> {
    let home = std::env::var_os("USERPROFILE").or_else(|| std::env::var_os("HOME"))?;
    let dir = PathBuf::from(home).join(".vscode").join("extensions");
    std::fs::create_dir_all(&dir).ok()?;
    Some(dir)
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

    // Themes and TextMate grammars are usable without JS (we parse/run them
    // natively), so they're installable even if the package also ships code.
    // Snippet/language-only packs are a supported class; anything else with a JS
    // entry needs the (not-yet-built) extension runtime, so it's unsupported.
    let (kind, theme_path) = if let Some(themes) = themes {
        let path = themes
            .iter()
            .find_map(|t| t.get("path").and_then(|p| p.as_str()))
            .map(|p| ext_dir.join(p.trim_start_matches("./")));
        (ExtKind::Theme, path)
    } else if !grammar_paths.is_empty() {
        (ExtKind::Grammar, None)
    } else if has_main {
        (ExtKind::Code, None)
    } else if has_snippets || has_languages {
        (ExtKind::Declarative, None)
    } else {
        (ExtKind::Code, None)
    };

    Some(Extension {
        name,
        publisher,
        description,
        version,
        kind,
        theme_path,
        grammar_paths,
        icon_path,
        readme_path,
        changelog_path,
        installed: false,
    })
}
