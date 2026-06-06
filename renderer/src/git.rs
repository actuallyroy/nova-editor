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
    let out = match cmd.output() {
        Ok(o) => o,
        Err(e) => {
            // `git` missing from PATH, or `root` unreadable — the usual cause of
            // "staging silently does nothing". Surface it instead of swallowing.
            eprintln!("[git] {args:?} failed to spawn in {root:?}: {e}");
            return None;
        }
    };
    if !out.status.success() {
        let err = String::from_utf8_lossy(&out.stderr);
        eprintln!("[git] {args:?} in {root:?} exited {}: {}", out.status, err.trim());
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
        // An untracked entry can be a file or a whole directory (git reports the
        // latter as "dir/"); try file removal first, then recursive dir removal.
        let target = root.join(path);
        let _ = std::fs::remove_file(&target).or_else(|_| std::fs::remove_dir_all(&target));
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

/// Stash all working-tree changes, including untracked files
/// (`git stash push --include-untracked`). Returns true on success.
pub fn stash(root: &Path) -> bool {
    git(root, &["stash", "push", "--include-untracked"]).is_some()
}

/// The diff that a commit would capture: staged changes (`git diff --cached`)
/// when anything is staged, otherwise the whole working tree (`git diff` plus
/// untracked file contents). Used to feed AI commit-message generation.
pub fn commit_diff(root: &Path) -> Option<String> {
    // `--cached` shows what's staged; if that's empty, fall back to the
    // working-tree diff so "generate" works before staging anything.
    let staged = git(root, &["diff", "--cached"]).unwrap_or_default();
    if !staged.trim().is_empty() {
        return Some(staged);
    }
    let unstaged = git(root, &["diff"]).unwrap_or_default();
    Some(unstaged)
}

/// Apply a unified-diff `patch` (built by `diff::block_patch`) via stdin. `cached`
/// targets the index (stage/unstage), else the working tree; `reverse` applies it
/// backwards (unstage / revert). Zero-context patches need `--unidiff-zero`.
/// Returns true on success.
pub fn apply_patch(root: &Path, patch: &str, cached: bool, reverse: bool) -> bool {
    use std::io::Write;
    let mut cmd = Command::new("git");
    cmd.arg("-C").arg(root).args(["apply", "--unidiff-zero"]);
    if cached {
        cmd.arg("--cached");
    }
    if reverse {
        cmd.arg("--reverse");
    }
    cmd.arg("-");
    cmd.stdin(std::process::Stdio::piped());
    cmd.stdout(std::process::Stdio::null());
    cmd.stderr(std::process::Stdio::piped());
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        cmd.creation_flags(CREATE_NO_WINDOW);
    }
    let mut child = match cmd.spawn() {
        Ok(c) => c,
        Err(e) => {
            eprintln!("[git] apply failed to spawn: {e}");
            return false;
        }
    };
    if let Some(stdin) = child.stdin.as_mut() {
        if stdin.write_all(patch.as_bytes()).is_err() {
            return false;
        }
    }
    match child.wait_with_output() {
        Ok(o) if o.status.success() => true,
        Ok(o) => {
            eprintln!("[git] apply rejected: {}", String::from_utf8_lossy(&o.stderr).trim());
            false
        }
        Err(_) => false,
    }
}

/// Push the current branch (`git push`). Returns true on success.
pub fn push(root: &Path) -> bool {
    git(root, &["push"]).is_some()
}

/// Pull the current branch (`git pull`). Returns true on success.
pub fn pull(root: &Path) -> bool {
    git(root, &["pull"]).is_some()
}

/// Fetch all remotes (`git fetch --all`). Returns true on success.
pub fn fetch(root: &Path) -> bool {
    git(root, &["fetch", "--all"]).is_some()
}

/// Local branch short-names (`git branch`), current branch first.
pub fn branches(root: &Path) -> Vec<String> {
    let Some(out) = git(root, &["branch", "--format=%(refname:short)"]) else {
        return Vec::new();
    };
    let cur = branch(root);
    let mut names: Vec<String> = out.lines().map(|l| l.trim().to_string()).filter(|l| !l.is_empty()).collect();
    // Current branch to the top so it reads as the default selection.
    if let Some(c) = &cur {
        names.sort_by_key(|n| (n != c, n.clone()));
    }
    names
}

