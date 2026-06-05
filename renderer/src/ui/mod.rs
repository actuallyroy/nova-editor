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
pub mod find_widget;
pub mod chat_panel;
pub mod info_page;
pub mod outline_panel;
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
#[derive(Clone)]
pub enum Intent {
    /// Open `path` and place the caret at (1-based `line`, byte `col`).
    OpenFile { path: PathBuf, line: usize, col: usize },
    /// Open a read-only git diff for a repo-relative `path` in an editor tab.
    /// `staged` diffs the index vs HEAD; otherwise the working tree vs HEAD.
    OpenDiff { path: String, staged: bool, untracked: bool },
    /// Open a combined diff of every file in a group (staged or unstaged) — one
    /// collapsible section per file — in a single editor tab.
    OpenAllDiffs { staged: bool },
    /// Open an extension's detail page.
    OpenExtDetail(OpenExt),
    /// Reload every open document from disk (after a Replace All rewrote files).
    ReloadOpenDocs,
    /// Open the current search results as a new (untitled) editor document.
    OpenSearchEditor { text: String },
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
    /// Stash all working-tree changes (including untracked).
    GitStash,
    GitRefresh,
    /// Generate a commit message from the current diff via Azure OpenAI (✨).
    GitGenerateCommitMessage,
    /// Commit then push (the Commit button's split-dropdown action).
    GitCommitPush { msg: String, stage_all: bool },
    /// Open the commit split-button dropdown menu (Commit / Commit & Push) at
    /// `anchor`, carrying the current message + stage-all state.
    OpenCommitMenu { anchor: (f32, f32), msg: String, stage_all: bool },
    /// Open the source-control "More actions" (…) menu at `anchor`. `tree_mode`
    /// drives the "View as List/Tree" label.
    OpenMoreMenu { anchor: (f32, f32), tree_mode: bool },
    /// Network git ops (the … menu).
    GitPush,
    GitPull,
    GitFetch,
    /// Toggle the changed-files view between flat list and folder tree.
    GitToggleView,
    GitStashPop,
    GitStashApply,
    /// Open the branch quick-pick (Checkout to…).
    GitOpenCheckout,
    /// Open the palette-as-input to create a new branch.
    GitOpenCreateBranch,
    /// Open the palette-as-input to rename the current branch.
    GitOpenRenameBranch,
    /// Open the branch quick-pick to delete a branch.
    GitOpenDeleteBranch,
}
