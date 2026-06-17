# PRD — Git Client v1

## Problem Statement

Developers who live in the terminal lack a lightweight, fast GUI git client that covers the daily commit loop without the bloat of tools like GitKraken or Sourcetree. Existing TUI clients (lazygit) are keyboard-only and opaque to new team members. Existing GUI clients are either Electron-based (slow, memory-hungry) or platform-specific. There is no Rust-native, cross-platform GUI git client that is simple, robust, and performant for everyday use.

## Solution

A native GUI git client for Linux and macOS, built with Rust and iced, that makes the daily commit loop (inspect changes → stage → commit → push/pull) fast and pleasant. The app opens a single Repository, launched from the terminal inside the project directory. It shows a three-panel layout: the File List on the left, the Diff View on the right, and the Commit Panel at the bottom. All operations are available via keyboard shortcuts with mouse as a secondary input method.

## User Stories

1. As a developer, I want to launch the client from my terminal with a single command, so that I can get to my changes immediately without leaving my workflow.
2. As a developer, I want the app to automatically detect the git Repository from my current directory, so that I don't have to specify a path.
3. As a developer, I want to see a clear error message in the terminal if I'm not inside a git Repository, so that I understand immediately what went wrong.
4. As a developer, I want to see all Unstaged Files in the top half of the File List, so that I can see what has changed at a glance.
5. As a developer, I want to see all Staged Files in the bottom half of the File List, so that I know exactly what will be included in my next Commit.
6. As a developer, I want to see Untracked Files listed alongside Unstaged Files, so that I can decide whether to stage or ignore them.
7. As a developer, I want to click on a file in the File List to load its Diff in the Diff View, so that I can review what changed before staging.
8. As a developer, I want to navigate between files in the File List using keyboard arrows, so that I can browse changes without reaching for the mouse.
9. As a developer, I want the Diff View to show added lines in green and removed lines in red, so that I can read changes at a glance.
10. As a developer, I want to Stage a file with a keyboard shortcut while it is selected in the File List, so that I can stage quickly without using the mouse.
11. As a developer, I want to Stage a file by clicking a button next to it in the File List, so that I can stage with the mouse when that's more convenient.
12. As a developer, I want to Unstage a Staged File with a keyboard shortcut, so that I can correct a staging mistake instantly.
13. As a developer, I want the File List to refresh automatically after I Stage or Unstage a file, so that the view always reflects the real state of the Staging Area.
14. As a developer, I want to type a commit message in the Commit Panel, so that I can describe my changes.
15. As a developer, I want to trigger a Commit with a keyboard shortcut from the Commit Panel, so that I can commit without clicking.
16. As a developer, I want the Staging Area and File List to clear after a successful Commit, so that I immediately see the clean state of the Repository.
17. As a developer, I want to see a Notification confirming that my Commit succeeded, so that I have confidence the operation completed.
18. As a developer, I want to Push my local Commits to the Remote with a keyboard shortcut, so that I can share my work quickly.
19. As a developer, I want to Pull Remote changes into my current branch with a keyboard shortcut, so that I can stay up to date with teammates.
20. As a developer, I want Push and Pull to use my existing SSH keys automatically, so that I don't have to manage credentials inside the app.
21. As a developer, I want the UI to remain responsive while a Push or Pull is in progress, so that I'm not blocked from reading the diff while waiting.
22. As a developer, I want to see a progress indicator in the Status Bar while a Push or Pull is running, so that I know the operation is ongoing.
23. As a developer, I want SSH errors (wrong key, host unreachable) to appear in the Status Bar and stay visible until I dismiss them, so that I don't miss them.
24. As a developer, I want git errors (push rejected, merge conflict on pull) to appear in the Status Bar with a clear message, so that I understand what went wrong and can act.
25. As a developer, I want to dismiss an error in the Status Bar with a keyboard shortcut, so that I can clear it without the mouse.
26. As a developer, I want success Notifications (committed, pushed, pulled) to disappear automatically after a few seconds, so that they don't clutter the UI.

## Implementation Decisions

### Module structure

The codebase is split into two top-level modules that never import each other directly:

- `git/` — owns the Git Worker thread and all git2 interactions (status, diff, stage, unstage, commit, push, pull)
- `ui/` — owns all iced widgets (File List, Diff View, Commit Panel, Status Bar, Notification)
- `app.rs` — the iced application root; holds `App` and its sub-structs, implements `update()` and `view()`, wires `git/` to `ui/` via messages

