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
    File,    // combined view: a per-file header row (collapsible)
    Gap,     // a collapsed run of unchanged lines (click/drag to expand)
}

/// A collapsed run of unchanged lines in the FULL diff. `[start, end)` are row
/// indices into `Diff.rows` (all of them Context). `top`/`bot` count how many of
/// those rows have been revealed from each end; when `top + bot >= end - start`
/// the whole run is showing and the gap disappears on the next projection.
#[derive(Clone)]
pub struct Gap {
    pub start: usize,
    pub end: usize,
    pub top: usize,
    pub bot: usize,
}

impl Gap {
    /// Rows still hidden in this gap.
    pub fn hidden(&self) -> usize {
        (self.end - self.start).saturating_sub(self.top + self.bot)
    }
    fn hidden_range(&self) -> std::ops::Range<usize> {
        (self.start + self.top)..(self.end - self.bot)
    }
}

/// Lines of unchanged context kept adjacent to a change before collapsing the
/// middle, and the smallest hidden run worth collapsing.
const GAP_MARGIN: usize = 3;
const GAP_MIN_HIDDEN: usize = 4;
/// Context to request from git: effectively the whole file, so every unchanged
/// line is present and expansion never needs to re-read the file.
const FULL_CONTEXT: &str = "1000000";

#[derive(Clone)]
pub struct DiffRow {
    pub kind: RowKind,
    pub left: Option<u32>,  // old line number (None = filler / hunk)
    pub right: Option<u32>, // new line number (None = filler / hunk)
    pub file: usize,        // index into Diff.files (which file this row belongs to)
}

#[derive(Clone)]
pub struct Diff {
    pub title: String,
    pub left_text: String,  // old side, one line per row (blank for filler/add)
    pub right_text: String, // new side, one line per row (blank for filler/del)
    pub rows: Vec<DiffRow>,
    pub combined: bool,     // true = multi-file view (has RowKind::File headers)
    pub files: Vec<String>, // display names per file index (combined view)
    /// Collapsed unchanged regions (single-file view). Empty ⇒ nothing to expand;
    /// this is the FULL diff, projected to the visible one via `project_gaps`.
    pub gaps: Vec<Gap>,
}

/// Leading pad on a combined-view file-header line, leaving room for the codicon
/// twistie that the renderer overlays at the row's left.
const FILE_HEADER_PAD: &str = "   ";

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
    // Full context (-U<huge>) so every unchanged line is present — long unchanged
    // runs are then collapsed into expandable gaps without re-reading the file.
    let ctx = format!("-U{FULL_CONTEXT}");
    let args: Vec<&str> = if staged {
        vec!["diff", "--no-color", &ctx, "--cached", "--", path]
    } else {
        vec!["diff", "--no-color", &ctx, "HEAD", "--", path]
    };
    let raw = git(root, &args).unwrap_or_default();
    let mut b = Builder::new(title);
    b.parse_into(&raw);
    let mut diff = b.finish();
    diff.collapse_unchanged();
    diff
}

/// Compare two arbitrary files (explorer "Select for Compare" → "Compare with
/// Selected"): `git diff --no-index` works outside any repository and exits
/// non-zero when the files differ, which `git()` deliberately ignores.
pub fn compute_files(a: &Path, b: &Path) -> Diff {
    let title = format!("{} ↔ {}", file_name(&a.to_string_lossy()), file_name(&b.to_string_lossy()));
    let cwd = a.parent().unwrap_or_else(|| Path::new(".")).to_path_buf();
    let (sa, sb) = (a.to_string_lossy().to_string(), b.to_string_lossy().to_string());
    let raw = git(&cwd, &["diff", "--no-color", "--no-index", "--", &sa, &sb]).unwrap_or_default();
    let mut bld = Builder::new(title);
    if raw.trim().is_empty() {
        bld.row(RowKind::Context, None, None, "Files are identical.", "Files are identical.");
    } else {
        bld.parse_into(&raw);
    }
    bld.finish()
}

