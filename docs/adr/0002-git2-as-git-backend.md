# git2 (libgit2) as git backend

All git operations (status, diff, stage, commit, push, pull) go through the **git2** crate (libgit2 bindings) rather than shelling out to the `git` binary.

The subprocess approach is common in TUI clients (lazygit shells out for almost everything) because it requires no API knowledge and always tracks the latest git features. We rejected it here because: (1) parsing `git` text output in a reactive GUI creates fragile coupling between output format and UI state; (2) each operation forks a process, which introduces latency perceptible in an interactive GUI; (3) git2 gives us structured data (diff hunks, status entries, tree objects) that we can pass directly into iced's view model without a parsing layer.

The known trade-off is that git2 lags behind git in supporting very recent features. For the v1 core loop (status, stage, commit, push/pull over SSH) this is not a concern — all of these are stable libgit2 APIs.
