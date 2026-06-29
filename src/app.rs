//! The iced application root.
//!
//! `App` holds the four state sub-structs (one per UI area), `update()` wires
//! `git` to `ui` via messages, and the [`Subscription`] turns the Git Worker's
//! [`GitEvent`] stream and the keyboard into messages.

use std::collections::HashSet;
use std::sync::mpsc::Sender;
use std::time::{Duration, Instant};

use futures::{SinkExt, Stream, StreamExt};
use iced::keyboard::key::Named;
use iced::keyboard::{Event as KeyEvent, Key};
use iced::{Point, Subscription, Task};

use crate::git::{
    self, BlameFile, BranchInfo, CherryPickOutcome, CommitDetail, CommitInfo, ConflictFile,
    ConflictSide, Diff, FileEntry, GitCommand, GitEvent, HeadInfo, MergeOutcome, ResetKind,
    RevertOutcome, StashDiff, StashInfo, TagInfo,
};
use crate::ui;

/// How long a success Notification stays on screen.
const NOTIFICATION_TTL: Duration = Duration::from_secs(4);

/// Launch the GUI. Returns once the window closes.
pub fn run() -> iced::Result {
    iced::application(App::new, update, view)
        .title(|_: &App| String::from("Git Client"))
        .theme(|_: &App| ui::theme())
        .subscription(subscription)
        .window_size((1024.0, 720.0))
        .run()
}

/// Which left-column view is showing.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ViewMode {
    /// The working-tree changes: File List + Diff + Commit Panel.
    #[default]
    Changes,
    /// The Commit history: the log list + the selected Commit's detail.
    History,
    /// The local branches: list, switch, create, and delete.
    Branches,
    /// The saved stashes: list, apply, pop, and drop.
    Stashes,
    /// The tags: list, create, delete, and push.
    Tags,
}

/// The application root state.
pub struct App {
    /// Channel to the Git Worker; `None` until the worker has booted.
    commands: Option<Sender<GitCommand>>,
    pub repo: RepoState,
    pub commit: CommitState,
    pub history: HistoryState,
    pub branches: BranchesState,
    pub stashes: StashesState,
    pub tags: TagsState,
    /// Which left-column view is active.
    pub view: ViewMode,
    pub status: StatusBar,
    pub notification: Notification,
    /// Label of an in-progress remote operation (Push/Pull), if any.
    pub operation: Option<String>,
    /// The files checked for a bulk action (the action targets). Distinct from
    /// `repo.selected`, which is the one file whose Diff is shown.
    pub checked: HashSet<Selection>,
    /// Directory nodes the user has collapsed in the File Tree. A directory is
    /// expanded unless its key is present here; see [`App::dir_collapsed`].
    pub collapsed: HashSet<String>,
    /// Whether Discard is armed, awaiting a confirming second press.
    pub discard_armed: bool,
    /// The last known cursor position (window coordinates), tracked while the
    /// a list view is open so a right-click menu can open at the pointer.
    pub cursor: Point,
    /// The open right-click context menu, if any.
    pub menu: Option<ContextMenu>,
}

/// An open right-click context menu: what it acts on, where to draw it, and
/// whether its one destructive action is armed (awaiting a confirming second
/// press, like the inline confirmations it replaces).
pub struct ContextMenu {
    pub target: MenuTarget,
    pub at: Point,
    pub armed: bool,
}

/// What a [`ContextMenu`] acts on, with the bits its actions need captured at
/// open time (so the menu reads from the click, not from shifting state).
#[derive(Debug, Clone)]
pub enum MenuTarget {
    /// A History commit: Revert / Reset (soft, mixed, hard).
    Commit { sha: String, short_sha: String },
    /// A branch: Checkout / Merge / Delete (locals only).
    Branch {
        name: String,
        is_remote: bool,
    },
    /// A saved stash: Apply / Pop / Drop.
    Stash { index: usize },
    /// A tag: Push / Delete.
    Tag { name: String },
}

/// The Commit history view's state: the loaded log, the selected Commit, and
/// its loaded detail.
#[derive(Default)]
pub struct HistoryState {
    pub commits: Vec<CommitInfo>,
    /// The SHA of the selected Commit, whose detail is shown.
    pub selected: Option<String>,
    pub detail: Option<CommitDetail>,
}

/// The Branches view's state: the loaded branches, the in-progress new-branch
/// name, and which branch (if any) is armed for a confirming delete.
#[derive(Default)]
pub struct BranchesState {
    pub branches: Vec<BranchInfo>,
    pub new_name: String,
    /// Whether Prune is armed, awaiting a confirming second press.
    pub prune_armed: bool,
}

/// The Stashes view's state: the loaded stashes, the in-progress stash message,
/// and which stash (if any) is armed for a confirming drop.
#[derive(Default)]
pub struct StashesState {
    pub stashes: Vec<StashInfo>,
    pub message: String,
    /// The stash whose Diff is shown in the detail panel, if any.
    pub selected: Option<usize>,
    /// The loaded Diff of the selected stash.
    pub diff: Option<StashDiff>,
}

/// The Tags view's state: the loaded tags, the in-progress new-tag name and
/// message, and which tag (if any) is armed for a confirming delete.
#[derive(Default)]
pub struct TagsState {
    pub tags: Vec<TagInfo>,
    pub new_name: String,
    /// Optional annotation message for the new tag (annotated when non-empty).
    pub message: String,
}

/// Working Tree and Staging Area contents, the HEAD/branch context, the
/// selected file, and its Diff.
#[derive(Default)]
pub struct RepoState {
    pub unstaged: Vec<FileEntry>,
    pub staged: Vec<FileEntry>,
    /// Files left in conflict by an in-progress merge, awaiting resolution.
    pub conflicted: Vec<FileEntry>,
    /// Current branch, sync state with the Remote, and last Commit.
    pub head: HeadInfo,
    pub selected: Option<Selection>,
    pub diff: Option<Diff>,
    /// The selected conflicted file parsed into regions, for region-by-region
    /// resolution. Set when a conflicted file is selected; cleared otherwise.
    pub conflict: Option<ConflictFile>,
    /// The in-progress manual edit of the selected conflicted file, when the user
    /// has opened the editor. The fallback when ours/theirs/both can't express the
    /// merge; cleared when saved, cancelled, or the selection changes.
    pub editing: Option<ConflictEdit>,
    /// The line-by-line blame of the selected file, when the user has switched the
    /// right panel to Blame. Cleared on returning to the diff or changing files.
    pub blame: Option<BlameFile>,
}

/// A conflicted file open in the manual editor: which file, and its live buffer.
pub struct ConflictEdit {
    pub path: String,
    pub content: iced::widget::text_editor::Content,
}

/// A file in the File List, identified by its path and which side it is on.
/// Used both for the active (diff) selection and for checked action targets.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct Selection {
    pub path: String,
    pub staged: bool,
}

/// The key under which a File Tree directory's collapsed state is stored. The
/// side prefix keeps the Unstaged and Staged copies of a path independent.
fn dir_key(staged: bool, path: &str) -> String {
    format!("{}\u{1}{path}", if staged { 's' } else { 'u' })
}

/// The key under which a Branch Tree folder's collapsed state is stored. The
/// `b` namespace and the section prefix keep it distinct from File Tree keys and
/// from the matching path on the other section.
fn branch_dir_key(remote: bool, path: &str) -> String {
    format!("b{}\u{1}{path}", if remote { 'r' } else { 'l' })
}

/// In-progress commit message, whether a Commit is running, and whether the
/// next Commit amends HEAD instead of creating a new Commit.
#[derive(Default)]
pub struct CommitState {
    pub message: String,
    pub committing: bool,
    pub amend: bool,
    /// Whether the in-flight Commit should be followed by a Push once it lands.
    /// Set by the "Commit & Push" action, consumed when the Commit completes.
    pub push_after_commit: bool,
}

/// The last error or warning; sticky until dismissed.
#[derive(Default)]
pub struct StatusBar {
    pub message: Option<String>,
}