/// Switch to an existing branch (`git checkout <branch>`). Returns true on success.
pub fn checkout(root: &Path, branch: &str) -> bool {
    git(root, &["checkout", branch]).is_some()
}

/// Create and switch to a new branch (`git checkout -b <name>`).
pub fn create_branch(root: &Path, name: &str) -> bool {
    if name.trim().is_empty() {
        return false;
    }
    git(root, &["checkout", "-b", name]).is_some()
}

/// Rename the current branch (`git branch -m <name>`).
pub fn rename_branch(root: &Path, name: &str) -> bool {
    if name.trim().is_empty() {
        return false;
    }
    git(root, &["branch", "-m", name]).is_some()
}

/// Delete a branch (`git branch -D <name>`, force so it works off-branch).
pub fn delete_branch(root: &Path, name: &str) -> bool {
    git(root, &["branch", "-D", name]).is_some()
}

/// Pop the most recent stash (`git stash pop`). Returns true on success.
pub fn stash_pop(root: &Path) -> bool {
    git(root, &["stash", "pop"]).is_some()
}

/// Apply the most recent stash without dropping it (`git stash apply`).
pub fn stash_apply(root: &Path) -> bool {
    git(root, &["stash", "apply"]).is_some()
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
///
/// `-uall` lists untracked files individually instead of collapsing a fully-
/// untracked directory into one "dir/" entry (like VSCode). That way every row is a
/// real file: it nests correctly in the tree and its diff shows actual content,
/// rather than a nameless directory row that can't be diffed.
pub fn status(root: &Path) -> Vec<Change> {
    let Some(out) = git(root, &["status", "--porcelain", "-uall"]) else {
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::process::Command;

    fn run(root: &Path, args: &[&str]) {
        let ok = Command::new("git").arg("-C").arg(root).args(args).status().map(|s| s.success()).unwrap_or(false);
        assert!(ok, "git {args:?} failed");
    }

    // End-to-end: a modified tracked file, once staged, must leave the worktree
    // (unstaged) side and appear on the index (staged) side of `git status`.
    #[test]
    fn stage_moves_file_to_index() {
        let dir = std::env::temp_dir().join(format!("aether-git-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("src")).unwrap();
        run(&dir, &["init", "-q"]);
        run(&dir, &["config", "user.email", "t@t"]);
        run(&dir, &["config", "user.name", "t"]);
        std::fs::write(dir.join("src/main.rs"), "fn main() {}\n").unwrap();
        run(&dir, &["add", "-A"]);
        run(&dir, &["commit", "-qm", "init"]);

        // Modify, confirm it shows as worktree-modified and NOT staged.
        std::fs::write(dir.join("src/main.rs"), "fn main() { /* edit */ }\n").unwrap();
        let before = status(&dir);
        let c = before.iter().find(|c| c.path == "src/main.rs").expect("file in status");
        assert_eq!(c.worktree, 'M', "should be worktree-modified before staging");
        assert!(!c.is_staged(), "should not be staged before staging");

        // Stage it via the real code path, then re-read.
        stage(&dir, "src/main.rs");
        let after = status(&dir);
        let c = after.iter().find(|c| c.path == "src/main.rs").expect("file still in status");
        assert!(c.is_staged(), "expected staged after stage(), got staged={:?} worktree={:?}", c.staged, c.worktree);
        assert_eq!(c.worktree, ' ', "worktree side should be clean after staging a fully-staged edit");

        // And unstage round-trips it back.
        unstage(&dir, "src/main.rs");
        let back = status(&dir);
        let c = back.iter().find(|c| c.path == "src/main.rs").expect("file in status");
        assert!(!c.is_staged(), "expected not staged after unstage()");
        assert_eq!(c.worktree, 'M', "worktree-modified again after unstaging");

        let _ = std::fs::remove_dir_all(&dir);
    }
}
