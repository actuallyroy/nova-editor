// Minimal git integration: shells out to the `git` CLI to read the current branch
// and the working-tree status. Read-only for now (staging/commit come later).

use std::path::Path;
use std::process::Command;

#[cfg(windows)]
const CREATE_NO_WINDOW: u32 = 0x0800_0000; // suppress the console flash on Windows

/// One changed path from `git status --porcelain`, with its two status codes
/// (`staged` = index side, `worktree` = working-tree side; ' ' = unchanged,
/// 'M' modified, 'A' added, 'D' deleted, 'R' renamed, '?' untracked, …).
pub struct Change {
    pub staged: char,
    pub worktree: char,
    pub path: String,
}

impl Change {
    /// True when the index side has a change staged for commit.
    pub fn is_staged(&self) -> bool {
        self.staged != ' ' && self.staged != '?'
    }
    /// A short human label for the dominant state (for the UI badge).
    pub fn label(&self) -> &'static str {
        match (self.staged, self.worktree) {
            ('?', _) => "U", // untracked
            ('A', _) => "A", // added
            ('D', _) | (_, 'D') => "D",
            ('R', _) => "R",
            _ => "M",
        }
    }
}

fn git(root: &Path, args: &[&str]) -> Option<String> {
    let mut cmd = Command::new("git");
    cmd.arg("-C").arg(root).args(args);
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        cmd.creation_flags(CREATE_NO_WINDOW);
    }
    let out = cmd.output().ok()?;
    if !out.status.success() {
        return None;
    }
    String::from_utf8(out.stdout).ok()
}

/// The current branch name (e.g. "main"), or None if not a git repo / detached.
pub fn branch(root: &Path) -> Option<String> {
    let s = git(root, &["rev-parse", "--abbrev-ref", "HEAD"])?;
    let s = s.trim();
    if s.is_empty() || s == "HEAD" {
        None
    } else {
        Some(s.to_string())
    }
}

/// Stage a path (`git add`).
pub fn stage(root: &Path, path: &str) {
    let _ = git(root, &["add", "--", path]);
}

/// Unstage a path (`git restore --staged`).
pub fn unstage(root: &Path, path: &str) {
    let _ = git(root, &["restore", "--staged", "--", path]);
}

/// Discard working-tree changes to a path: delete it if untracked, else revert it
/// to the index/HEAD (`git restore`).
pub fn discard(root: &Path, path: &str, untracked: bool) {
    if untracked {
        let _ = std::fs::remove_file(root.join(path));
    } else {
        let _ = git(root, &["restore", "--", path]);
    }
}

/// Stage every change (`git add -A`).
pub fn stage_all(root: &Path) {
    let _ = git(root, &["add", "-A"]);
}

/// Unstage everything (`git restore --staged .`).
pub fn unstage_all(root: &Path) {
    let _ = git(root, &["restore", "--staged", "."]);
}

/// Discard all tracked working-tree changes (`git restore .`). Leaves untracked
/// files alone (deleting those is destructive and needs explicit confirmation).
pub fn discard_all(root: &Path) {
    let _ = git(root, &["restore", "."]);
}

/// Push the current branch (`git push`). Returns true on success.
pub fn push(root: &Path) -> bool {
    git(root, &["push"]).is_some()
}

/// Commit with `msg`. When `stage_all` is set (nothing was explicitly staged),
/// stages every change first (`git add -A`). Returns true on success.
pub fn commit(root: &Path, msg: &str, stage_all: bool) -> bool {
    if msg.trim().is_empty() {
        return false;
    }
    if stage_all {
        let _ = git(root, &["add", "-A"]);
    }
    git(root, &["commit", "-m", msg]).is_some()
}

/// Changed paths in the working tree (staged, modified, and untracked).
pub fn status(root: &Path) -> Vec<Change> {
    let Some(out) = git(root, &["status", "--porcelain"]) else {
        return Vec::new();
    };
    out.lines()
        .filter_map(|line| {
            // Format: "XY <path>" (XY are the two status chars, then a space).
            let bytes = line.as_bytes();
            if bytes.len() < 4 {
                return None;
            }
            let staged = bytes[0] as char;
            let worktree = bytes[1] as char;
            // Renames show "old -> new"; keep the new path.
            let rest = line[3..].trim();
            let path = unquote(rest.rsplit(" -> ").next().unwrap_or(rest));
            Some(Change { staged, worktree, path })
        })
        .collect()
}

/// Undo git's path quoting. With the default `core.quotePath`, paths containing
/// spaces or non-ASCII bytes are wrapped in double quotes and C-escaped (e.g.
/// `"Screenshot 2026.png"`, `"caf\303\251"`). Unquoted paths pass through as-is.
fn unquote(s: &str) -> String {
    let b = s.as_bytes();
    if b.len() < 2 || b[0] != b'"' || b[b.len() - 1] != b'"' {
        return s.to_string();
    }
    let mut out: Vec<u8> = Vec::with_capacity(b.len());
    let mut it = b[1..b.len() - 1].iter().copied().peekable();
    while let Some(c) = it.next() {
        if c != b'\\' {
            out.push(c);
            continue;
        }
        match it.next() {
            Some(b'n') => out.push(b'\n'),
            Some(b't') => out.push(b'\t'),
            Some(b'r') => out.push(b'\r'),
            Some(b'"') => out.push(b'"'),
            Some(b'\\') => out.push(b'\\'),
            // Octal escape \NNN (up to 3 digits) — a raw byte of a UTF-8 sequence.
            Some(d @ b'0'..=b'7') => {
                let mut val = (d - b'0') as u32;
                for _ in 0..2 {
                    match it.peek() {
                        Some(&e @ b'0'..=b'7') => {
                            val = val * 8 + (e - b'0') as u32;
                            it.next();
                        }
                        _ => break,
                    }
                }
                out.push(val as u8);
            }
            Some(other) => out.push(other),
            None => {}
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}