/// A transient success message with an expiry.
#[derive(Default)]
pub struct Notification {
    pub message: Option<String>,
    expires_at: Option<Instant>,
}

/// The top-level message type, grouped by domain (see PRD "Message model").
#[derive(Debug, Clone)]
pub enum Message {
    /// The Git Worker booted and handed back its command channel.
    WorkerReady(Sender<GitCommand>),
    /// A pure UI interaction.
    Ui(UiMessage),
    /// A user-initiated git operation.
    Git(GitMessage),
    /// A result arriving from the Git Worker.
    GitEvent(GitEvent),
    /// The cursor moved; updates the tracked pointer position. Handled outside
    /// [`UiMessage`] so it never cancels pending confirmations.
    CursorMoved(Point),
    /// Periodic tick used to expire Notifications.
    Tick,
    /// A key press with no binding; ignored.
    Ignored,
}

#[derive(Debug, Clone)]
pub enum UiMessage {
    /// Make a file active (show its Diff).
    FileSelected {
        path: String,
        staged: bool,
    },
    /// Toggle one file's checkbox (action target).
    ToggleChecked {
        path: String,
        staged: bool,
    },
    /// Check or uncheck every file in a section (Unstaged or Staged).
    ToggleSection {
        staged: bool,
    },
    /// Collapse or expand a directory node in the File Tree.
    ToggleDir {
        staged: bool,
        path: String,
    },
    /// Collapse or expand a folder node in the Branch Tree.
    ToggleBranchDir {
        remote: bool,
        path: String,
    },
    /// Check or uncheck every file under a directory node, given its descendant
    /// file paths. Checks all unless all are already checked, then clears them.
    ToggleDirChecked {
        staged: bool,
        paths: Vec<String>,
    },
    CommitMessageChanged(String),
    /// Toggle whether the next Commit amends HEAD.
    ToggleAmend,
    DismissStatus,
    SelectNext,
    SelectPrevious,
    /// Switch the left-column view (Changes / History).
    ShowView(ViewMode),
    /// Select a Commit in the History view and load its detail.
    CommitSelected(String),
    /// Open a right-click context menu for the given target (at the cursor).
    OpenMenu(MenuTarget),
    /// Arm the open menu's destructive action (its confirming first press).
    ArmMenu,
    /// The new-branch name field changed.
    NewBranchNameChanged(String),
    /// The new-tag name field changed.
    NewTagNameChanged(String),
    /// The new-tag annotation message field changed.
    TagMessageChanged(String),
    /// The stash message field changed.
    StashMessageChanged(String),
    /// Select a stash in the Stashes view and load its Diff.
    StashSelected(usize),
    /// Open the manual editor for the selected conflicted file, seeded with its
    /// current Working Tree content (markers and all).
    EditConflict,
    /// An edit inside the manual conflict editor; applied to the live buffer.
    ConflictEdited(iced::widget::text_editor::Action),
    /// Close the manual conflict editor without saving.
    CancelConflictEdit,
    /// Switch the right panel to the line-by-line Blame of the selected file.
    ShowBlame,
    /// Return from Blame to the file's Diff.
    HideBlame,
}

#[derive(Debug, Clone)]
pub enum GitMessage {
    /// Stage the active file (keyboard shortcut).
    StageSelected,
    /// Unstage the active file (keyboard shortcut).
    UnstageSelected,
    /// Stage the checked Unstaged files (or all of them if none are checked).
    StageChecked,
    /// Unstage the checked Staged files (or all of them if none are checked).
    UnstageChecked,
    /// Discard the checked Unstaged files (or all). First press arms the
    /// confirmation; the second press performs it.
    DiscardChecked,
    /// Stage a single hunk of a file (from the Working Tree Diff).
    StageHunk { path: String, hunk: usize },
    /// Unstage a single hunk of a file (from the Staging Area Diff).
    UnstageHunk { path: String, hunk: usize },
    /// Re-read the Working Tree and Staging Area (picks up edits made outside
    /// the app) and reload the active Diff.
    Refresh,
    Commit,
    /// Commit the staged changes, then Push once the Commit lands.
    CommitAndPush,
    /// Switch to the named local branch.
    Checkout(String),
    /// Create a local branch from the in-progress name and switch to it.
    CreateBranch,
    /// Delete the named branch. First press arms a confirmation; the second
    /// performs it.
    DeleteBranch(String),
    /// Merge the named branch into the current branch.
    Merge(String),
    /// Resolve a whole conflicted file by taking one side (ours/theirs/both).
    ResolveConflict { path: String, side: ConflictSide },
    /// Resolve one conflict region of a file by taking one side.
    ResolveHunk {
        path: String,
        index: usize,
        side: ConflictSide,
    },
    /// Save the manual editor's buffer as the conflicted file's content.
    SaveConflictEdit,
    /// Abort the in-progress merge, restoring the pre-merge state.
    AbortMerge,
    /// Delete all local branches absent from the Remote. First press arms a
    /// confirmation; the second performs it.
    PruneBranches,
    /// Create a tag at HEAD from the in-progress name (annotated when the
    /// message field is non-empty).
    CreateTag,
    /// Delete the named tag. First press arms a confirmation; the second
    /// performs it.
    DeleteTag(String),
    /// Push the named tag to the Remote.
    PushTag(String),
    /// Move the current branch to a Commit with the given reset mode. A Hard
    /// reset first arms a confirmation in the menu; the second press performs it.
    Reset { sha: String, kind: ResetKind },
    /// Revert a Commit (apply its inverse on top of HEAD).
    Revert(String),
    /// Cherry-pick a Commit (apply its changes on top of HEAD).
    CherryPick(String),
    Push,
    Pull,
    /// Update remote-tracking branches from the Remote without merging.
    Fetch,
    /// Stash the checked files (or all of them if none are checked).
    Stash,
    /// Stash every change, with the stash message field as its label.
    StashAll,
    /// Apply the stash at the given index, keeping it in the list.
    StashApply(usize),
    /// Apply the stash at the given index and remove it.
    StashPop(usize),
    /// Drop the stash at the given index. First press arms a confirmation; the
    /// second performs it.
    StashDrop(usize),
}

impl App {
    fn new() -> Self {
        App {
            commands: None,
            repo: RepoState::default(),
            commit: CommitState::default(),
            history: HistoryState::default(),
            branches: BranchesState::default(),
            stashes: StashesState::default(),
            tags: TagsState::default(),
            view: ViewMode::default(),
            status: StatusBar::default(),
            notification: Notification::default(),
            operation: None,
            checked: HashSet::new(),
            collapsed: HashSet::new(),
            discard_armed: false,
            cursor: Point::ORIGIN,
            menu: None,
        }
    }

    /// Whether a File Tree directory is collapsed. `staged` separates the two
    /// sections so the same path can be expanded on one side and not the other.
    pub fn dir_collapsed(&self, staged: bool, path: &str) -> bool {
        self.collapsed.contains(&dir_key(staged, path))
    }

    /// Whether a Branch Tree folder is collapsed. `remote` separates the LOCAL
    /// and REMOTE sections so the same path can differ between them.
    pub fn branch_dir_collapsed(&self, remote: bool, path: &str) -> bool {
        self.collapsed.contains(&branch_dir_key(remote, path))
    }

    /// Send a command to the Git Worker, if it has booted.
    fn dispatch(&self, command: GitCommand) {
        if let Some(commands) = &self.commands {
            let _ = commands.send(command);
        }
    }

