// A minimal Language Server Protocol (LSP) client. Aether speaks LSP to a language
// server it launches as a child process (stdio, Content-Length framed JSON-RPC),
// so language-server-backed VS Code extensions (ESLint first) work without a
// Node extension host. No async runtime — blocking I/O on dedicated threads, with
// results posted to the UI over the existing `WorkerMsg` channel.
//
// Threading: every outbound write goes through one `outgoing` channel drained by a
// writer thread, so both the main thread (requests/notifications) and the reader
// thread (replies to server→client requests like `workspace/configuration`) can
// write without sharing the pipe. The reader thread parses server traffic and
// either replies itself or forwards to the UI as `WorkerMsg::Lsp*`.

use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::mpsc::Sender;

use serde_json::{json, Value};

use crate::marketplace::WorkerMsg;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Severity {
    Error,
    Warning,
    Info,
    Hint,
}

impl Severity {
    fn from_lsp(n: i64) -> Severity {
        match n {
            1 => Severity::Error,
            2 => Severity::Warning,
            3 => Severity::Info,
            _ => Severity::Hint,
        }
    }
}

/// One diagnostic, with LSP (line, UTF-16 character) positions. The editor maps
/// these to byte ranges per document when rendering.
#[derive(Clone, Debug)]
pub struct Diagnostic {
    pub start_line: u32,
    pub start_char: u32, // UTF-16 code units within the line
    pub end_line: u32,
    pub end_char: u32,
    pub severity: Severity,
    pub message: String,
    pub source: Option<String>,
    pub code: Option<String>,      // rule id, e.g. "no-unused-vars"
    pub code_href: Option<String>, // codeDescription.href — docs URL for the rule
}

/// Aggregated info for the diagnostic(s) under the pointer, used by the hover card.
#[derive(Clone, PartialEq)]
pub struct DiagHover {
    pub message: String,          // joined messages of all diagnostics at the point
    pub source: Option<String>,   // e.g. "eslint"
    pub code: Option<String>,     // rule id, e.g. "no-unused-vars"
    pub href: Option<String>,     // docs URL for the rule, if the server provided one
}

// ---- URI helpers ----

/// `file://` URI for a path (good enough for local files on win/mac/linux).
pub fn path_to_uri(path: &Path) -> String {
    let s = path.to_string_lossy().replace('\\', "/");
    if cfg!(windows) {
        // file:///C:/foo
        format!("file:///{}", s.trim_start_matches('/'))
    } else {
        format!("file://{s}")
    }
}

/// Compare two `file://` URIs for the same file, tolerating the normalization a
/// language server applies (drive-letter case on Windows, `%3A`/`%20` encoding,
/// slash direction). Without this, ESLint's `file:///e:/…` won't match Aether's
/// `file:///E:/…` and diagnostics get silently dropped.
pub fn same_uri(a: &str, b: &str) -> bool {
    fn norm(u: &str) -> String {
        let s = u.trim_start_matches("file://").trim_start_matches('/');
        let s = s.replace("%3A", ":").replace("%3a", ":").replace("%20", " ").replace('\\', "/");
        if cfg!(windows) {
            s.to_ascii_lowercase()
        } else {
            s
        }
    }
    norm(a) == norm(b)
}

pub fn uri_to_path(uri: &str) -> Option<PathBuf> {
    let rest = uri.strip_prefix("file://")?;
    let rest = if cfg!(windows) { rest.trim_start_matches('/') } else { rest };
    // Minimal percent-decode for spaces.
    Some(PathBuf::from(rest.replace("%20", " ")))
}

// ---- Language id from file extension ----

pub fn language_id(ext: &str) -> Option<&'static str> {
    Some(match ext.to_ascii_lowercase().as_str() {
        "js" | "cjs" | "mjs" => "javascript",
        "jsx" => "javascriptreact",
        "ts" => "typescript",
        "tsx" => "typescriptreact",
        "vue" => "vue",
        "json" | "jsonc" => "json",
        "rs" => "rust",
        _ => return None,
    })
}

// ---- Server registry (data-driven) ----
//
// Everything below the registry is generic LSP. A server is fully described by this
// struct — adding rust-analyzer / pyright / gopls / clangd is a new `ServerSpec`
// entry, no new code paths. `resolve` finds the executable + args (None ⇒ skip),
// `init_options`/`config_reply` carry any server-specific protocol bits, and the
// `pull_*` flags say which features Aether requests (diagnostics / semantic tokens).

/// A navigation-request flavor (Go to Definition family / references / symbols).
/// One id→kind map routes all their responses, since the wire shape is shared.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum LocKind {
    Definition,
    Declaration,
    TypeDefinition,
    Implementation,
    References,
    WorkspaceSymbol,
}

impl LocKind {
    pub fn method(self) -> &'static str {
        match self {
            LocKind::Definition => "textDocument/definition",
            LocKind::Declaration => "textDocument/declaration",
            LocKind::TypeDefinition => "textDocument/typeDefinition",
            LocKind::Implementation => "textDocument/implementation",
            LocKind::References => "textDocument/references",
            LocKind::WorkspaceSymbol => "workspace/symbol",
        }
    }
    pub fn label(self) -> &'static str {
        match self {
            LocKind::Definition => "definition",
            LocKind::Declaration => "declaration",
            LocKind::TypeDefinition => "type definition",
            LocKind::Implementation => "implementation",
            LocKind::References => "references",
            LocKind::WorkspaceSymbol => "symbol",
        }
    }
}

/// One resolved target from a location-shaped response (Location, LocationLink,
/// or SymbolInformation).
#[derive(Clone, Debug)]
pub struct LspLocation {
    pub uri: String,
    pub line: u32,      // 0-based
    pub character: u32, // UTF-16 col
    pub name: Option<String>, // symbol name (workspace/symbol results)
}

/// Parse a location-shaped response: a single Location, Location[],
/// LocationLink[], or SymbolInformation[]/WorkspaceSymbol[]. Null → empty.
pub fn parse_locations(r: &Value) -> Vec<LspLocation> {
    fn one(v: &Value) -> Option<LspLocation> {
        // SymbolInformation / WorkspaceSymbol: { name, location: {uri, range} }.
        if let Some(loc) = v.get("location") {
            let mut l = one(loc)?;
            l.name = v.get("name").and_then(|n| n.as_str()).map(String::from);
            return Some(l);
        }
        // LocationLink: prefer the precise selection range.
        let (uri, range) = if let Some(u) = v.get("targetUri") {
            (u, v.get("targetSelectionRange").or(v.get("targetRange")))
        } else {
            (v.get("uri")?, v.get("range"))
        };
        let start = range?.get("start")?;
        Some(LspLocation {
            uri: uri.as_str()?.to_string(),
            line: start.get("line")?.as_u64()? as u32,
            character: start.get("character").and_then(|c| c.as_u64()).unwrap_or(0) as u32,
            name: None,
        })
    }
    match r {
        Value::Array(a) => a.iter().filter_map(one).collect(),
        Value::Object(_) => one(r).into_iter().collect(),
        _ => Vec::new(),
    }
}

/// One LSP TextEdit: replace `range` with `new_text`. Positions are
/// (line, UTF-16 col), like diagnostics.
#[derive(Clone, Debug)]
pub struct TextEdit {
    pub start_line: u32,
    pub start_char: u32,
    pub end_line: u32,
    pub end_char: u32,
    pub new_text: String,
}

