//! Message and data types exchanged between the UI and the Git Worker.
//!
//! Naming follows `CONTEXT.md`: a [`GitCommand`] flows UI -> Worker, a
//! [`GitEvent`] flows Worker -> UI.

use std::fmt;

/// A request sent from the UI to the Git Worker, asking for a git operation.
#[derive(Debug, Clone)]
pub enum GitCommand {
    /// Re-read the Working Tree and Staging Area.
    RefreshStatus,
    /// Load the Diff for a single file. `staged` selects which side to show:
    /// the Staging Area (HEAD -> index) when `true`, otherwise the Working
    /// Tree (index -> workdir).
    LoadDiff { path: String, staged: bool },
    /// Move an Unstaged or Untracked File into the Staging Area.
    StageFile(String),
    /// Move every Unstaged and Untracked File into the Staging Area.
    StageAll,
    /// Remove a Staged File from the Staging Area.
    UnstageFile(String),
    /// Empty the Staging Area, returning everything to the Working Tree.
    UnstageAll,
    /// Discard Working Tree changes for one file: revert a tracked file to its
    /// committed/staged state, or delete an Untracked File (nothing to revert).
    Discard(String),
    /// Discard all Working Tree changes (revert tracked files, delete untracked).
    DiscardAll,
    /// Persist the Staging Area as a new Commit with the given message.
    Commit(String),
    /// Push local Commits on the current branch to the Remote.
    Push,
    /// Pull Remote changes into the current branch.
    Pull,
}

/// The result of a completed [`GitCommand`], sent from the Git Worker to the UI.
#[derive(Debug, Clone)]
pub enum GitEvent {
    /// The current Working Tree and Staging Area contents, plus the HEAD/branch
    /// context — one consistent snapshot per refresh.
    StatusLoaded {
        unstaged: Vec<FileEntry>,
        staged: Vec<FileEntry>,
        head: HeadInfo,
    },
    /// A freshly loaded Diff for the selected file.
    DiffLoaded(Diff),
    /// A Commit was created; carries its short SHA.
    Committed(String),
    /// A Push completed successfully.
    Pushed,
    /// A Pull completed successfully.
    Pulled,
    /// Any operation failed.
    Error(GitError),
}

/// The state of HEAD and its relationship to the Remote — what the UI needs to
/// show the current branch, sync state, and last Commit. Built best-effort:
/// missing pieces (no commits yet, no upstream, no remote) are `None`/zero
/// rather than errors.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct HeadInfo {
    /// The current branch's short name, or `None` when HEAD is detached or the
    /// branch is unborn with no resolvable name.
    pub branch: Option<String>,
    /// HEAD points at a commit directly rather than a branch.
    pub detached: bool,
    /// An `origin` Remote is configured (gates Push/Pull).
    pub has_remote: bool,
    /// The configured upstream tracking branch, if any.
    pub upstream: Option<String>,
    /// Commits the local branch is ahead of its upstream.
    pub ahead: usize,
    /// Commits the local branch is behind its upstream.
    pub behind: usize,
    /// The Commit at HEAD, or `None` in a repository with no commits yet.
    pub last_commit: Option<CommitSummary>,
}

/// A one-line summary of a Commit, for display.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CommitSummary {
    pub short_sha: String,
    pub summary: String,
}

/// One entry in the File List: a path plus how it changed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileEntry {
    pub path: String,
    pub change: ChangeKind,
}

/// The nature of a file's change, used for a one-letter badge in the File List.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ChangeKind {
    Added,
    Modified,
    Deleted,
    Renamed,
    Untracked,
    Typechange,
}

/// The line-level changes for a single file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Diff {
    pub path: String,
    pub staged: bool,
    pub lines: Vec<DiffLine>,
}

/// A single rendered line of a [`Diff`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DiffLine {
    pub kind: DiffLineKind,
    /// Line number on the old side, for the Diff View gutter (`None` for
    /// additions and headers).
    pub old_lineno: Option<u32>,
    /// Line number on the new side (`None` for deletions and headers).
    pub new_lineno: Option<u32>,
    pub content: String,
}

/// What a [`DiffLine`] represents, used to color it in the Diff View.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DiffLineKind {
    Addition,
    Deletion,
    Context,
    /// A hunk header (`@@ ... @@`).
    Header,
}

/// A failure from any git operation, with the operation that produced it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GitError {
    pub context: String,
    pub message: String,
}

impl GitError {
    pub fn new(context: impl Into<String>, source: &git2::Error) -> Self {
        GitError {
            context: context.into(),
            message: source.message().to_string(),
        }
    }

    pub fn custom(context: impl Into<String>, message: impl Into<String>) -> Self {
        GitError {
            context: context.into(),
            message: message.into(),
        }
    }
}

impl fmt::Display for GitError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}: {}", self.context, self.message)
    }
}