    fn update_ui(&mut self, message: UiMessage) {
        // Interacting elsewhere cancels a pending Discard or Prune confirmation.
        self.discard_armed = false;
        self.branches.prune_armed = false;
        // Any interaction other than opening/arming a menu dismisses it — so a
        // click outside lands on its target AND closes the menu in one go.
        if !matches!(message, UiMessage::OpenMenu(_) | UiMessage::ArmMenu) {
            self.menu = None;
        }

        match message {
            UiMessage::FileSelected { path, staged } => self.select(path, staged),
            UiMessage::ToggleChecked { path, staged } => {
                let item = Selection { path, staged };
                if !self.checked.remove(&item) {
                    self.checked.insert(item);
                }
            }
            UiMessage::ToggleSection { staged } => self.toggle_section(staged),
            UiMessage::ToggleDir { staged, path } => {
                let key = dir_key(staged, &path);
                if !self.collapsed.remove(&key) {
                    self.collapsed.insert(key);
                }
            }
            UiMessage::ToggleBranchDir { remote, path } => {
                let key = branch_dir_key(remote, &path);
                if !self.collapsed.remove(&key) {
                    self.collapsed.insert(key);
                }
            }
            UiMessage::ToggleDirChecked { staged, paths } => self.toggle_dir_checks(staged, paths),
            UiMessage::CommitMessageChanged(value) => self.commit.message = value,
            UiMessage::ToggleAmend => self.toggle_amend(),
            UiMessage::DismissStatus => self.status.message = None,
            UiMessage::SelectNext => self.move_selection(1),
            UiMessage::SelectPrevious => self.move_selection(-1),
            UiMessage::ShowView(view) => self.show_view(view),
            UiMessage::CommitSelected(sha) => self.select_commit(sha),
            UiMessage::OpenMenu(target) => self.open_menu(target),
            UiMessage::ArmMenu => {
                if let Some(menu) = &mut self.menu {
                    menu.armed = true;
                }
            }
            UiMessage::NewBranchNameChanged(name) => self.branches.new_name = name,
            UiMessage::NewTagNameChanged(name) => self.tags.new_name = name,
            UiMessage::TagMessageChanged(message) => self.tags.message = message,
            UiMessage::StashMessageChanged(message) => self.stashes.message = message,
            UiMessage::StashSelected(index) => self.select_stash(index),
            UiMessage::EditConflict => self.open_conflict_editor(),
            UiMessage::ConflictEdited(action) => {
                if let Some(edit) = &mut self.repo.editing {
                    edit.content.perform(action);
                }
            }
            UiMessage::CancelConflictEdit => self.repo.editing = None,
            UiMessage::ShowBlame => {
                if let Some(selection) = &self.repo.selected {
                    self.repo.blame = None;
                    self.dispatch(GitCommand::LoadBlame(selection.path.clone()));
                }
            }
            UiMessage::HideBlame => self.repo.blame = None,
        }
    }

    /// Open the manual editor for the selected conflicted file, seeding the buffer
    /// with the file's current Working Tree content (markers included).
    fn open_conflict_editor(&mut self) {
        if let Some(file) = &self.repo.conflict {
            self.repo.editing = Some(ConflictEdit {
                path: file.path.clone(),
                content: iced::widget::text_editor::Content::with_text(&file.raw),
            });
        }
    }

    /// Select a stash and request its Diff for the detail panel.
    fn select_stash(&mut self, index: usize) {
        self.stashes.selected = Some(index);
        self.stashes.diff = None;
        self.dispatch(GitCommand::LoadStashDiff(index));
    }

    /// Switch the left-column view, (re)loading the data that view needs.
    fn show_view(&mut self, view: ViewMode) {
        self.view = view;
        match view {
            ViewMode::History => self.dispatch(GitCommand::LoadHistory),
            ViewMode::Branches => self.dispatch(GitCommand::LoadBranches),
            ViewMode::Stashes => self.dispatch(GitCommand::LoadStashes),
            ViewMode::Tags => self.dispatch(GitCommand::LoadTags),
            ViewMode::Changes => {}
        }
    }

    /// Open a right-click context menu for `target` at the cursor. A commit
    /// target is also selected so its detail shows alongside the menu.
    fn open_menu(&mut self, target: MenuTarget) {
        self.menu = Some(ContextMenu {
            target: target.clone(),
            at: self.cursor,
            armed: false,
        });
        if let MenuTarget::Commit { sha, .. } = target {
            self.select_commit(sha);
        }
    }

    /// Select a Commit and request its detail.
    fn select_commit(&mut self, sha: String) {
        self.history.selected = Some(sha.clone());
        self.history.detail = None;
        self.dispatch(GitCommand::LoadCommitDetail(sha));
    }

    fn update_git(&mut self, message: GitMessage) {
        // Re-pressing Discard or Prune keeps its own arm; any other action
        // cancels it. (The context-menu actions confirm via the menu itself.)
        if !matches!(message, GitMessage::DiscardChecked) {
            self.discard_armed = false;
        }
        if !matches!(message, GitMessage::PruneBranches) {
            self.branches.prune_armed = false;
        }
        // Any git action fires on this press, so it also dismisses the menu.
        self.menu = None;

        match message {
            GitMessage::StageSelected => {
                if let Some(selection) = &self.repo.selected
                    && !selection.staged
                {
                    self.dispatch(GitCommand::StageFile(selection.path.clone()));
                }
            }
            GitMessage::UnstageSelected => {
                if let Some(selection) = &self.repo.selected
                    && selection.staged
                {
                    self.dispatch(GitCommand::UnstageFile(selection.path.clone()));
                }
            }
            GitMessage::StageChecked => {
                let paths = self.checked_paths(false);
                if paths.is_empty() {
                    self.dispatch(GitCommand::StageAll);
                } else {
                    for path in paths {
                        self.dispatch(GitCommand::StageFile(path));
                    }
                }
            }
            GitMessage::UnstageChecked => {
                let paths = self.checked_paths(true);
                if paths.is_empty() {
                    self.dispatch(GitCommand::UnstageAll);
                } else {
                    for path in paths {
                        self.dispatch(GitCommand::UnstageFile(path));
                    }
                }
            }
            GitMessage::DiscardChecked => {
                // First press arms; the confirming second press discards.
                if !self.discard_armed {
                    self.discard_armed = true;
                    return;
                }
                self.discard_armed = false;
                let paths = self.checked_paths(false);
                if paths.is_empty() {
                    self.dispatch(GitCommand::DiscardAll);
                } else {
                    for path in paths {
                        self.dispatch(GitCommand::Discard(path));
                    }
                }
            }
            GitMessage::Refresh => {
                self.dispatch(GitCommand::RefreshStatus);
                // Also refresh the open Diff in case the file changed on disk.
                if let Some(selection) = &self.repo.selected {
                    self.dispatch(GitCommand::LoadDiff {
                        path: selection.path.clone(),
                        staged: selection.staged,
                    });
                }
                // Keep the history fresh while it is on screen.
                if self.view == ViewMode::History {
                    self.dispatch(GitCommand::LoadHistory);
                }
            }
            GitMessage::StageHunk { path, hunk } => {
                self.dispatch(GitCommand::StageHunk { path, hunk });
            }
            GitMessage::UnstageHunk { path, hunk } => {
                self.dispatch(GitCommand::UnstageHunk { path, hunk });
            }
            GitMessage::Commit => self.start_commit(false),
            GitMessage::CommitAndPush => self.start_commit(true),
            GitMessage::Checkout(name) => self.dispatch(GitCommand::Checkout(name)),
            GitMessage::Merge(name) => self.dispatch(GitCommand::Merge(name)),
            GitMessage::ResolveConflict { path, side } => {
                self.dispatch(GitCommand::ResolveConflict { path, side })
            }
            GitMessage::ResolveHunk { path, index, side } => {
                self.dispatch(GitCommand::ResolveHunk { path, index, side })
            }
            GitMessage::SaveConflictEdit => {
                if let Some(edit) = self.repo.editing.take() {
                    self.dispatch(GitCommand::SaveConflict {
                        path: edit.path,
                        content: edit.content.text(),
                    });
                }
            }
            GitMessage::AbortMerge => self.dispatch(GitCommand::AbortMerge),
            GitMessage::CreateBranch => {
                let name = self.branches.new_name.trim().to_string();
                if !name.is_empty() {
                    self.dispatch(GitCommand::CreateBranch(name));
                }
            }
            GitMessage::DeleteBranch(name) => {
                // The confirmation is handled in the context menu (ArmMenu), so
                // by the time this arrives it is confirmed.
                self.dispatch(GitCommand::DeleteBranch(name));
            }
            GitMessage::PruneBranches => {
                // First press arms; the confirming second prunes.
                if self.branches.prune_armed {
                    self.branches.prune_armed = false;
                    self.dispatch(GitCommand::PruneBranches);
                } else {
                    self.branches.prune_armed = true;
                }
            }
            GitMessage::CreateTag => {
                let name = self.tags.new_name.trim().to_string();
                if !name.is_empty() {
                    let message = self.tags.message.trim();
                    let message = (!message.is_empty()).then(|| message.to_string());
                    self.dispatch(GitCommand::CreateTag { name, message });
                }
            }
            GitMessage::DeleteTag(name) => self.dispatch(GitCommand::DeleteTag(name)),
            GitMessage::PushTag(name) => {
                self.start_remote("Pushing tag…", GitCommand::PushTag(name))
            }
            GitMessage::Reset { sha, kind } => {
                // The Hard-reset confirmation is handled in the menu (ArmMenu).
                self.dispatch(GitCommand::Reset { sha, kind });
            }
            GitMessage::Revert(sha) => self.dispatch(GitCommand::Revert(sha)),
            GitMessage::CherryPick(sha) => self.dispatch(GitCommand::CherryPick(sha)),
            GitMessage::Push => self.start_remote("Pushing…", GitCommand::Push),
            GitMessage::Pull => self.start_remote("Pulling…", GitCommand::Pull),
            GitMessage::Fetch => self.start_remote("Fetching…", GitCommand::Fetch),
            GitMessage::Stash => {
                // Stash the checked files (deduped across both sides); with none
                // checked, `paths` is empty and everything is stashed.
                let mut paths: Vec<String> =
                    self.checked.iter().map(|item| item.path.clone()).collect();
                paths.sort();
                paths.dedup();
                self.dispatch(GitCommand::StashPush {
                    message: None,
                    paths,
                });
            }
            GitMessage::StashAll => {
                let message = self.stashes.message.trim();
                let message = (!message.is_empty()).then(|| message.to_string());
                self.dispatch(GitCommand::StashPush {
                    message,
                    paths: vec![],
                });
            }
            GitMessage::StashApply(index) => self.dispatch(GitCommand::StashApply(index)),
            GitMessage::StashPop(index) => self.dispatch(GitCommand::StashPop(index)),
            GitMessage::StashDrop(index) => self.dispatch(GitCommand::StashDrop(index)),
        }
    }