/// Combined "Open Changes" view: every entry's diff stacked under a per-file header
/// row (`RowKind::File`). `entries` is `(repo-relative path, untracked)`; `staged`
/// selects the index-vs-HEAD diff. The header text carries an expanded chevron;
/// `project` rewrites it per collapse state.
pub fn compute_all(root: &Path, entries: &[(String, bool)], staged: bool) -> Diff {
    let title = if staged { "Staged Changes".to_string() } else { "Changes".to_string() };
    let mut b = Builder::new(title);
    b.combined = true;
    for (path, untracked) in entries {
        let name = file_name(path).to_string();
        let fidx = b.files.len();
        b.files.push(name.clone());
        b.cur_file = fidx;
        b.row(RowKind::File, None, None, &format!("{}{}", FILE_HEADER_PAD, name), "");
        if *untracked {
            let content = std::fs::read_to_string(root.join(path)).unwrap_or_default();
            let mut n = 1u32;
            for line in content.replace('\r', "").lines() {
                b.row(RowKind::Add, None, Some(n), "", line);
                n += 1;
            }
        } else {
            let args: Vec<&str> = if staged {
                vec!["diff", "--no-color", "--cached", "--", path]
            } else {
                vec!["diff", "--no-color", "HEAD", "--", path]
            };
            let raw = git(root, &args).unwrap_or_default();
            b.parse_into(&raw);
        }
        b.flush();
    }
    if b.files.is_empty() {
        b.row(RowKind::Context, None, None, "No changes.", "No changes.");
    }
    b.into_diff()
}

/// Re-derive the visible side of a combined diff for the given collapsed file set:
/// every file's header row stays (its chevron flipped to match), but a collapsed
/// file's body rows are dropped. Returns a fresh `Diff` to install as the view.
pub fn project(full: &Diff, collapsed: &std::collections::HashSet<usize>) -> Diff {
    let lefts: Vec<&str> = full.left_text.split('\n').collect();
    let rights: Vec<&str> = full.right_text.split('\n').collect();
    let mut left_text = String::new();
    let mut right_text = String::new();
    let mut rows = Vec::new();
    for (i, r) in full.rows.iter().enumerate() {
        if r.kind == RowKind::File {
            // The collapse chevron is overlaid by the renderer; keep just the padded name.
            let name = full.files.get(r.file).map(String::as_str).unwrap_or("");
            left_text.push_str(&format!("{}{}\n", FILE_HEADER_PAD, name));
            right_text.push('\n');
            rows.push(r.clone());
            continue;
        }
        if collapsed.contains(&r.file) {
            continue; // body of a collapsed file
        }
        left_text.push_str(lefts.get(i).copied().unwrap_or(""));
        left_text.push('\n');
        right_text.push_str(rights.get(i).copied().unwrap_or(""));
        right_text.push('\n');
        rows.push(r.clone());
    }
    Diff {
        title: full.title.clone(),
        left_text,
        right_text,
        rows,
        combined: true,
        files: full.files.clone(),
        gaps: Vec::new(),
    }
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
    combined: bool,
    files: Vec<String>,
    cur_file: usize,
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
            combined: false,
            files: Vec::new(),
            cur_file: 0,
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
        self.rows.push(DiffRow { kind, left, right, file: self.cur_file });
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

    fn parse_into(&mut self, raw: &str) {
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
    }

    fn finish(mut self) -> Diff {
        if self.rows.is_empty() {
            self.row(RowKind::Context, None, None, "No changes.", "No changes.");
        }
        self.into_diff()
    }

    fn into_diff(self) -> Diff {
        Diff {
            title: self.title,
            left_text: self.left_text,
            right_text: self.right_text,
            rows: self.rows,
            combined: self.combined,
            files: self.files,
            gaps: Vec::new(),
        }
    }
}

impl Diff {
    /// Find long runs of unchanged (Context) rows and mark their interiors as
    /// collapsed gaps, keeping `GAP_MARGIN` context lines next to each change.
    /// Single-file diffs only (the combined view collapses per file instead).
    pub fn collapse_unchanged(&mut self) {
        if self.combined {
            return;
        }
        let mut gaps = Vec::new();
        let mut i = 0;
        while i < self.rows.len() {
            if self.rows[i].kind != RowKind::Context {
                i += 1;
                continue;
            }
            let start = i;
            while i < self.rows.len() && self.rows[i].kind == RowKind::Context {
                i += 1;
            }
            // [start, i) is a Context run. Keep a margin at each end (but none at the
            // file's very top/bottom — there's no change there to anchor to).
            let top_margin = if start == 0 { 0 } else { GAP_MARGIN };
            let bot_margin = if i == self.rows.len() { 0 } else { GAP_MARGIN };
            let hidden_start = start + top_margin;
            let hidden_end = i.saturating_sub(bot_margin);
            if hidden_end > hidden_start && hidden_end - hidden_start >= GAP_MIN_HIDDEN {
                gaps.push(Gap { start: hidden_start, end: hidden_end, top: 0, bot: 0 });
            }
        }
        self.gaps = gaps;
    }
}