/// Parse a TextEdit[] response (formatting) — elements: { range, newText }.
pub fn parse_text_edits(r: &Value) -> Vec<TextEdit> {
    fn one(v: &Value) -> Option<TextEdit> {
        let range = v.get("range")?;
        let (s, e) = (range.get("start")?, range.get("end")?);
        Some(TextEdit {
            start_line: s.get("line")?.as_u64()? as u32,
            start_char: s.get("character")?.as_u64()? as u32,
            end_line: e.get("line")?.as_u64()? as u32,
            end_char: e.get("character")?.as_u64()? as u32,
            new_text: v.get("newText")?.as_str()?.to_string(),
        })
    }
    r.as_array().map_or(Vec::new(), |a| a.iter().filter_map(one).collect())
}

/// Parse a WorkspaceEdit (rename response) into per-uri edit lists. Handles both
/// the `changes` map and the newer `documentChanges` array (text edits only —
/// create/rename/delete file ops are skipped).
pub fn parse_workspace_edit(r: &Value) -> Vec<(String, Vec<TextEdit>)> {
    let mut out: Vec<(String, Vec<TextEdit>)> = Vec::new();
    if let Some(changes) = r.get("changes").and_then(|c| c.as_object()) {
        for (uri, edits) in changes {
            out.push((uri.clone(), parse_text_edits(edits)));
        }
    }
    if let Some(dc) = r.get("documentChanges").and_then(|c| c.as_array()) {
        for change in dc {
            let Some(uri) = change
                .get("textDocument")
                .and_then(|t| t.get("uri"))
                .and_then(|u| u.as_str())
            else {
                continue; // a create/rename/delete file operation — not applied
            };
            let edits = change.get("edits").map_or(Vec::new(), parse_text_edits);
            out.push((uri.to_string(), edits));
        }
    }
    out
}

/// Shape check for location-flavored responses (vs completion lists, which are
/// also arrays): single Location object, null, or an array whose first element
/// carries `uri` / `targetUri` / `location`.
fn looks_like_locations(r: &Value) -> bool {
    match r {
        Value::Null => true,
        Value::Object(_) => r.get("uri").is_some() && r.get("range").is_some(),
        Value::Array(a) => match a.first() {
            None => false, // ambiguous — let completion take empty arrays
            Some(e) => {
                e.get("uri").is_some() || e.get("targetUri").is_some() || e.get("location").is_some()
            }
        },
        _ => false,
    }
}

pub struct ServerSpec {
    pub name: &'static str,
    pub languages: &'static [&'static str],
    /// (program, args) to launch, given the installed-extension roots. None ⇒ not available.
    pub resolve: fn(ext_roots: &[PathBuf]) -> Option<(String, Vec<String>)>,
    pub init_options: fn(root: &Path) -> Value,
    pub config_reply: fn() -> Value,
    /// Request diagnostics via `textDocument/diagnostic` (pull model, e.g. ESLint).
    pub pull_diagnostics: bool,
    /// Accept this server's `publishDiagnostics` (push model, e.g. rust-analyzer).
    /// Servers without either flag have their diagnostics ignored — avoids the TS
    /// server's (empty) push clobbering ESLint's pulled diagnostics on JS/TS.
    pub push_diagnostics: bool,
    pub pull_semantic: bool,
    /// Serve `textDocument/completion` from this server (rust-analyzer, tsserver, …).
    pub completion: bool,
}

/// Whether a server's pushed diagnostics should be applied (looked up by name).
pub fn server_accepts_push(name: &str) -> bool {
    registry().iter().any(|s| s.name == name && s.push_diagnostics)
}

fn resolve_eslint(ext_roots: &[PathBuf]) -> Option<(String, Vec<String>)> {
    let node = resolve_node()?;
    let server = eslint_server_path(ext_roots)?;
    Some((node, vec![server.to_string_lossy().into_owned(), "--stdio".to_string()]))
}

fn resolve_typescript(_ext_roots: &[PathBuf]) -> Option<(String, Vec<String>)> {
    let node = resolve_node()?;
    let cli = typescript_ls_cli()?;
    Some((node, vec![cli.to_string_lossy().into_owned(), "--stdio".to_string()]))
}

fn resolve_rust_analyzer(_ext_roots: &[PathBuf]) -> Option<(String, Vec<String>)> {
    // A standalone binary that speaks LSP over stdio with no args.
    rust_analyzer_bin().map(|p| (p, Vec::new()))
}

pub const ESLINT: ServerSpec = ServerSpec {
    name: "eslint",
    languages: &["javascript", "javascriptreact", "typescript", "typescriptreact", "vue"],
    resolve: resolve_eslint,
    init_options: eslint_init_options,
    config_reply: eslint_config_reply,
    pull_diagnostics: true,
    push_diagnostics: false,
    pull_semantic: false,
    completion: false, // ESLint is a linter — no completion
};

pub const TYPESCRIPT: ServerSpec = ServerSpec {
    name: "typescript",
    languages: &["javascript", "javascriptreact", "typescript", "typescriptreact"],
    resolve: resolve_typescript,
    init_options: ts_init_options,
    config_reply: empty_config,
    pull_diagnostics: false, // ESLint owns JS/TS diagnostics for now (avoid clobber)
    push_diagnostics: false, // ignore TS push diagnostics so they don't clobber ESLint
    pull_semantic: true,
    completion: true,
};

pub const RUST_ANALYZER: ServerSpec = ServerSpec {
    name: "rust-analyzer",
    languages: &["rust"],
    resolve: resolve_rust_analyzer,
    init_options: rust_init,
    config_reply: empty_config,
    // Both models, deliberately: we declare the LSP 3.17 diagnostic-pull capability,
    // so rust-analyzer serves its NATIVE diagnostics (live, in-memory — unresolved
    // names, type errors) via pull only and stops pushing them. Pulling on every
    // debounced change keeps those live while typing; flycheck (`cargo check`, the
    // full compiler) still arrives via push after each save.
    pull_diagnostics: true,
    push_diagnostics: true,
    pull_semantic: true,
    completion: true,
};

fn empty_init(_root: &Path) -> Value {
    json!({})
}

fn rust_init(_root: &Path) -> Value {
    // Dedicated check dir (`target/rust-analyzer`): the server's `cargo check`
    // otherwise shares the target lock with the user's own builds and can sit
    // blocked for minutes before the first diagnostics appear.
    json!({ "cargo": { "targetDir": true } })
}
fn empty_config() -> Value {
    json!({})
}

/// All registered servers. Extend this list to support more — generic from here.
pub fn registry() -> &'static [ServerSpec] {
    &[ESLINT, TYPESCRIPT, RUST_ANALYZER]
}

/// True if any registered server serves this language id.
pub fn server_for_language(lang: &str) -> bool {
    registry().iter().any(|s| s.languages.contains(&lang))
}

// ---- Node + ESLint server resolution ----

/// Spawn a child process without flashing a console window on Windows
/// (CREATE_NO_WINDOW). A no-op on other platforms. ALL process spawns in this module
/// must go through this — otherwise a probe like `node --version`, run every sync,
/// pops a console window each time ("dancing terminals").
fn quiet_command(program: &str) -> Command {
    let mut c = Command::new(program);
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        c.creation_flags(0x0800_0000); // CREATE_NO_WINDOW
    }
    c
}

