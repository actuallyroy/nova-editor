// Source Control sidebar view, modeled on VSCode's SCM: a commit message box + a
// blue Commit button, then "Staged Changes" / "Changes" groups (each with a count
// badge). Rows show filename (bright) + dimmed dir, ellipsized to fit, a colored
// status letter on the right, and per-row hover actions:
//   • Changes:        Open file · Discard · Stage (+)
//   • Staged Changes: Open file · Unstage (−)
// Owns its workspace root + commit message. Deferred: per-file-type icons + GRAPH.

use std::path::PathBuf;

use glyphon::{Color, FontSystem, TextArea};

use crate::git;
use crate::quad::Quad;
use crate::theme;
use crate::ui::Intent;
use crate::widgets::{ListView, Rect, TextInput, TextLabel};

const ROW_H: f32 = 22.0;
const PAD_X: f32 = 30.0; // row text indent
const STATUS_W: f32 = 22.0; // status-letter column at the right edge
const ACTION_W: f32 = 20.0; // per hover-action icon
// Text never grows under the status letter + the (up to 3) hover-action icons:
// STATUS_W (22) + 3*ACTION_W (60) + an 8px gap.
const RIGHT_RESERVE: f32 = 90.0;

/// A changed file as shown in one group. `fname_len` is recomputed by `update`
/// each time the sidebar width changes (so the ellipsis is reactive to resize).
struct Row {
    path: String,     // repo-relative (git's slashes)
    fname: String,    // file name
    dir: String,      // parent dir (dimmed)
    fname_len: usize, // byte length of the bright file-name prefix in the current display
    badge: usize,     // index into BADGE_*
    untracked: bool,  // for Discard (delete vs revert)
}

#[derive(Clone, Copy, PartialEq)]
enum Act {
    Open,
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
    last_w: f32, // sidebar width the rows were last ellipsized for (-1 = stale)
    hovered: Option<(bool, usize)>, // (is_staged_group, row index)
    branch: Option<String>,
    l_changes: TextLabel,
    l_staged: TextLabel,
    l_unstaged: TextLabel,
    l_commit: TextLabel,
    count_staged: TextLabel,
    count_unstaged: TextLabel,
    badges: [TextLabel; 5],
    ic_open: TextLabel,
    ic_discard: TextLabel,
    ic_stage: TextLabel,
    ic_unstage: TextLabel,
    ic_refresh: TextLabel,
    ic_more: TextLabel,
    ic_chevron: TextLabel,
    hovered_header: Option<bool>, // Some(true)=Staged header, Some(false)=Changes header
    root: PathBuf,
}

impl SourceControlPanel {
    pub fn new(fs: &mut FontSystem, root: PathBuf) -> Self {
        let mk = |fs: &mut FontSystem, s: &str| {
            let mut l = TextLabel::new(fs, theme::SIDEBAR_WIDTH, ROW_H);
            l.set(fs, s, theme::UI_FAMILY());
            l
        };
        let icon = |fs: &mut FontSystem, c: char| {
            let mut l = TextLabel::new(fs, 24.0, ROW_H);
            l.set(fs, &c.to_string(), theme::ICON_FAMILY);
            l
        };
        let badges = std::array::from_fn(|i| {
            let mut l = TextLabel::new(fs, 24.0, ROW_H);
            l.set(fs, BADGE_LETTERS[i], theme::UI_FAMILY());
            l
        });
        let mut msg = TextInput::new(fs, theme::SIDEBAR_WIDTH, 30.0);
        msg.set_placeholder(fs, " Message (Ctrl+Enter to commit)");
        Self {
            msg,
            msg_active: false,
            staged: ListView::new(fs, theme::SIDEBAR_WIDTH, 4000.0, ROW_H, PAD_X),
            unstaged: ListView::new(fs, theme::SIDEBAR_WIDTH, 4000.0, ROW_H, PAD_X),
            staged_rows: Vec::new(),
            unstaged_rows: Vec::new(),
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
            ic_open: icon(fs, theme::ICON_FILE),
            ic_discard: icon(fs, theme::ICON_DISCARD),
            ic_stage: icon(fs, theme::ICON_ADD),
            ic_unstage: icon(fs, theme::ICON_REMOVE),
            ic_refresh: icon(fs, theme::ICON_REFRESH),
            ic_more: icon(fs, theme::ICON_ELLIPSIS),
            ic_chevron: icon(fs, theme::ICON_CHEVRON_DOWN),
            hovered_header: None,
            root,
        }
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
        let w = if self.last_w > 0.0 { self.last_w } else { theme::SIDEBAR_WIDTH };
        self.reflow(fs, w);
    }

