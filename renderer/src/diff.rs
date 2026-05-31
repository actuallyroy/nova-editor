// Git diff model for the Source Control diff view.
//
// `compute` shells out to `git diff` (or reads an untracked file whole) and parses
// the unified-diff output into aligned side-by-side rows: each `DiffRow` carries an
// optional old (left) and new (right) line, so deletions sit on the left, additions
// on the right, and context lines on both. The renderer draws two panes from
// `left_text` / `right_text` (one line per row, blank = filler) plus per-row
// backgrounds and gutters driven by `rows`.

use std::path::Path;
use std::process::Command;

#[cfg(windows)]
const CREATE_NO_WINDOW: u32 = 0x0800_0000;

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum RowKind {
    Context, // unchanged: present on both sides
    Add,     // added: right side only (left is filler)
    Del,     // removed: left side only (right is filler)
    Hunk,    // an "@@ ... @@" separator spanning both sides
}

pub struct DiffRow {
    pub kind: RowKind,
    pub left: Option<u32>,  // old line number (None = filler / hunk)
    pub right: Option<u32>, // new line number (None = filler / hunk)
}

pub struct Diff {
    pub title: String,
    pub left_text: String,  // old side, one line per row (blank for filler/add)
    pub right_text: String, // new side, one line per row (blank for filler/del)
    pub rows: Vec<DiffRow>,
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
    String::from_utf8(out.stdout).ok()
}

fn file_name(path: &str) -> &str {
    path.rsplit(['/', '\\']).next().unwrap_or(path)
}

/// Build the diff for one repo-relative `path`.
/// - `staged`: diff the index against HEAD (`git diff --cached`).
/// - else tracked: diff the working tree against HEAD (`git diff HEAD`).
/// - `untracked`: no git side — show the whole working file as additions.
pub fn compute(root: &Path, path: &str, staged: bool, untracked: bool) -> Diff {
    let name = file_name(path).to_string();
    if untracked {
        return whole_file_as_added(root, path, &name);
    }
    let label = if staged { "Index" } else { "Working Tree" };
    let title = format!("{name} ({label})");
    let args: Vec<&str> = if staged {
        vec!["diff", "--no-color", "--cached", "--", path]
    } else {
        vec!["diff", "--no-color", "HEAD", "--", path]
    };
    let raw = git(root, &args).unwrap_or_default();
    Builder::new(title).parse(&raw).finish()
}

/// Untracked file: every line is an addition (right side only).
fn whole_file_as_added(root: &Path, path: &str, name: &str) -> Diff {
    let title = format!("{name} (Untracked)");
    let content = std::fs::read_to_string(root.join(path)).unwrap_or_default();
    let mut b = Builder::new(title);
    let mut n = 1u32;
    for line in content.replace('\r', "").lines() {
        b.row(RowKind::Add, None, Some(n), "", line);
        n += 1;
    }
    b.finish()
}

/// Accumulates aligned side-by-side rows while parsing a unified diff.
struct Builder {
    title: String,
    left_text: String,
    right_text: String,
    rows: Vec<DiffRow>,
    // Pending deletions/additions within the current change block, paired on flush.
    dels: Vec<(u32, String)>,
    adds: Vec<(u32, String)>,
}

impl Builder {
    fn new(title: String) -> Self {
        Self {
            title,
            left_text: String::new(),
            right_text: String::new(),
            rows: Vec::new(),
            dels: Vec::new(),
            adds: Vec::new(),
        }
    }

    /// Emit one aligned row (left/right text may be empty for a filler side).
    fn row(&mut self, kind: RowKind, left: Option<u32>, right: Option<u32>, l: &str, r: &str) {
        self.left_text.push_str(l);
        self.left_text.push('\n');
        self.right_text.push_str(r);
        self.right_text.push('\n');
        self.rows.push(DiffRow { kind, left, right });
    }

    /// Emit the pending change block: deletions first (left side, right is filler),
    /// then additions (right side, left is filler). Simple and unambiguous — each
    /// row clearly belongs to one side.
    fn flush(&mut self) {
        let dels = std::mem::take(&mut self.dels);
        let adds = std::mem::take(&mut self.adds);
        for (o, t) in dels {
            self.row(RowKind::Del, Some(o), None, &t, "");
        }
        for (o, t) in adds {
            self.row(RowKind::Add, None, Some(o), "", &t);
        }
    }

    fn parse(mut self, raw: &str) -> Self {
        let mut old = 0u32;
        let mut new = 0u32;
        let mut in_hunk = false;
        for line in raw.split('\n') {
            if line.starts_with("@@") {
                self.flush();
                if let Some((o, n)) = parse_hunk_header(line) {
                    old = o;
                    new = n;
                    in_hunk = true;
                    self.row(RowKind::Hunk, None, None, line.trim_end(), "");
                }
                continue;
            }
            if !in_hunk {
                continue; // skip file header (diff --git, index, ---, +++)
            }
            match line.as_bytes().first() {
                Some(b'-') => {
                    self.dels.push((old, line[1..].to_string()));
                    old += 1;
                }
                Some(b'+') => {
                    self.adds.push((new, line[1..].to_string()));
                    new += 1;
                }
                Some(b' ') => {
                    self.flush();
                    let t = line[1..].to_string();
                    self.row(RowKind::Context, Some(old), Some(new), &t, &t);
                    old += 1;
                    new += 1;
                }
                _ => {}
            }
        }
        self.flush();
        self
    }

    fn finish(mut self) -> Diff {
        if self.rows.is_empty() {
            self.row(RowKind::Context, None, None, "No changes.", "No changes.");
        }
        Diff {
            title: self.title,
            left_text: self.left_text,
            right_text: self.right_text,
            rows: self.rows,
        }
    }
}

/// Parse the starting line numbers out of "@@ -a,b +c,d @@". Returns (old, new).
fn parse_hunk_header(line: &str) -> Option<(u32, u32)> {
    let body = line.strip_prefix("@@ ")?;
    let body = body.split(" @@").next()?;
    let mut parts = body.split_whitespace();
    let old = parts.next()?.trim_start_matches('-').split(',').next()?.parse().ok()?;
    let new = parts.next()?.trim_start_matches('+').split(',').next()?.parse().ok()?;
    Some((old, new))
}