/// Resolve a `node` executable, cached for the session: PATH, then common install
/// dirs (incl. a macOS login-shell lookup so GUI launches see nvm/homebrew node).
/// Cached because `ensure()` calls it every sync while a JS/TS server isn't running,
/// and re-spawning a probe each tick is both wasteful and (pre-fix) window-flashing.
pub fn resolve_node() -> Option<String> {
    static CACHE: std::sync::OnceLock<Option<String>> = std::sync::OnceLock::new();
    CACHE.get_or_init(resolve_node_uncached).clone()
}

fn resolve_node_uncached() -> Option<String> {
    #[cfg(windows)]
    let names = ["node.exe", "node"];
    #[cfg(not(windows))]
    let names = ["node"];
    // Direct PATH probe.
    for n in names {
        if quiet_command(n).arg("--version").stdout(Stdio::null()).stderr(Stdio::null()).status().is_ok() {
            return Some(n.to_string());
        }
    }
    #[cfg(not(windows))]
    {
        for p in ["/opt/homebrew/bin/node", "/usr/local/bin/node", "/usr/bin/node"] {
            if Path::new(p).exists() {
                return Some(p.to_string());
            }
        }
        // Login shell (picks up nvm / fnm / asdf shims for GUI-launched apps).
        if let Ok(out) = quiet_command("/bin/sh").args(["-lc", "command -v node"]).output() {
            let path = String::from_utf8_lossy(&out.stdout).trim().to_string();
            if !path.is_empty() && Path::new(&path).exists() {
                return Some(path);
            }
        }
    }
    None
}

/// Locate the globally-installed `typescript-language-server` CLI entry
/// (`lib/cli.mjs`), launched with node. Probes the standard npm-global roots.
pub fn typescript_ls_cli() -> Option<PathBuf> {
    let mut roots: Vec<PathBuf> = Vec::new();
    #[cfg(windows)]
    if let Some(appdata) = std::env::var_os("APPDATA") {
        roots.push(PathBuf::from(appdata).join("npm").join("node_modules"));
    }
    #[cfg(not(windows))]
    {
        for p in ["/usr/local/lib/node_modules", "/opt/homebrew/lib/node_modules", "/usr/lib/node_modules"] {
            roots.push(PathBuf::from(p));
        }
        if let Some(home) = std::env::var_os("HOME") {
            roots.push(PathBuf::from(home).join(".npm-global/lib/node_modules"));
        }
    }
    for r in roots {
        let p = r.join("typescript-language-server").join("lib").join("cli.mjs");
        if p.exists() {
            return Some(p);
        }
    }
    None
}

/// `initializationOptions` for typescript-language-server (minimal; it auto-detects
/// tsserver from the workspace or its bundled copy).
pub fn ts_init_options(_root: &Path) -> Value {
    json!({ "hostInfo": "aether", "preferences": {} })
}

/// Resolve the `rust-analyzer` binary: PATH, ~/.cargo/bin, rustup, common dirs.
pub fn rust_analyzer_bin() -> Option<String> {
    static CACHE: std::sync::OnceLock<Option<String>> = std::sync::OnceLock::new();
    CACHE.get_or_init(rust_analyzer_bin_uncached).clone()
}

fn rust_analyzer_bin_uncached() -> Option<String> {
    let probe = |p: &str| {
        quiet_command(p)
            .arg("--version")
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .map(|s| s.success())
            .unwrap_or(false)
    };
    if probe("rust-analyzer") {
        return Some("rust-analyzer".to_string());
    }
    if let Some(home) = std::env::var_os("USERPROFILE").or_else(|| std::env::var_os("HOME")) {
        let cargo = PathBuf::from(&home).join(".cargo").join("bin").join(if cfg!(windows) {
            "rust-analyzer.exe"
        } else {
            "rust-analyzer"
        });
        if cargo.exists() {
            return Some(cargo.to_string_lossy().into_owned());
        }
    }
    #[cfg(not(windows))]
    {
        for p in ["/opt/homebrew/bin/rust-analyzer", "/usr/local/bin/rust-analyzer"] {
            if Path::new(p).exists() {
                return Some(p.to_string());
            }
        }
        if let Ok(out) = quiet_command("/bin/sh").args(["-lc", "command -v rust-analyzer"]).output() {
            let path = String::from_utf8_lossy(&out.stdout).trim().to_string();
            if !path.is_empty() && Path::new(&path).exists() {
                return Some(path);
            }
        }
    }
    None
}

/// Locate the ESLint extension's bundled stdio server (`server/out/eslintServer.js`)
/// among the installed extensions under `ext_dir` roots.
pub fn eslint_server_path(ext_roots: &[PathBuf]) -> Option<PathBuf> {
    for root in ext_roots {
        if let Ok(entries) = std::fs::read_dir(root) {
            for e in entries.flatten() {
                let name = e.file_name().to_string_lossy().to_ascii_lowercase();
                if name.contains("dbaeumer.vscode-eslint") {
                    let p = e.path().join("server").join("out").join("eslintServer.js");
                    if p.exists() {
                        return Some(p);
                    }
                }
            }
        }
    }
    None
}

// ---- The client ----

pub struct LspClient {
    pub server: &'static str,
    pub root: PathBuf,
    outgoing: Sender<Vec<u8>>,
    next_id: i64,
    initialized: bool,
    /// didOpen/didChange queued until the initialize handshake completes.
    pending: Vec<Value>,
    /// Last didChange version sent per URI. Both the completion flush and the
    /// debounced sync can try to send the same version — servers may treat a
    /// repeated version as a protocol error and stop tracking the document, so
    /// duplicates are dropped here.
    sent_versions: std::collections::HashMap<String, i32>,
}

