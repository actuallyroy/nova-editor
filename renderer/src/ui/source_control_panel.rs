// Source Control sidebar view, modeled on VSCode's SCM: a commit message box + a
// blue Commit button, then "Staged Changes" / "Changes" groups (each with a count
// badge). Rows show filename (bright) + dimmed dir, ellipsized to fit, a colored
// status letter on the right, and per-row hover actions:
//   • Changes:        Open Changes (diff) · Open File · Discard · Stage (+)
//   • Staged Changes: Open Changes (diff) · Open File · Unstage (−)
// A header toolbar offers Stash · View as Tree/List · Refresh · More. The changed
// files render either as a flat list or a collapsible folder tree (with single-
// child folder chains compacted, like VSCode). Owns its workspace root + commit
// message. Deferred: per-file-type icons + GRAPH.

use std::collections::{BTreeMap, HashSet};
use std::path::PathBuf;

use glyphon::{Attrs, Color, Family, FontSystem, TextArea};

use crate::git;
use crate::quad::Quad;
use crate::theme;
use crate::ui::Intent;
use crate::widgets::{ListView, Rect, ScrollOpts, ScrollView, TextInput, TextLabel};

// Geometry is reactive, not frozen: row height tracks the sidebar's (which scales
// with UI zoom) and the paddings scale with zoom too, so the whole panel stays
// proportional at any zoom — no hardcoded pixel sizes.
fn row_h() -> f32 { theme::TREE_ROW_HEIGHT() }
fn pad_x() -> f32 { 30.0 * theme::ui_zoom() } // row text indent
fn status_w() -> f32 { 22.0 * theme::ui_zoom() } // status-letter column at the right edge
fn action_w() -> f32 { 20.0 * theme::ui_zoom() } // per hover-action icon
// Text never grows under the status letter + the (up to 3) hover-action icons.
fn right_reserve() -> f32 { status_w() + 3.0 * action_w() + 8.0 * theme::ui_zoom() }

/// A changed file as shown in one group. `fname_len` is recomputed by `update`
/// each time the sidebar width changes (so the ellipsis is reactive to resize).
struct Row {
    path: String,     // repo-relative (git's slashes)
    fname: String,    // file name
    dir: String,      // parent dir (dimmed)
    fname_len: usize, // byte length of the bright file-name prefix in the current display
    badge: usize,     // index into BADGE_*
    untracked: bool,  // for Discard (delete vs revert)
    new_file: bool,   // untracked or staged-added: no prior version, so no diff
}

/// One visible line in a group: either a folder (tree mode only) or a file row.
/// In list mode every item is a `File` at depth 0; in tree mode files nest under
/// `Folder`s whose collapse state is keyed in `collapsed`.
enum Vis {
    Folder { key: String, label: String, depth: usize, collapsed: bool },
    File { row: usize, depth: usize },
}

/// Temporary tree built from the flat change paths to produce the `Vis` list.
#[derive(Default)]
struct TNode {
    children: BTreeMap<String, TNode>,
    files: Vec<usize>, // row indices whose file lives directly in this folder
}

#[derive(Clone, Copy, PartialEq)]
enum Act {
    Diff, // Open Changes (diff)
    Open, // Open File
    Discard,
    Stage,
    Unstage,
}

const BADGE_LETTERS: [&str; 5] = ["M", "A", "D", "R", "U"];
const BADGE_RGB: [(u8, u8, u8); 5] = [
    (224, 168, 61),
    (102, 199, 117),
    (219, 84, 84),
    (115, 158, 230),
    (102, 199, 117),
];
/// Vertical intersection of `r` with viewport `vp` (None when fully outside).
fn vclip(r: Rect, vp: Rect) -> Option<Rect> {
    let top = r.y.max(vp.y);
    let bot = (r.y + r.h).min(vp.y + vp.h);
    (bot > top).then(|| Rect { x: r.x, y: top, w: r.w, h: bot - top })
}

fn badge_for(code: char) -> usize {
    match code {
        'A' => 1,
        'D' => 2,
        'R' => 3,
        '?' => 4,
        _ => 0,
    }
}

pub struct SourceControlPanel {
    msg: TextInput,
    msg_active: bool,
    staged: ListView,
    unstaged: ListView,
    staged_rows: Vec<Row>,
    unstaged_rows: Vec<Row>,
    staged_vis: Vec<Vis>,
    unstaged_vis: Vec<Vis>,
    tree_mode: bool,
    collapsed: HashSet<String>, // keyed folders (per group, see `walk`)
    last_w: f32, // sidebar width the rows were last ellipsized for (-1 = stale)
    hovered: Option<(bool, usize)>, // (is_staged_group, visible-item index)
    branch: Option<String>,
    l_changes: TextLabel,
    l_staged: TextLabel,
    l_unstaged: TextLabel,
    l_commit: TextLabel,
    count_staged: TextLabel,
    count_unstaged: TextLabel,
    badges: [TextLabel; 5],
    ic_diff: TextLabel,
    ic_open: TextLabel,
    ic_discard: TextLabel,
    ic_stage: TextLabel,
    ic_unstage: TextLabel,
    ic_refresh: TextLabel,
    ic_more: TextLabel,
    ic_stash: TextLabel,
    ic_tree: TextLabel,
    ic_flat: TextLabel,
    ic_chevron: TextLabel,
    hovered_header: Option<bool>, // Some(true)=Staged header, Some(false)=Changes header
    /// Scroll state of the groups area (headers + file lists) — the shared
    /// ScrollView owns the offset, clamping, and the auto-hiding scrollbar.
    pub scroll: ScrollView,
    root: PathBuf,
    change_count: usize, // unique changed files (for the activity-bar badge)
}

