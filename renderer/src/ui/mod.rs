// Shared seams for self-contained UI panels.
//
// A "panel" is a struct that owns one feature's state + glyphon buffers, and knows
// how to shape itself (`update`), draw itself (`draw`/`draw_pass`), and handle its
// own input. Panels live directly on `App` and are driven by thin orchestrators in
// `render.rs` (drawing) and `main.rs` (input). Cross-cutting side-effects a panel
// can't perform itself (opening a file, toggling another panel) are returned as
// `Intent`s and applied centrally by `App::apply_intent`.
//
// This module only defines the shared types; each panel lives in its own file.

use std::path::PathBuf;

pub mod editor_view;
pub mod explorer_panel;
pub mod ext_detail_view;
pub mod extensions_panel;
pub mod search_panel;
pub mod source_control_panel;
pub mod feedback_form;
pub mod terminal_panel;

use crate::extensions::OpenExt;

/// A side-effect a panel requests of `App` — kept as data so cross-cutting actions
/// (which touch shared state like the workspace) stay centralized in one place,
/// applied by `App::apply_intent`.
pub enum Intent {
    /// Open `path` and place the caret at (1-based `line`, byte `col`).
    OpenFile { path: PathBuf, line: usize, col: usize },
    /// Open a read-only git diff for a repo-relative `path` in an editor tab.
    /// `staged` diffs the index vs HEAD; otherwise the working tree vs HEAD.
    OpenDiff { path: String, staged: bool, untracked: bool },
    /// Open an extension's detail page.
    OpenExtDetail(OpenExt),
    /// Reload every open document from disk (after a Replace All rewrote files).
    ReloadOpenDocs,
    /// Commit with `msg`; `stage_all` stages everything first (nothing was staged).
    GitCommit { msg: String, stage_all: bool },
    /// Stage / unstage / discard a single repo-relative path.
    GitStage(String),
    GitUnstage(String),
    GitDiscard { path: String, untracked: bool },
    /// Group-level (composite) git actions, and toolbar actions.
    GitStageAll,
    GitUnstageAll,
    GitDiscardAll,
    GitRefresh,
    /// Commit then push (the Commit button's split-dropdown action).
    GitCommitPush { msg: String, stage_all: bool },
}