impl LspClient {
    /// Launch `program args…` as an LSP server rooted at `root`. `init_options` is
    /// the server-specific `initializationOptions`. `cfg_reply` builds the reply to
    /// `workspace/configuration` items (ESLint blocks on this before linting).
    pub fn start(
        server: &'static str,
        program: &str,
        args: &[String],
        root: PathBuf,
        init_options: Value,
        cfg_reply: Value,
        tx: Sender<WorkerMsg>,
    ) -> Option<LspClient> {
        let mut cmd = quiet_command(program);
        cmd.args(args)
            .current_dir(&root)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        let mut child = cmd.spawn().ok()?;
        let mut stdin = child.stdin.take()?;
        let stdout = child.stdout.take()?;
        let stderr = child.stderr.take();

        let (out_tx, out_rx) = std::sync::mpsc::channel::<Vec<u8>>();

        // Writer thread: drain outgoing frames to the child's stdin.
        std::thread::spawn(move || {
            while let Ok(buf) = out_rx.recv() {
                if stdin.write_all(&buf).is_err() || stdin.flush().is_err() {
                    break;
                }
            }
        });

        // Drain stderr so the server doesn't block; surface it as a status line.
        if let Some(err) = stderr {
            let tx2 = tx.clone();
            std::thread::spawn(move || {
                let mut r = BufReader::new(err);
                let mut line = String::new();
                while r.read_line(&mut line).map(|n| n > 0).unwrap_or(false) {
                    let msg = line.trim_end().to_string();
                    if !msg.is_empty() {
                        let _ = tx2.send(WorkerMsg::LspLog { server, message: msg });
                    }
                    line.clear();
                }
            });
        }

        // Reader thread: parse server→client messages.
        let reply_tx = out_tx.clone();
        let cfg = cfg_reply;
        std::thread::spawn(move || {
            let mut reader = BufReader::new(stdout);
            loop {
                match read_message(&mut reader) {
                    Some(msg) => handle_server_message(server, &msg, &reply_tx, &cfg, &tx),
                    None => {
                        let _ = tx.send(WorkerMsg::LspExited { server });
                        break;
                    }
                }
            }
        });

        let mut client = LspClient {
            server,
            root: root.clone(),
            outgoing: out_tx,
            next_id: 1,
            initialized: false,
            pending: Vec::new(),
            sent_versions: std::collections::HashMap::new(),
        };
        // Kick off the initialize handshake.
        let id = client.next_id;
        client.next_id += 1;
        let init = json!({
            "jsonrpc": "2.0",
            "id": id,
            "method": "initialize",
            "params": {
                "processId": std::process::id(),
                "rootUri": path_to_uri(&root),
                "rootPath": root.to_string_lossy(),
                "workspaceFolders": [{ "uri": path_to_uri(&root), "name": root.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default() }],
                "initializationOptions": init_options,
                "capabilities": client_capabilities(),
            }
        });
        client.send_raw(init);
        Some(client)
    }

    fn send_raw(&self, msg: Value) {
        let _ = self.outgoing.send(frame(&msg));
    }

    /// Called by App when the initialize *response* arrives (id 1): finish the
    /// handshake and flush any queued didOpen/didChange.
    pub fn on_initialized(&mut self) {
        if self.initialized {
            return;
        }
        self.initialized = true;
        self.send_raw(json!({ "jsonrpc": "2.0", "method": "initialized", "params": {} }));
        for m in std::mem::take(&mut self.pending) {
            self.send_raw(m);
        }
    }

    fn notify(&mut self, method: &str, params: Value) {
        let msg = json!({ "jsonrpc": "2.0", "method": method, "params": params });
        if self.initialized {
            self.send_raw(msg);
        } else {
            self.pending.push(msg);
        }
    }

    pub fn did_open(&mut self, uri: &str, language_id: &str, version: i32, text: &str) {
        self.notify(
            "textDocument/didOpen",
            json!({ "textDocument": { "uri": uri, "languageId": language_id, "version": version, "text": text } }),
        );
    }

    pub fn did_change(&mut self, uri: &str, version: i32, text: &str) {
        // Drop duplicates: a repeated version can make a server stop tracking the doc.
        if self.sent_versions.get(uri) == Some(&version) {
            return;
        }
        self.sent_versions.insert(uri.to_string(), version);
        self.notify(
            "textDocument/didChange",
            json!({ "textDocument": { "uri": uri, "version": version }, "contentChanges": [{ "text": text }] }),
        );
    }

    pub fn did_save(&mut self, uri: &str, text: &str) {
        self.notify("textDocument/didSave", json!({ "textDocument": { "uri": uri }, "text": text }));
    }

    pub fn did_close(&mut self, uri: &str) {
        self.notify("textDocument/didClose", json!({ "textDocument": { "uri": uri } }));
    }

    /// Pull diagnostics for a document (LSP 3.17 `textDocument/diagnostic`). Returns
    /// the request id so the caller can map the response back to this URI.
    pub fn pull_diagnostics(&mut self, uri: &str) -> i64 {
        let id = self.next_id;
        self.next_id += 1;
        let msg = json!({
            "jsonrpc": "2.0",
            "id": id,
            "method": "textDocument/diagnostic",
            "params": { "textDocument": { "uri": uri } }
        });
        if self.initialized {
            self.send_raw(msg);
        } else {
            self.pending.push(msg);
        }
        id
    }

    /// Request full semantic tokens (LSP 3.17 `textDocument/semanticTokens/full`).
    /// Returns the request id so the caller can map the response to this URI.
    pub fn pull_semantic_tokens(&mut self, uri: &str) -> i64 {
        let id = self.next_id;
        self.next_id += 1;
        let msg = json!({
            "jsonrpc": "2.0",
            "id": id,
            "method": "textDocument/semanticTokens/full",
            "params": { "textDocument": { "uri": uri } }
        });
        if self.initialized {
            self.send_raw(msg);
        } else {
            self.pending.push(msg);
        }
        id
    }

    /// Request completions at a position. Returns the request id (map the response).
    pub fn request_completion(&mut self, uri: &str, line: u32, character: u32) -> i64 {
        let id = self.next_id;
        self.next_id += 1;
        let msg = json!({
            "jsonrpc": "2.0",
            "id": id,
            "method": "textDocument/completion",
            "params": {
                "textDocument": { "uri": uri },
                "position": { "line": line, "character": character }
            }
        });
        if self.initialized {
            self.send_raw(msg);
        } else {
            self.pending.push(msg);
        }
        id
    }

    /// Fire a position-based request (`textDocument/definition` family). The
    /// references flavor adds its required `context` param.
    pub fn request_at(&mut self, kind: LocKind, uri: &str, line: u32, character: u32) -> i64 {
        let id = self.next_id;
        self.next_id += 1;
        let mut params = json!({
            "textDocument": { "uri": uri },
            "position": { "line": line, "character": character }
        });
        if kind == LocKind::References {
            params["context"] = json!({ "includeDeclaration": true });
        }
        let msg = json!({ "jsonrpc": "2.0", "id": id, "method": kind.method(), "params": params });
        if self.initialized {
            self.send_raw(msg);
        } else {
            self.pending.push(msg);
        }
        id
    }

    /// Fire a `textDocument/formatting` (whole doc) or `rangeFormatting`
    /// (selection) request. Returns the request id.
    pub fn request_formatting(&mut self, uri: &str, range: Option<(u32, u32, u32, u32)>, tab_size: usize, insert_spaces: bool) -> i64 {
        let id = self.next_id;
        self.next_id += 1;
        let mut params = json!({
            "textDocument": { "uri": uri },
            "options": { "tabSize": tab_size, "insertSpaces": insert_spaces }
        });
        let method = match range {
            Some((sl, sc, el, ec)) => {
                params["range"] = json!({
                    "start": { "line": sl, "character": sc },
                    "end": { "line": el, "character": ec }
                });
                "textDocument/rangeFormatting"
            }
            None => "textDocument/formatting",
        };
        let msg = json!({ "jsonrpc": "2.0", "id": id, "method": method, "params": params });
        if self.initialized {
            self.send_raw(msg);
        } else {
            self.pending.push(msg);
        }
        id
    }

    /// Fire a `textDocument/rename` request; the response is a WorkspaceEdit.
    pub fn request_rename(&mut self, uri: &str, line: u32, character: u32, new_name: &str) -> i64 {
        let id = self.next_id;
        self.next_id += 1;
        let msg = json!({
            "jsonrpc": "2.0",
            "id": id,
            "method": "textDocument/rename",
            "params": {
                "textDocument": { "uri": uri },
                "position": { "line": line, "character": character },
                "newName": new_name
            }
        });
        if self.initialized {
            self.send_raw(msg);
        } else {
            self.pending.push(msg);
        }
        id
    }