    /// Paths of the checked files on one side of the File List.
    fn checked_paths(&self, staged: bool) -> Vec<String> {
        self.checked
            .iter()
            .filter(|item| item.staged == staged)
            .map(|item| item.path.clone())
            .collect()
    }

    /// Check every file in a section, or uncheck them all if they already are.
    fn toggle_section(&mut self, staged: bool) {
        let list = if staged {
            &self.repo.staged
        } else {
            &self.repo.unstaged
        };
        let all_checked = !list.is_empty()
            && list.iter().all(|entry| {
                self.checked.contains(&Selection {
                    path: entry.path.clone(),
                    staged,
                })
            });

        let items: Vec<Selection> = list
            .iter()
            .map(|entry| Selection {
                path: entry.path.clone(),
                staged,
            })
            .collect();

        for item in items {
            if all_checked {
                self.checked.remove(&item);
            } else {
                self.checked.insert(item);
            }
        }
    }

    /// Check or uncheck every file under a directory: clear them if all are
    /// already checked, otherwise check the lot.
    fn toggle_dir_checks(&mut self, staged: bool, paths: Vec<String>) {
        let all_checked = !paths.is_empty()
            && paths.iter().all(|path| {
                self.checked.contains(&Selection {
                    path: path.clone(),
                    staged,
                })
            });

        for path in paths {
            let item = Selection { path, staged };
            if all_checked {
                self.checked.remove(&item);
            } else {
                self.checked.insert(item);
            }
        }
    }

    fn update_event(&mut self, event: GitEvent) {
        match event {
            GitEvent::StatusLoaded {
                unstaged,
                staged,
                conflicted,
                head,
            } => {
                self.repo.unstaged = unstaged;
                self.repo.staged = staged;
                self.repo.conflicted = conflicted;
                self.repo.head = head;
                self.reconcile_selection();
                self.prune_checked();
                // With conflicts present and nothing selected, open the first one
                // so the resolver is shown straight away.
                if self.repo.selected.is_none()
                    && let Some(first) = self.repo.conflicted.first()
                {
                    self.select(first.path.clone(), false);
                }
            }
            GitEvent::DiffLoaded(diff) => {
                // Discard a Diff that no longer matches the current selection
                // (the user may have moved on before it arrived).
                let matches = self
                    .repo
                    .selected
                    .as_ref()
                    .is_some_and(|s| s.path == diff.path && s.staged == diff.staged);
                if matches {
                    self.repo.diff = Some(diff);
                }
            }
            GitEvent::ConflictLoaded(file) => {
                // Keep it only if it still matches the selected conflicted file.
                let matches = self
                    .repo
                    .selected
                    .as_ref()
                    .is_some_and(|s| !s.staged && s.path == file.path);
                if matches {
                    self.repo.conflict = Some(file);
                }
            }
            GitEvent::BlameLoaded(file) => {
                // Keep it only if it still matches the selected file.
                let matches = self
                    .repo
                    .selected
                    .as_ref()
                    .is_some_and(|s| s.path == file.path);
                if matches {
                    self.repo.blame = Some(file);
                }
            }
            GitEvent::HistoryLoaded(commits) => {
                self.history.commits = commits;
                self.reconcile_commit_selection();
                // Default to the newest Commit so the detail panel is never
                // empty on entering the History view.
                if self.history.selected.is_none()
                    && let Some(first) = self.history.commits.first()
                {
                    self.select_commit(first.sha.clone());
                }
            }
            GitEvent::CommitDetailLoaded(detail) => {
                // Ignore a detail that no longer matches the selection (the user
                // may have moved on before it arrived).
                if self.history.selected.as_deref() == Some(detail.sha.as_str()) {
                    self.history.detail = Some(detail);
                }
            }
            GitEvent::ResetDone(sha) => {
                self.notify(format!("Reset to {sha}"));
                // HEAD moved, so the history and any open diff are stale.
                self.dispatch(GitCommand::LoadHistory);
            }
            GitEvent::Reverted { outcome } => match outcome {
                RevertOutcome::Created => {
                    self.notify("Reverted commit".to_string());
                    self.dispatch(GitCommand::LoadHistory);
                }
                RevertOutcome::Conflicts(n) => {
                    // Surface the conflicts; the user resolves them in the
                    // Changes view and commits (which finishes the revert).
                    let files = if n == 1 { "file" } else { "files" };
                    self.status.message = Some(format!(
                        "Revert has conflicts in {n} {files} — resolve them, then commit"
                    ));
                    self.view = ViewMode::Changes;
                }
            },
            GitEvent::CherryPicked { outcome } => match outcome {
                CherryPickOutcome::Created => {
                    self.notify("Cherry-picked commit".to_string());
                    self.dispatch(GitCommand::LoadHistory);
                }
                CherryPickOutcome::Conflicts(n) => {
                    // Surface the conflicts; the user resolves them in the
                    // Changes view and commits (which finishes the cherry-pick).
                    let files = if n == 1 { "file" } else { "files" };
                    self.status.message = Some(format!(
                        "Cherry-pick has conflicts in {n} {files} — resolve them, then commit"
                    ));
                    self.view = ViewMode::Changes;
                }
            },
            GitEvent::Committed(sha) => {
                self.commit.committing = false;
                self.commit.message.clear();
                self.commit.amend = false;
                self.notify(format!("Committed {sha}"));
                // A new (or amended) Commit changes history; keep it current.
                self.dispatch(GitCommand::LoadHistory);
                // "Commit & Push" chains a Push once the Commit has landed.
                if std::mem::take(&mut self.commit.push_after_commit) {
                    self.start_remote("Pushing…", GitCommand::Push);
                }
            }
            GitEvent::BranchesLoaded(branches) => {
                self.branches.branches = branches;
            }
            GitEvent::CheckedOut(name) => {
                self.notify(format!("Switched to {name}"));
                self.branches.new_name.clear();
                // The new branch has its own history; refresh if it is showing.
                if self.view == ViewMode::History {
                    self.dispatch(GitCommand::LoadHistory);
                }
            }
            GitEvent::BranchDeleted(name) => {
                self.notify(format!("Deleted {name}"));
            }
            GitEvent::BranchesPruned(pruned) => {
                self.branches.prune_armed = false;
                self.notify(match pruned.len() {
                    0 => "No branches to prune".to_string(),
                    1 => format!("Pruned {}", pruned[0]),
                    n => format!("Pruned {n} branches"),
                });
            }
            GitEvent::TagsLoaded(tags) => {
                self.tags.tags = tags;
            }
            GitEvent::TagCreated(name) => {
                self.tags.new_name.clear();
                self.tags.message.clear();
                self.notify(format!("Created tag {name}"));
            }
            GitEvent::TagDeleted(name) => {
                self.notify(format!("Deleted tag {name}"));
            }
            GitEvent::Pushed => {
                self.operation = None;
                self.notify("Pushed to remote".to_string());
            }
            GitEvent::Pulled => {
                self.operation = None;
                self.notify("Pulled from remote".to_string());
            }
            GitEvent::Fetched => {
                self.operation = None;
                self.notify("Fetched from remote".to_string());
            }
            GitEvent::StashesLoaded(stashes) => {
                self.stashes.stashes = stashes;
                // The list was (re)loaded, so the shown Diff may now point at a
                // different or vanished stash: reset the selection.
                self.stashes.selected = None;
                self.stashes.diff = None;
            }
            GitEvent::StashDiffLoaded(diff) => {
                // Ignore a Diff that no longer matches the selection.
                if self.stashes.selected == Some(diff.index) {
                    self.stashes.diff = Some(diff);
                }
            }
            GitEvent::Stashed => {
                self.stashes.message.clear();
                self.notify("Stashed changes".to_string());
            }
            GitEvent::StashApplied => self.notify("Applied stash".to_string()),
            GitEvent::StashDropped => {
                self.notify("Dropped stash".to_string());
            }
            GitEvent::Merged { branch, outcome } => {
                match outcome {
                    MergeOutcome::UpToDate => self.notify(format!("Already up to date with {branch}")),
                    MergeOutcome::FastForwarded => {
                        self.notify(format!("Fast-forwarded to {branch}"))
                    }
                    MergeOutcome::Created => self.notify(format!("Merged {branch}")),
                    MergeOutcome::Conflicts(n) => {
                        // Surface the conflicts; the user resolves them in the
                        // Changes view and commits (which finishes the merge).
                        let files = if n == 1 { "file" } else { "files" };
                        self.status.message = Some(format!(
                            "Merge of {branch} has conflicts in {n} {files} — resolve them, then commit"
                        ));
                        self.view = ViewMode::Changes;
                    }
                }
                // A merge changes history and the working tree; keep views fresh.
                if self.view == ViewMode::History {
                    self.dispatch(GitCommand::LoadHistory);
                }
            }
            GitEvent::Error(error) => {
                self.commit.committing = false;
                // A failed Commit must not trigger the queued Push.
                self.commit.push_after_commit = false;
                self.operation = None;
                self.status.message = Some(error.to_string());
            }
        }
    }

