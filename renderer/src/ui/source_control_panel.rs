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

use glyphon::{Color, FontSystem, TextArea};

use crate::git;
use crate::quad::Quad;
use crate::theme;
use crate::ui::Intent;
use crate::widgets::{IconButton, IconList, IconRow, Rect, ScrollOpts, ScrollView, TextInput, TextLabel};

// Geometry is reactive, not frozen: row height tracks the sidebar's (which scales
// with UI zoom) and the paddings scale with zoom too, so the whole panel stays
// proportional at any zoom — no hardcoded pixel sizes.
fn row_h() -> f32 { theme::TREE_ROW_HEIGHT() }
fn pad_x() -> f32 { 30.0 * theme::ui_zoom() } // row text indent
fn hdr_chev_w() -> f32 { 18.0 * theme::ui_zoom() } // collapse chevron column on a group header
fn status_w() -> f32 { 22.0 * theme::ui_zoom() } // status-letter column at the right edge
fn action_w() -> f32 { 20.0 * theme::ui_zoom() } // per hover-action icon
// Text never grows under the status letter + the (up to 3) hover-action icons.

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
    Stash, // group header: stash all working changes
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
    staged: IconList,
    unstaged: IconList,
    staged_rows: Vec<Row>,
    unstaged_rows: Vec<Row>,
    staged_vis: Vec<Vis>,
    unstaged_vis: Vec<Vis>,
    tree_mode: bool,
    staged_open: bool,   // "Staged Changes" group expanded
    unstaged_open: bool, // "Changes" group expanded
    chev_down: IconButton,
    chev_right: IconButton,
    collapsed: HashSet<String>, // keyed folders (per group, see `walk`)
    last_w: f32, // sidebar width the rows were last ellipsized for (-1 = stale)
    hovered: Option<(bool, usize)>, // (is_staged_group, visible-item index)
    selected: Option<(bool, usize)>, // clicked/highlighted row (persists; opens diff too)
    hover_reflowed: Option<(bool, usize)>, // hovered row the buffer was last re-truncated for
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
    ic_sparkle: TextLabel,
    /// AI commit-message generation is in flight (✨ shows a pending state, and we
    /// drop a stale result if the user typed/committed meanwhile).
    pub generating: bool,
    /// When the current generation started — drives the pulsing-border phase.
    gen_since: Option<std::time::Instant>,
    /// A drag-select inside the commit message box is in progress.
    msg_dragging: bool,
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
        // Multiline so long messages wrap (and scroll) instead of overflowing one
        // line. Base wrap width ~ the sidebar content width (scaled by zoom at draw).
        let mut msg = TextInput::new(fs, 240.0, 30.0).multiline(true);
        msg.set_placeholder(fs, " Message (Ctrl+Enter to commit)");
        Self {
            msg,
            msg_active: false,
            // Wider indent (4 spaces/level) so each level's icon clears the indent
            // guide lines drawn in tree mode (2 spaces left icons spilling over them).
            staged: IconList::new(fs, theme::SIDEBAR_WIDTH(), 4000.0, row_h(), pad_x()).with_indent(4),
            unstaged: IconList::new(fs, theme::SIDEBAR_WIDTH(), 4000.0, row_h(), pad_x()).with_indent(4),
            staged_rows: Vec::new(),
            unstaged_rows: Vec::new(),
            staged_vis: Vec::new(),
            unstaged_vis: Vec::new(),
            selected: None,
            hover_reflowed: None,
            tree_mode: false,
            staged_open: true,
            unstaged_open: true,
            chev_down: IconButton::new(fs, theme::ICON_CHEVRON_DOWN, theme::ICON_FAMILY, theme::UI_FONT_SIZE()),
            chev_right: IconButton::new(fs, theme::ICON_CHEVRON_RIGHT, theme::ICON_FAMILY, theme::UI_FONT_SIZE()),
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
            ic_sparkle: icon(fs, theme::ICON_SPARKLE),
            generating: false,
            gen_since: None,
            msg_dragging: false,
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
            &mut self.ic_sparkle,
        ] {
            l.reshape(fs);
        }
        for b in &mut self.badges {
            b.reshape(fs);
        }
        self.staged.reshape_icons(fs);
        self.unstaged.reshape_icons(fs);
        self.chev_down.reshape(fs);
        self.chev_right.reshape(fs);
        self.last_w = -1.0; // force row reflow (IconList re-shapes via shape epoch)
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
    /// Mark an AI commit-message generation as in flight (✨ dims, button ignores
    /// further clicks until a result arrives).
    pub fn begin_generating(&mut self) {
        self.generating = true;
        self.gen_since = Some(std::time::Instant::now());
    }
    /// Deliver a generated commit message (or just clear the pending state on
    /// failure). Replaces whatever is in the box and re-wraps it.
    pub fn set_generated_message(&mut self, fs: &mut FontSystem, msg: Option<&str>) {
        self.generating = false;
        self.gen_since = None;
        if let Some(text) = msg {
            self.msg.set_text(fs, text);
        }
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
        self.set_selected(None); // vis indices change on refresh; drop the highlight
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
        // Wheel over the commit message box scrolls the message (it can hold more
        // than the visible ~3 lines); elsewhere it scrolls the groups area.
        if Self::msg_rect(region).contains(pt) {
            self.msg.scroll_by(-dy);
            return true;
        }
        if !Self::groups_viewport(region).contains(pt) {
            return false;
        }
        self.scroll.on_wheel(0.0, dy)
    }

    /// Re-ellipsize + re-shape the rows for the current sidebar width. Called from
    /// the render shape phase; only does work when the width actually changed, so
    /// the ellipsis stays correct as the sidebar is resized.
    pub fn update(&mut self, fs: &mut FontSystem, region: Rect) {
        // Keep the commit box's wrap width matched to its drawn width (minus the
        // text padding) so messages wrap at the box edge instead of overflowing.
        let box_w = (Self::msg_rect(region).w - 2.0 * theme::zpx(6.0)).max(20.0);
        self.msg.set_wrap_width(fs, box_w);
        // Reflow on a width change OR when the hovered row changes — the hovered row
        // is re-truncated to make room for its action icons, so its label differs.
        if (region.w - self.last_w).abs() < 0.5 && self.hovered == self.hover_reflowed {
            return;
        }
        self.last_w = region.w;
        self.hover_reflowed = self.hovered;
        self.reflow(fs, region.w);
        // Feed the scroll component the viewport + unscrolled content height (it
        // clamps the offset and sizes the thumb from these).
        let vp = Self::groups_viewport(region);
        let ul = self.unstaged_list(region);
        let content_h = (ul.y + ul.h + self.scroll.offset().1) - Self::groups_top(region) + theme::zpx(8.0);
        self.scroll.set_metrics(vp, (vp.w, content_h));
    }

    fn reflow(&mut self, fs: &mut FontSystem, w: f32) {
        // Names use the full row width, reserving only the status-letter column — so
        // they're shown in full and ellipsized only when they actually overflow the
        // panel (not pre-truncated to make room for the on-hover action icons).
        let avail = (w - pad_x() - status_w() - 8.0 * theme::ui_zoom()).max(20.0);
        for rows in [&mut self.staged_rows, &mut self.unstaged_rows] {
            for r in rows.iter_mut() {
                r.fname_len = Self::display(&r.fname, &r.dir, avail).1;
            }
        }
        // Rebuild the visible-item lists (flat or tree) for the current collapse state.
        self.staged_vis = self.build_vis(&self.staged_rows, true);
        self.unstaged_vis = self.build_vis(&self.unstaged_rows, false);
        let sw = theme::SIDEBAR_WIDTH();
        let s_hover = match self.hovered { Some((true, i)) => Some(i), _ => None };
        let u_hover = match self.hovered { Some((false, i)) => Some(i), _ => None };
        let (sk, ss) = self.vis_rows(&self.staged_vis, &self.staged_rows, avail, s_hover);
        let (uk, us) = self.vis_rows(&self.unstaged_vis, &self.unstaged_rows, avail, u_hover);
        self.staged.set_rows(fs, &sk, &ss, sw, 4000.0);
        self.unstaged.set_rows(fs, &uk, &us, sw, 4000.0);
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

    /// Build one [`IconRow`] per visible item for an [`IconList`]. Folders get a
    /// centered chevron; in tree mode files get a file-type icon + bright name; in
    /// list mode files have no icon and show `name` (bright) + `dir` (dim). The
    /// returned key encodes mode + structure so a mode/collapse toggle re-shapes.
    fn vis_rows(&self, vis: &[Vis], rows: &[Row], avail: f32, hovered: Option<usize>) -> (String, Vec<IconRow>) {
        let fg = theme::FG_TEXT();
        let dim = theme::FG_DIM();
        let z = theme::ui_zoom();
        // The hovered row's action icons appear at the right, so that one row reserves
        // the action column too (others keep the full width). Reserve the widest case
        // (3 actions) so the icons never sit over the name.
        let hover_reserve = 3.0 * action_w() + 4.0 * z;
        let row_avail = |i: usize| if Some(i) == hovered { (avail - hover_reserve).max(20.0) } else { avail };
        // Width an indented tree row's name may use: its base avail minus the indent
        // steps and the icon column. Ellipsized (not hard-clipped) past this.
        let tree_avail = |i: usize, depth: usize| (row_avail(i) - depth as f32 * 11.0 * z - 20.0 * z).max(20.0);
        let mut key = format!("{}{:?}\n", self.tree_mode as u8, hovered); // mode/hover → reshape
        let mut out: Vec<IconRow> = Vec::new();
        for (i, v) in vis.iter().enumerate() {
            match v {
                Vis::Folder { label, depth, collapsed, .. } => {
                    let g = if *collapsed { theme::ICON_CHEVRON_RIGHT } else { theme::ICON_CHEVRON_DOWN };
                    let (text, _) = Self::display(label, "", tree_avail(i, *depth));
                    out.push(IconRow { depth: *depth, icon: Some((g, fg, 1.25)), label: vec![(text.clone(), fg)] });
                    key.push_str(&format!("d{}{}{}\n", depth, *collapsed as u8, text));
                }
                Vis::File { row, depth } => {
                    let r = &rows[*row];
                    if self.tree_mode {
                        let (g, col) = theme::file_icon(&r.fname);
                        let (text, _) = Self::display(&r.fname, "", tree_avail(i, *depth));
                        out.push(IconRow { depth: *depth, icon: Some((g, col, 1.0)), label: vec![(text.clone(), fg)] });
                        key.push_str(&format!("f{}{}\n", depth, text));
                    } else {
                        // List mode: file-type icon + "name  dir" ellipsized; name
                        // bright, dir dim. Leave room for the icon column.
                        let (g, col) = theme::file_icon(&r.fname);
                        let la = (row_avail(i) - 20.0 * z).max(20.0);
                        let (full, flen) = Self::display(&r.fname, &r.dir, la);
                        let (name, rest) = full.split_at(flen.min(full.len()));
                        let mut label = vec![(name.to_string(), fg)];
                        if !rest.is_empty() {
                            label.push((rest.to_string(), dim));
                        }
                        out.push(IconRow { depth: 0, icon: Some((g, col, 1.0)), label });
                        key.push_str(&format!("l{}{}\n", g, full));
                    }
                }
            }
        }
        (key, out)
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
        // ~3 text lines tall so a few lines are visible; longer messages wrap and
        // scroll within this box (the input keeps the caret in view).
        let h = 3.0 * theme::UI_LINE_HEIGHT() + 8.0 * z;
        Rect { x: r.x + 10.0 * z, y: r.y + row_h() + 8.0 * z, w: r.w - 20.0 * z, h }
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
        // Collapsed group → zero-height list (rows hidden; the group below moves up).
        let n = if self.staged_open { self.staged_vis.len() } else { 0 };
        Rect { x: r.x, y: h.y + row_h(), w: r.w, h: n as f32 * row_h() }
    }
    /// Whether the "Staged Changes" group is shown at all (hidden when nothing is
    /// staged, VSCode-style — the "Changes" group then sits at the top).
    fn has_staged(&self) -> bool {
        !self.staged_rows.is_empty()
    }
    /// Total height the staged section occupies (header + its open list); 0 when
    /// there's nothing staged.
    fn staged_section_h(&self) -> f32 {
        if !self.has_staged() {
            return 0.0;
        }
        row_h() + if self.staged_open { self.staged_vis.len() as f32 * row_h() } else { 0.0 }
    }
    fn unstaged_hdr(&self, r: Rect) -> Rect {
        let z = theme::ui_zoom();
        let gap = if self.has_staged() { 8.0 * z } else { 0.0 };
        let y = Self::groups_top(r) - self.scroll.offset().1 + self.staged_section_h() + gap;
        Rect { x: r.x + 8.0 * z, y, w: r.w - 16.0 * z, h: row_h() }
    }
    fn unstaged_list(&self, r: Rect) -> Rect {
        let h = self.unstaged_hdr(r);
        let n = if self.unstaged_open { self.unstaged_vis.len() } else { 0 };
        Rect { x: r.x, y: h.y + row_h(), w: r.w, h: n as f32 * row_h() }
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

    /// [sparkle, stash, tree-toggle, refresh, more] toolbar rects, right-aligned in
    /// the CHANGES header row.
    fn toolbar_rects(r: Rect) -> [Rect; 5] {
        let bw = 22.0 * theme::ui_zoom();
        let ch = Self::changes_hdr(r);
        let more = Rect { x: ch.x + ch.w - bw, y: ch.y, w: bw, h: row_h() };
        let refresh = Rect { x: more.x - bw, ..more };
        let tree = Rect { x: refresh.x - bw, ..more };
        let stash = Rect { x: tree.x - bw, ..more };
        let sparkle = Rect { x: stash.x - bw, ..more };
        [sparkle, stash, tree, refresh, more]
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
        let acts: &[Act] = if staged {
            &[Act::Diff, Act::Stash, Act::Unstage]
        } else {
            &[Act::Diff, Act::Stash, Act::Discard, Act::Stage]
        };
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
        // While the AI is generating, pulse a thicker accent border around the box
        // as a loading affordance; otherwise the usual hairline border.
        if self.generating {
            let t = self.gen_since.map_or(0.0, |s| now.saturating_duration_since(s).as_secs_f32());
            // ~1.1s period sine, eased to [0.35, 1.0] so it never fully fades.
            let pulse = 0.5 - 0.5 * (t * std::f32::consts::TAU / 1.1).cos();
            let a = 0.35 + 0.65 * pulse;
            let glow = theme::zpx(2.5) * (0.6 + 0.4 * pulse); // border breathes in width too
            let halo = Rect { x: m.x - glow, y: m.y - glow, w: m.w + glow * 2.0, h: m.h + glow * 2.0 };
            let [r, g, b, _] = theme::ACCENT();
            bg.push(halo.rounded_quad([r, g, b, a], ir + glow));
        } else {
            bg.push(border.rounded_quad(theme::SEARCH_BORDER(), ir + 1.0));
        }
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
        // Selected-row highlight (clicked item; persists). Only when its group is
        // shown + expanded.
        if let Some((staged, idx)) = self.selected {
            let shown = if staged { self.has_staged() && self.staged_open } else { self.unstaged_open };
            if shown {
                let lr = if staged { self.staged_list(region) } else { self.unstaged_list(region) };
                let y = lr.y + idx as f32 * row_h();
                if let Some(r) = vclip(Rect { x: region.x, y, w: region.w, h: row_h() }, Self::groups_viewport(region)) {
                    bg.push(Quad::new(r.x, r.y, r.w, r.h, theme::TREE_SELECTED()));
                }
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
        // Tree-mode indent guides (hover-revealed + animated, owned by the IconList).
        if self.tree_mode {
            let vp = Self::groups_viewport(region);
            if self.has_staged() && self.staged_open {
                self.staged.draw_guides(vp, self.staged_list(region).y, now, bg);
            }
            if self.unstaged_open {
                self.unstaged.draw_guides(vp, self.unstaged_list(region).y, now, bg);
            }
        }
        // Auto-hiding scrollbar for the groups area.
        self.scroll.draw(now, fg);
    }

    pub fn draw_text<'b>(&'b self, region: Rect, areas: &mut Vec<TextArea<'b>>) {
        let ch = Self::changes_hdr(region);
        self.l_changes.push(ch.x, ch, theme::FG_DIM(), areas);
        // Top toolbar: ✨ generate · stash · tree/list toggle · refresh · more.
        let [sparkle, stash, tree, refresh, more] = Self::toolbar_rects(region);
        // The sparkle accents when active, dims while a generation is in flight.
        let spark_c = if self.generating { theme::FG_DIM() } else { theme::FG_ACTIVE() };
        self.push_icon(&self.ic_sparkle, sparkle, spark_c, areas);
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
        // Staged Changes group — only shown when something is staged.
        if self.has_staged() {
            let sh = self.staged_hdr(region);
            self.draw_group_chevron(sh, vp, self.staged_open, areas);
            self.l_staged.push_in(sh.x + hdr_chev_w(), sh, vp, theme::FG_TEXT(), areas);
            self.push_count(&self.count_staged, sh, vp, areas);
            if self.hovered_header == Some(true) {
                self.draw_header_actions(region, true, areas);
            }
            if self.staged_open {
                let sh_idx = match self.hovered {
                    Some((true, i)) => Some(i),
                    _ => None,
                };
                self.draw_rows(&self.staged, &self.staged_vis, &self.staged_rows, self.staged_list(region), vp, true, sh_idx, areas);
            }
        }

        let uh = self.unstaged_hdr(region);
        self.draw_group_chevron(uh, vp, self.unstaged_open, areas);
        self.l_unstaged.push_in(uh.x + hdr_chev_w(), uh, vp, theme::FG_TEXT(), areas);
        if !self.unstaged_vis.is_empty() {
            self.push_count(&self.count_unstaged, uh, vp, areas);
        }
        if self.hovered_header == Some(false) {
            self.draw_header_actions(region, false, areas);
        }
        if self.unstaged_open {
            let uh_idx = match self.hovered {
                Some((false, i)) => Some(i),
                _ => None,
            };
            self.draw_rows(&self.unstaged, &self.unstaged_vis, &self.unstaged_rows, self.unstaged_list(region), vp, false, uh_idx, areas);
        }
    }

    /// The group-header collapse twistie (▾ open / ▸ collapsed), centered in the
    /// chevron column at the header's left and clipped to the scroll viewport.
    fn draw_group_chevron<'b>(&'b self, hdr: Rect, vp: Rect, open: bool, areas: &mut Vec<TextArea<'b>>) {
        let slot = Rect { x: hdr.x, y: hdr.y, w: hdr_chev_w(), h: row_h() };
        let chev = if open { &self.chev_down } else { &self.chev_right };
        chev.draw_clipped(slot, vp, theme::FG_DIM(), areas);
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
            Act::Stash => &self.ic_stash,
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn draw_rows<'b>(
        &'b self,
        list: &'b IconList,
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
        // Clip the row text to leave the status column clear (names are already
        // ellipsized to reserve the action column, so they end before the icons).
        let unclipped = Rect { w: (region.w - status_w() - theme::zpx(4.0)).max(0.0), ..region };
        let Some(text_clip) = vclip(unclipped, vp) else { return };
        // The IconList draws each row's text (segment colors baked in) + centered
        // icon overlay; the status letter + hover actions are decorations drawn on
        // top, positioned from the row geometry.
        list.draw_slice(text_clip, region.y, theme::FG_TEXT(), areas);
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
        } else if self.has_staged() && self.staged_hdr(region).contains(pt) {
            Some(true)
        } else if self.unstaged_hdr(region).contains(pt) {
            Some(false)
        } else {
            None
        };
        let changed = new != self.hovered || new_hdr != self.hovered_header;
        self.hovered = new;
        self.hovered_header = new_hdr;
        // Indent guides appear while hovering anywhere in the groups area (eased),
        // with the hovered row's parent guide highlighted (active).
        let g1 = self.staged.set_guides_hovered(in_groups);
        let g2 = self.unstaged.set_guides_hovered(in_groups);
        let a1 = self.staged.set_hover_row(match new {
            Some((true, i)) => Some(i),
            _ => None,
        });
        let a2 = self.unstaged.set_hover_row(match new {
            Some((false, i)) => Some(i),
            _ => None,
        });
        changed || g1 || g2 || a1 || a2
    }

    /// Toggle the changed-files view between flat list and folder tree (the … menu).
    pub fn toggle_view(&mut self) {
        self.tree_mode = !self.tree_mode;
        self.last_w = -1.0; // force a reflow into the new mode next frame
    }

    /// Highlight (select) a row — persists until another is clicked or the changes
    /// refresh. The selected row's parent indent guide stays highlighted (the
    /// guides' persistent "active" guide), matching the editor's open-file behavior.
    fn set_selected(&mut self, sel: Option<(bool, usize)>) {
        self.selected = sel;
        self.staged.set_active_row(match sel {
            Some((true, i)) => Some(i),
            _ => None,
        });
        self.unstaged.set_active_row(match sel {
            Some((false, i)) => Some(i),
            _ => None,
        });
    }

    /// Next frame time while either group's indent-guide fade is animating.
    pub fn guides_next_wake(&self, now: std::time::Instant) -> Option<std::time::Instant> {
        match (self.staged.guides_next_wake(now), self.unstaged.guides_next_wake(now)) {
            (Some(a), Some(b)) => Some(a.min(b)),
            (a, b) => a.or(b),
        }
    }

    /// The file row under `pt` as (repo-relative path, in-staged-group, untracked) —
    /// drives the right-click context menu.
    pub fn row_at_point(&self, pt: (f32, f32), region: Rect) -> Option<(String, bool, bool)> {
        if !Self::groups_viewport(region).contains(pt) {
            return None;
        }
        for staged in [true, false] {
            let (lr, vis, rows, list): (Rect, &[Vis], &[Row], &IconList) = if staged {
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

    /// True when `pt` is over the commit message box — drives the I-beam cursor.
    pub fn over_message(&self, pt: (f32, f32), region: Rect) -> bool {
        Self::msg_rect(region).contains(pt)
    }

    /// Extend the commit-box selection while the mouse is dragged. Returns true
    /// when a box drag-select is active (caller redraws).
    pub fn on_drag(&mut self, pt: (f32, f32), region: Rect) -> bool {
        if self.msg_dragging && self.msg_active {
            self.msg.on_drag(Self::msg_rect(region), theme::zpx(6.0), pt.0, pt.1);
            return true;
        }
        false
    }

    pub fn on_release(&mut self) {
        self.msg_dragging = false;
    }

    pub fn on_press(&mut self, pt: (f32, f32), region: Rect, clicks: u32, out: &mut Vec<Intent>) -> bool {
        // The groups scrollbar claims its presses (thumb drag / track jump).
        if self.scroll.press(pt) {
            return true;
        }
        if Self::msg_rect(region).contains(pt) {
            self.msg_active = true;
            // on_click handles multiline caret-by-(x,y), double-click word, and
            // triple-click select-all; it also sets focus.
            self.msg.on_click(Self::msg_rect(region), theme::zpx(6.0), pt.0, pt.1, clicks);
            self.msg_dragging = clicks <= 1; // a single press may start a drag-select
            return true;
        }
        // Top toolbar.
        let [sparkle, stash, tree, refresh, more] = Self::toolbar_rects(region);
        if sparkle.contains(pt) {
            if !self.generating {
                out.push(Intent::GitGenerateCommitMessage);
            }
            return true;
        }
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
            out.push(Intent::OpenMoreMenu { anchor: (more.x, more.y + more.h), tree_mode: self.tree_mode });
            return true;
        }
        // Commit split button: main = Commit; chevron opens a dropdown (Commit /
        // Commit & Push).
        if Self::commit_chevron(region).contains(pt) {
            let c = Self::commit_chevron(region);
            out.push(Intent::OpenCommitMenu {
                anchor: (c.x, c.y + c.h),
                msg: self.message(),
                stage_all: self.nothing_staged(),
            });
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
        // hover, but a click lands on the pointer, so don't gate on hover state). The
        // staged group is absent when nothing is staged.
        for staged in [true, false] {
            if staged && !self.has_staged() {
                continue;
            }
            for (act, ar) in self.header_actions(region, staged) {
                if ar.contains(pt) {
                    out.push(match act {
                        Act::Diff => Intent::OpenAllDiffs { staged },
                        Act::Stage => Intent::GitStageAll,
                        Act::Unstage => Intent::GitUnstageAll,
                        Act::Stash => Intent::GitStash,
                        _ => Intent::GitDiscardAll,
                    });
                    return true;
                }
            }
        }
        // Group-header body (not an action): toggle the whole group's collapse, like
        // the folder rows and the OUTLINE section.
        if self.has_staged() && self.staged_hdr(region).contains(pt) {
            self.staged_open = !self.staged_open;
            return true;
        }
        if self.unstaged_hdr(region).contains(pt) {
            self.unstaged_open = !self.unstaged_open;
            return true;
        }
        // Row hit: resolve into either a folder toggle (deferred so we don't mutate
        // `self` while a `&self` borrow of the vis/rows is live) or file actions.
        let mut toggle: Option<String> = None;
        let mut select: Option<(bool, usize)> = None;
        let mut handled = false;
        for staged in [true, false] {
            let (lr, vis, rows, list): (Rect, &[Vis], &[Row], &IconList) = if staged {
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
                        // Clicking a file row selects/highlights it (persists) — in
                        // addition to whatever action/diff the click triggers.
                        select = Some((staged, i));
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
                                    Act::Stash => {} // not a per-file action
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
        if let Some((staged, i)) = select {
            self.set_selected(Some((staged, i)));
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
            // Plain Enter inserts a newline (the box is multiline; Ctrl+Enter commits).
            Key::Named(NamedKey::Enter) => {
                self.msg.insert(fs, "\n");
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
        let consumed = p.on_press(pt, region, 1, &mut out);
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
        let consumed = p.on_press(pt, region, 1, &mut out);
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
        let consumed = p.on_press(pt, region, 1, &mut out);
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
        let consumed = p.on_press(pt, region, 1, &mut out);
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
        let consumed = p.on_press(pt, region, 1, &mut out);
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
        p.on_press(body_pt, region, 1, &mut out2);
        assert!(out2.is_empty(), "body click emits no intents");
        assert!(!p.collapsed.is_empty(), "body click collapsed the folder");
    }

    fn make_row(path: &str, code: char) -> Row {
        SourceControlPanel::row(path, code)
    }

    // Clicking a group header body toggles that group's collapse, hiding its rows
    // (the list region shrinks to zero height so the group below moves up).
    #[test]
    fn group_header_click_collapses_group() {
        let _z = zoom_guard();
        let (mut p, mut fs) = panel();
        p.unstaged_rows = vec![make_row("a.rs", 'M'), make_row("b.rs", 'M')];
        let region = Rect { x: 0.0, y: 0.0, w: 300.0, h: 1000.0 };
        p.update(&mut fs, region);
        assert!(p.unstaged_open);
        assert!(p.unstaged_list(region).h > 0.0, "open group has rows");

        // Click the header body (left of any action icons / count pill).
        let uh = p.unstaged_hdr(region);
        let mut out = Vec::new();
        assert!(p.on_press((uh.x + hdr_chev_w() + 4.0, uh.y + row_h() * 0.5), region, 1, &mut out));
        assert!(out.is_empty(), "collapse emits no git intents");
        assert!(!p.unstaged_open, "group collapsed");
        assert_eq!(p.unstaged_list(region).h, 0.0, "collapsed group hides its rows");

        // A row click in the (now-hidden) list area does nothing.
        let mut out2 = Vec::new();
        p.on_press((region.x + 20.0, uh.y + row_h() * 1.5), region, 1, &mut out2);
        assert!(out2.is_empty(), "no row hits while collapsed");
    }
}