    /// Fire a `workspace/symbol` query.
    pub fn request_symbols(&mut self, query: &str) -> i64 {
        let id = self.next_id;
        self.next_id += 1;
        let msg = json!({
            "jsonrpc": "2.0",
            "id": id,
            "method": "workspace/symbol",
            "params": { "query": query }
        });
        if self.initialized {
            self.send_raw(msg);
        } else {
            self.pending.push(msg);
        }
        id
    }

    pub fn shutdown(&mut self) {
        let id = self.next_id;
        self.next_id += 1;
        self.send_raw(json!({ "jsonrpc": "2.0", "id": id, "method": "shutdown" }));
        self.send_raw(json!({ "jsonrpc": "2.0", "method": "exit" }));
    }
}

/// Owns the running language-server clients, the document-sync loop, and routing of
/// server responses back onto documents — so `App` only calls `sync` + the `apply_*`
/// handlers and holds none of this state itself.
#[derive(Default)]
pub struct LspManager {
    clients: Vec<LspClient>,
    last_sync: Option<std::time::Instant>,
    diag_pending: std::collections::HashMap<i64, String>, // diagnostic request id → uri
    sem_pending: std::collections::HashMap<i64, String>,  // semantic-tokens request id → uri
    sem_legend: Vec<String>,                              // semantic token-type names
    comp_pending: Option<i64>,                            // latest completion request id
    loc_pending: std::collections::HashMap<i64, LocKind>, // navigation request id → flavor
    fmt_pending: std::collections::HashMap<i64, String>,  // formatting request id → uri
    rename_pending: std::collections::HashSet<i64>,       // rename request ids
}

impl LspManager {
    pub fn new() -> Self {
        Self::default()
    }

    fn client_mut(&mut self, server: &str) -> Option<&mut LspClient> {
        self.clients.iter_mut().find(|c| c.server == server)
    }

    /// Ensure a registered server is running for `root` (generic, data-driven). No-op
    /// if already up; returns false if the server can't be resolved/spawned.
    fn ensure(&mut self, spec: &'static ServerSpec, root: &Path, ext_roots: &[PathBuf], tx: &Sender<WorkerMsg>) -> bool {
        if self.clients.iter().any(|c| c.server == spec.name) {
            return true;
        }
        let Some((program, args)) = (spec.resolve)(ext_roots) else { return false };
        let client = LspClient::start(
            spec.name,
            &program,
            &args,
            root.to_path_buf(),
            (spec.init_options)(root),
            (spec.config_reply)(),
            tx.clone(),
        );
        match client {
            Some(c) => {
                self.clients.push(c);
                true
            }
            None => false,
        }
    }

    /// Request semantic tokens from a server; returns the request id.
    pub fn pull_semantic_tokens(&mut self, server: &str, uri: &str) -> Option<i64> {
        self.client_mut(server).map(|c| c.pull_semantic_tokens(uri))
    }

    /// Request completions from whatever running server serves `lang` with completion
    /// (generic — rust-analyzer, tsserver, …). Records the id so stale responses drop.
    pub fn request_completion(&mut self, lang: &str, uri: &str, line: u32, character: u32) -> Option<i64> {
        let server = registry().iter().find(|s| s.completion && s.languages.contains(&lang))?.name;
        let id = self.client_mut(server)?.request_completion(uri, line, character);
        self.comp_pending = Some(id);
        Some(id)
    }

    /// True if `id` is the newest completion request (drop superseded responses).
    pub fn is_current_completion(&self, id: i64) -> bool {
        self.comp_pending == Some(id)
    }

    /// Fire a Go-to / references request on whatever running server serves `lang`.
    /// Returns false when no server is up for the language.
    pub fn request_locations(&mut self, lang: &str, uri: &str, line: u32, character: u32, kind: LocKind) -> bool {
        let Some(name) = registry()
            .iter()
            .find(|s| s.languages.contains(&lang) && self.clients.iter().any(|c| c.server == s.name))
            .map(|s| s.name)
        else {
            return false;
        };
        if let Some(c) = self.client_mut(name) {
            let id = c.request_at(kind, uri, line, character);
            self.loc_pending.insert(id, kind);
            return true;
        }
        false
    }

    /// Fire a workspace-symbol query, preferring the server for `lang` but falling
    /// back to any running client (the palette's `#` mode works without a focused
    /// doc of that language).
    pub fn request_workspace_symbols(&mut self, lang: Option<&str>, query: &str) -> bool {
        let preferred = lang.and_then(|l| {
            registry()
                .iter()
                .find(|s| s.languages.contains(&l) && self.clients.iter().any(|c| c.server == s.name))
                .map(|s| s.name)
        });
        let Some(client) = (match preferred {
            Some(name) => self.client_mut(name),
            None => self.clients.first_mut(),
        }) else {
            return false;
        };
        let id = client.request_symbols(query);
        self.loc_pending.insert(id, LocKind::WorkspaceSymbol);
        true
    }

    /// Resolve a location-shaped response to the request flavor it answers (None ⇒
    /// not ours / superseded).
    pub fn take_locations(&mut self, id: i64) -> Option<LocKind> {
        self.loc_pending.remove(&id)
    }

    /// Format the whole document (or just `range`) on whatever running server
    /// serves `lang`. Returns false when no server is up.
    pub fn request_formatting(&mut self, lang: &str, uri: &str, range: Option<(u32, u32, u32, u32)>) -> bool {
        let Some(name) = registry()
            .iter()
            .find(|s| s.languages.contains(&lang) && self.clients.iter().any(|c| c.server == s.name))
            .map(|s| s.name)
        else {
            return false;
        };
        let s = crate::settings::current();
        if let Some(c) = self.client_mut(name) {
            let id = c.request_formatting(uri, range, s.editor_tab_size, s.editor_insert_spaces);
            self.fmt_pending.insert(id, uri.to_string());
            return true;
        }
        false
    }

    /// The uri a formatting response applies to (None ⇒ not a formatting reply).
    pub fn take_formatting(&mut self, id: i64) -> Option<String> {
        self.fmt_pending.remove(&id)
    }

    /// Rename the symbol at the caret across the workspace.
    pub fn request_rename(&mut self, lang: &str, uri: &str, line: u32, character: u32, new_name: &str) -> bool {
        let Some(name) = registry()
            .iter()
            .find(|s| s.languages.contains(&lang) && self.clients.iter().any(|c| c.server == s.name))
            .map(|s| s.name)
        else {
            return false;
        };
        if let Some(c) = self.client_mut(name) {
            let id = c.request_rename(uri, line, character, new_name);
            self.rename_pending.insert(id);
            return true;
        }
        false
    }

    /// True when `id` answers one of our rename requests.
    pub fn take_rename(&mut self, id: i64) -> bool {
        self.rename_pending.remove(&id)
    }