impl SourceControlPanel {
    pub fn new(fs: &mut FontSystem, root: PathBuf) -> Self {
        let mk = |fs: &mut FontSystem, s: &str| {
            let mut l = TextLabel::new(fs, theme::SIDEBAR_WIDTH(), row_h());
            l.set(fs, s, theme::UI_FAMILY());
            l
        };
        let icon = |fs: &mut FontSystem, c: char| {
            let mut l = TextLabel::new(fs, 24.0, row_h());
            l.set(fs, &c.to_string(), theme::ICON_FAMILY);
            l
        };
        let badges = std::array::from_fn(|i| {
            let mut l = TextLabel::new(fs, 24.0, row_h());
            l.set(fs, BADGE_LETTERS[i], theme::UI_FAMILY());
            l
        });
        let mut msg = TextInput::new(fs, theme::SIDEBAR_WIDTH(), 30.0);
        msg.set_placeholder(fs, " Message (Ctrl+Enter to commit)");
        Self {
            msg,
            msg_active: false,
            staged: ListView::new(fs, theme::SIDEBAR_WIDTH(), 4000.0, row_h(), pad_x()),
            unstaged: ListView::new(fs, theme::SIDEBAR_WIDTH(), 4000.0, row_h(), pad_x()),
            staged_rows: Vec::new(),
            unstaged_rows: Vec::new(),
            staged_vis: Vec::new(),
            unstaged_vis: Vec::new(),
            tree_mode: false,
            collapsed: HashSet::new(),
            last_w: -1.0,
            hovered: None,
            branch: None,
            l_changes: mk(fs, "CHANGES"),
            l_staged: mk(fs, "Staged Changes"),
            l_unstaged: mk(fs, "Changes"),
            l_commit: mk(fs, "✓ Commit"),
            count_staged: mk(fs, "0"),
            count_unstaged: mk(fs, "0"),
            badges,
            ic_diff: icon(fs, theme::ICON_OPEN_CHANGES),
            ic_open: icon(fs, theme::ICON_FILE),
            ic_discard: icon(fs, theme::ICON_DISCARD),
            ic_stage: icon(fs, theme::ICON_ADD),
            ic_unstage: icon(fs, theme::ICON_REMOVE),
            ic_refresh: icon(fs, theme::ICON_REFRESH),
            ic_more: icon(fs, theme::ICON_ELLIPSIS),
            ic_stash: icon(fs, theme::ICON_STASH),
            ic_tree: icon(fs, theme::ICON_LIST_TREE),
            ic_flat: icon(fs, theme::ICON_LIST_FLAT),
            ic_chevron: icon(fs, theme::ICON_CHEVRON_DOWN),
            hovered_header: None,
            scroll: ScrollView::new(ScrollOpts { vertical: true, horizontal: false, stick_to_end: false }),
            root,
            change_count: 0,
        }
    }

    /// Number of unique changed files (for the Source Control activity-bar badge).
    pub fn change_count(&self) -> usize {
        self.change_count
    }

    /// Re-shape all owned text after a UI-zoom change.
    pub fn reshape(&mut self, fs: &mut FontSystem) {
        self.msg.rezoom(fs);
        for l in [
            &mut self.l_changes,
            &mut self.l_staged,
            &mut self.l_unstaged,
            &mut self.l_commit,
            &mut self.count_staged,
            &mut self.count_unstaged,
            &mut self.ic_diff,
            &mut self.ic_open,
            &mut self.ic_discard,
            &mut self.ic_stage,
            &mut self.ic_unstage,
            &mut self.ic_refresh,
            &mut self.ic_more,
            &mut self.ic_stash,
            &mut self.ic_tree,
            &mut self.ic_flat,
            &mut self.ic_chevron,
        ] {
            l.reshape(fs);
        }
        for b in &mut self.badges {
            b.reshape(fs);
        }
        self.last_w = -1.0; // force row reflow (ListView re-shapes via shape epoch)
    }

    pub fn set_root(&mut self, root: PathBuf) {
        self.root = root;
    }
    pub fn focused(&self) -> bool {
        self.msg_active
    }
    pub fn set_unfocused(&mut self) {
        self.msg_active = false;
        self.msg.focus(false);
    }
    pub fn clear_message(&mut self, fs: &mut FontSystem) {
        self.msg.clear(fs);
    }
    fn message(&self) -> String {
        self.msg.text().trim().to_string()
    }
    fn nothing_staged(&self) -> bool {
        self.staged_rows.is_empty()
    }

    pub fn refresh(&mut self, fs: &mut FontSystem) {
        self.branch = git::branch(&self.root);
        let changes = git::status(&self.root);
        self.change_count = changes.len();
        self.staged_rows.clear();
        self.unstaged_rows.clear();
        for c in &changes {
            if c.staged != ' ' && c.staged != '?' {
                self.staged_rows.push(Self::row(&c.path, c.staged));
            }
            if c.worktree != ' ' {
                self.unstaged_rows.push(Self::row(&c.path, c.worktree));
            }
        }
        self.count_staged.set(fs, &self.staged_rows.len().to_string(), theme::UI_FAMILY());
        self.count_unstaged.set(fs, &self.unstaged_rows.len().to_string(), theme::UI_FAMILY());
        self.hovered = None;
        self.last_w = -1.0; // force re-ellipsize on the next `update`
        // Shape immediately at the last known width so a refresh isn't blank for a frame.
        let w = if self.last_w > 0.0 { self.last_w } else { theme::SIDEBAR_WIDTH() };
        self.reflow(fs, w);
    }

    fn row(path: &str, code: char) -> Row {
        // `git status --porcelain` reports a fully-untracked directory as a single
        // "dir/" entry (trailing slash). Without trimming it, rsplit('/') returns an
        // empty final segment, so the row renders nameless — the "ghost row" bug.
        let path = path.trim_end_matches(['/', '\\']);
        let fname = path.rsplit(['/', '\\']).next().unwrap_or(path);
        let dir = path[..path.len().saturating_sub(fname.len())].trim_end_matches(['/', '\\']);
        Row {
            path: path.to_string(),
            fname: fname.to_string(),
            dir: dir.to_string(),
            fname_len: fname.len(),
            badge: badge_for(code),
            untracked: code == '?',
            new_file: code == '?' || code == 'A',
        }
    }

    /// Scroll the groups area by a wheel delta when the cursor is over it. Returns
    /// true when the offset changed (caller redraws).
    pub fn on_wheel(&mut self, pt: (f32, f32), region: Rect, dy: f32) -> bool {
        if !Self::groups_viewport(region).contains(pt) {
            return false;
        }
        self.scroll.on_wheel(0.0, dy)
    }

    /// Re-ellipsize + re-shape the rows for the current sidebar width. Called from
    /// the render shape phase; only does work when the width actually changed, so
    /// the ellipsis stays correct as the sidebar is resized.
    pub fn update(&mut self, fs: &mut FontSystem, region: Rect) {
        if (region.w - self.last_w).abs() < 0.5 {
            return;
        }
        self.last_w = region.w;
        self.reflow(fs, region.w);
        // Feed the scroll component the viewport + unscrolled content height (it
        // clamps the offset and sizes the thumb from these).
        let vp = Self::groups_viewport(region);
        let ul = self.unstaged_list(region);
        let content_h = (ul.y + ul.h + self.scroll.offset().1) - Self::groups_top(region) + theme::zpx(8.0);
        self.scroll.set_metrics(vp, (vp.w, content_h));
    }