    fn row(path: &str, code: char) -> Row {
        let fname = path.rsplit(['/', '\\']).next().unwrap_or(path);
        let dir = path[..path.len().saturating_sub(fname.len())].trim_end_matches(['/', '\\']);
        Row {
            path: path.to_string(),
            fname: fname.to_string(),
            dir: dir.to_string(),
            fname_len: fname.len(),
            badge: badge_for(code),
            untracked: code == '?',
        }
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
    }

    fn reflow(&mut self, fs: &mut FontSystem, w: f32) {
        let avail = (w - PAD_X - RIGHT_RESERVE).max(20.0);
        for rows in [&mut self.staged_rows, &mut self.unstaged_rows] {
            for r in rows.iter_mut() {
                r.fname_len = Self::display(&r.fname, &r.dir, avail).1;
            }
        }
        let key = |rows: &[Row]| {
            rows.iter()
                .map(|r| Self::display(&r.fname, &r.dir, avail).0)
                .collect::<Vec<_>>()
                .join("\n")
        };
        let (sk, uk) = (key(&self.staged_rows), key(&self.unstaged_rows));
        self.staged.set_text(fs, &sk, theme::SIDEBAR_WIDTH, 4000.0);
        self.unstaged.set_text(fs, &uk, theme::SIDEBAR_WIDTH, 4000.0);
    }

    /// "filename  dir" truncated with an ellipsis to fit `avail` px (leaving room
    /// for the status + hover action icons). Returns the string + byte length of the
    /// bright file-name prefix within it.
    fn display(fname: &str, dir: &str, avail: f32) -> (String, usize) {
        let budget = (avail / 6.5).max(4.0) as usize;
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
        Rect { x: r.x + 8.0, y: r.y + 4.0, w: r.w - 12.0, h: ROW_H }
    }
    fn msg_rect(r: Rect) -> Rect {
        Rect { x: r.x + 10.0, y: r.y + ROW_H + 8.0, w: r.w - 20.0, h: 30.0 }
    }
    fn commit_rect(r: Rect) -> Rect {
        let m = Self::msg_rect(r);
        Rect { x: m.x, y: m.y + m.h + 6.0, w: m.w, h: 28.0 }
    }
    fn staged_hdr(r: Rect) -> Rect {
        let c = Self::commit_rect(r);
        Rect { x: r.x + 8.0, y: c.y + c.h + 10.0, w: r.w - 16.0, h: ROW_H }
    }
    fn staged_list(&self, r: Rect) -> Rect {
        let h = Self::staged_hdr(r);
        Rect { x: r.x, y: h.y + ROW_H, w: r.w, h: self.staged_rows.len() as f32 * ROW_H }
    }
    fn unstaged_hdr(&self, r: Rect) -> Rect {
        let sl = self.staged_list(r);
        Rect { x: r.x + 8.0, y: sl.y + sl.h + 8.0, w: r.w - 16.0, h: ROW_H }
    }
    fn unstaged_list(&self, r: Rect) -> Rect {
        let h = self.unstaged_hdr(r);
        Rect { x: r.x, y: h.y + ROW_H, w: r.w, h: self.unstaged_rows.len() as f32 * ROW_H }
    }

