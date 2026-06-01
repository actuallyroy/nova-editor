// OpenVSX marketplace client. Runs on a background thread (blocking HTTP via
// ureq) so the UI never stalls; results are sent back over a channel and polled
// from the event loop. Search + .vsix download/extract live here. Kept behind a
// small RemoteExt type so a different registry could be swapped in later.

use std::io::Read;
use std::path::{Path, PathBuf};
use std::sync::mpsc::Sender;

use serde_json::Value;

const BASE: &str = "https://open-vsx.org";

/// Messages from marketplace worker threads, polled in the event loop's idle tick
/// so the UI never blocks on the network.
pub enum WorkerMsg {
    Search { gen: u64, results: Vec<RemoteExt> },
    Installed { result: Result<(), String> },
    Readme { gen: u64, text: Option<String> },
    Changelog { gen: u64, text: Option<String> },
    Image { key: String, frames: Vec<crate::media::DecodedFrame> },
    // Find-in-files (see search.rs): streamed batches of matches + a completion marker.
    SearchHits { gen: u64, files: Vec<crate::search::FileMatches> },
    SearchDone { gen: u64 },
    // Auto-update (see update.rs): a newer release is available / a manual check
    // found none / an install finished.
    UpdateAvailable { version: String },
    UpdateNone,
    UpdateDone { ok: bool },
    // Feedback form: screenshot uploaded (if any) + GitHub issue created — Ok(url)
    // on success, Err(message) otherwise.
    FeedbackDone { result: Result<String, String> },
    // ---- Language server (see lsp.rs) ----
    LspInitialized,                                              // initialize response arrived
    LspDiagnostics { uri: String, diags: Vec<crate::lsp::Diagnostic> }, // push publishDiagnostics
    LspDiagnosticReport { id: i64, diags: Vec<crate::lsp::Diagnostic> }, // pull diagnostic response
    LspLog { server: &'static str, message: String },           // server log / stderr line
    LspExited { server: &'static str },                         // server process ended
}

/// Where a README image comes from: a remote URL or a local file path.
pub enum ImgSource {
    Http(String),
    File(std::path::PathBuf),
}

/// Run a search on a background thread and send the results back over `tx`. The
/// `gen` lets the receiver discard stale responses from superseded queries.
pub fn search_async(tx: Sender<WorkerMsg>, query: String, gen: u64) {
    std::thread::spawn(move || {
        let results = search(&query, 25);
        let _ = tx.send(WorkerMsg::Search { gen, results });
    });
}

/// Download + install an extension on a background thread, reporting back over `tx`.
pub fn install_async(tx: Sender<WorkerMsg>, ext: RemoteExt, root: PathBuf) {
    std::thread::spawn(move || {
        let result = install(&ext, &root).map(|_| ());
        let _ = tx.send(WorkerMsg::Installed { result });
    });
}

/// Fetch a README over HTTP on a background thread; `gen` discards stale fetches.
/// Returns the raw markdown — the Markdown widget does the rendering.
pub fn readme_async(tx: Sender<WorkerMsg>, url: String, gen: u64) {
    std::thread::spawn(move || {
        let text = get_string(&url);
        let _ = tx.send(WorkerMsg::Readme { gen, text });
    });
}

/// Fetch a CHANGELOG over HTTP on a background thread; `gen` discards stale fetches.
pub fn changelog_async(tx: Sender<WorkerMsg>, url: String, gen: u64) {
    std::thread::spawn(move || {
        let text = get_string(&url);
        let _ = tx.send(WorkerMsg::Changelog { gen, text });
    });
}

/// Fetch + DECODE a README image on a background thread (so a big animated GIF
/// never blocks the UI), then ship the frames to the main thread for cheap upload.
/// `key` is the markdown's raw image reference (used to match it back).
pub fn image_async(tx: Sender<WorkerMsg>, key: String, src: ImgSource) {
    std::thread::spawn(move || {
        let bytes = match src {
            ImgSource::Http(url) => get_bytes(&url, 32 * 1024 * 1024),
            ImgSource::File(path) => std::fs::read(&path).ok(),
        };
        let frames = bytes.map(|b| crate::media::decode(&b)).unwrap_or_default();
        let _ = tx.send(WorkerMsg::Image { key, frames });
    });
}

/// One marketplace search result (icon bytes fetched eagerly, best-effort).
#[derive(Clone)]
pub struct RemoteExt {
    pub name: String,
    pub namespace: String, // publisher
    pub display: String,
    pub description: String,
    pub version: String,
    pub downloads: u64,
    pub rating: f32, // average rating 0..5 (0 = unrated)
    pub download_url: Option<String>,
    pub readme_url: Option<String>,
    pub changelog_url: Option<String>,
    pub icon: Option<Vec<u8>>, // raw PNG/JPG bytes, decoded on the GPU thread
}

impl RemoteExt {
    /// Stable atlas key / install id: `namespace.name`.
    pub fn id(&self) -> String {
        format!("{}.{}", self.namespace, self.name)
    }
}

fn get_string(url: &str) -> Option<String> {
    ureq::get(url).call().ok()?.into_string().ok()
}

fn get_bytes(url: &str, max: usize) -> Option<Vec<u8>> {
    let resp = ureq::get(url).call().ok()?;
    let mut buf = Vec::new();
    resp.into_reader().take(max as u64).read_to_end(&mut buf).ok()?;
    (!buf.is_empty()).then_some(buf)
}

/// Search OpenVSX. Fetches up to `size` results and eagerly downloads raster
/// icons for the first several (skipping SVG, which we can't rasterize yet).
pub fn search(query: &str, size: usize) -> Vec<RemoteExt> {
    let url = format!(
        "{BASE}/api/-/search?query={}&size={}&offset=0&includeAllVersions=false",
        urlencode(query),
        size
    );
    let Some(body) = get_string(&url) else {
        return Vec::new();
    };
    let Ok(v) = serde_json::from_str::<Value>(&body) else {
        return Vec::new();
    };
    let mut out = Vec::new();
    if let Some(arr) = v["extensions"].as_array() {
        for (i, e) in arr.iter().enumerate() {
            let name = e["name"].as_str().unwrap_or("").to_string();
            let namespace = e["namespace"].as_str().unwrap_or("").to_string();
            if name.is_empty() || namespace.is_empty() {
                continue;
            }
            let files = &e["files"];
            let icon_url = files.get("icon").and_then(|u| u.as_str());
            // Only fetch a handful of raster icons to keep search snappy.
            let icon = icon_url
                .filter(|u| {
                    let l = u.to_lowercase();
                    i < 12 && (l.ends_with(".png") || l.ends_with(".jpg") || l.ends_with(".jpeg"))
                })
                .and_then(|u| get_bytes(u, 512 * 1024));
            out.push(RemoteExt {
                name,
                namespace,
                display: e["displayName"].as_str().unwrap_or("").to_string(),
                description: e["description"].as_str().unwrap_or("").to_string(),
                version: e["version"].as_str().unwrap_or("").to_string(),
                downloads: e["downloadCount"].as_u64().unwrap_or(0),
                rating: e["averageRating"].as_f64().unwrap_or(0.0) as f32,
                download_url: files.get("download").and_then(|u| u.as_str()).map(String::from),
                readme_url: files.get("readme").and_then(|u| u.as_str()).map(String::from),
                changelog_url: files.get("changelog").and_then(|u| u.as_str()).map(String::from),
                icon,
            });
        }
    }
    out
}

/// Download a `.vsix` (a zip) and extract its `extension/` payload into
/// `~/.vscode/extensions/<namespace>.<name>-<version>/`. Returns the new dir.
pub fn install(ext: &RemoteExt, ext_root: &Path) -> Result<PathBuf, String> {
    let url = ext.download_url.as_ref().ok_or("no download url")?;
    let resp = ureq::get(url).call().map_err(|e| e.to_string())?;
    let mut bytes = Vec::new();
    resp.into_reader()
        .take(64 * 1024 * 1024)
        .read_to_end(&mut bytes)
        .map_err(|e| e.to_string())?;

    let dest = ext_root.join(format!("{}.{}-{}", ext.namespace, ext.name, ext.version));
    let reader = std::io::Cursor::new(bytes);
    let mut zip = zip::ZipArchive::new(reader).map_err(|e| e.to_string())?;
    for i in 0..zip.len() {
        let mut file = zip.by_index(i).map_err(|e| e.to_string())?;
        let Some(enclosed) = file.enclosed_name() else {
            continue;
        };
        // VSIX wraps the package under `extension/`; strip that prefix.
        let Ok(rel) = enclosed.strip_prefix("extension") else {
            continue;
        };
        if rel.as_os_str().is_empty() {
            continue;
        }
        let out_path = dest.join(rel);
        if file.is_dir() {
            std::fs::create_dir_all(&out_path).map_err(|e| e.to_string())?;
        } else {
            if let Some(parent) = out_path.parent() {
                std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
            }
            let mut out = std::fs::File::create(&out_path).map_err(|e| e.to_string())?;
            std::io::copy(&mut file, &mut out).map_err(|e| e.to_string())?;
        }
    }
    Ok(dest)
}

/// Resolve a (possibly relative) URL `rel` against a document `base` URL.
/// Absolute `rel` is returned as-is; otherwise it's joined to base's directory.
pub fn join_url(base: &str, rel: &str) -> Option<String> {
    if rel.starts_with("http://") || rel.starts_with("https://") {
        return Some(rel.to_string());
    }
    let cut = base.rfind('/')?;
    Some(format!("{}/{}", &base[..cut], rel.trim_start_matches("./")))
}

/// Minimal percent-encoding for the query string (encode anything non-alnum).
fn urlencode(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for b in s.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => out.push(b as char),
            _ => out.push_str(&format!("%{:02X}", b)),
        }
    }
    out
}