    /// Select a file. A conflicted file loads its parsed regions (for region-by-
    /// region resolution); any other file loads its Diff.
    fn select(&mut self, path: String, staged: bool) {
        // Switching files abandons any open manual edit of the previous one.
        if self.repo.editing.as_ref().is_some_and(|e| e.path != path) {
            self.repo.editing = None;
        }
        // Selecting a file returns to its diff; a stale blame would be the wrong
        // file's, so drop it (the user re-opens Blame if they want it).
        if self.repo.blame.as_ref().is_some_and(|b| b.path != path) {
            self.repo.blame = None;
        }
        self.repo.selected = Some(Selection {
            path: path.clone(),
            staged,
        });
        let conflicted = !staged && self.repo.conflicted.iter().any(|e| e.path == path);
        if conflicted {
            self.repo.diff = None;
            self.repo.conflict = None;
            self.dispatch(GitCommand::LoadConflict(path));
        } else {
            self.repo.conflict = None;
            self.dispatch(GitCommand::LoadDiff { path, staged });
        }
    }

    /// Move selection through the flat list of Unstaged then Staged files.
    fn move_selection(&mut self, delta: isize) {
        let entries: Vec<Selection> = self
            .repo
            .unstaged
            .iter()
            .map(|entry| Selection {
                path: entry.path.clone(),
                staged: false,
            })
            .chain(self.repo.staged.iter().map(|entry| Selection {
                path: entry.path.clone(),
                staged: true,
            }))
            .collect();

        if entries.is_empty() {
            return;
        }

        let current = self
            .repo
            .selected
            .as_ref()
            .and_then(|selection| entries.iter().position(|entry| entry == selection));

        let next = match current {
            Some(index) => (index as isize + delta).clamp(0, entries.len() as isize - 1) as usize,
            None if delta > 0 => 0,
            None => entries.len() - 1,
        };

        let selection = entries[next].clone();
        self.select(selection.path, selection.staged);
    }

    /// Flip the amend toggle. Turning it on with an empty field prefills the
    /// message with the last Commit's summary, the usual starting point.
    fn toggle_amend(&mut self) {
        self.commit.amend = !self.commit.amend;
        if self.commit.amend
            && self.commit.message.trim().is_empty()
            && let Some(commit) = &self.repo.head.last_commit
        {
            self.commit.message = commit.summary.clone();
        }
    }

    /// Begin a Commit (or Amend). When `then_push` is set, the Commit will be
    /// followed by a Push once it lands (see the `Committed` event handler).
    fn start_commit(&mut self, then_push: bool) {
        let message = self.commit.message.trim().to_string();
        if message.is_empty() {
            self.status.message = Some("Cannot commit: the message is empty".to_string());
            return;
        }
        if !self.repo.conflicted.is_empty() {
            self.status.message =
                Some("Cannot commit: resolve the merge conflicts first".to_string());
            return;
        }

        // Amend replaces HEAD, so it needs a Commit to amend but not staged
        // changes (amending only the message is valid).
        if self.commit.amend {
            if self.repo.head.last_commit.is_none() {
                self.status.message = Some("Cannot amend: there is no commit yet".to_string());
                return;
            }
            self.commit.committing = true;
            self.commit.push_after_commit = then_push;
            self.dispatch(GitCommand::Amend(message));
            return;
        }

        if self.repo.staged.is_empty() {
            self.status.message = Some("Cannot commit: nothing is staged".to_string());
            return;
        }
        self.commit.committing = true;
        self.commit.push_after_commit = then_push;
        self.dispatch(GitCommand::Commit(message));
    }

    fn start_remote(&mut self, label: &str, command: GitCommand) {
        self.operation = Some(label.to_string());
        self.dispatch(command);
    }

    /// Drop the selection and Diff if the selected file is gone after a refresh.
    fn reconcile_selection(&mut self) {
        let still_present = self.repo.selected.as_ref().is_some_and(|selection| {
            // A working-tree selection may be an unstaged or a conflicted file.
            let in_list = |list: &[FileEntry]| list.iter().any(|e| e.path == selection.path);
            if selection.staged {
                in_list(&self.repo.staged)
            } else {
                in_list(&self.repo.unstaged) || in_list(&self.repo.conflicted)
            }
        });

        if self.repo.selected.is_some() && !still_present {
            self.repo.selected = None;
            self.repo.diff = None;
            self.repo.blame = None;
        }

        // The conflict view only applies while the selected file is still
        // conflicted; drop it once the file leaves the conflicted list.
        let still_conflicted = self
            .repo
            .selected
            .as_ref()
            .is_some_and(|s| !s.staged && self.repo.conflicted.iter().any(|e| e.path == s.path));
        if !still_conflicted {
            self.repo.conflict = None;
            self.repo.editing = None;
        }
    }