/// Project a full single-file diff (with collapsed gaps) to the visible diff: each
/// still-hidden gap interior becomes one `Gap` separator row (its `file` field
/// carries the gap index so clicks/drags can find it); everything else passes
/// through. No-op when there are no gaps.
pub fn project_gaps(full: &Diff) -> Diff {
    if full.gaps.is_empty() {
        return full.clone();
    }
    let lefts: Vec<&str> = full.left_text.split('\n').collect();
    let rights: Vec<&str> = full.right_text.split('\n').collect();
    let mut left_text = String::new();
    let mut right_text = String::new();
    let mut rows = Vec::new();
    let mut emit = |row: DiffRow, l: &str, r: &str, lt: &mut String, rt: &mut String, out: &mut Vec<DiffRow>| {
        lt.push_str(l);
        lt.push('\n');
        rt.push_str(r);
        rt.push('\n');
        out.push(row);
    };
    let mut i = 0usize;
    while i < full.rows.len() {
        // A gap whose hidden range starts here?
        if let Some((gi, gap)) = full.gaps.iter().enumerate().find(|(_, g)| g.hidden_range().start == i && g.hidden() > 0) {
            let n = gap.hidden();
            let label = format!("    ⋯  {n} unchanged line{}  ⋯", if n == 1 { "" } else { "s" });
            // Label on BOTH sides so the collapsed band reads symmetrically.
            emit(
                DiffRow { kind: RowKind::Gap, left: None, right: None, file: gi },
                &label,
                &label,
                &mut left_text,
                &mut right_text,
                &mut rows,
            );
            i = gap.hidden_range().end; // skip the hidden interior
            continue;
        }
        emit(
            full.rows[i].clone(),
            lefts.get(i).copied().unwrap_or(""),
            rights.get(i).copied().unwrap_or(""),
            &mut left_text,
            &mut right_text,
            &mut rows,
        );
        i += 1;
    }
    Diff {
        title: full.title.clone(),
        left_text,
        right_text,
        rows,
        combined: false,
        files: full.files.clone(),
        gaps: full.gaps.clone(),
    }
}

/// Contiguous runs of changed rows (Add/Del) in `d` — one entry per change block,
/// as `[start, end)` row indices. Drives the per-block Stage/Revert buttons.
pub fn change_blocks(d: &Diff) -> Vec<(usize, usize)> {
    let is_change = |k: RowKind| matches!(k, RowKind::Add | RowKind::Del);
    let mut out = Vec::new();
    let mut i = 0;
    while i < d.rows.len() {
        if is_change(d.rows[i].kind) {
            let s = i;
            while i < d.rows.len() && is_change(d.rows[i].kind) {
                i += 1;
            }
            out.push((s, i));
        } else {
            i += 1;
        }
    }
    out
}

/// Build a minimal unified-diff patch (zero-context, applied with `--unidiff-zero`)
/// for the change block at `[bs, be)` of `full`. The `-` lines are the old (HEAD)
/// side, `+` the new side. Used to stage / unstage / revert that single block.
pub fn block_patch(full: &Diff, path: &str, bs: usize, be: usize) -> Option<String> {
    let lefts: Vec<&str> = full.left_text.split('\n').collect();
    let rights: Vec<&str> = full.right_text.split('\n').collect();
    let (mut dels, mut adds): (Vec<&str>, Vec<&str>) = (Vec::new(), Vec::new());
    let (mut first_del_left, mut first_add_right) = (None, None);
    for i in bs..be {
        match full.rows[i].kind {
            RowKind::Del => {
                dels.push(lefts.get(i).copied().unwrap_or(""));
                first_del_left = first_del_left.or(full.rows[i].left);
            }
            RowKind::Add => {
                adds.push(rights.get(i).copied().unwrap_or(""));
                first_add_right = first_add_right.or(full.rows[i].right);
            }
            _ => {}
        }
    }
    if dels.is_empty() && adds.is_empty() {
        return None;
    }
    // The unchanged line directly above the block anchors pure-add / pure-del hunks.
    let prev = (0..bs).rev().map(|i| &full.rows[i]).find(|r| r.left.is_some() || r.right.is_some());
    let old_start = first_del_left.or_else(|| prev.and_then(|r| r.left)).unwrap_or(0);
    let new_start = first_add_right.or_else(|| prev.and_then(|r| r.right)).unwrap_or(0);
    let mut p = format!("diff --git a/{path} b/{path}\n--- a/{path}\n+++ b/{path}\n");
    p.push_str(&format!("@@ -{},{} +{},{} @@\n", old_start, dels.len(), new_start, adds.len()));
    for d in &dels {
        p.push('-');
        p.push_str(d);
        p.push('\n');
    }
    for a in &adds {
        p.push('+');
        p.push_str(a);
        p.push('\n');
    }
    Some(p)
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
