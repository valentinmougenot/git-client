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
use iced::{Subscription, Task};

use crate::git::{
    self, CommitDetail, CommitInfo, Diff, FileEntry, GitCommand, GitEvent, HeadInfo,
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

/// Which of the two left-column views is showing.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ViewMode {
    /// The working-tree changes: File List + Diff + Commit Panel.
    #[default]
    Changes,
    /// The Commit history: the log list + the selected Commit's detail.
    History,
}

/// The application root state.
pub struct App {
    /// Channel to the Git Worker; `None` until the worker has booted.
    commands: Option<Sender<GitCommand>>,
    pub repo: RepoState,
    pub commit: CommitState,
    pub history: HistoryState,
    /// Which left-column view is active.
    pub view: ViewMode,
    pub status: StatusBar,
    pub notification: Notification,
    /// Label of an in-progress remote operation (Push/Pull), if any.
    pub operation: Option<String>,
    /// The files checked for a bulk action (the action targets). Distinct from
    /// `repo.selected`, which is the one file whose Diff is shown.
    pub checked: HashSet<Selection>,
    /// Whether Discard is armed, awaiting a confirming second press.
    pub discard_armed: bool,
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

/// Working Tree and Staging Area contents, the HEAD/branch context, the
/// selected file, and its Diff.
#[derive(Default)]
pub struct RepoState {
    pub unstaged: Vec<FileEntry>,
    pub staged: Vec<FileEntry>,
    /// Current branch, sync state with the Remote, and last Commit.
    pub head: HeadInfo,
    pub selected: Option<Selection>,
    pub diff: Option<Diff>,
}

/// A file in the File List, identified by its path and which side it is on.
/// Used both for the active (diff) selection and for checked action targets.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct Selection {
    pub path: String,
    pub staged: bool,
}

/// In-progress commit message, whether a Commit is running, and whether the
/// next Commit amends HEAD instead of creating a new Commit.
#[derive(Default)]
pub struct CommitState {
    pub message: String,
    pub committing: bool,
    pub amend: bool,
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
    /// Re-read the Working Tree and Staging Area (picks up edits made outside
    /// the app) and reload the active Diff.
    Refresh,
    Commit,
    Push,
    Pull,
}

impl App {
    fn new() -> Self {
        App {
            commands: None,
            repo: RepoState::default(),
            commit: CommitState::default(),
            history: HistoryState::default(),
            view: ViewMode::default(),
            status: StatusBar::default(),
            notification: Notification::default(),
            operation: None,
            checked: HashSet::new(),
            discard_armed: false,
        }
    }

    /// Send a command to the Git Worker, if it has booted.
    fn dispatch(&self, command: GitCommand) {
        if let Some(commands) = &self.commands {
            let _ = commands.send(command);
        }
    }

    fn update_ui(&mut self, message: UiMessage) {
        // Interacting elsewhere cancels a pending Discard confirmation.
        self.discard_armed = false;

        match message {
            UiMessage::FileSelected { path, staged } => self.select(path, staged),
            UiMessage::ToggleChecked { path, staged } => {
                let item = Selection { path, staged };
                if !self.checked.remove(&item) {
                    self.checked.insert(item);
                }
            }
            UiMessage::ToggleSection { staged } => self.toggle_section(staged),
            UiMessage::CommitMessageChanged(value) => self.commit.message = value,
            UiMessage::ToggleAmend => self.toggle_amend(),
            UiMessage::DismissStatus => self.status.message = None,
            UiMessage::SelectNext => self.move_selection(1),
            UiMessage::SelectPrevious => self.move_selection(-1),
            UiMessage::ShowView(view) => self.show_view(view),
            UiMessage::CommitSelected(sha) => self.select_commit(sha),
        }
    }

    /// Switch the left-column view. Entering History (re)loads the log.
    fn show_view(&mut self, view: ViewMode) {
        self.view = view;
        if view == ViewMode::History {
            self.dispatch(GitCommand::LoadHistory);
        }
    }

    /// Select a Commit and request its detail.
    fn select_commit(&mut self, sha: String) {
        self.history.selected = Some(sha.clone());
        self.history.detail = None;
        self.dispatch(GitCommand::LoadCommitDetail(sha));
    }