    /// Drop the Commit selection and its detail if that Commit is no longer in
    /// the loaded history (e.g. after an amend or a rebase elsewhere).
    fn reconcile_commit_selection(&mut self) {
        let still_present = self
            .history
            .selected
            .as_ref()
            .is_some_and(|sha| self.history.commits.iter().any(|c| &c.sha == sha));
        if self.history.selected.is_some() && !still_present {
            self.history.selected = None;
            self.history.detail = None;
        }
    }

    /// Drop checked entries whose file no longer exists on its side (it was
    /// staged, committed, or discarded), so the checkbox state stays truthful.
    fn prune_checked(&mut self) {
        let repo = &self.repo;
        self.checked.retain(|item| {
            let list = if item.staged {
                &repo.staged
            } else {
                &repo.unstaged
            };
            list.iter().any(|entry| entry.path == item.path)
        });
    }

    fn notify(&mut self, message: String) {
        self.notification.message = Some(message);
        self.notification.expires_at = Some(Instant::now() + NOTIFICATION_TTL);
    }

    fn expire_notification(&mut self) {
        if let Some(expiry) = self.notification.expires_at
            && Instant::now() >= expiry
        {
            self.notification.message = None;
            self.notification.expires_at = None;
        }
    }
}

/// The iced `update` entry point.
fn update(app: &mut App, message: Message) -> Task<Message> {
    match message {
        Message::WorkerReady(commands) => {
            app.commands = Some(commands);
            app.dispatch(GitCommand::RefreshStatus);
        }
        Message::Ui(message) => app.update_ui(message),
        Message::Git(message) => app.update_git(message),
        Message::GitEvent(event) => app.update_event(event),
        Message::CursorMoved(point) => app.cursor = point,
        Message::Tick => app.expire_notification(),
        Message::Ignored => {}
    }
    Task::none()
}

/// The iced `view` entry point.
fn view(app: &App) -> iced::Element<'_, Message> {
    ui::root(app)
}

/// All the streams the app listens to: the Git Worker, the keyboard, and a
/// timer that expires Notifications.
fn subscription(app: &App) -> Subscription<Message> {
    let mut subs = vec![
        Subscription::run(git_worker),
        iced::keyboard::listen().map(on_key),
        iced::time::every(Duration::from_millis(500)).map(|_| Message::Tick),
    ];
    // Track the pointer in the list views, where a right-click menu opens at
    // the cursor; the Changes view has no menu, so it skips the tracking (and
    // its per-move re-renders).
    if app.view != ViewMode::Changes {
        subs.push(iced::event::listen_with(|event, _status, _window| {
            match event {
                iced::Event::Mouse(iced::mouse::Event::CursorMoved { position }) => {
                    Some(Message::CursorMoved(position))
                }
                _ => None,
            }
        }));
    }
    Subscription::batch(subs)
}

/// Spawn the Git Worker thread and bridge its events into the iced runtime.
///
/// The command channel (UI -> Worker) is `std::sync::mpsc`; the event channel
/// (Worker -> UI) is a `futures` channel so it can be polled as a `Stream`.
fn git_worker() -> impl Stream<Item = Message> {
    iced::stream::channel(
        64,
        |mut output: futures::channel::mpsc::Sender<Message>| async move {
            let (command_tx, command_rx) = std::sync::mpsc::channel::<GitCommand>();
            let (event_tx, mut event_rx) = futures::channel::mpsc::unbounded::<GitEvent>();

            std::thread::Builder::new()
                .name("git-worker".to_string())
                .spawn(move || git::run(command_rx, event_tx))
                .expect("failed to spawn the Git Worker thread");

            // Hand the command channel to the app; it triggers the first refresh.
            let _ = output.send(Message::WorkerReady(command_tx)).await;

            while let Some(event) = event_rx.next().await {
                if output.send(Message::GitEvent(event)).await.is_err() {
                    break;
                }
            }
        },
    )
}

/// Translate a keyboard event into a [`Message`]. The command modifier is
/// Ctrl on Linux and Cmd on macOS.
fn on_key(event: KeyEvent) -> Message {
    let KeyEvent::KeyPressed { key, modifiers, .. } = event else {
        return Message::Ignored;
    };

    let command = modifiers.command();
    match key.as_ref() {
        Key::Named(Named::ArrowDown) => Message::Ui(UiMessage::SelectNext),
        Key::Named(Named::ArrowUp) => Message::Ui(UiMessage::SelectPrevious),
        Key::Named(Named::Escape) => Message::Ui(UiMessage::DismissStatus),
        Key::Character("1") if command => Message::Ui(UiMessage::ShowView(ViewMode::Changes)),
        Key::Character("2") if command => Message::Ui(UiMessage::ShowView(ViewMode::History)),
        Key::Character("3") if command => Message::Ui(UiMessage::ShowView(ViewMode::Branches)),
        Key::Character("4") if command => Message::Ui(UiMessage::ShowView(ViewMode::Stashes)),
        Key::Character("5") if command => Message::Ui(UiMessage::ShowView(ViewMode::Tags)),
        Key::Named(Named::F5) => Message::Git(GitMessage::Refresh),
        Key::Character("r") if command => Message::Git(GitMessage::Refresh),
        Key::Named(Named::Enter) if command => Message::Git(GitMessage::Commit),
        Key::Character("s") if command => Message::Git(GitMessage::StageSelected),
        Key::Character("u") if command => Message::Git(GitMessage::UnstageSelected),
        Key::Character("p") if command && modifiers.shift() => Message::Git(GitMessage::Pull),
        Key::Character("p") if command => Message::Git(GitMessage::Push),
        Key::Character("f") if command => Message::Git(GitMessage::Fetch),
        _ => Message::Ignored,
    }
}

#[cfg(test)]
mod tests {
    //! Seam 2 (PRD "Testing Decisions"): `update()` as a near-pure function —
    //! given an `App` and a `Message`, assert on the resulting state. No iced
    //! rendering is invoked.

    use super::*;
    use crate::git::{ChangeKind, Diff, DiffLine, DiffLineKind};

    /// Drive `update` and drop the returned `Task` (there is no runtime here).
    fn update(app: &mut App, message: Message) {
        let _ = super::update(app, message);
    }

    fn entry(path: &str, change: ChangeKind) -> FileEntry {
        FileEntry {
            path: path.to_string(),
            change,
        }
    }

    /// A `StatusLoaded` event with default (empty) HEAD context.
    fn status_event(unstaged: Vec<FileEntry>, staged: Vec<FileEntry>) -> Message {
        Message::GitEvent(GitEvent::StatusLoaded {
            unstaged,
            staged,
            conflicted: Vec::new(),
            head: HeadInfo::default(),
        })
    }

    fn diff(path: &str, staged: bool) -> Diff {
        Diff {
            path: path.to_string(),
            staged,
            lines: vec![DiffLine {
                kind: DiffLineKind::Addition,
                old_lineno: None,
                new_lineno: Some(1),
                content: "x".to_string(),
            }],
        }
    }

    #[test]
    fn status_loaded_populates_the_file_lists() {
        let mut app = App::new();

        update(
            &mut app,
            status_event(
                vec![entry("a.txt", ChangeKind::Modified)],
                vec![entry("b.txt", ChangeKind::Added)],
            ),
        );

        assert_eq!(app.repo.unstaged.len(), 1);
        assert_eq!(app.repo.unstaged[0].path, "a.txt");
        assert_eq!(app.repo.staged[0].path, "b.txt");
    }

    #[test]
    fn status_loaded_clears_selection_when_file_disappears() {
        let mut app = App::new();
        app.repo.selected = Some(Selection {
            path: "a.txt".to_string(),
            staged: false,
        });
        app.repo.diff = Some(diff("a.txt", false));

        // A refresh in which "a.txt" is no longer unstaged.
        update(&mut app, status_event(vec![], vec![]));

        assert!(app.repo.selected.is_none());
        assert!(app.repo.diff.is_none());
    }