### App state

`App` is composed of four sub-structs, each mapping to a distinct UI area:

- `RepoState` — current Unstaged Files, Staged Files, selected file path, and loaded Diff
- `CommitState` — in-progress commit message and whether a commit is running
- `StatusBar` — current error or warning string, present until dismissed
- `Notification` — optional transient success message with an expiry timestamp

`App` also holds the `Sender<GitCommand>` used to dispatch operations to the Git Worker.

### Message model

Messages are grouped by domain to keep `update()` arms readable:

- `Message::Ui(UiMessage)` — pure UI interactions (file selected, commit message changed, status dismissed, …)
- `Message::Git(GitMessage)` — user-initiated git operations (stage, unstage, commit, push, pull, …)
- `Message::GitEvent(GitEvent)` — results arriving from the Git Worker (status loaded, diff loaded, committed, pushed, pulled, error)

### Git Worker

A single `std::thread` is spawned at startup. It owns the `git2::Repository` handle for the lifetime of the app. It receives `GitCommand` values from the UI via `mpsc::Receiver` and sends `GitEvent` values back via `mpsc::Sender`. The worker processes commands sequentially — one at a time. After every write operation (stage, unstage, commit, pull), it automatically re-runs status and sends `GitEvent::StatusLoaded` without waiting for the UI to ask.

The `Subscription` in `app.rs` wraps the `GitEvent` receiver into an iced event stream. See ADR `0003` for the rationale behind a dedicated thread over `tokio::spawn_blocking`.

### GitEvent shape

Events are typed by operation, with a single `Error` variant for all failures:

```
GitEvent::StatusLoaded { unstaged, staged }
GitEvent::DiffLoaded(Diff)
GitEvent::Committed(sha)
GitEvent::Pushed
GitEvent::Pulled
GitEvent::Error(GitError)
```

### Repository discovery

On startup, `git2::Repository::discover(".")` is called before the iced window opens. If it fails, the process prints an error to stderr and exits with code 1. No GUI window is shown.

### Remote authentication

SSH only. The Git Worker uses `git2::RemoteCallbacks` with the agent or default key at `~/.ssh/id_rsa` / `~/.ssh/id_ed25519`. No HTTPS credential handling is implemented in v1.

### iced version

iced 0.14.0. See ADR `0001`.

## Testing Decisions

Good tests for this project verify observable behavior (what the Git Worker produces given a command) not implementation details (how git2 is called internally). Tests should use real, temporary git repositories — no mocking of git2.

### Seam 1 — Git Worker (primary)

Spin up a temporary git repo with `tempfile` + `git2::Repository::init`, send `GitCommand` values through the channel, assert on the `GitEvent` values that come back. This is the highest-value seam: it exercises the full git logic path without any UI dependency.

Operations to cover: `RefreshStatus` (clean repo, modified files, staged files), `StageFile`, `UnstageFile`, `Commit`, `LoadDiff`.

### Seam 2 — `update()` state transitions

Test `update()` as a pure(-ish) function: given an `App` state and a `Message`, assert on the resulting state fields. Useful for coordination logic (e.g. `Message::GitEvent(GitEvent::StatusLoaded)` correctly populates `repo_state.unstaged` and clears `repo_state.diff` when the selected file is no longer present).

No iced rendering is invoked in these tests.

## Out of Scope

- **Hunk-level and line-level staging** — v1 stages whole files only
- **Commit history / log view** — no graph, no browse-by-commit
- **HTTPS authentication** — SSH only
- **Multiple repositories** — one repo per app instance
- **Syntax highlighting in the Diff View** — plain green/red coloring only
- **Interactive rebase, cherry-pick, stash, tag management** — not part of the core loop
- **Windows support** — Linux and macOS only
- **Submodule handling**
- **`.gitignore` editing**

## Further Notes

- Domain vocabulary is defined in `CONTEXT.md` at the repo root. Use it consistently in code (type names, function names, comments).
- Architectural decisions are documented in `docs/adr/`. Read ADRs `0001` (iced), `0002` (git2), and `0003` (threading) before implementing.
- The Git Worker's sequential processing means Push and Pull can block the worker for several seconds on slow connections. The UI must always show a "in progress" indicator in the Status Bar when a remote operation is running, so the user is never left wondering if the app has hung.