    /// The running server name that serves `lang` with completion, if any.
    pub fn completion_server(&self, lang: &str) -> Option<&'static str> {
        let name = registry().iter().find(|s| s.completion && s.languages.contains(&lang))?.name;
        self.clients.iter().find(|c| c.server == name).map(|c| c.server)
    }

    /// Re-pull semantic tokens for every doc open to `server` (the server signalled
    /// via `workspace/semanticTokens/refresh` that its analysis is ready/changed).
    pub fn refresh_semantic(&mut self, docs: &[crate::document::Document], server: &str) {
        let uris: Vec<String> = docs
            .iter()
            .filter(|d| d.lsp_servers.iter().any(|s| *s == server))
            .filter_map(|d| d.uri())
            .collect();
        for uri in uris {
            if let Some(id) = self.pull_semantic_tokens(server, &uri) {
                self.sem_pending.insert(id, uri);
            }
        }
    }

    /// Finish the handshake on whichever client just got its initialize response.
    pub fn on_initialized(&mut self) {
        for c in &mut self.clients {
            c.on_initialized();
        }
    }

    pub fn drop_server(&mut self, server: &str) {
        self.clients.retain(|c| c.server != server);
    }

    /// After an extension is uninstalled, stop any running server that can no longer
    /// be resolved (its files were removed) and clear the diagnostics it produced, so
    /// uninstalling takes effect immediately — no restart. Docs it served are marked
    /// for re-sync so they reopen against whatever servers remain. Returns true if a
    /// server was stopped (caller should redraw).
    pub fn reconcile(&mut self, docs: &mut [crate::document::Document], ext_roots: &[PathBuf]) -> bool {
        let stopped: Vec<&'static ServerSpec> = registry()
            .iter()
            .filter(|spec| self.clients.iter().any(|c| c.server == spec.name) && (spec.resolve)(ext_roots).is_none())
            .collect();
        if stopped.is_empty() {
            return false;
        }
        for spec in &stopped {
            if let Some(c) = self.client_mut(spec.name) {
                c.shutdown();
            }
            self.clients.retain(|c| c.server != spec.name);
        }
        for d in docs.iter_mut() {
            // Forget the stopped servers so the doc re-opens to them if reinstalled,
            // and clear diagnostics from any server that just went away.
            d.lsp_servers.retain(|s| !stopped.iter().any(|spec| spec.name == *s));
            if let Some(lang) = d.language_id() {
                if stopped.iter().any(|s| s.languages.contains(&lang)) {
                    d.diagnostics.clear();
                }
            }
        }
        true
    }

    pub fn did_open(&mut self, server: &str, uri: &str, language_id: &str, version: i32, text: &str) {
        if let Some(c) = self.client_mut(server) {
            c.did_open(uri, language_id, version, text);
        }
    }
    pub fn did_change(&mut self, server: &str, uri: &str, version: i32, text: &str) {
        if let Some(c) = self.client_mut(server) {
            c.did_change(uri, version, text);
        }
    }
    pub fn did_save(&mut self, server: &str, uri: &str, text: &str) {
        if let Some(c) = self.client_mut(server) {
            c.did_save(uri, text);
        }
    }
    pub fn did_close(&mut self, server: &str, uri: &str) {
        if let Some(c) = self.client_mut(server) {
            c.did_close(uri);
        }
    }

    /// Request diagnostics for a doc; returns the request id (map it to the URI).
    pub fn pull_diagnostics(&mut self, server: &str, uri: &str) -> Option<i64> {
        self.client_mut(server).map(|c| c.pull_diagnostics(uri))
    }

    pub fn shutdown_all(&mut self) {
        for c in &mut self.clients {
            c.shutdown();
        }
        self.clients.clear();
    }

    // ---- Orchestration (driven from App's idle tick / worker loop) ----

    /// Open any served document not yet sent to its servers, send a debounced
    /// full-text didChange for edited ones, and request the features each server
    /// declares (diagnostics / semantic tokens). Fully driven by `registry()` — no
    /// server is named here, so adding one is a `ServerSpec` entry.
    pub fn sync(
        &mut self,
        docs: &mut [crate::document::Document],
        cwd: &Path,
        ext_roots: &[PathBuf],
        tx: &Sender<WorkerMsg>,
    ) {
        let now = std::time::Instant::now();
        let debounce = self
            .last_sync
            .map_or(true, |t| now.duration_since(t) > std::time::Duration::from_millis(250));
        struct Work {
            uri: String,
            lang: &'static str,
            version: i32,
            text: String,
        }
        // Candidate docs: any with a served language. Open-state is tracked per server
        // (`d.lsp_servers`), so a doc already open to TypeScript still needs a didOpen
        // when ESLint is installed later. We carry the doc index to mutate it afterwards.
        let mut work: Vec<(usize, Work)> = Vec::new();
        for (i, d) in docs.iter().enumerate() {
            if d.large {
                continue; // large-file mode: never ship multi-MB docs to servers
            }
            let (Some(lang), Some(uri)) = (d.language_id(), d.uri()) else { continue };
            if !server_for_language(lang) {
                continue;
            }
            work.push((i, Work { uri, lang, version: d.version, text: d.text() }));
        }
        if work.is_empty() {
            return;
        }
        // Start every registered server that serves at least one candidate's language.
        let mut live: Vec<&'static ServerSpec> = Vec::new();
        for spec in registry() {
            let serves = work.iter().any(|(_, w)| spec.languages.contains(&w.lang));
            if serves && self.ensure(spec, cwd, ext_roots, tx) {
                live.push(spec);
            }
        }
        if live.is_empty() {
            // No server available yet (nothing installed). Leave docs untouched so the
            // next sync picks them up once a server appears.
            self.last_sync = Some(now);
            return;
        }
        for (i, w) in &work {
            let change = docs[*i].lsp_dirty && debounce;
            let mut serviced = false;
            for spec in &live {
                if !spec.languages.contains(&w.lang) {
                    continue;
                }
                let first_open = !docs[*i].lsp_servers.contains(&spec.name);
                if first_open {
                    self.did_open(spec.name, &w.uri, w.lang, w.version, &w.text);
                    docs[*i].lsp_servers.push(spec.name);
                } else if change {
                    self.did_change(spec.name, &w.uri, w.version, &w.text);
                } else {
                    continue; // already open + no change → nothing to send this server
                }
                if spec.pull_diagnostics {
                    if let Some(id) = self.pull_diagnostics(spec.name, &w.uri) {
                        self.diag_pending.insert(id, w.uri.clone());
                    }
                }
                if spec.pull_semantic {
                    if let Some(id) = self.pull_semantic_tokens(spec.name, &w.uri) {
                        self.sem_pending.insert(id, w.uri.clone());
                    }
                }
                serviced = true;
            }
            // Clear the dirty flag once we've pushed the change to the live servers
            // (a server installed later re-opens with full text, so nothing is lost).
            if change && serviced {
                docs[*i].lsp_dirty = false;
            }
        }
        self.last_sync = Some(now);
    }

    /// Initialize response arrived: store the semantic legend and finish handshakes.
    pub fn on_initialized_legend(&mut self, sem_token_types: Vec<String>) {
        if !sem_token_types.is_empty() {
            self.sem_legend = sem_token_types;
        }
        self.on_initialized();
    }

    fn doc_for<'a>(docs: &'a mut [crate::document::Document], uri: &str) -> Option<&'a mut crate::document::Document> {
        docs.iter_mut().find(|d| d.uri().map_or(false, |u| same_uri(&u, uri)))
    }

    /// Push `publishDiagnostics` → the matching document.
    pub fn apply_diagnostics_push(&self, docs: &mut [crate::document::Document], uri: &str, diags: Vec<Diagnostic>) {
        if let Some(d) = Self::doc_for(docs, uri) {
            d.diagnostics = diags;
        }
    }

    /// Pull diagnostic report (by request id) → the matching document.
    pub fn apply_diagnostic_report(&mut self, docs: &mut [crate::document::Document], id: i64, diags: Vec<Diagnostic>) {
        if let Some(uri) = self.diag_pending.remove(&id) {
            if let Some(d) = Self::doc_for(docs, &uri) {
                d.diagnostics = diags;
            }
        }
    }

    /// Semantic-tokens report (by request id) → decode + overlay on the document.
    /// Returns true if a document was updated (caller should redraw).
    pub fn apply_semantic(
        &mut self,
        docs: &mut [crate::document::Document],
        fs: &mut glyphon::FontSystem,
        id: i64,
        data: Vec<u32>,
    ) -> bool {
        let Some(uri) = self.sem_pending.remove(&id) else { return false };
        let toks = crate::highlight::decode_semantic(&data, &self.sem_legend);
        if let Some(d) = Self::doc_for(docs, &uri) {
            d.set_semantic(&toks);
            d.reshape(fs);
            true
        } else {
            false
        }
    }
}