    #[test]
    fn status_loaded_keeps_selection_when_file_remains() {
        let mut app = App::new();
        app.repo.selected = Some(Selection {
            path: "a.txt".to_string(),
            staged: false,
        });

        update(
            &mut app,
            status_event(vec![entry("a.txt", ChangeKind::Modified)], vec![]),
        );

        assert_eq!(
            app.repo.selected.as_ref().map(|s| s.path.as_str()),
            Some("a.txt")
        );
    }

    #[test]
    fn diff_loaded_is_applied_only_when_it_matches_the_selection() {
        let mut app = App::new();
        app.repo.selected = Some(Selection {
            path: "a.txt".to_string(),
            staged: false,
        });

        // A diff for a different file is ignored.
        update(
            &mut app,
            Message::GitEvent(GitEvent::DiffLoaded(diff("other.txt", false))),
        );
        assert!(app.repo.diff.is_none());

        // The matching diff is stored.
        update(
            &mut app,
            Message::GitEvent(GitEvent::DiffLoaded(diff("a.txt", false))),
        );
        assert_eq!(
            app.repo.diff.as_ref().map(|d| d.path.as_str()),
            Some("a.txt")
        );
    }

    #[test]
    fn committed_clears_the_message_and_shows_a_notification() {
        let mut app = App::new();
        app.commit.message = "my change".to_string();
        app.commit.committing = true;

        update(
            &mut app,
            Message::GitEvent(GitEvent::Committed("abc1234".to_string())),
        );

        assert!(app.commit.message.is_empty());
        assert!(!app.commit.committing);
        assert_eq!(
            app.notification.message.as_deref(),
            Some("Committed abc1234")
        );
    }

    #[test]
    fn commit_and_push_arms_the_push_then_fires_it_when_the_commit_lands() {
        let mut app = App::new();
        app.repo.staged = vec![entry("a.txt", ChangeKind::Added)];
        app.repo.head.has_remote = true;
        app.commit.message = "ship it".to_string();

        // The combined action commits and queues a Push.
        update(&mut app, Message::Git(GitMessage::CommitAndPush));
        assert!(app.commit.committing);
        assert!(app.commit.push_after_commit);

        // When the Commit lands, the Push starts and the flag is consumed.
        update(
            &mut app,
            Message::GitEvent(GitEvent::Committed("abc1234".to_string())),
        );
        assert!(!app.commit.push_after_commit);
        assert_eq!(app.operation.as_deref(), Some("Pushing…"));
    }

    #[test]
    fn a_plain_commit_does_not_queue_a_push() {
        let mut app = App::new();
        app.repo.staged = vec![entry("a.txt", ChangeKind::Added)];
        app.commit.message = "just commit".to_string();

        update(&mut app, Message::Git(GitMessage::Commit));
        assert!(!app.commit.push_after_commit);

        update(
            &mut app,
            Message::GitEvent(GitEvent::Committed("abc1234".to_string())),
        );
        assert!(app.operation.is_none());
    }

    #[test]
    fn a_failed_commit_clears_a_queued_push() {
        let mut app = App::new();
        app.commit.committing = true;
        app.commit.push_after_commit = true;

        update(
            &mut app,
            Message::GitEvent(GitEvent::Error(crate::git::GitError::custom(
                "commit", "boom",
            ))),
        );

        assert!(!app.commit.push_after_commit);
        assert!(app.operation.is_none());
    }

    #[test]
    fn error_event_surfaces_in_the_status_bar_and_clears_operation() {
        let mut app = App::new();
        app.operation = Some("Pushing…".to_string());

        update(
            &mut app,
            Message::GitEvent(GitEvent::Error(crate::git::GitError::custom(
                "push", "rejected",
            ))),
        );

        assert!(app.operation.is_none());
        assert_eq!(app.status.message.as_deref(), Some("push: rejected"));
    }

    #[test]
    fn commit_message_changed_updates_state() {
        let mut app = App::new();
        update(
            &mut app,
            Message::Ui(UiMessage::CommitMessageChanged("wip".to_string())),
        );
        assert_eq!(app.commit.message, "wip");
    }

    #[test]
    fn discard_requires_a_confirming_second_press() {
        let mut app = App::new();

        // First press only arms the confirmation.
        update(&mut app, Message::Git(GitMessage::DiscardChecked));
        assert!(app.discard_armed);

        // Second press fires and disarms.
        update(&mut app, Message::Git(GitMessage::DiscardChecked));
        assert!(!app.discard_armed);
    }

    #[test]
    fn opening_a_menu_then_clicking_elsewhere_dismisses_it() {
        let mut app = App::new();

        update(
            &mut app,
            Message::Ui(UiMessage::OpenMenu(MenuTarget::Stash { index: 1 })),
        );
        assert!(app.menu.is_some());

        // Any other interaction (the click lands on its target) closes the menu.
        update(&mut app, Message::Git(GitMessage::StageChecked));
        assert!(app.menu.is_none());
    }

    #[test]
    fn a_destructive_menu_action_arms_then_fires() {
        let mut app = App::new();
        update(
            &mut app,
            Message::Ui(UiMessage::OpenMenu(MenuTarget::Stash { index: 2 })),
        );

        // The first (Arm) press keeps the menu open and marks it armed.
        update(&mut app, Message::Ui(UiMessage::ArmMenu));
        assert!(app.menu.as_ref().is_some_and(|m| m.armed));

        // The confirming press fires the real action and dismisses the menu.
        update(&mut app, Message::Git(GitMessage::StashDrop(2)));
        assert!(app.menu.is_none());
    }

    #[test]
    fn right_clicking_another_row_repositions_the_menu() {
        let mut app = App::new();
        update(
            &mut app,
            Message::Ui(UiMessage::OpenMenu(MenuTarget::Stash { index: 0 })),
        );
        update(
            &mut app,
            Message::Ui(UiMessage::OpenMenu(MenuTarget::Tag {
                name: "v1".into(),
            })),
        );

        // The menu now targets the newly right-clicked row, not the first.
        assert!(matches!(
            app.menu.as_ref().map(|m| &m.target),
            Some(MenuTarget::Tag { name }) if name == "v1"
        ));
    }

    #[test]
    fn other_actions_cancel_a_pending_discard() {
        let mut app = App::new();
        update(&mut app, Message::Git(GitMessage::DiscardChecked));
        assert!(app.discard_armed);

        // Any unrelated interaction disarms it.
        update(&mut app, Message::Git(GitMessage::StageChecked));
        assert!(!app.discard_armed);
    }

    #[test]
    fn toggling_a_checkbox_adds_then_removes_it() {
        let mut app = App::new();
        let toggle = || {
            Message::Ui(UiMessage::ToggleChecked {
                path: "a.txt".to_string(),
                staged: false,
            })
        };

        update(&mut app, toggle());
        assert!(app.checked.contains(&Selection {
            path: "a.txt".to_string(),
            staged: false,
        }));

        update(&mut app, toggle());
        assert!(app.checked.is_empty());
    }

    #[test]
    fn toggle_section_checks_all_then_clears() {
        let mut app = App::new();
        app.repo.unstaged = vec![
            entry("a.txt", ChangeKind::Modified),
            entry("b.txt", ChangeKind::Modified),
        ];

        update(
            &mut app,
            Message::Ui(UiMessage::ToggleSection { staged: false }),
        );
        assert_eq!(app.checked.len(), 2);

        update(
            &mut app,
            Message::Ui(UiMessage::ToggleSection { staged: false }),
        );
        assert!(app.checked.is_empty());
    }

