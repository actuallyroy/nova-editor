// AI-assisted commit messages via Azure OpenAI.
//
// Auth is delegated to the Azure CLI — we shell out to `az account
// get-access-token` for a short-lived bearer token (resource
// `https://cognitiveservices.azure.com`) instead of baking an API key into the
// binary. The endpoint + deployment are non-secret and ship as defaults, but
// can be overridden with AETHER_AZURE_ENDPOINT / AETHER_AZURE_DEPLOYMENT /
// AETHER_AZURE_API_VERSION so a different resource can be pointed at without a
// rebuild.

use std::process::Command;
use std::sync::mpsc::Sender;
use std::sync::Mutex;
use std::time::{Duration, Instant};

use crate::marketplace::WorkerMsg;

const DEFAULT_ENDPOINT: &str = "https://scorp-tech-innovations.cognitiveservices.azure.com";
const DEFAULT_DEPLOYMENT: &str = "gpt-5.4-nano";
const DEFAULT_API_VERSION: &str = "2024-10-21";
const TOKEN_RESOURCE: &str = "https://cognitiveservices.azure.com";

// Cap how much diff we send: huge diffs blow the context window and cost, and a
// summary message doesn't need every line. ~24k chars ≈ a few hundred lines.
const MAX_DIFF_CHARS: usize = 24_000;

fn endpoint() -> String {
    std::env::var("AETHER_AZURE_ENDPOINT")
        .unwrap_or_else(|_| DEFAULT_ENDPOINT.to_string())
        .trim_end_matches('/')
        .to_string()
}
fn deployment() -> String {
    std::env::var("AETHER_AZURE_DEPLOYMENT").unwrap_or_else(|_| DEFAULT_DEPLOYMENT.to_string())
}
fn api_version() -> String {
    std::env::var("AETHER_AZURE_API_VERSION").unwrap_or_else(|_| DEFAULT_API_VERSION.to_string())
}

// Cached bearer token: `az` is slow to spawn (~1s), so reuse the token until it
// nears expiry. Tokens last ~60-90min; we refresh well before that.
static TOKEN_CACHE: Mutex<Option<(String, Instant)>> = Mutex::new(None);

#[cfg(windows)]
const CREATE_NO_WINDOW: u32 = 0x0800_0000;

/// Fetch a bearer token via the Azure CLI, reusing a cached one when still fresh.
fn get_token() -> Result<String, String> {
    if let Ok(guard) = TOKEN_CACHE.lock() {
        if let Some((tok, fetched)) = guard.as_ref() {
            if fetched.elapsed() < Duration::from_secs(45 * 60) {
                return Ok(tok.clone());
            }
        }
    }
    let mut cmd = Command::new(if cfg!(windows) { "az.cmd" } else { "az" });
    cmd.args([
        "account",
        "get-access-token",
        "--resource",
        TOKEN_RESOURCE,
        "--query",
        "accessToken",
        "-o",
        "tsv",
    ]);
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        cmd.creation_flags(CREATE_NO_WINDOW);
    }
    let out = cmd.output().map_err(|e| {
        format!("could not run `az` (is the Azure CLI installed and on PATH?): {e}")
    })?;
    if !out.status.success() {
        let err = String::from_utf8_lossy(&out.stderr);
        return Err(format!(
            "az get-access-token failed (run `az login`): {}",
            err.trim()
        ));
    }
    let tok = String::from_utf8_lossy(&out.stdout).trim().to_string();
    if tok.is_empty() {
        return Err("az returned an empty token (run `az login`)".to_string());
    }
    if let Ok(mut guard) = TOKEN_CACHE.lock() {
        *guard = Some((tok.clone(), Instant::now()));
    }
    Ok(tok)
}

/// Call the Azure OpenAI chat-completions endpoint to summarize `diff` into a
/// commit message. Synchronous — call from a worker thread.
fn request_message(diff: &str) -> Result<String, String> {
    let token = get_token()?;

    let mut diff = diff.trim();
    if diff.is_empty() {
        return Err("no changes to summarize".to_string());
    }
    if diff.len() > MAX_DIFF_CHARS {
        // Cut on a char boundary so the slice is valid UTF-8.
        let mut end = MAX_DIFF_CHARS;
        while !diff.is_char_boundary(end) {
            end -= 1;
        }
        diff = &diff[..end];
    }

    let url = format!(
        "{}/openai/deployments/{}/chat/completions?api-version={}",
        endpoint(),
        deployment(),
        api_version()
    );

    let system = "You are a tool that writes git commit messages. Given a unified \
diff, reply with ONLY the commit message — no markdown fences, no preamble, no \
explanation. Use the Conventional Commits style: a concise <=72-char subject line \
(imperative mood, e.g. \"fix: …\", \"feat: …\", \"refactor: …\"), then, only if the \
change is non-trivial, a blank line and a short bullet body. Do not invent changes \
that are not in the diff.";

    let body = serde_json::json!({
        "messages": [
            { "role": "system", "content": system },
            { "role": "user", "content": format!("Write a commit message for this diff:\n\n{diff}") }
        ],
        "max_completion_tokens": 2000
    });

    let body_str = serde_json::to_string(&body).map_err(|e| format!("bad request JSON: {e}"))?;
    let resp = ureq::post(&url)
        .set("Authorization", &format!("Bearer {token}"))
        .set("Content-Type", "application/json")
        .send_string(&body_str)
        .map_err(|e| match e {
            ureq::Error::Status(code, r) => {
                let msg = r.into_string().unwrap_or_default();
                format!("Azure returned HTTP {code}: {}", msg.trim())
            }
            ureq::Error::Transport(t) => format!("request to Azure failed: {t}"),
        })?;

    let text = resp.into_string().map_err(|e| format!("bad Azure response: {e}"))?;
    let json: serde_json::Value =
        serde_json::from_str(&text).map_err(|e| format!("bad Azure JSON: {e}"))?;
    let content = json
        .pointer("/choices/0/message/content")
        .and_then(|v| v.as_str())
        .ok_or_else(|| "no message content in Azure response".to_string())?;

    // Strip any stray markdown code fences the model might add despite the prompt.
    let cleaned = content
        .trim()
        .trim_start_matches("```")
        .trim_end_matches("```")
        .trim()
        .to_string();
    if cleaned.is_empty() {
        return Err("Azure returned an empty message".to_string());
    }
    Ok(cleaned)
}

/// Off-thread: summarize `diff` into a commit message and deliver it (or an
/// error) back to the UI thread as `WorkerMsg::CommitMessage`.
pub fn generate_commit_async(diff: String, tx: Sender<WorkerMsg>) {
    std::thread::spawn(move || {
        let result = request_message(&diff);
        let _ = tx.send(WorkerMsg::CommitMessage { result });
    });
}