/// A parsed `textDocument/completion` item — the subset we render + insert. Generic
/// across servers (rust-analyzer, tsserver, …); `kind` is the LSP CompletionItemKind.
#[derive(Clone)]
pub struct CompletionItem {
    pub label: String,
    pub insert: String, // insertText / textEdit.newText, else label
    pub detail: String, // type/signature hint
    pub kind: u8,       // LSP CompletionItemKind (1..=25); 0 = unknown
}

/// Parse a completion response (`CompletionList{items}` or a bare `CompletionItem[]`).
fn parse_completion(result: &Value) -> Vec<CompletionItem> {
    let items = if let Some(arr) = result.as_array() {
        arr.clone()
    } else if let Some(arr) = result.get("items").and_then(|i| i.as_array()) {
        arr.clone()
    } else {
        return Vec::new();
    };
    items
        .iter()
        .filter_map(|it| {
            let label = it.get("label")?.as_str()?.trim().to_string();
            // Snippet items (insertTextFormat==2) carry `$1`/`${..}` placeholders — fall
            // back to the label so we never insert raw snippet syntax.
            let snippet = it.get("insertTextFormat").and_then(|v| v.as_u64()) == Some(2);
            let raw = it
                .get("insertText")
                .and_then(|v| v.as_str())
                .or_else(|| it.get("textEdit").and_then(|t| t.get("newText")).and_then(|v| v.as_str()))
                .unwrap_or(&label);
            let insert = if snippet || raw.contains('$') { label.clone() } else { raw.to_string() };
            let detail = it.get("detail").and_then(|v| v.as_str()).unwrap_or("").to_string();
            let kind = it.get("kind").and_then(|v| v.as_u64()).unwrap_or(0) as u8;
            Some(CompletionItem { label, insert, detail, kind })
        })
        .collect()
}

/// Frame a JSON value as a Content-Length delimited LSP message.
fn frame(msg: &Value) -> Vec<u8> {
    let body = serde_json::to_vec(msg).unwrap_or_default();
    let mut out = format!("Content-Length: {}\r\n\r\n", body.len()).into_bytes();
    out.extend_from_slice(&body);
    out
}

/// Read one Content-Length framed message; None on EOF/error.
fn read_message<R: BufRead>(reader: &mut R) -> Option<Value> {
    let mut content_len = 0usize;
    loop {
        let mut line = String::new();
        if reader.read_line(&mut line).ok()? == 0 {
            return None; // EOF
        }
        let t = line.trim_end();
        if t.is_empty() {
            break; // end of headers
        }
        if let Some(v) = t.strip_prefix("Content-Length:") {
            content_len = v.trim().parse().ok()?;
        }
    }
    let mut buf = vec![0u8; content_len];
    reader.read_exact(&mut buf).ok()?;
    serde_json::from_slice(&buf).ok()
}

/// Dispatch a server→client message: reply to requests it blocks on, forward
/// diagnostics/logs to the UI, ignore the rest.
fn handle_server_message(server: &'static str, msg: &Value, reply_tx: &Sender<Vec<u8>>, cfg_reply: &Value, tx: &Sender<WorkerMsg>) {
    let method = msg.get("method").and_then(|m| m.as_str());
    let id = msg.get("id");
    match (method, id) {
        // ---- Server→client requests (must be answered or the server blocks) ----
        (Some("workspace/configuration"), Some(id)) => {
            // Reply with one config object per requested item.
            let n = msg
                .get("params")
                .and_then(|p| p.get("items"))
                .and_then(|i| i.as_array())
                .map(|a| a.len())
                .unwrap_or(1);
            let result = Value::Array(vec![cfg_reply.clone(); n.max(1)]);
            let _ = reply_tx.send(frame(&json!({ "jsonrpc": "2.0", "id": id, "result": result })));
        }
        (Some("client/registerCapability"), Some(id))
        | (Some("client/unregisterCapability"), Some(id))
        | (Some("window/workDoneProgress/create"), Some(id)) => {
            let _ = reply_tx.send(frame(&json!({ "jsonrpc": "2.0", "id": id, "result": Value::Null })));
        }
        // The server's analysis became ready/changed: it asks us to re-pull semantic
        // tokens (the initial pull at didOpen usually lands before indexing finishes
        // and comes back empty — without honoring this, files stay base-colored until
        // the next edit).
        (Some("workspace/semanticTokens/refresh"), Some(id)) => {
            let _ = reply_tx.send(frame(&json!({ "jsonrpc": "2.0", "id": id, "result": Value::Null })));
            let _ = tx.send(WorkerMsg::LspSemanticRefresh { server });
        }
        (Some("workspace/diagnostic/refresh"), Some(id)) => {
            let _ = reply_tx.send(frame(&json!({ "jsonrpc": "2.0", "id": id, "result": Value::Null })));
        }
        (Some("workspace/applyEdit"), Some(id)) => {
            // Phase 4 applies these; for now report not-applied so the server moves on.
            let _ = reply_tx.send(frame(&json!({ "jsonrpc": "2.0", "id": id, "result": { "applied": false } })));
        }
        // ---- Notifications ----
        (Some("textDocument/publishDiagnostics"), None) => {
            if let Some((uri, diags)) = parse_diagnostics(msg.get("params")) {
                let _ = tx.send(WorkerMsg::LspDiagnostics { server, uri, diags });
            }
        }
        (Some("window/logMessage"), None) | (Some("window/showMessage"), None) => {
            if let Some(m) = msg.get("params").and_then(|p| p.get("message")).and_then(|m| m.as_str()) {
                let _ = tx.send(WorkerMsg::LspLog { server, message: m.to_string() });
            }
        }
        // ---- Responses to our requests ----
        (None, Some(id)) => {
            let result = msg.get("result");
            if let Some(caps) = result.and_then(|r| r.get("capabilities")) {
                // initialize response → finish the handshake; carry the semantic-tokens
                // legend (token-type names) so App can decode token reports.
                let sem_token_types = caps
                    .get("semanticTokensProvider")
                    .and_then(|p| p.get("legend"))
                    .and_then(|l| l.get("tokenTypes"))
                    .and_then(|t| t.as_array())
                    .map(|a| a.iter().filter_map(|v| v.as_str().map(String::from)).collect())
                    .unwrap_or_default();
                let _ = tx.send(WorkerMsg::LspInitialized { sem_token_types });
            } else if let Some(r) = result {
                if r.get("data").is_some() {
                    // Semantic tokens report: { resultId?, data: [u32; 5n] }.
                    let data: Vec<u32> = r
                        .get("data")
                        .and_then(|d| d.as_array())
                        .map(|a| a.iter().filter_map(|n| n.as_u64().map(|x| x as u32)).collect())
                        .unwrap_or_default();
                    let _ = tx.send(WorkerMsg::LspSemanticTokens { id: id.as_i64().unwrap_or(0), data });
                } else if r.as_array().map_or(false, |a| {
                    a.first().map_or(false, |e| e.get("newText").is_some())
                }) {
                    // TextEdit[] (formatting). Checked before locations/completion —
                    // `newText` is unique to edits.
                    let edits = parse_text_edits(r);
                    let _ = tx.send(WorkerMsg::LspTextEdits { id: id.as_i64().unwrap_or(0), edits });
                } else if r.get("changes").is_some() || r.get("documentChanges").is_some() {
                    // WorkspaceEdit (rename).
                    let changes = parse_workspace_edit(r);
                    let _ = tx.send(WorkerMsg::LspWorkspaceEdit { id: id.as_i64().unwrap_or(0), changes });
                } else if looks_like_locations(r) {
                    // Location / LocationLink / SymbolInformation results (or null —
                    // "no definition found"). Routed by id on the main thread; empty
                    // arrays also land here and are dropped by whoever doesn't own
                    // the id.
                    let locs = parse_locations(r);
                    let _ = tx.send(WorkerMsg::LspLocations { id: id.as_i64().unwrap_or(0), locs });
                } else if r.is_array() || r.get("isIncomplete").is_some() {
                    // Completion: a bare CompletionItem[] or a CompletionList. Checked
                    // before the diagnostic branch since both carry an `items` array.
                    let items = parse_completion(r);
                    let _ = tx.send(WorkerMsg::LspCompletion { id: id.as_i64().unwrap_or(0), items });
                } else if r.get("kind").is_some() {
                    // Pull-diagnostics report: { kind: "full"|"unchanged", items: [...] }.
                    let diags = parse_diag_array(r.get("items"));
                    let _ = tx.send(WorkerMsg::LspDiagnosticReport { id: id.as_i64().unwrap_or(0), diags });
                }
            }
        }
        _ => {}
    }
}

