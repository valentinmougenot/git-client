# Git Client

A single-repo GUI git client for Linux and macOS, focused on the daily commit loop: inspect changes, stage files, commit, push and pull.

## Language

### Repository state

**Repository**:
The single git repository currently open. Only one is open at a time; the app is launched from the repo's directory.
_Avoid_: project, workspace, repo session

**Working Tree**:
The files on disk that have been modified but not yet staged.
_Avoid_: working directory, unstaged area

**Staging Area**:
The set of changes queued for the next commit (the git index).
_Avoid_: index, cache, staged area

**Unstaged File**:
A tracked file with changes present in the Working Tree but not yet added to the Staging Area.
_Avoid_: dirty file, modified file

**Staged File**:
A file whose changes have been added to the Staging Area and will be included in the next Commit.
_Avoid_: indexed file, added file

**Untracked File**:
A file unknown to git — not tracked, not ignored.
_Avoid_: new file (ambiguous with a newly staged file)

**Diff**:
The line-level changes for a single file between its last committed/staged state and its current state.
_Avoid_: patch, delta, changes

**Commit**:
A snapshot of the Staging Area persisted to the repository history, identified by a SHA and a message.
_Avoid_: save, snapshot

### UI panels

**File List**:
The left column showing Unstaged Files (top half) and Staged Files (bottom half). Selecting an entry loads its Diff into the Diff View.
_Avoid_: sidebar, file panel, status panel

**Diff View**:
The right panel displaying the Diff of the currently selected file.
_Avoid_: diff panel, change view

**Commit Panel**:
The bottom-right area where the user writes a commit message and triggers a Commit.
_Avoid_: commit form, commit box

**Status Bar**:
The persistent strip at the very bottom of the window that shows the last error or warning. Stays visible until explicitly dismissed.
_Avoid_: footer, error bar, notification bar

**Notification**:
A transient success message that appears briefly (over the Status Bar) then disappears automatically.
_Avoid_: toast, alert, snackbar

### Internal architecture

**GitCommand**:
A message sent from the UI to the Git Worker, requesting a git operation (refresh status, load diff, stage, commit, push, pull).
_Avoid_: action, request, event (reserved for the other direction)

**GitEvent**:
A message sent from the Git Worker back to the UI, carrying the result of a completed GitCommand or an Error.
_Avoid_: response, callback, result

**Git Worker**:
The dedicated background thread that owns the git2 Repository handle and processes GitCommands sequentially.
_Avoid_: git thread, background thread, service

**RepoState**:
The sub-struct of `App` holding the current Working Tree and Staging Area contents, the selected file, and the loaded Diff.
_Avoid_: repository state, git state

**CommitState**:
The sub-struct of `App` holding the in-progress commit message and whether a commit operation is running.
_Avoid_: commit form state, commit data

### Operations

**Stage**:
The action of moving an Unstaged File (or Untracked File) into the Staging Area. In v1, applies to a whole file at once.
_Avoid_: add, index

**Unstage**:
The action of removing a Staged File from the Staging Area, returning it to Unstaged.
_Avoid_: reset, unadd

**Push**:
Sending local Commits on the current branch to the configured Remote via SSH.
_Avoid_: publish, upload, sync (sync implies pull too)

**Pull**:
Fetching and integrating Remote changes into the current branch (fetch + merge or rebase, delegated to git2).
_Avoid_: sync, update, download

**Remote**:
A configured upstream git server reachable via SSH. In v1, only SSH remotes are supported.
_Avoid_: origin (that's a specific remote name, not the concept), server, upstream