    #[test]
    fn toggle_dir_checks_all_descendants_then_clears() {
        let mut app = App::new();
        let paths = vec!["src/ui/mod.rs".to_string(), "src/app.rs".to_string()];

        update(
            &mut app,
            Message::Ui(UiMessage::ToggleDirChecked {
                staged: false,
                paths: paths.clone(),
            }),
        );
        assert_eq!(app.checked.len(), 2);
        assert!(app.checked.contains(&Selection {
            path: "src/ui/mod.rs".to_string(),
            staged: false,
        }));

        // A second toggle, with all already checked, clears them.
        update(
            &mut app,
            Message::Ui(UiMessage::ToggleDirChecked {
                staged: false,
                paths,
            }),
        );
        assert!(app.checked.is_empty());
    }

    #[test]
    fn toggle_dir_checks_completes_a_partial_selection() {
        let mut app = App::new();
        app.checked.insert(Selection {
            path: "src/app.rs".to_string(),
            staged: false,
        });

        // With only one of two files checked, toggling checks the rest.
        update(
            &mut app,
            Message::Ui(UiMessage::ToggleDirChecked {
                staged: false,
                paths: vec!["src/ui/mod.rs".to_string(), "src/app.rs".to_string()],
            }),
        );
        assert_eq!(app.checked.len(), 2);
    }

    #[test]
    fn refresh_prunes_checked_files_that_disappeared() {
        let mut app = App::new();
        app.checked.insert(Selection {
            path: "a.txt".to_string(),
            staged: false,
        });

        // A refresh where "a.txt" is no longer unstaged drops it from the set.
        update(&mut app, status_event(vec![], vec![]));
        assert!(app.checked.is_empty());
    }

    #[test]
    fn dismiss_status_clears_the_status_bar() {
        let mut app = App::new();
        app.status.message = Some("boom".to_string());
        update(&mut app, Message::Ui(UiMessage::DismissStatus));
        assert!(app.status.message.is_none());
    }

    #[test]
    fn select_next_moves_through_unstaged_then_staged() {
        let mut app = App::new();
        app.repo.unstaged = vec![entry("a.txt", ChangeKind::Modified)];
        app.repo.staged = vec![entry("b.txt", ChangeKind::Added)];

        // No selection yet: SelectNext picks the first unstaged file.
        update(&mut app, Message::Ui(UiMessage::SelectNext));
        assert_eq!(
            app.repo
                .selected
                .as_ref()
                .map(|s| (s.path.as_str(), s.staged)),
            Some(("a.txt", false))
        );

        // Next crosses into the staged section.
        update(&mut app, Message::Ui(UiMessage::SelectNext));
        assert_eq!(
            app.repo
                .selected
                .as_ref()
                .map(|s| (s.path.as_str(), s.staged)),
            Some(("b.txt", true))
        );
    }

    #[test]
    fn empty_commit_message_is_rejected_with_a_status_message() {
        let mut app = App::new();
        app.repo.staged = vec![entry("a.txt", ChangeKind::Added)];
        app.commit.message = "   ".to_string();

        update(&mut app, Message::Git(GitMessage::Commit));

        assert!(!app.commit.committing);
        assert!(app.status.message.is_some());
    }

    #[test]
    fn amend_toggle_prefills_message_and_commits_without_staging() {
        let mut app = App::new();
        app.repo.head.last_commit = Some(crate::git::CommitSummary {
            short_sha: "abc1234".to_string(),
            summary: "previous message".to_string(),
        });

        // Toggling amend on prefills the empty field with the last summary.
        update(&mut app, Message::Ui(UiMessage::ToggleAmend));
        assert!(app.commit.amend);
        assert_eq!(app.commit.message, "previous message");

        // Amend proceeds with nothing staged (only the message need change).
        assert!(app.repo.staged.is_empty());
        update(&mut app, Message::Git(GitMessage::Commit));
        assert!(app.commit.committing);
        assert!(app.status.message.is_none());
    }

    #[test]
    fn editing_a_conflict_seeds_the_buffer_then_save_clears_it() {
        let mut app = App::new();
        app.repo.selected = Some(Selection {
            path: "a.txt".to_string(),
            staged: false,
        });
        app.repo.conflict = Some(ConflictFile {
            path: "a.txt".to_string(),
            segments: Vec::new(),
            raw: "<<<<<<< HEAD\nours\n=======\ntheirs\n>>>>>>>\n".to_string(),
        });

        // Opening the editor seeds it from the conflict's raw content.
        update(&mut app, Message::Ui(UiMessage::EditConflict));
        let edit = app.repo.editing.as_ref().expect("editor should be open");
        assert_eq!(edit.path, "a.txt");
        assert!(edit.content.text().contains("<<<<<<<"));

        // Saving closes the editor (the worker takes over from here).
        update(&mut app, Message::Git(GitMessage::SaveConflictEdit));
        assert!(app.repo.editing.is_none());
    }

    #[test]
    fn blame_is_kept_when_it_matches_the_selection_then_hidden() {
        let mut app = App::new();
        app.repo.selected = Some(Selection {
            path: "a.txt".to_string(),
            staged: false,
        });

        let blame = crate::git::BlameFile {
            path: "a.txt".to_string(),
            lines: vec![crate::git::BlameLine {
                short_sha: "abc1234".to_string(),
                author: "Tester".to_string(),
                time: 1_700_000_000,
                content: "x".to_string(),
            }],
        };

        // Blame for the selected file is stored…
        update(&mut app, Message::GitEvent(GitEvent::BlameLoaded(blame)));
        assert!(app.repo.blame.is_some());

        // …and the Diff toggle clears it.
        update(&mut app, Message::Ui(UiMessage::HideBlame));
        assert!(app.repo.blame.is_none());
    }

    #[test]
    fn blame_for_another_file_is_ignored() {
        let mut app = App::new();
        app.repo.selected = Some(Selection {
            path: "a.txt".to_string(),
            staged: false,
        });

        let blame = crate::git::BlameFile {
            path: "other.txt".to_string(),
            lines: vec![],
        };
        update(&mut app, Message::GitEvent(GitEvent::BlameLoaded(blame)));
        assert!(app.repo.blame.is_none());
    }

    #[test]
    fn selecting_another_file_drops_a_stale_blame() {
        let mut app = App::new();
        app.repo.unstaged = vec![entry("b.txt", ChangeKind::Modified)];
        app.repo.selected = Some(Selection {
            path: "a.txt".to_string(),
            staged: false,
        });
        app.repo.blame = Some(crate::git::BlameFile {
            path: "a.txt".to_string(),
            lines: vec![],
        });

        update(
            &mut app,
            Message::Ui(UiMessage::FileSelected {
                path: "b.txt".to_string(),
                staged: false,
            }),
        );
        assert!(app.repo.blame.is_none());
    }

    #[test]
    fn cancelling_a_conflict_edit_closes_the_editor() {
        let mut app = App::new();
        app.repo.selected = Some(Selection {
            path: "a.txt".to_string(),
            staged: false,
        });
        app.repo.conflict = Some(ConflictFile {
            path: "a.txt".to_string(),
            segments: Vec::new(),
            raw: "x\n".to_string(),
        });

        update(&mut app, Message::Ui(UiMessage::EditConflict));
        assert!(app.repo.editing.is_some());

        update(&mut app, Message::Ui(UiMessage::CancelConflictEdit));
        assert!(app.repo.editing.is_none());
    }

    #[test]
    fn switching_files_abandons_an_open_conflict_edit() {
        let mut app = App::new();
        app.repo.unstaged = vec![entry("b.txt", ChangeKind::Modified)];
        app.repo.editing = Some(ConflictEdit {
            path: "a.txt".to_string(),
            content: iced::widget::text_editor::Content::with_text("x"),
        });

        // Selecting a different file drops the stale edit.
        update(
            &mut app,
            Message::Ui(UiMessage::FileSelected {
                path: "b.txt".to_string(),
                staged: false,
            }),
        );
        assert!(app.repo.editing.is_none());
    }

    #[test]
    fn amend_is_rejected_when_there_is_no_commit_yet() {
        let mut app = App::new();
        app.commit.amend = true;
        app.commit.message = "anything".to_string();

        update(&mut app, Message::Git(GitMessage::Commit));

        assert!(!app.commit.committing);
        assert!(app.status.message.is_some());
    }
}
