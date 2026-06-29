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
    /// Stage a single hunk of a file, identified by its index in the file's
    /// Working Tree (unstaged) Diff.
    StageHunk { path: String, hunk: usize },
    /// Unstage a single hunk of a file, identified by its index in the file's
    /// Staging Area (staged) Diff.
    UnstageHunk { path: String, hunk: usize },
    /// Discard Working Tree changes for one file: revert a tracked file to its
    /// committed/staged state, or delete an Untracked File (nothing to revert).
    Discard(String),
    /// Discard all Working Tree changes (revert tracked files, delete untracked).
    DiscardAll,
    /// Persist the Staging Area as a new Commit with the given message.
    Commit(String),
    /// Replace the HEAD Commit, reusing the Staging Area as its tree and the
    /// given message (a `git commit --amend`).
    Amend(String),
    /// Load the most recent Commits on the current branch, newest first.
    LoadHistory,
    /// Load one Commit's metadata and full Diff (against its first parent).
    LoadCommitDetail(String),
    /// Move the current branch to the given Commit, with the chosen reset mode
    /// (soft keeps the index and Working Tree; mixed resets the index; hard
    /// resets both, discarding uncommitted changes).
    Reset { sha: String, kind: ResetKind },
    /// Revert the given Commit: apply its inverse on top of HEAD. A clean revert
    /// is committed immediately; a conflicting one is left to resolve and commit.
    Revert(String),
    /// Cherry-pick the given Commit: apply its changes on top of HEAD. A clean
    /// pick is committed immediately; a conflicting one is left to resolve and
    /// commit.
    CherryPick(String),
    /// Load the local branches and their sync state.
    LoadBranches,
    /// Load the tags, pointing at their target Commits.
    LoadTags,
    /// Create a tag at HEAD. With a non-empty `message` it is annotated;
    /// otherwise it is lightweight.
    CreateTag {
        name: String,
        message: Option<String>,
    },
    /// Delete the named tag.
    DeleteTag(String),
    /// Push the named tag to the Remote.
    PushTag(String),
    /// Switch the Working Tree and HEAD to the named local branch.
    Checkout(String),
    /// Create a new local branch at HEAD and switch to it.
    CreateBranch(String),
    /// Delete the named local branch (never the current one).
    DeleteBranch(String),
    /// Delete every local branch that has no counterpart on the Remote (never
    /// the current one) — a cleanup of branches gone or never pushed.
    PruneBranches,
    /// Push local Commits on the current branch to the Remote.
    Push,
    /// Pull Remote changes into the current branch.
    Pull,
    /// Update the remote-tracking branches from `origin` without merging.
    Fetch,
    /// Load the saved stashes, newest (stash@{0}) first.
    LoadStashes,
    /// Save the Working Tree and Staging Area as a new stash, including untracked
    /// files. When `paths` is non-empty only those paths are stashed; otherwise
    /// everything is. A `message` is honoured only for a full stash (libgit2 has
    /// no way to set one on a path-limited stash).
    StashPush {
        message: Option<String>,
        paths: Vec<String>,
    },
    /// Load the Diff of the stash at the given index (its changes vs its base).
    LoadStashDiff(usize),
    /// Merge the named branch into the current branch.
    Merge(String),
    /// Resolve a whole conflicted file by taking one side, then stage it.
    ResolveConflict { path: String, side: ConflictSide },
    /// Parse a conflicted file's Working Tree content into its conflict regions,
    /// for region-by-region resolution.
    LoadConflict(String),
    /// Resolve one conflict region of a file (the `index`-th, in order) by taking
    /// one side. When the file then has no markers left, it is staged.
    ResolveHunk {
        path: String,
        index: usize,
        side: ConflictSide,
    },
    /// Save hand-edited content for a conflicted file to the Working Tree. When the
    /// saved content has no conflict markers left, the file is staged.
    SaveConflict { path: String, content: String },
    /// Abort an in-progress merge, restoring the pre-merge state.
    AbortMerge,
    /// Apply the stash at the given index without removing it.
    StashApply(usize),
    /// Apply the stash at the given index and remove it from the list.
    StashPop(usize),
    /// Remove the stash at the given index without applying it.
    StashDrop(usize),
}

/// The result of a completed [`GitCommand`], sent from the Git Worker to the UI.
#[derive(Debug, Clone)]
pub enum GitEvent {
    /// The current Working Tree and Staging Area contents, plus the HEAD/branch
    /// context — one consistent snapshot per refresh.
    StatusLoaded {
        unstaged: Vec<FileEntry>,
        staged: Vec<FileEntry>,
        /// Files left with merge conflicts to resolve.
        conflicted: Vec<FileEntry>,
        head: HeadInfo,
    },
    /// A freshly loaded Diff for the selected file.
    DiffLoaded(Diff),
    /// A Commit was created; carries its short SHA.
    Committed(String),
    /// The recent Commit history, newest first.
    HistoryLoaded(Vec<CommitInfo>),
    /// One Commit's metadata and full Diff.
    CommitDetailLoaded(CommitDetail),
    /// A reset completed; carries the short SHA the branch now points at.
    ResetDone(String),
    /// A revert finished; carries how it resolved.
    Reverted { outcome: RevertOutcome },
    /// A cherry-pick finished; carries how it resolved.
    CherryPicked { outcome: CherryPickOutcome },
    /// The local branches and their sync state.
    BranchesLoaded(Vec<BranchInfo>),
    /// The tags and their target Commits.
    TagsLoaded(Vec<TagInfo>),
    /// A tag was created; carries its name.
    TagCreated(String),
    /// A tag was deleted; carries its name.
    TagDeleted(String),
    /// A branch was checked out (or created and checked out); carries its name.
    CheckedOut(String),
    /// A branch was deleted; carries its name.
    BranchDeleted(String),
    /// Local branches absent from the Remote were pruned; carries their names.
    BranchesPruned(Vec<String>),
    /// A Push completed successfully.
    Pushed,
    /// A Pull completed successfully.
    Pulled,
    /// A Fetch completed successfully.
    Fetched,
    /// The saved stashes, newest first.
    StashesLoaded(Vec<StashInfo>),
    /// The Diff of one stash, for the detail panel.
    StashDiffLoaded(StashDiff),
    /// Changes were saved to a new stash.
    Stashed,
    /// A stash was applied (with or without being dropped).
    StashApplied,
    /// A stash was dropped without being applied.
    StashDropped,
    /// A merge finished; carries the merged branch and how it resolved.
    Merged {
        branch: String,
        outcome: MergeOutcome,
    },
    /// A conflicted file parsed into its regions, for region-by-region resolution.
    ConflictLoaded(ConflictFile),
    /// Any operation failed.
    Error(GitError),
}