    fn reflow(&mut self, fs: &mut FontSystem, w: f32) {
        let avail = (w - pad_x() - right_reserve()).max(20.0);
        for rows in [&mut self.staged_rows, &mut self.unstaged_rows] {
            for r in rows.iter_mut() {
                r.fname_len = Self::display(&r.fname, &r.dir, avail).1;
            }
        }
        // Rebuild the visible-item lists (flat or tree) for the current collapse state.
        self.staged_vis = self.build_vis(&self.staged_rows, true);
        self.unstaged_vis = self.build_vis(&self.unstaged_rows, false);
        let sw = theme::SIDEBAR_WIDTH();
        if self.tree_mode {
            let (sk, ss) = Self::tree_spans(&self.staged_vis, &self.staged_rows);
            let (uk, us) = Self::tree_spans(&self.unstaged_vis, &self.unstaged_rows);
            self.staged.set_rich(fs, &sk, &ss, sw, 4000.0);
            self.unstaged.set_rich(fs, &uk, &us, sw, 4000.0);
        } else {
            let key = |rows: &[Row]| {
                rows.iter()
                    .map(|r| Self::display(&r.fname, &r.dir, avail).0)
                    .collect::<Vec<_>>()
                    .join("\n")
            };
            // `set_text` uses the key AS the buffer text, so no prefix here. The
            // tree-mode key is prefixed ("T\n") and so never collides with these,
            // meaning a mode toggle always re-shapes.
            let (sk, uk) = (key(&self.staged_rows), key(&self.unstaged_rows));
            self.staged.set_text(fs, &sk, sw, 4000.0);
            self.unstaged.set_text(fs, &uk, sw, 4000.0);
        }
    }

    /// Build the visible-item list for one group. Flat in list mode; a compacted
    /// folder tree (honoring `collapsed`) in tree mode.
    fn build_vis(&self, rows: &[Row], staged: bool) -> Vec<Vis> {
        if !self.tree_mode {
            return (0..rows.len()).map(|row| Vis::File { row, depth: 0 }).collect();
        }
        let mut root = TNode::default();
        for (i, r) in rows.iter().enumerate() {
            let parts: Vec<&str> = r.path.split('/').filter(|s| !s.is_empty()).collect();
            let n = parts.len();
            let mut node = &mut root;
            for seg in parts.iter().take(n.saturating_sub(1)) {
                node = node.children.entry((*seg).to_string()).or_default();
            }
            node.files.push(i);
        }
        let mut out = Vec::new();
        self.walk(&root, String::new(), 0, staged, rows, &mut out);
        out
    }

    /// DFS the build tree, emitting folder items (single-child chains compacted)
    /// then file items, skipping subtrees under collapsed folders.
    fn walk(&self, node: &TNode, prefix: String, depth: usize, staged: bool, rows: &[Row], out: &mut Vec<Vis>) {
        for (seg, child) in &node.children {
            // Compact a chain of single-child folders ("a" → "a/b/c") like VSCode.
            let mut label = seg.clone();
            let mut cur = child;
            loop {
                if !(cur.files.is_empty() && cur.children.len() == 1) {
                    break;
                }
                let (cseg, cchild) = cur.children.iter().next().unwrap();
                label = format!("{}/{}", label, cseg);
                cur = cchild;
            }
            let full = if prefix.is_empty() { label.clone() } else { format!("{}/{}", prefix, label) };
            // Key is per-group (staged flag) + full folder path so collapse state is stable.
            let key = format!("{}\u{0}{}", staged as u8, full);
            let collapsed = self.collapsed.contains(&key);
            out.push(Vis::Folder { key, label, depth, collapsed });
            if !collapsed {
                self.walk(cur, full, depth + 1, staged, rows, out);
            }
        }
        let mut files = node.files.clone();
        files.sort_by(|&a, &b| rows[a].fname.cmp(&rows[b].fname));
        for row in files {
            out.push(Vis::File { row, depth });
        }
    }