    /// Hover-action icon rects for a row, right-to-left ending before the status.
    fn action_rects(region: Rect, y: f32, staged: bool) -> Vec<(Act, Rect)> {
        let acts: &[Act] = if staged {
            &[Act::Open, Act::Unstage]
        } else {
            &[Act::Open, Act::Discard, Act::Stage]
        };
        let end = region.x + region.w - STATUS_W - 4.0;
        let start = end - acts.len() as f32 * ACTION_W;
        acts.iter()
            .enumerate()
            .map(|(i, &a)| (a, Rect { x: start + i as f32 * ACTION_W, y, w: ACTION_W, h: ROW_H }))
            .collect()
    }

    /// [refresh, more] toolbar rects, right-aligned in the CHANGES header row.
    fn toolbar_rects(r: Rect) -> [Rect; 2] {
        let ch = Self::changes_hdr(r);
        let more = Rect { x: ch.x + ch.w - 22.0, y: ch.y, w: 22.0, h: ROW_H };
        let refresh = Rect { x: more.x - 22.0, y: ch.y, w: 22.0, h: ROW_H };
        [refresh, more]
    }
    fn commit_main(r: Rect) -> Rect {
        let c = Self::commit_rect(r);
        Rect { w: c.w - 28.0, ..c }
    }
    fn commit_chevron(r: Rect) -> Rect {
        let c = Self::commit_rect(r);
        Rect { x: c.x + c.w - 28.0, w: 28.0, ..c }
    }
    /// Group-header composite actions (left of the count pill, on hover):
    /// Staged → Unstage All; Changes → Discard All + Stage All.
    fn header_actions(&self, r: Rect, staged: bool) -> Vec<(Act, Rect)> {
        let (hdr, count) = if staged {
            (Self::staged_hdr(r), &self.count_staged)
        } else {
            (self.unstaged_hdr(r), &self.count_unstaged)
        };
        let acts: &[Act] = if staged { &[Act::Unstage] } else { &[Act::Discard, Act::Stage] };
        let end = hdr.x + hdr.w - (count.width() + 12.0) - 6.0;
        let start = end - acts.len() as f32 * ACTION_W;
        acts.iter()
            .enumerate()
            .map(|(i, &a)| (a, Rect { x: start + i as f32 * ACTION_W, y: hdr.y, w: ACTION_W, h: ROW_H }))
            .collect()
    }

    // ---- Drawing ----
    pub fn draw_quads(&self, region: Rect, blink: bool, bg: &mut Vec<Quad>, fg: &mut Vec<Quad>) {
        let m = Self::msg_rect(region);
        let border = Rect { x: m.x - 1.0, y: m.y - 1.0, w: m.w + 2.0, h: m.h + 2.0 };
        bg.push(border.rounded_quad(theme::SEARCH_BORDER(), 3.0));
        bg.push(m.rounded_quad(theme::SEARCH_BG(), 2.0));
        if self.msg_active {
            self.msg.selection_quads(m, 6.0, bg);
            if blink {
                fg.push(self.msg.caret_quad(m, 6.0));
            }
        }
        bg.push(Self::commit_rect(region).rounded_quad([0.06, 0.40, 0.62, 1.0], 3.0));
        // Split divider between the Commit label and its dropdown chevron.
        let cc = Self::commit_chevron(region);
        bg.push(Quad::new(cc.x, cc.y + 5.0, 1.0, cc.h - 10.0, [0.03, 0.28, 0.46, 1.0]));
        for (hdr, label) in [
            (Self::staged_hdr(region), &self.count_staged),
            (self.unstaged_hdr(region), &self.count_unstaged),
        ] {
            let w = label.width() + 12.0;
            let pill = Rect { x: hdr.x + hdr.w - w, y: hdr.y + 3.0, w, h: ROW_H - 6.0 };
            bg.push(pill.rounded_quad([0.20, 0.30, 0.42, 1.0], pill.h * 0.5));
        }
        // Hovered-row highlight (so the action icons read as part of an active row).
        if let Some((staged, idx)) = self.hovered {
            let lr = if staged { self.staged_list(region) } else { self.unstaged_list(region) };
            let y = lr.y + idx as f32 * ROW_H;
            bg.push(Quad::new(region.x, y, region.w, ROW_H, theme::TREE_HOVER()));
        }
    }