/// A conflicted file's Working Tree content, split into ordered segments so the
/// UI can resolve each conflict region on its own.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConflictFile {
    pub path: String,
    pub segments: Vec<ConflictSegment>,
    /// The file's full Working Tree content, markers and all, as the seed for the
    /// manual editor (the fallback when ours/theirs/both can't express the merge).
    pub raw: String,
}

/// One ordered piece of a [`ConflictFile`]: either agreed-upon context, or a
/// conflict region with the two competing sides.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ConflictSegment {
    /// Lines outside any conflict (identical on both sides).
    Context(Vec<String>),
    /// One conflict region: our side (current branch) and their side (merged-in).
    Conflict { ours: Vec<String>, theirs: Vec<String> },
}

/// How a merge resolved.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MergeOutcome {
    /// HEAD already contained the branch; nothing to do.
    UpToDate,
    /// HEAD was fast-forwarded to the branch tip (no merge commit).
    FastForwarded,
    /// A merge commit was created.
    Created,
    /// The merge left conflicts in the given number of files to resolve.
    Conflicts(usize),
}

/// Which reset mode to apply when moving the branch to another Commit.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ResetKind {
    /// Move HEAD only; keep the index and Working Tree (changes become staged).
    Soft,
    /// Move HEAD and reset the index; keep the Working Tree (changes unstaged).
    Mixed,
    /// Move HEAD and reset both the index and Working Tree, discarding changes.
    Hard,
}

/// How a revert resolved.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RevertOutcome {
    /// The revert applied cleanly and was committed.
    Created,
    /// The revert left conflicts in the given number of files to resolve.
    Conflicts(usize),
}

/// How a cherry-pick resolved.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CherryPickOutcome {
    /// The pick applied cleanly and was committed.
    Created,
    /// The pick left conflicts in the given number of files to resolve.
    Conflicts(usize),
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

/// One local branch in the Branches view: its name, whether it is checked out,
/// and how it sits relative to its upstream.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BranchInfo {
    /// The branch name: a local short name (`feature`) or, for a remote branch,
    /// its remote-qualified name (`origin/feature`).
    pub name: String,
    /// A remote-tracking branch (under `refs/remotes`) rather than a local one.
    pub is_remote: bool,
    /// This branch is the currently checked-out HEAD (never set for remotes).
    pub is_head: bool,
    /// The configured upstream tracking branch, if any.
    pub upstream: Option<String>,
    /// Commits ahead of the upstream.
    pub ahead: usize,
    /// Commits behind the upstream.
    pub behind: usize,
}

/// One tag in the Tags view: its name and the Commit it points at.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TagInfo {
    /// The tag's short name (the `v1.0` in `refs/tags/v1.0`).
    pub name: String,
    /// The short SHA of the Commit the tag ultimately points to.
    pub target: String,
    /// The summary line of that Commit.
    pub summary: String,
    /// The annotation message, for an annotated tag (`None` for lightweight).
    pub message: Option<String>,
    /// An annotated tag (a tag object), rather than a lightweight ref.
    pub is_annotated: bool,
}

/// One saved stash in the Stashes view.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StashInfo {
    /// Position in the stash list, i.e. the `N` in `stash@{N}`. Used to apply,
    /// pop, or drop it. Index 0 is the most recent.
    pub index: usize,
    /// The stash's description, e.g. `WIP on main: 1a2b3c add feature`.
    pub message: String,
}

/// The Diff of one stash (its changes against its base commit), for the detail
/// panel. A header line is injected before each changed file, like a commit's.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StashDiff {
    pub index: usize,
    pub lines: Vec<DiffLine>,
}

/// One row in the History view: enough to list a Commit.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CommitInfo {
    /// The full SHA, used to request the detail.
    pub sha: String,
    pub short_sha: String,
    pub summary: String,
    pub author: String,
    /// Commit time, Unix seconds (formatted relative to now in the UI).
    pub time: i64,
    /// The full SHAs of this Commit's parents, in order (first parent first).
    /// Used to lay out the commit graph; a merge has more than one.
    pub parents: Vec<String>,
}

/// A single Commit's metadata and full Diff, shown when one is selected in the
/// History view.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CommitDetail {
    pub sha: String,
    pub short_sha: String,
    pub author: String,
    pub email: String,
    pub time: i64,
    /// The full commit message (summary and body).
    pub message: String,
    /// The Diff against the first parent, with a header line per changed file.
    pub lines: Vec<DiffLine>,
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
    /// A merge left this file with conflicts to resolve.
    Conflicted,
}

/// Which side to take when resolving a conflicted file in one click.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConflictSide {
    /// Keep the current branch's version.
    Ours,
    /// Keep the merged-in branch's version.
    Theirs,
    /// Keep both, ours then theirs.
    Both,
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