    /// Rich-text spans for the tree: indent + chevron glyph (folders) + name. The
    /// returned key encodes the structure so a collapse/expand re-shapes the buffer.
    fn tree_spans(vis: &[Vis], rows: &[Row]) -> (String, Vec<(String, Attrs<'static>)>) {
        let ui = Attrs::new().family(Family::Name(theme::UI_FAMILY())).color(theme::FG_TEXT());
        let chev = Attrs::new().family(Family::Name(theme::ICON_FAMILY)).color(theme::FG_TEXT());
        // A transparent chevron: occupies the exact same width as a real one so file
        // names align one indent level right of their parent folder (not slightly left).
        let blank = Attrs::new().family(Family::Name(theme::ICON_FAMILY)).color(Color::rgba(0, 0, 0, 0));
        let mut key = String::from("T\n");
        let mut spans: Vec<(String, Attrs<'static>)> = Vec::new();
        for v in vis {
            match v {
                Vis::Folder { label, depth, collapsed, .. } => {
                    spans.push(("  ".repeat(*depth), ui));
                    let g = if *collapsed { theme::ICON_CHEVRON_RIGHT } else { theme::ICON_CHEVRON_DOWN };
                    spans.push((format!("{} ", g), chev));
                    spans.push((format!("{}\n", label), ui));
                    key.push_str(&format!("d{}{}{}\n", depth, *collapsed as u8, label));
                }
                Vis::File { row, depth } => {
                    // Same structure as a folder (indent + twistie-slot + name), but the
                    // twistie is an invisible chevron so the column widths match exactly.
                    spans.push(("  ".repeat(*depth), ui));
                    spans.push((format!("{} ", theme::ICON_CHEVRON_DOWN), blank));
                    spans.push((format!("{}\n", rows[*row].fname), ui));
                    key.push_str(&format!("f{}{}\n", depth, rows[*row].fname));
                }
            }
        }
        (key, spans)
    }

    /// "filename  dir" truncated with an ellipsis to fit `avail` px (leaving room
    /// for the status + hover action icons). Returns the string + byte length of the
    /// bright file-name prefix within it.
    fn display(fname: &str, dir: &str, avail: f32) -> (String, usize) {
        let budget = (avail / (6.5 * theme::ui_zoom())).max(4.0) as usize;
        let full = if dir.is_empty() { fname.to_string() } else { format!("{}  {}", fname, dir) };
        if full.chars().count() <= budget {
            return (full, fname.len());
        }
        if fname.chars().count() + 1 >= budget {
            let t: String = fname.chars().take(budget.saturating_sub(1)).collect();
            let s = format!("{}…", t);
            let len = s.len();
            (s, len)
        } else {
            let t: String = full.chars().take(budget.saturating_sub(1)).collect();
            (format!("{}…", t), fname.len())
        }
    }

    // ---- Geometry ----
    fn changes_hdr(r: Rect) -> Rect {
        let z = theme::ui_zoom();
        Rect { x: r.x + 8.0 * z, y: r.y + 4.0 * z, w: r.w - 12.0 * z, h: row_h() }
    }
    fn msg_rect(r: Rect) -> Rect {
        let z = theme::ui_zoom();
        Rect { x: r.x + 10.0 * z, y: r.y + row_h() + 8.0 * z, w: r.w - 20.0 * z, h: 30.0 * z }
    }
    fn commit_rect(r: Rect) -> Rect {
        let z = theme::ui_zoom();
        let m = Self::msg_rect(r);
        Rect { x: m.x, y: m.y + m.h + 6.0 * z, w: m.w, h: 28.0 * z }
    }
    /// Top of the scrollable groups area (just under the commit button).
    fn groups_top(r: Rect) -> f32 {
        let c = Self::commit_rect(r);
        c.y + c.h + 10.0 * theme::ui_zoom()
    }
    /// Fixed viewport the group headers + file lists scroll within.
    fn groups_viewport(r: Rect) -> Rect {
        let top = Self::groups_top(r);
        Rect { x: r.x, y: top, w: r.w, h: (r.y + r.h - top).max(0.0) }
    }
    fn staged_hdr(&self, r: Rect) -> Rect {
        let z = theme::ui_zoom();
        Rect { x: r.x + 8.0 * z, y: Self::groups_top(r) - self.scroll.offset().1, w: r.w - 16.0 * z, h: row_h() }
    }
    fn staged_list(&self, r: Rect) -> Rect {
        let h = self.staged_hdr(r);
        Rect { x: r.x, y: h.y + row_h(), w: r.w, h: self.staged_vis.len() as f32 * row_h() }
    }
    fn unstaged_hdr(&self, r: Rect) -> Rect {
        let sl = self.staged_list(r);
        Rect { x: r.x + 8.0 * theme::ui_zoom(), y: sl.y + sl.h + 8.0 * theme::ui_zoom(), w: r.w - 16.0 * theme::ui_zoom(), h: row_h() }
    }
    fn unstaged_list(&self, r: Rect) -> Rect {
        let h = self.unstaged_hdr(r);
        Rect { x: r.x, y: h.y + row_h(), w: r.w, h: self.unstaged_vis.len() as f32 * row_h() }
    }

    /// Hover-action icon rects for a row, right-to-left ending before the status.
    fn action_rects(region: Rect, y: f32, staged: bool) -> Vec<(Act, Rect)> {
        let acts: &[Act] = if staged {
            &[Act::Open, Act::Unstage]
        } else {
            &[Act::Open, Act::Discard, Act::Stage]
        };
        Self::rects_for(acts, region, y)
    }

    /// Hover actions on a tree-mode FOLDER row: stage/unstage every file under it
    /// (#34). Discard is deliberately absent — it confirms per file.
    fn folder_action_rects(region: Rect, y: f32, staged: bool) -> Vec<(Act, Rect)> {
        let acts: &[Act] = if staged { &[Act::Unstage] } else { &[Act::Stage] };
        Self::rects_for(acts, region, y)
    }

    fn rects_for(acts: &[Act], region: Rect, y: f32) -> Vec<(Act, Rect)> {
        let end = region.x + region.w - status_w() - 4.0 * theme::ui_zoom();
        let start = end - acts.len() as f32 * action_w();
        acts.iter()
            .enumerate()
            .map(|(i, &a)| (a, Rect { x: start + i as f32 * action_w(), y, w: action_w(), h: row_h() }))
            .collect()
    }

    /// Repo-relative folder path inside a `Vis::Folder` collapse key
    /// (`"{staged}\0{path}"`).
    fn folder_path(key: &str) -> &str {
        key.splitn(2, '\u{0}').nth(1).unwrap_or("")
    }

    /// [stash, tree-toggle, refresh, more] toolbar rects, right-aligned in the
    /// CHANGES header row.
    fn toolbar_rects(r: Rect) -> [Rect; 4] {
        let bw = 22.0 * theme::ui_zoom();
        let ch = Self::changes_hdr(r);
        let more = Rect { x: ch.x + ch.w - bw, y: ch.y, w: bw, h: row_h() };
        let refresh = Rect { x: more.x - bw, ..more };
        let tree = Rect { x: refresh.x - bw, ..more };
        let stash = Rect { x: tree.x - bw, ..more };
        [stash, tree, refresh, more]
    }
    fn commit_main(r: Rect) -> Rect {
        let c = Self::commit_rect(r);
        Rect { w: c.w - 28.0 * theme::ui_zoom(), ..c }
    }
    fn commit_chevron(r: Rect) -> Rect {
        let c = Self::commit_rect(r);
        let cw = 28.0 * theme::ui_zoom();
        Rect { x: c.x + c.w - cw, w: cw, ..c }
    }
    /// Group-header composite actions (left of the count pill, on hover):
    /// Open Changes (all files) first, then Staged → Unstage All; Changes → Discard
    /// All + Stage All.
    fn header_actions(&self, r: Rect, staged: bool) -> Vec<(Act, Rect)> {
        let (hdr, count) = if staged {
            (self.staged_hdr(r), &self.count_staged)
        } else {
            (self.unstaged_hdr(r), &self.count_unstaged)
        };
        let acts: &[Act] = if staged { &[Act::Diff, Act::Unstage] } else { &[Act::Diff, Act::Discard, Act::Stage] };
        let end = hdr.x + hdr.w - (count.width() + theme::zpx(12.0)) - theme::zpx(6.0);
        let start = end - acts.len() as f32 * action_w();
        acts.iter()
            .enumerate()
            .map(|(i, &a)| (a, Rect { x: start + i as f32 * action_w(), y: hdr.y, w: action_w(), h: row_h() }))
            .collect()
    }

    // ---- Drawing ----
    pub fn draw_quads(&self, region: Rect, blink: bool, now: std::time::Instant, bg: &mut Vec<Quad>, fg: &mut Vec<Quad>) {
        let m = Self::msg_rect(region);
        let ir = theme::zpx(7.0);
        let border = Rect { x: m.x - 1.0, y: m.y - 1.0, w: m.w + 2.0, h: m.h + 2.0 };
        bg.push(border.rounded_quad(theme::SEARCH_BORDER(), ir + 1.0));
        bg.push(m.rounded_quad(theme::SEARCH_BG(), ir));
        if self.msg_active {
            self.msg.selection_quads(m, theme::zpx(6.0), bg);
            if blink {
                fg.push(self.msg.caret_quad(m, theme::zpx(6.0)));
            }
        }
        bg.push(Self::commit_rect(region).rounded_quad(theme::ACCENT_DIM(), theme::zpx(5.0)));
        // Split divider between the Commit label and its dropdown chevron.
        let cc = Self::commit_chevron(region);
        bg.push(Quad::new(cc.x, cc.y + theme::zpx(5.0), 1.0, cc.h - theme::zpx(10.0), [0.0, 0.0, 0.0, 0.25]));
        for (hdr, label, empty) in [
            (self.staged_hdr(region), &self.count_staged, self.staged_vis.is_empty()),
            (self.unstaged_hdr(region), &self.count_unstaged, self.unstaged_vis.is_empty()),
        ] {
            if empty {
                continue; // no count badge for an empty group (VSCode-style)
            }
            let w = label.width() + theme::zpx(12.0);
            let pill = Rect { x: hdr.x + hdr.w - w, y: hdr.y + theme::zpx(3.0), w, h: row_h() - theme::zpx(6.0) };
            // Pills scroll with their headers — drop them once they leave the
            // viewport instead of floating over the toolbar/commit button.
            let vp = Self::groups_viewport(region);
            if pill.y >= vp.y && pill.y + pill.h <= vp.y + vp.h {
                bg.push(pill.rounded_quad(theme::ACCENT_DIM(), pill.h * 0.5));
            }
        }
        // Hovered-row highlight (so the action icons read as part of an active row).
        if let Some((staged, idx)) = self.hovered {
            let lr = if staged { self.staged_list(region) } else { self.unstaged_list(region) };
            let y = lr.y + idx as f32 * row_h();
            if let Some(r) = vclip(Rect { x: region.x, y, w: region.w, h: row_h() }, Self::groups_viewport(region)) {
                bg.push(Quad::new(r.x, r.y, r.w, r.h, theme::TREE_HOVER()));
            }
        }
        // Tree-mode indent guides: a faint vertical line per ancestor level, drawn
        // per row so consecutive rows form continuous lines (like VSCode).
        if self.tree_mode {
            let vp = Self::groups_viewport(region);
            self.indent_guides(&self.staged, &self.staged_vis, self.staged_list(region), vp, bg);
            self.indent_guides(&self.unstaged, &self.unstaged_vis, self.unstaged_list(region), vp, bg);
        }
        // Auto-hiding scrollbar for the groups area.
        self.scroll.draw(now, fg);
    }

    fn vis_depth(v: &Vis) -> usize {
        match v {
            Vis::Folder { depth, .. } | Vis::File { depth, .. } => *depth,
        }
    }

    /// The chevron (twistie) center x per depth, measured from the shaped buffer so
    /// guides land exactly on each level's twistie at any zoom. Index = depth; the
    /// chevron occupies the 3 bytes right after the `2*depth` indent spaces.
    fn depth_centers(list: &ListView, vis: &[Vis]) -> Vec<f32> {
        let mut centers: Vec<f32> = Vec::new();
        for (i, v) in vis.iter().enumerate() {
            let d = Self::vis_depth(v);
            if centers.len() > d && centers[d] >= 0.0 {
                continue; // already measured this depth
            }
            let start = 2 * d; // leading indent is 2 spaces per depth
            if let Some((x0, x1)) = list.line_x_range(i, start, start + 3) {
                while centers.len() <= d {
                    centers.push(-1.0);
                }
                centers[d] = (x0 + x1) * 0.5;
            }
        }
        centers
    }

    fn indent_guides(&self, list: &ListView, vis: &[Vis], lr: Rect, vp: Rect, bg: &mut Vec<Quad>) {
        if vis.is_empty() {
            return;
        }
        // Anchor to the first level's measured twistie center, then step by one
        // consistent indent unit — so guides are evenly spaced by construction (no
        // per-depth measurement jitter or gaps).
        let centers = Self::depth_centers(list, vis);
        let base = centers.iter().copied().find(|&c| c >= 0.0).unwrap_or(7.0 * theme::ui_zoom());
        let unit = match (centers.first(), centers.get(1)) {
            (Some(&a), Some(&b)) if a >= 0.0 && b >= 0.0 => b - a,
            _ => 13.0 * theme::ui_zoom(),
        };
        let origin = lr.x + list.pad_x();
        let w = theme::zpx(1.0).max(1.0);
        let color = [0.50, 0.50, 0.58, 0.7];
        for (i, v) in vis.iter().enumerate() {
            let depth = Self::vis_depth(v);
            let y = lr.y + i as f32 * row_h();
            // One guide per ancestor level, evenly spaced from the anchor. Snap x to a
            // whole pixel so the 1px line stays crisp (no anti-aliased smear).
            for k in 0..depth {
                let x = (origin + base + k as f32 * unit).round();
                if let Some(r) = vclip(Rect { x, y, w, h: row_h() }, vp) {
                    bg.push(Quad::new(r.x, r.y, r.w, r.h, color));
                }
            }
        }
    }

    pub fn draw_text<'b>(&'b self, region: Rect, areas: &mut Vec<TextArea<'b>>) {
        let ch = Self::changes_hdr(region);
        self.l_changes.push(ch.x, ch, theme::FG_DIM(), areas);
        // Top toolbar: stash · tree/list toggle · refresh · more.
        let [stash, tree, refresh, more] = Self::toolbar_rects(region);
        self.push_icon(&self.ic_stash, stash, theme::FG_DIM(), areas);
        let toggle = if self.tree_mode { &self.ic_flat } else { &self.ic_tree };
        self.push_icon(toggle, tree, theme::FG_DIM(), areas);
        self.push_icon(&self.ic_refresh, refresh, theme::FG_DIM(), areas);
        self.push_icon(&self.ic_more, more, theme::FG_DIM(), areas);

        let m = Self::msg_rect(region);
        let mc = if self.msg.text().is_empty() { theme::FG_DIM() } else { theme::FG_TEXT() };
        self.msg.draw(m, theme::zpx(6.0), mc, areas);
        // Commit split button: label centered in the main part + dropdown chevron.
        let cm = Self::commit_main(region);
        self.l_commit.push(cm.x + (cm.w - self.l_commit.width()) * 0.5, cm, theme::FG_TEXT(), areas);
        self.push_icon(&self.ic_chevron, Self::commit_chevron(region), theme::FG_TEXT(), areas);

        let vp = Self::groups_viewport(region);
        let sh = self.staged_hdr(region);
        self.l_staged.push_in(sh.x, sh, vp, theme::FG_TEXT(), areas);
        if !self.staged_vis.is_empty() {
            self.push_count(&self.count_staged, sh, vp, areas);
        }
        if self.hovered_header == Some(true) {
            self.draw_header_actions(region, true, areas);
        }
        let sh_idx = match self.hovered {
            Some((true, i)) => Some(i),
            _ => None,
        };
        self.draw_rows(&self.staged, &self.staged_vis, &self.staged_rows, self.staged_list(region), vp, true, sh_idx, areas);

        let uh = self.unstaged_hdr(region);
        self.l_unstaged.push_in(uh.x, uh, vp, theme::FG_TEXT(), areas);
        if !self.unstaged_vis.is_empty() {
            self.push_count(&self.count_unstaged, uh, vp, areas);
        }
        if self.hovered_header == Some(false) {
            self.draw_header_actions(region, false, areas);
        }
        let uh_idx = match self.hovered {
            Some((false, i)) => Some(i),
            _ => None,
        };
        self.draw_rows(&self.unstaged, &self.unstaged_vis, &self.unstaged_rows, self.unstaged_list(region), vp, false, uh_idx, areas);
    }

    fn push_icon<'b>(&self, label: &'b TextLabel, r: Rect, color: glyphon::Color, areas: &mut Vec<TextArea<'b>>) {
        label.push(r.x + (r.w - label.width()) * 0.5, r, color, areas);
    }