    pub fn draw_text<'b>(&'b self, region: Rect, areas: &mut Vec<TextArea<'b>>) {
        let ch = Self::changes_hdr(region);
        self.l_changes.push(ch.x, ch, theme::FG_DIM(), areas);
        // Top toolbar: refresh + more.
        let [refresh, more] = Self::toolbar_rects(region);
        self.push_icon(&self.ic_refresh, refresh, theme::FG_DIM(), areas);
        self.push_icon(&self.ic_more, more, theme::FG_DIM(), areas);

        let m = Self::msg_rect(region);
        let mc = if self.msg.text().is_empty() { theme::FG_DIM() } else { theme::FG_TEXT() };
        self.msg.draw(m, 6.0, mc, areas);
        // Commit split button: label centered in the main part + dropdown chevron.
        let cm = Self::commit_main(region);
        self.l_commit.push(cm.x + (cm.w - self.l_commit.width()) * 0.5, cm, theme::FG_TEXT(), areas);
        self.push_icon(&self.ic_chevron, Self::commit_chevron(region), theme::FG_TEXT(), areas);

        let sh = Self::staged_hdr(region);
        self.l_staged.push(sh.x, sh, theme::FG_TEXT(), areas);
        self.push_count(&self.count_staged, sh, areas);
        if self.hovered_header == Some(true) {
            self.draw_header_actions(region, true, areas);
        }
        let sh_idx = match self.hovered {
            Some((true, i)) => Some(i),
            _ => None,
        };
        self.draw_rows(&self.staged, &self.staged_rows, self.staged_list(region), true, sh_idx, areas);