fn parse_diagnostics(params: Option<&Value>) -> Option<(String, Vec<Diagnostic>)> {
    let p = params?;
    let uri = p.get("uri")?.as_str()?.to_string();
    Some((uri, parse_diag_array(p.get("diagnostics"))))
}

/// Parse an LSP diagnostics array (shared by push `publishDiagnostics` and pull
/// `textDocument/diagnostic` reports).
fn parse_diag_array(arr: Option<&Value>) -> Vec<Diagnostic> {
    let mut out = Vec::new();
    let Some(arr) = arr.and_then(|d| d.as_array()) else { return out };
    for d in arr {
        let Some(range) = d.get("range") else { continue };
        let (Some(s), Some(e)) = (range.get("start"), range.get("end")) else { continue };
        let g = |v: Option<&Value>, k: &str| v.and_then(|x| x.get(k)).and_then(|n| n.as_u64()).unwrap_or(0) as u32;
        out.push(Diagnostic {
            start_line: g(Some(s), "line"),
            start_char: g(Some(s), "character"),
            end_line: g(Some(e), "line"),
            end_char: g(Some(e), "character"),
            severity: Severity::from_lsp(d.get("severity").and_then(|v| v.as_i64()).unwrap_or(1)),
            message: d.get("message").and_then(|m| m.as_str()).unwrap_or("").to_string(),
            source: d.get("source").and_then(|m| m.as_str()).map(|s| s.to_string()),
            // `code` may be a string (ESLint rule) or a number (tsc error code).
            code: d.get("code").and_then(|c| {
                c.as_str().map(|s| s.to_string()).or_else(|| c.as_i64().map(|n| n.to_string()))
            }),
            code_href: d
                .get("codeDescription")
                .and_then(|cd| cd.get("href"))
                .and_then(|h| h.as_str())
                .map(|s| s.to_string()),
        });
    }
    out
}

/// The capabilities Aether advertises. Conservative for Phase 1 (sync + diagnostics);
/// hover/completion/codeAction get added as those phases land.
fn client_capabilities() -> Value {
    json!({
        "textDocument": {
            "synchronization": { "dynamicRegistration": true, "didSave": true },
            "publishDiagnostics": { "relatedInformation": false },
            "hover": { "contentFormat": ["plaintext", "markdown"] },
            "completion": { "completionItem": { "snippetSupport": false } },
            "codeAction": { "codeActionLiteralSupport": { "codeActionKind": { "valueSet": ["quickfix", "source.fixAll"] } } },
            "diagnostic": { "dynamicRegistration": true, "relatedDocumentSupport": false },
            "semanticTokens": {
                "requests": { "full": true, "range": false },
                "tokenTypes": ["namespace","type","class","enum","interface","struct","typeParameter","parameter","variable","property","enumMember","event","function","method","macro","keyword","modifier","comment","string","number","regexp","operator","decorator"],
                "tokenModifiers": ["declaration","definition","readonly","static","deprecated","abstract","async","modification","documentation","defaultLibrary"],
                "formats": ["relative"]
            }
        },
        "workspace": { "configuration": true, "workspaceFolders": true, "applyEdit": true }
    })
}

/// ESLint's `initializationOptions` (mirrors what the VS Code extension sends).
pub fn eslint_init_options(root: &Path) -> Value {
    json!({
        "validate": "on",
        "packageManager": "npm",
        "useESLintClass": true,
        "experimental": {},
        "codeAction": { "disableRuleComment": { "enable": true, "location": "separateLine" }, "showDocumentation": { "enable": true } },
        "codeActionOnSave": { "enable": false, "mode": "all" },
        "format": false,
        "quiet": false,
        "onIgnoredFiles": "off",
        "options": {},
        "rulesCustomizations": [],
        "run": "onType",
        "nodePath": null,
        "workingDirectory": { "mode": "location" },
        "workspaceFolder": { "uri": path_to_uri(root), "name": root.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default() }
    })
}

/// The per-document config Aether returns for ESLint's `workspace/configuration`.
pub fn eslint_config_reply() -> Value {
    json!({
        "validate": "on",
        "packageManager": "npm",
        "useESLintClass": true,
        "codeActionOnSave": { "enable": false, "mode": "all" },
        "format": false,
        "quiet": false,
        "onIgnoredFiles": "off",
        "options": {},
        "rulesCustomizations": [],
        "run": "onType",
        "problems": { "shortenToSingleLine": false },
        "nodePath": null,
        "workingDirectory": { "mode": "location" },
        "experimental": {}
    })
}