    /// `push_icon`, clipped to the scrollable groups viewport.
    fn push_icon_in<'b>(&self, label: &'b TextLabel, r: Rect, vp: Rect, color: glyphon::Color, areas: &mut Vec<TextArea<'b>>) {
        label.push_in(r.x + (r.w - label.width()) * 0.5, r, vp, color, areas);
    }

    fn draw_header_actions<'b>(&'b self, region: Rect, staged: bool, areas: &mut Vec<TextArea<'b>>) {
        let vp = Self::groups_viewport(region);
        for (act, ar) in self.header_actions(region, staged) {
            self.push_icon_in(self.icon_for(act), ar, vp, theme::FG_TEXT(), areas);
        }
    }

    fn push_count<'b>(&self, label: &'b TextLabel, hdr: Rect, vp: Rect, areas: &mut Vec<TextArea<'b>>) {
        let w = label.width() + theme::zpx(12.0);
        let pill = Rect { x: hdr.x + hdr.w - w, y: hdr.y, w, h: row_h() };
        // Bright text on the count pill (FG_DIM was unreadable on the dark-blue pill).
        label.push_in(pill.x + (pill.w - label.width()) * 0.5, pill, vp, theme::FG_TEXT(), areas);
    }

    fn icon_for(&self, act: Act) -> &TextLabel {
        match act {
            Act::Diff => &self.ic_diff,
            Act::Open => &self.ic_open,
            Act::Discard => &self.ic_discard,
            Act::Stage => &self.ic_stage,
            Act::Unstage => &self.ic_unstage,
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn draw_rows<'b>(
        &'b self,
        list: &'b ListView,
        vis: &[Vis],
        rows: &[Row],
        region: Rect,
        vp: Rect,
        staged: bool,
        hovered_idx: Option<usize>,
        areas: &mut Vec<TextArea<'b>>,
    ) {
        if vis.is_empty() {
            return;
        }
        // Clip the row text to leave the status/action column clear, intersected with
        // the scroll viewport so scrolled rows can't bleed over the commit button.
        let unclipped = Rect { w: (region.w - status_w() - theme::zpx(4.0)).max(0.0), ..region };
        let Some(text_clip) = vclip(unclipped, vp) else { return };
        // Tree mode embeds its colors in the rich spans; list mode draws dim then a
        // bright band over each file-name prefix.
        let base = if self.tree_mode { theme::FG_TEXT() } else { theme::FG_DIM() };
        list.draw_at(text_clip, region.y, base, areas);
        let pad = list.pad_x();
        for (i, v) in vis.iter().enumerate() {
            let y = region.y + i as f32 * row_h();
            // Folder rows: stage/unstage-all icons on hover (#34); no badge.
            if matches!(v, Vis::Folder { .. }) {
                if hovered_idx == Some(i) {
                    for (act, ar) in Self::folder_action_rects(region, y, staged) {
                        let lbl = self.icon_for(act);
                        lbl.push_in(ar.x + (ar.w - lbl.width()) * 0.5, ar, vp, theme::FG_TEXT(), areas);
                    }
                }
                continue;
            }
            let Vis::File { row, .. } = v else { continue };
            let r = &rows[*row];
            if !self.tree_mode {
                // Bright file-name prefix (same origin, clipped to its width).
                if let Some((_x0, x1)) = list.line_x_range(i, 0, r.fname_len) {
                    let w = (pad + x1).min(text_clip.w);
                    if let Some(band) = vclip(Rect { x: region.x, y, w, h: row_h() }, vp) {
                        list.draw_at(band, region.y, theme::FG_TEXT(), areas);
                    }
                }
            }
            // Status letter at the far right.
            let (rr, gg, bb) = BADGE_RGB[r.badge];
            let st = Rect { x: region.x + region.w - status_w(), y, w: status_w(), h: row_h() };
            self.badges[r.badge].push_in(st.x, st, vp, Color::rgb(rr, gg, bb), areas);
            // Hover actions for this row.
            if hovered_idx == Some(i) {
                for (act, ar) in Self::action_rects(region, y, staged) {
                    let lbl = self.icon_for(act);
                    lbl.push_in(ar.x + (ar.w - lbl.width()) * 0.5, ar, vp, theme::FG_TEXT(), areas);
                }
            }
        }
    }

    // ---- Input ----
    pub fn hover(&mut self, pt: (f32, f32), region: Rect) -> bool {
        let in_groups = Self::groups_viewport(region).contains(pt);
        let sl = self.staged_list(region);
        let ul = self.unstaged_list(region);
        let new = if !in_groups {
            None
        } else if sl.contains(pt) {
            self.staged.row_at(sl, pt, self.staged_vis.len()).map(|i| (true, i))
        } else if ul.contains(pt) {
            self.unstaged.row_at(ul, pt, self.unstaged_vis.len()).map(|i| (false, i))
        } else {
            None
        };
        let new_hdr = if !in_groups {
            None
        } else if self.staged_hdr(region).contains(pt) {
            Some(true)
        } else if self.unstaged_hdr(region).contains(pt) {
            Some(false)
        } else {
            None
        };
        let changed = new != self.hovered || new_hdr != self.hovered_header;
        self.hovered = new;
        self.hovered_header = new_hdr;
        changed
    }

    /// The file row under `pt` as (repo-relative path, in-staged-group, untracked) —
    /// drives the right-click context menu.
    pub fn row_at_point(&self, pt: (f32, f32), region: Rect) -> Option<(String, bool, bool)> {
        if !Self::groups_viewport(region).contains(pt) {
            return None;
        }
        for staged in [true, false] {
            let (lr, vis, rows, list): (Rect, &[Vis], &[Row], &ListView) = if staged {
                (self.staged_list(region), &self.staged_vis, &self.staged_rows, &self.staged)
            } else {
                (self.unstaged_list(region), &self.unstaged_vis, &self.unstaged_rows, &self.unstaged)
            };
            if !lr.contains(pt) {
                continue;
            }
            if let Some(i) = list.row_at(lr, pt, vis.len()) {
                if let Vis::File { row, .. } = &vis[i] {
                    let r = &rows[*row];
                    return Some((r.path.clone(), staged, r.untracked));
                }
            }
            return None;
        }
        None
    }

    pub fn on_press(&mut self, pt: (f32, f32), region: Rect, out: &mut Vec<Intent>) -> bool {
        // The groups scrollbar claims its presses (thumb drag / track jump).
        if self.scroll.press(pt) {
            return true;
        }
        if Self::msg_rect(region).contains(pt) {
            self.msg_active = true;
            self.msg.focus(true);
            self.msg.set_caret_from_x(Self::msg_rect(region), theme::zpx(6.0), pt.0);
            return true;
        }
        // Top toolbar.
        let [stash, tree, refresh, more] = Self::toolbar_rects(region);
        if stash.contains(pt) {
            out.push(Intent::GitStash);
            return true;
        }
        if tree.contains(pt) {
            self.tree_mode = !self.tree_mode;
            self.last_w = -1.0; // force a reflow into the new mode on the next frame
            return true;
        }
        if refresh.contains(pt) {
            out.push(Intent::GitRefresh);
            return true;
        }
        if more.contains(pt) {
            return true; // "More actions" menu — TODO
        }
        // Commit split button: chevron = Commit & Push, main = Commit.
        if Self::commit_chevron(region).contains(pt) {
            let msg = self.message();
            if !msg.is_empty() {
                out.push(Intent::GitCommitPush { msg, stage_all: self.nothing_staged() });
            }
            return true;
        }
        if Self::commit_main(region).contains(pt) {
            let msg = self.message();
            if !msg.is_empty() {
                out.push(Intent::GitCommit { msg, stage_all: self.nothing_staged() });
            }
            return true;
        }
        // Everything below lives in the scrollable groups area.
        if !Self::groups_viewport(region).contains(pt) {
            return false;
        }
        // Group-header composite actions (hit-tested by position; they only draw on
        // hover, but a click lands on the pointer, so don't gate on hover state).
        for staged in [true, false] {
            for (act, ar) in self.header_actions(region, staged) {
                if ar.contains(pt) {
                    out.push(match act {
                        Act::Diff => Intent::OpenAllDiffs { staged },
                        Act::Stage => Intent::GitStageAll,
                        Act::Unstage => Intent::GitUnstageAll,
                        _ => Intent::GitDiscardAll,
                    });
                    return true;
                }
            }
        }
        // Row hit: resolve into either a folder toggle (deferred so we don't mutate
        // `self` while a `&self` borrow of the vis/rows is live) or file actions.
        let mut toggle: Option<String> = None;
        let mut handled = false;
        for staged in [true, false] {
            let (lr, vis, rows, list): (Rect, &[Vis], &[Row], &ListView) = if staged {
                (self.staged_list(region), &self.staged_vis, &self.staged_rows, &self.staged)
            } else {
                (self.unstaged_list(region), &self.unstaged_vis, &self.unstaged_rows, &self.unstaged)
            };
            if !lr.contains(pt) {
                continue;
            }
            if let Some(i) = list.row_at(lr, pt, vis.len()) {
                match &vis[i] {
                    Vis::Folder { key, .. } => {
                        // Action icons first: stage/unstage every file under the
                        // folder (#34); anywhere else toggles the collapse.
                        let y = lr.y + i as f32 * row_h();
                        let mut acted = false;
                        for (act, ar) in Self::folder_action_rects(lr, y, staged) {
                            if ar.contains(pt) {
                                let dir = format!("{}/", Self::folder_path(key));
                                for r in rows.iter().filter(|r| r.path.starts_with(&dir)) {
                                    match act {
                                        Act::Stage => out.push(Intent::GitStage(r.path.clone())),
                                        Act::Unstage => out.push(Intent::GitUnstage(r.path.clone())),
                                        _ => {}
                                    }
                                }
                                acted = true;
                                break;
                            }
                        }
                        if !acted {
                            toggle = Some(key.clone());
                        }
                    }
                    Vis::File { row, .. } => {
                        let r = &rows[*row];
                        let y = lr.y + i as f32 * row_h();
                        let mut acted = false;
                        for (act, ar) in Self::action_rects(lr, y, staged) {
                            if ar.contains(pt) {
                                let p = r.path.clone();
                                match act {
                                    Act::Diff => out.push(Intent::OpenDiff { path: p, staged, untracked: r.untracked }),
                                    Act::Open => out.push(Intent::OpenFile { path: self.root.join(&p), line: 1, col: 0 }),
                                    Act::Stage => out.push(Intent::GitStage(p)),
                                    Act::Unstage => out.push(Intent::GitUnstage(p)),
                                    Act::Discard => out.push(Intent::GitDiscard { path: p, untracked: r.untracked }),
                                }
                                acted = true;
                                break;
                            }
                        }
                        if !acted {
                            // Row body opens the diff (VSCode opens the diff on row click) —
                            // except a newly created file (untracked or staged-added) has no
                            // prior version, so a diff is pointless: open the file itself. The
                            // per-row "Open Changes" icon still forces a diff if wanted.
                            if r.new_file {
                                out.push(Intent::OpenFile { path: self.root.join(&r.path), line: 1, col: 0 });
                            } else {
                                out.push(Intent::OpenDiff { path: r.path.clone(), staged, untracked: r.untracked });
                            }
                        }
                    }
                }
            }
            handled = true;
            break;
        }
        if let Some(k) = toggle {
            // Toggle this folder's collapse state, then force a reflow next frame.
            if !self.collapsed.remove(&k) {
                self.collapsed.insert(k);
            }
            self.last_w = -1.0;
            return true;
        }
        if handled {
            return true;
        }
        self.set_unfocused();
        true
    }

    pub fn on_key(
        &mut self,
        event: &winit::event::KeyEvent,
        ctrl: bool,
        shift: bool,
        fs: &mut FontSystem,
        clip: Option<&mut arboard::Clipboard>,
        out: &mut Vec<Intent>,
    ) -> bool {
        use winit::keyboard::{Key, NamedKey};
        if !self.msg_active {
            return false;
        }
        match event.logical_key.as_ref() {
            Key::Named(NamedKey::Escape) => {
                self.set_unfocused();
                return true;
            }
            Key::Named(NamedKey::Enter) if ctrl => {
                let msg = self.message();
                if !msg.is_empty() {
                    out.push(Intent::GitCommit { msg, stage_all: self.nothing_staged() });
                }
                return true;
            }
            _ => {}
        }
        match crate::edit_input(&mut self.msg, fs, clip, event, ctrl, shift) {
            Some(_) => true,
            None => !ctrl,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    // These tests read/mutate the global UI zoom; serialize them so the 2x-zoom test
    // can't change zoom out from under another running in parallel.
    static ZOOM_LOCK: Mutex<()> = Mutex::new(());
    fn zoom_guard() -> std::sync::MutexGuard<'static, ()> {
        ZOOM_LOCK.lock().unwrap_or_else(|e| e.into_inner())
    }

    fn panel() -> (SourceControlPanel, FontSystem) {
        let mut fs = FontSystem::new();
        let p = SourceControlPanel::new(&mut fs, PathBuf::from("/tmp/repo"));
        (p, fs)
    }

    // Clicking the per-row Stage (+) icon on an unstaged file must emit GitStage for
    // THAT file — not fall through to the row-body OpenDiff.
    #[test]
    fn unstaged_stage_icon_emits_git_stage() {
        let _z = zoom_guard();
        let (mut p, mut fs) = panel();
        p.unstaged_rows = vec![make_row("src/main.rs", 'M')];
        let region = Rect { x: 0.0, y: 0.0, w: 300.0, h: 1000.0 };
        p.update(&mut fs, region); // builds vis + shapes rows for this width

        let lr = p.unstaged_list(region);
        let y = lr.y; // first (only) row
        let stage = SourceControlPanel::action_rects(lr, y, false)
            .into_iter()
            .find(|(a, _)| matches!(a, Act::Stage))
            .expect("stage action present")
            .1;
        let pt = (stage.x + stage.w * 0.5, stage.y + stage.h * 0.5);

        let mut out = Vec::new();
        let consumed = p.on_press(pt, region, &mut out);
        assert!(consumed, "press should be consumed");
        assert!(
            matches!(out.as_slice(), [Intent::GitStage(path)] if path == "src/main.rs"),
            "expected GitStage(src/main.rs), got {} intents",
            out.len()
        );
    }

    // Clicking the per-row Unstage (−) icon on a staged file must emit GitUnstage.
    #[test]
    fn staged_unstage_icon_emits_git_unstage() {
        let _z = zoom_guard();
        let (mut p, mut fs) = panel();
        p.staged_rows = vec![make_row("src/main.rs", 'M')];
        let region = Rect { x: 0.0, y: 0.0, w: 300.0, h: 1000.0 };
        p.update(&mut fs, region);

        let lr = p.staged_list(region);
        let y = lr.y;
        let unstage = SourceControlPanel::action_rects(lr, y, true)
            .into_iter()
            .find(|(a, _)| matches!(a, Act::Unstage))
            .expect("unstage action present")
            .1;
        let pt = (unstage.x + unstage.w * 0.5, unstage.y + unstage.h * 0.5);

        let mut out = Vec::new();
        let consumed = p.on_press(pt, region, &mut out);
        assert!(consumed, "press should be consumed");
        assert!(
            matches!(out.as_slice(), [Intent::GitUnstage(path)] if path == "src/main.rs"),
            "expected GitUnstage(src/main.rs), got {} intents",
            out.len()
        );
    }

    // Same as the flat-mode test, but in tree mode with a nested path so a folder
    // row sits above the file — the file's vis index is offset by the folder rows.
    #[test]
    fn tree_mode_stage_icon_emits_git_stage() {
        let _z = zoom_guard();
        let (mut p, mut fs) = panel();
        p.tree_mode = true;
        p.unstaged_rows = vec![make_row("src/ui/main.rs", 'M')];
        let region = Rect { x: 0.0, y: 0.0, w: 300.0, h: 1000.0 };
        p.update(&mut fs, region);

        // Find the file's vis index (folders are emitted before it).
        let file_i = p
            .unstaged_vis
            .iter()
            .position(|v| matches!(v, Vis::File { .. }))
            .expect("file row present");
        let lr = p.unstaged_list(region);
        let y = lr.y + file_i as f32 * row_h();
        let stage = SourceControlPanel::action_rects(lr, y, false)
            .into_iter()
            .find(|(a, _)| matches!(a, Act::Stage))
            .expect("stage action present")
            .1;
        let pt = (stage.x + stage.w * 0.5, stage.y + stage.h * 0.5);

        let mut out = Vec::new();
        let consumed = p.on_press(pt, region, &mut out);
        assert!(consumed, "press should be consumed");
        assert!(
            matches!(out.as_slice(), [Intent::GitStage(path)] if path == "src/ui/main.rs"),
            "expected GitStage(src/ui/main.rs), got {} intents",
            out.len()
        );
    }

    // The user runs at a scaled UI zoom. Verify the per-row action hit-test still
    // lands on the right row + icon at zoom 2.0, including a NON-first row (where any
    // drift between ListView::row_at and action_rects would surface). Resets zoom so
    // it doesn't leak into the other tests.
    #[test]
    fn stage_icon_hits_at_zoom_2x() {
        let _z = zoom_guard();
        theme::set_ui_zoom(2.0);
        let mut fs = FontSystem::new();
        let mut p = SourceControlPanel::new(&mut fs, PathBuf::from("/tmp/repo"));
        p.unstaged_rows = vec![make_row("a.rs", 'M'), make_row("b.rs", 'M'), make_row("c.rs", 'M')];
        let region = Rect { x: 0.0, y: 0.0, w: 320.0, h: 1200.0 };
        p.update(&mut fs, region);

        // Target the SECOND file (vis index 1 in flat mode).
        let lr = p.unstaged_list(region);
        let y = lr.y + 1.0 * row_h();
        let stage = SourceControlPanel::action_rects(lr, y, false)
            .into_iter()
            .find(|(a, _)| matches!(a, Act::Stage))
            .unwrap()
            .1;
        let pt = (stage.x + stage.w * 0.5, stage.y + stage.h * 0.5);

        let mut out = Vec::new();
        let consumed = p.on_press(pt, region, &mut out);
        theme::set_ui_zoom(1.0); // restore before asserting (so a panic still resets)

        assert!(consumed, "press should be consumed at zoom 2x");
        assert!(
            matches!(out.as_slice(), [Intent::GitStage(path)] if path == "b.rs"),
            "expected GitStage(b.rs) at zoom 2x, got {} intents",
            out.len()
        );
    }

    // A fully-untracked directory arrives as "dir/" — the row must show the directory
    // name, not a blank "ghost row".
    #[test]
    fn untracked_directory_has_a_name() {
        let r = make_row("scripts/", '?');
        assert_eq!(r.fname, "scripts");
        assert!(r.fname_len > 0, "ghost row: empty file name for an untracked dir");
        assert_eq!(r.path, "scripts", "path keeps the dir name (no trailing slash)");
        assert!(r.untracked);

        // Nested untracked dir keeps its parent path for the tree, name = leaf.
        let r2 = make_row("a/b/", '?');
        assert_eq!(r2.fname, "b");
        assert_eq!(r2.dir, "a");
    }

    // Clicking the stage (+) icon on a tree-mode FOLDER row stages every file
    // under that folder (#34); clicking the row body still toggles the collapse.
    #[test]
    fn folder_stage_icon_stages_all_descendants() {
        let _z = zoom_guard();
        let (mut p, mut fs) = panel();
        p.tree_mode = true;
        p.unstaged_rows = vec![
            make_row("src/a.rs", 'M'),
            make_row("src/b.rs", 'M'),
            make_row("README.md", 'M'),
        ];
        let region = Rect { x: 0.0, y: 0.0, w: 300.0, h: 1000.0 };
        p.update(&mut fs, region);

        // Vis order: folder "src" (idx 0), its two files, then README at root.
        let lr = p.unstaged_list(region);
        let y = lr.y; // folder row
        let stage = SourceControlPanel::folder_action_rects(lr, y, false)
            .into_iter()
            .find(|(a, _)| matches!(a, Act::Stage))
            .expect("folder stage action present")
            .1;
        let pt = (stage.x + stage.w * 0.5, stage.y + stage.h * 0.5);

        let mut out = Vec::new();
        let consumed = p.on_press(pt, region, &mut out);
        assert!(consumed, "press should be consumed");
        let staged: Vec<&str> = out
            .iter()
            .filter_map(|i| match i {
                Intent::GitStage(p) => Some(p.as_str()),
                _ => None,
            })
            .collect();
        assert_eq!(staged, ["src/a.rs", "src/b.rs"], "stages exactly the folder's files");

        // Clicking the folder row BODY (left of the icons) toggles collapse instead.
        let body_pt = (lr.x + 10.0, lr.y + row_h() * 0.5);
        let mut out2 = Vec::new();
        p.on_press(body_pt, region, &mut out2);
        assert!(out2.is_empty(), "body click emits no intents");
        assert!(!p.collapsed.is_empty(), "body click collapsed the folder");
    }

    fn make_row(path: &str, code: char) -> Row {
        SourceControlPanel::row(path, code)
    }
}
