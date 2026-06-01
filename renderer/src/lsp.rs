// A minimal Language Server Protocol (LSP) client. Nova speaks LSP to a language
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

use std::io::{BufRead, BufReader, Read as _, Write};
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
/// slash direction). Without this, ESLint's `file:///e:/…` won't match Nova's
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
        _ => return None,
    })
}

// ---- Server registry ----

/// A configured language server: how to launch it and what languages it serves.
pub struct ServerSpec {
    pub name: &'static str,
    pub languages: &'static [&'static str],
}

/// ESLint, the first supported server.
pub const ESLINT: ServerSpec = ServerSpec {
    name: "eslint",
    languages: &["javascript", "javascriptreact", "typescript", "typescriptreact", "vue"],
};

/// All registered servers (extend this to support more — the rest is generic).
pub fn registry() -> &'static [ServerSpec] {
    &[ESLINT]
}

/// The server (if any) that serves a given language id.
pub fn server_for_language(lang: &str) -> Option<&'static ServerSpec> {
    registry().iter().find(|s| s.languages.contains(&lang))
}

// ---- Node + ESLint server resolution ----

/// Resolve a `node` executable: PATH, then common install dirs (incl. a macOS
/// login-shell lookup so GUI launches see nvm/homebrew node).
pub fn resolve_node() -> Option<String> {
    #[cfg(windows)]
    let names = ["node.exe", "node"];
    #[cfg(not(windows))]
    let names = ["node"];
    // Direct PATH probe.
    for n in names {
        if Command::new(n).arg("--version").stdout(Stdio::null()).stderr(Stdio::null()).status().is_ok() {
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
        if let Ok(out) = Command::new("/bin/sh").args(["-lc", "command -v node"]).output() {
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
        let mut cmd = Command::new(program);
        cmd.args(args)
            .current_dir(&root)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        #[cfg(windows)]
        {
            use std::os::windows::process::CommandExt;
            cmd.creation_flags(0x0800_0000); // CREATE_NO_WINDOW
        }
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
                    Some(msg) => handle_server_message(&msg, &reply_tx, &cfg, &tx),
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

    pub fn shutdown(&mut self) {
        let id = self.next_id;
        self.next_id += 1;
        self.send_raw(json!({ "jsonrpc": "2.0", "id": id, "method": "shutdown" }));
        self.send_raw(json!({ "jsonrpc": "2.0", "method": "exit" }));
    }
}

/// Owns the running language-server clients and routes document lifecycle events
/// to the right one. Phase 1: one client per server name, rooted at the workspace.
#[derive(Default)]
pub struct LspManager {
    clients: Vec<LspClient>,
}

impl LspManager {
    pub fn new() -> Self {
        Self::default()
    }

    fn client_mut(&mut self, server: &str) -> Option<&mut LspClient> {
        self.clients.iter_mut().find(|c| c.server == server)
    }

    /// Ensure the ESLint server is running for `root`; returns false if node or the
    /// ESLint server can't be found (caller can surface a status). No-op if already up.
    pub fn ensure_eslint(&mut self, root: &Path, ext_roots: &[PathBuf], tx: &Sender<WorkerMsg>) -> Result<(), String> {
        if self.client_mut("eslint").is_some() {
            return Ok(());
        }
        let node = resolve_node().ok_or("node not found on PATH")?;
        let server = eslint_server_path(ext_roots).ok_or("ESLint extension not installed")?;
        let args = vec![server.to_string_lossy().into_owned(), "--stdio".to_string()];
        let client = LspClient::start(
            "eslint",
            &node,
            &args,
            root.to_path_buf(),
            eslint_init_options(root),
            eslint_config_reply(),
            tx.clone(),
        )
        .ok_or("failed to spawn ESLint server")?;
        self.clients.push(client);
        Ok(())
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
fn handle_server_message(msg: &Value, reply_tx: &Sender<Vec<u8>>, cfg_reply: &Value, tx: &Sender<WorkerMsg>) {
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
        (Some("workspace/applyEdit"), Some(id)) => {
            // Phase 4 applies these; for now report not-applied so the server moves on.
            let _ = reply_tx.send(frame(&json!({ "jsonrpc": "2.0", "id": id, "result": { "applied": false } })));
        }
        // ---- Notifications ----
        (Some("textDocument/publishDiagnostics"), None) => {
            if let Some((uri, diags)) = parse_diagnostics(msg.get("params")) {
                let _ = tx.send(WorkerMsg::LspDiagnostics { uri, diags });
            }
        }
        (Some("window/logMessage"), None) | (Some("window/showMessage"), None) => {
            if let Some(m) = msg.get("params").and_then(|p| p.get("message")).and_then(|m| m.as_str()) {
                let _ = tx.send(WorkerMsg::LspLog { server: "lsp", message: m.to_string() });
            }
        }
        // ---- Responses to our requests ----
        (None, Some(id)) => {
            let result = msg.get("result");
            if result.and_then(|r| r.get("capabilities")).is_some() {
                // initialize response → finish the handshake.
                let _ = tx.send(WorkerMsg::LspInitialized);
            } else if let Some(r) = result {
                // A pull-diagnostics report: { kind: "full"|"unchanged", items: [...] }.
                // App maps the request id back to the document URI.
                if r.get("items").is_some() || r.get("kind").is_some() {
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
        });
    }
    out
}

/// The capabilities Nova advertises. Conservative for Phase 1 (sync + diagnostics);
/// hover/completion/codeAction get added as those phases land.
fn client_capabilities() -> Value {
    json!({
        "textDocument": {
            "synchronization": { "dynamicRegistration": true, "didSave": true },
            "publishDiagnostics": { "relatedInformation": false },
            "hover": { "contentFormat": ["plaintext", "markdown"] },
            "completion": { "completionItem": { "snippetSupport": false } },
            "codeAction": { "codeActionLiteralSupport": { "codeActionKind": { "valueSet": ["quickfix", "source.fixAll"] } } },
            "diagnostic": { "dynamicRegistration": true, "relatedDocumentSupport": false }
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

/// The per-document config Nova returns for ESLint's `workspace/configuration`.
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