    fn update_git(&mut self, message: GitMessage) {
        // Any git action other than re-pressing Discard cancels its pending
        // confirmation.
        if !matches!(message, GitMessage::DiscardChecked) {
            self.discard_armed = false;
        }

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
            GitMessage::Commit => self.start_commit(),
            GitMessage::Push => self.start_remote("Pushing…", GitCommand::Push),
            GitMessage::Pull => self.start_remote("Pulling…", GitCommand::Pull),
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

    fn update_event(&mut self, event: GitEvent) {
        match event {
            GitEvent::StatusLoaded {
                unstaged,
                staged,
                head,
            } => {
                self.repo.unstaged = unstaged;
                self.repo.staged = staged;
                self.repo.head = head;
                self.reconcile_selection();
                self.prune_checked();
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
            GitEvent::Committed(sha) => {
                self.commit.committing = false;
                self.commit.message.clear();
                self.commit.amend = false;
                self.notify(format!("Committed {sha}"));
                // A new (or amended) Commit changes history; keep it current.
                self.dispatch(GitCommand::LoadHistory);
            }
            GitEvent::Pushed => {
                self.operation = None;
                self.notify("Pushed to remote".to_string());
            }
            GitEvent::Pulled => {
                self.operation = None;
                self.notify("Pulled from remote".to_string());
            }
            GitEvent::Error(error) => {
                self.commit.committing = false;
                self.operation = None;
                self.status.message = Some(error.to_string());
            }
        }
    }

    /// Select a file and request its Diff.
    fn select(&mut self, path: String, staged: bool) {
        self.repo.selected = Some(Selection {
            path: path.clone(),
            staged,
        });
        self.dispatch(GitCommand::LoadDiff { path, staged });
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

    fn start_commit(&mut self) {
        let message = self.commit.message.trim().to_string();
        if message.is_empty() {
            self.status.message = Some("Cannot commit: the message is empty".to_string());
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
            self.dispatch(GitCommand::Amend(message));
            return;
        }

        if self.repo.staged.is_empty() {
            self.status.message = Some("Cannot commit: nothing is staged".to_string());
            return;
        }
        self.commit.committing = true;
        self.dispatch(GitCommand::Commit(message));
    }

    fn start_remote(&mut self, label: &str, command: GitCommand) {
        self.operation = Some(label.to_string());
        self.dispatch(command);
    }

    /// Drop the selection and Diff if the selected file is gone after a refresh.
    fn reconcile_selection(&mut self) {
        let still_present = self.repo.selected.as_ref().is_some_and(|selection| {
            let list = if selection.staged {
                &self.repo.staged
            } else {
                &self.repo.unstaged
            };
            list.iter().any(|entry| entry.path == selection.path)
        });

        if self.repo.selected.is_some() && !still_present {
            self.repo.selected = None;
            self.repo.diff = None;
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
fn subscription(_app: &App) -> Subscription<Message> {
    Subscription::batch([
        Subscription::run(git_worker),
        iced::keyboard::listen().map(on_key),
        iced::time::every(Duration::from_millis(500)).map(|_| Message::Tick),
    ])
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
        Key::Named(Named::F5) => Message::Git(GitMessage::Refresh),
        Key::Character("r") if command => Message::Git(GitMessage::Refresh),
        Key::Named(Named::Enter) if command => Message::Git(GitMessage::Commit),
        Key::Character("s") if command => Message::Git(GitMessage::StageSelected),
        Key::Character("u") if command => Message::Git(GitMessage::UnstageSelected),
        Key::Character("p") if command && modifiers.shift() => Message::Git(GitMessage::Pull),
        Key::Character("p") if command => Message::Git(GitMessage::Push),
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
    fn amend_is_rejected_when_there_is_no_commit_yet() {
        let mut app = App::new();
        app.commit.amend = true;
        app.commit.message = "anything".to_string();

        update(&mut app, Message::Git(GitMessage::Commit));

        assert!(!app.commit.committing);
        assert!(app.status.message.is_some());
    }
}