        let uh = self.unstaged_hdr(region);
        self.l_unstaged.push(uh.x, uh, theme::FG_TEXT(), areas);
        self.push_count(&self.count_unstaged, uh, areas);
        if self.hovered_header == Some(false) {
            self.draw_header_actions(region, false, areas);
        }
        let uh_idx = match self.hovered {
            Some((false, i)) => Some(i),
            _ => None,
        };
        self.draw_rows(&self.unstaged, &self.unstaged_rows, self.unstaged_list(region), false, uh_idx, areas);
    }

    fn push_icon<'b>(&self, label: &'b TextLabel, r: Rect, color: glyphon::Color, areas: &mut Vec<TextArea<'b>>) {
        label.push(r.x + (r.w - label.width()) * 0.5, r, color, areas);
    }

    fn draw_header_actions<'b>(&'b self, region: Rect, staged: bool, areas: &mut Vec<TextArea<'b>>) {
        for (act, ar) in self.header_actions(region, staged) {
            self.push_icon(self.icon_for(act), ar, theme::FG_TEXT(), areas);
        }
    }

    fn push_count<'b>(&self, label: &'b TextLabel, hdr: Rect, areas: &mut Vec<TextArea<'b>>) {
        let w = label.width() + 12.0;
        let pill = Rect { x: hdr.x + hdr.w - w, y: hdr.y, w, h: ROW_H };
        label.push(pill.x + (pill.w - label.width()) * 0.5, pill, theme::FG_DIM(), areas);
    }

    fn icon_for(&self, act: Act) -> &TextLabel {
        match act {
            Act::Open => &self.ic_open,
            Act::Discard => &self.ic_discard,
            Act::Stage => &self.ic_stage,
            Act::Unstage => &self.ic_unstage,
        }
    }

    fn draw_rows<'b>(
        &'b self,
        list: &'b ListView,
        rows: &[Row],
        region: Rect,
        staged: bool,
        hovered_idx: Option<usize>,
        areas: &mut Vec<TextArea<'b>>,
    ) {
        if rows.is_empty() {
            return;
        }
        // Clip the row text to leave the status/action column clear.
        let text_clip = Rect { w: (region.w - STATUS_W - 4.0).max(0.0), ..region };
        list.draw_at(text_clip, region.y, theme::FG_DIM(), areas);
        let pad = list.pad_x();
        for (i, r) in rows.iter().enumerate() {
            let y = region.y + i as f32 * ROW_H;
            // Bright file-name prefix (same origin, clipped to its width).
            if let Some((_x0, x1)) = list.line_x_range(i, 0, r.fname_len) {
                let w = (pad + x1).min(text_clip.w);
                let band = Rect { x: region.x, y, w, h: ROW_H };
                list.draw_at(band, region.y, theme::FG_TEXT(), areas);
            }
            // Status letter at the far right.
            let (rr, gg, bb) = BADGE_RGB[r.badge];
            let st = Rect { x: region.x + region.w - STATUS_W, y, w: 18.0, h: ROW_H };
            self.badges[r.badge].push(st.x, st, Color::rgb(rr, gg, bb), areas);
            // Hover actions for this row.
            if hovered_idx == Some(i) {
                for (act, ar) in Self::action_rects(region, y, staged) {
                    let lbl = self.icon_for(act);
                    lbl.push(ar.x + (ar.w - lbl.width()) * 0.5, ar, theme::FG_TEXT(), areas);
                }
            }
        }
    }

    // ---- Input ----
    pub fn hover(&mut self, pt: (f32, f32), region: Rect) -> bool {
        let sl = self.staged_list(region);
        let ul = self.unstaged_list(region);
        let new = if sl.contains(pt) {
            self.staged.row_at(sl, pt, self.staged_rows.len()).map(|i| (true, i))
        } else if ul.contains(pt) {
            self.unstaged.row_at(ul, pt, self.unstaged_rows.len()).map(|i| (false, i))
        } else {
            None
        };
        let new_hdr = if Self::staged_hdr(region).contains(pt) {
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

    pub fn on_press(&mut self, pt: (f32, f32), region: Rect, out: &mut Vec<Intent>) -> bool {
        if Self::msg_rect(region).contains(pt) {
            self.msg_active = true;
            self.msg.focus(true);
            self.msg.set_caret_from_x(Self::msg_rect(region), 6.0, pt.0);
            return true;
        }
        // Top toolbar.
        let [refresh, more] = Self::toolbar_rects(region);
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
        // Group-header composite actions (hit-tested by position; they only draw on
        // hover, but a click lands on the pointer, so don't gate on hover state).
        for staged in [true, false] {
            for (act, ar) in self.header_actions(region, staged) {
                if ar.contains(pt) {
                    out.push(match act {
                        Act::Stage => Intent::GitStageAll,
                        Act::Unstage => Intent::GitUnstageAll,
                        _ => Intent::GitDiscardAll,
                    });
                    return true;
                }
            }
        }
        for staged in [true, false] {
            let (lr, rows): (Rect, &[Row]) = if staged {
                (self.staged_list(region), &self.staged_rows)
            } else {
                (self.unstaged_list(region), &self.unstaged_rows)
            };
            if !lr.contains(pt) {
                continue;
            }
            let list = if staged { &self.staged } else { &self.unstaged };
            if let Some(i) = list.row_at(lr, pt, rows.len()) {
                let row = &rows[i];
                let y = lr.y + i as f32 * ROW_H;
                // Action icon hit?
                for (act, ar) in Self::action_rects(lr, y, staged) {
                    if ar.contains(pt) {
                        let p = row.path.clone();
                        match act {
                            Act::Open => out.push(Intent::OpenFile { path: self.root.join(&p), line: 1, col: 0 }),
                            Act::Stage => out.push(Intent::GitStage(p)),
                            Act::Unstage => out.push(Intent::GitUnstage(p)),
                            Act::Discard => out.push(Intent::GitDiscard { path: p, untracked: row.untracked }),
                        }
                        return true;
                    }
                }
                // Otherwise the row body opens the diff (VSCode opens the diff on
                // row click; the Open-file icon opens the actual file).
                out.push(Intent::OpenDiff { path: row.path.clone(), staged, untracked: row.untracked });
            }
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
