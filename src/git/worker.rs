//! The Git Worker: a dedicated thread that owns the [`git2::Repository`] and
//! processes [`GitCommand`]s sequentially, emitting [`GitEvent`]s.
//!
//! See ADR `0003` for why this is a single pinned thread rather than a pool.

use std::path::Path;
use std::sync::mpsc::Receiver;

use git2::{
    ApplyLocation, ApplyOptions, Cred, CredentialType, DiffFormat, DiffOptions, FetchOptions,
    ObjectType, PushOptions, RemoteCallbacks, Repository, Status, StatusOptions,
};

use super::types::*;

/// A channel sink for [`GitEvent`]s headed back to the UI.
///
/// Abstracting over the concrete sender keeps the operation logic testable
/// without an iced runtime: tests collect events into a `Vec`.
pub trait EventSink {
    fn emit(&self, event: GitEvent);
}

impl EventSink for futures::channel::mpsc::UnboundedSender<GitEvent> {
    fn emit(&self, event: GitEvent) {
        // The UI side outlives the worker in practice; a closed channel just
        // means the window is gone, so a failed send is safe to ignore.
        let _ = self.unbounded_send(event);
    }
}

/// Run the worker loop until the command channel closes.
///
/// The Repository is discovered from the current directory inside the worker
/// thread (it was already validated in `main`, so discovery here will not
/// fail in normal operation).
pub fn run(commands: Receiver<GitCommand>, events: impl EventSink) {
    let repo = match Repository::discover(".") {
        Ok(repo) => repo,
        Err(error) => {
            events.emit(GitEvent::Error(GitError::new("open repository", &error)));
            return;
        }
    };

    while let Ok(command) = commands.recv() {
        process(&repo, command, &events);
    }
}

/// Handle a single command. Public so `update()`-level tests can drive the
/// worker logic directly against a temporary repository.
pub fn process(repo: &Repository, command: GitCommand, events: &impl EventSink) {
    match command {
        GitCommand::RefreshStatus => emit_status(repo, events),
        GitCommand::LoadDiff { path, staged } => match load_diff(repo, &path, staged) {
            Ok(diff) => events.emit(GitEvent::DiffLoaded(diff)),
            Err(error) => events.emit(GitEvent::Error(GitError::new("load diff", &error))),
        },
        GitCommand::StageFile(path) => {
            if let Err(error) = stage(repo, &path) {
                events.emit(GitEvent::Error(GitError::new("stage file", &error)));
            }
            emit_status(repo, events);
        }
        GitCommand::StageAll => {
            if let Err(error) = stage_all(repo) {
                events.emit(GitEvent::Error(GitError::new("stage all", &error)));
            }
            emit_status(repo, events);
        }
        GitCommand::UnstageFile(path) => {
            if let Err(error) = unstage(repo, &path) {
                events.emit(GitEvent::Error(GitError::new("unstage file", &error)));
            }
            emit_status(repo, events);
        }
        GitCommand::UnstageAll => {
            if let Err(error) = unstage_all(repo) {
                events.emit(GitEvent::Error(GitError::new("unstage all", &error)));
            }
            emit_status(repo, events);
        }
        GitCommand::Discard(path) => {
            if let Err(error) = discard(repo, &path) {
                events.emit(GitEvent::Error(error));
            }
            emit_status(repo, events);
        }
        GitCommand::DiscardAll => {
            if let Err(error) = discard_all(repo) {
                events.emit(GitEvent::Error(error));
            }
            emit_status(repo, events);
        }
        GitCommand::StageHunk { path, hunk } => {
            if let Err(error) = stage_hunk(repo, &path, hunk) {
                events.emit(GitEvent::Error(GitError::new("stage hunk", &error)));
            }
            emit_status(repo, events);
            // Refresh the Working Tree diff the user is looking at.
            if let Ok(diff) = load_diff(repo, &path, false) {
                events.emit(GitEvent::DiffLoaded(diff));
            }
        }
        GitCommand::UnstageHunk { path, hunk } => {
            if let Err(error) = unstage_hunk(repo, &path, hunk) {
                events.emit(GitEvent::Error(GitError::new("unstage hunk", &error)));
            }
            emit_status(repo, events);
            if let Ok(diff) = load_diff(repo, &path, true) {
                events.emit(GitEvent::DiffLoaded(diff));
            }
        }
        GitCommand::Commit(message) => {
            match commit(repo, &message) {
                Ok(sha) => events.emit(GitEvent::Committed(sha)),
                Err(error) => events.emit(GitEvent::Error(GitError::new("commit", &error))),
            }
            emit_status(repo, events);
        }
        GitCommand::Amend(message) => {
            match amend(repo, &message) {
                Ok(sha) => events.emit(GitEvent::Committed(sha)),
                Err(error) => events.emit(GitEvent::Error(GitError::new("amend", &error))),
            }
            emit_status(repo, events);
        }
        GitCommand::LoadHistory => match load_history(repo, HISTORY_LIMIT) {
            Ok(commits) => events.emit(GitEvent::HistoryLoaded(commits)),
            Err(error) => events.emit(GitEvent::Error(GitError::new("load history", &error))),
        },
        GitCommand::LoadCommitDetail(sha) => match load_commit_detail(repo, &sha) {
            Ok(detail) => events.emit(GitEvent::CommitDetailLoaded(detail)),
            Err(error) => events.emit(GitEvent::Error(GitError::new("load commit", &error))),
        },
        GitCommand::LoadBranches => emit_branches(repo, events),
        GitCommand::Checkout(name) => {
            match checkout_branch(repo, &name) {
                Ok(()) => events.emit(GitEvent::CheckedOut(name)),
                Err(error) => events.emit(GitEvent::Error(error)),
            }
            emit_status(repo, events);
            emit_branches(repo, events);
        }
        GitCommand::CreateBranch(name) => {
            match create_branch(repo, &name) {
                Ok(()) => events.emit(GitEvent::CheckedOut(name)),
                Err(error) => events.emit(GitEvent::Error(error)),
            }
            emit_status(repo, events);
            emit_branches(repo, events);
        }
        GitCommand::DeleteBranch(name) => {
            match delete_branch(repo, &name) {
                Ok(()) => events.emit(GitEvent::BranchDeleted(name)),
                Err(error) => events.emit(GitEvent::Error(error)),
            }
            emit_branches(repo, events);
        }
        GitCommand::Push => {
            match push(repo) {
                Ok(()) => events.emit(GitEvent::Pushed),
                Err(error) => events.emit(GitEvent::Error(error)),
            }
            // A first push sets the upstream and resets ahead/behind, so refresh
            // the branch context and list.
            emit_status(repo, events);
            emit_branches(repo, events);
        }
        GitCommand::Pull => {
            match pull(repo) {
                Ok(()) => events.emit(GitEvent::Pulled),
                Err(error) => events.emit(GitEvent::Error(error)),
            }
            // A pull changes the branch tip and its ahead/behind, so refresh the
            // branch list too — not just the status.
            emit_status(repo, events);
            emit_branches(repo, events);
        }
        GitCommand::Fetch => {
            match fetch(repo) {
                Ok(()) => events.emit(GitEvent::Fetched),
                Err(error) => events.emit(GitEvent::Error(error)),
            }
            // Fetching moves the remote-tracking refs and the ahead/behind
            // counts, so refresh both the status context and the branch list.
            emit_status(repo, events);
            emit_branches(repo, events);
        }
    }
}

/// Read the Working Tree, Staging Area, and HEAD context, and emit one
/// `StatusLoaded` event carrying all three as a consistent snapshot.
fn emit_status(repo: &Repository, events: &impl EventSink) {
    match status(repo) {
        Ok((unstaged, staged)) => events.emit(GitEvent::StatusLoaded {
            unstaged,
            staged,
            head: head_info(repo),
        }),
        Err(error) => events.emit(GitEvent::Error(GitError::new("refresh status", &error))),
    }
}

/// Read HEAD and its relationship to the Remote, best-effort. Every piece is
/// optional: an empty repo, a detached HEAD, or a branch with no upstream each
/// just leave the corresponding fields empty rather than failing the refresh.
fn head_info(repo: &Repository) -> HeadInfo {
    let has_remote = repo.find_remote("origin").is_ok();

    let head = match repo.head() {
        Ok(head) => head,
        // No resolvable HEAD (typically an unborn branch with no commits yet):
        // still surface the branch name read from the symbolic ref.
        Err(_) => {
            return HeadInfo {
                branch: unborn_branch_name(repo),
                has_remote,
                ..HeadInfo::default()
            };
        }
    };

    let detached = repo.head_detached().unwrap_or(false);
    let branch = if detached {
        None
    } else {
        head.shorthand().ok().map(str::to_string)
    };

    let last_commit = head.peel_to_commit().ok().map(|commit| CommitSummary {
        short_sha: short_sha(commit.id()),
        summary: commit.summary().ok().flatten().unwrap_or_default().to_string(),
    });

    let (upstream, ahead, behind) = upstream_divergence(repo, branch.as_deref());

    HeadInfo {
        branch,
        detached,
        has_remote,
        upstream,
        ahead,
        behind,
        last_commit,
    }
}

/// The configured upstream of `branch` and how far the local branch is
/// ahead/behind it. Returns `(None, 0, 0)` when there is no branch or upstream.
fn upstream_divergence(repo: &Repository, branch: Option<&str>) -> (Option<String>, usize, usize) {
    let Some(name) = branch else {
        return (None, 0, 0);
    };
    let Ok(local) = repo.find_branch(name, git2::BranchType::Local) else {
        return (None, 0, 0);
    };
    let Ok(upstream) = local.upstream() else {
        return (None, 0, 0);
    };

    let upstream_name = upstream.name().ok().flatten().map(str::to_string);

    match (local.get().target(), upstream.get().target()) {
        (Some(local_oid), Some(upstream_oid)) => {
            let (ahead, behind) = repo
                .graph_ahead_behind(local_oid, upstream_oid)
                .unwrap_or((0, 0));
            (upstream_name, ahead, behind)
        }
        _ => (upstream_name, 0, 0),
    }
}

/// The branch name of an unborn HEAD, read from its symbolic target
/// (`refs/heads/<name>`), if resolvable.
fn unborn_branch_name(repo: &Repository) -> Option<String> {
    repo.find_reference("HEAD")
        .ok()?
        .symbolic_target()
        .ok()
        .flatten()
        .map(|target| target.trim_start_matches("refs/heads/").to_string())
}

/// Collect the Unstaged/Untracked files and the Staged files.
fn status(repo: &Repository) -> Result<(Vec<FileEntry>, Vec<FileEntry>), git2::Error> {
    let mut options = StatusOptions::new();
    options
        .include_untracked(true)
        .recurse_untracked_dirs(true)
        .include_ignored(false)
        .renames_head_to_index(true);

    let statuses = repo.statuses(Some(&mut options))?;
    let mut unstaged = Vec::new();
    let mut staged = Vec::new();

    for entry in statuses.iter() {
        let status = entry.status();
        let path = match entry.path() {
            Ok(path) => path.to_string(),
            // Non-UTF-8 paths are skipped rather than mangled.
            Err(_) => continue,
        };

        if let Some(change) = worktree_change(status) {
            unstaged.push(FileEntry {
                path: path.clone(),
                change,
            });
        }
        if let Some(change) = index_change(status) {
            staged.push(FileEntry { path, change });
        }
    }

    Ok((unstaged, staged))
}

/// Map the Working-Tree-side status bits to a [`ChangeKind`], if any.
fn worktree_change(status: Status) -> Option<ChangeKind> {
    if status.contains(Status::WT_NEW) {
        Some(ChangeKind::Untracked)
    } else if status.contains(Status::WT_MODIFIED) {
        Some(ChangeKind::Modified)
    } else if status.contains(Status::WT_DELETED) {
        Some(ChangeKind::Deleted)
    } else if status.contains(Status::WT_RENAMED) {
        Some(ChangeKind::Renamed)
    } else if status.contains(Status::WT_TYPECHANGE) {
        Some(ChangeKind::Typechange)
    } else {
        None
    }
}

/// Map the index-side (Staging Area) status bits to a [`ChangeKind`], if any.
fn index_change(status: Status) -> Option<ChangeKind> {
    if status.contains(Status::INDEX_NEW) {
        Some(ChangeKind::Added)
    } else if status.contains(Status::INDEX_MODIFIED) {
        Some(ChangeKind::Modified)
    } else if status.contains(Status::INDEX_DELETED) {
        Some(ChangeKind::Deleted)
    } else if status.contains(Status::INDEX_RENAMED) {
        Some(ChangeKind::Renamed)
    } else if status.contains(Status::INDEX_TYPECHANGE) {
        Some(ChangeKind::Typechange)
    } else {
        None
    }
}

/// Stage a whole file: add modifications/additions, or record a deletion.
fn stage(repo: &Repository, path: &str) -> Result<(), git2::Error> {
    let mut index = repo.index()?;
    let path = Path::new(path);
    let status = repo.status_file(path)?;

    if status.contains(Status::WT_DELETED) {
        index.remove_path(path)?;
    } else {
        index.add_path(path)?;
    }
    index.write()
}

/// Unstage a file, returning it to the Working Tree.
///
/// With a HEAD commit this resets the index entry to the HEAD version; in a
/// repository with no commits yet it simply removes the entry from the index.
fn unstage(repo: &Repository, path: &str) -> Result<(), git2::Error> {
    let path = Path::new(path);
    match repo.head() {
        Ok(head) => {
            let target = head.peel(ObjectType::Commit)?;
            repo.reset_default(Some(&target), [path])
        }
        Err(_) => {
            let mut index = repo.index()?;
            index.remove_path(path)?;
            index.write()
        }
    }
}

/// Stage a single hunk: build the Working Tree diff for the file and apply only
/// the hunk at `index` to the index. Untracked files have no index entry to
/// patch, so they fall back to staging the whole file.
fn stage_hunk(repo: &Repository, path: &str, index: usize) -> Result<(), git2::Error> {
    if repo.status_file(Path::new(path))?.contains(Status::WT_NEW) {
        return stage(repo, path);
    }

    let mut options = DiffOptions::new();
    options.pathspec(path);
    let diff = repo.diff_index_to_workdir(None, Some(&mut options))?;
    apply_hunk(repo, &diff, index)
}

/// Unstage a single hunk: build the Staging Area diff *reversed* (so applying it
/// undoes the staged change) and apply only the hunk at `index` to the index.
fn unstage_hunk(repo: &Repository, path: &str, index: usize) -> Result<(), git2::Error> {
    let head_tree = match repo.head() {
        Ok(head) => Some(head.peel_to_tree()?),
        Err(_) => None,
    };

    let mut options = DiffOptions::new();
    options.pathspec(path).reverse(true);
    let diff = repo.diff_tree_to_index(head_tree.as_ref(), None, Some(&mut options))?;
    apply_hunk(repo, &diff, index)
}

/// Apply exactly the hunk at `index` of `diff` to the index. The callback is
/// invoked once per hunk in order, matching the indexing the UI built from the
/// same diff.
fn apply_hunk(repo: &Repository, diff: &git2::Diff, index: usize) -> Result<(), git2::Error> {
    let mut seen = 0;
    let mut options = ApplyOptions::new();
    options.hunk_callback(|_hunk| {
        let keep = seen == index;
        seen += 1;
        keep
    });
    repo.apply(diff, ApplyLocation::Index, Some(&mut options))
}

/// Stage every Unstaged and Untracked File at once.
///
/// `add_all` picks up new and modified files (respecting `.gitignore`);
/// `update_all` records deletions of tracked files removed from the workdir.
fn stage_all(repo: &Repository) -> Result<(), git2::Error> {
    let mut index = repo.index()?;
    index.add_all(["*"], git2::IndexAddOption::DEFAULT, None)?;
    index.update_all(["*"], None)?;
    index.write()
}

/// Empty the Staging Area without touching the Working Tree (a mixed reset).
fn unstage_all(repo: &Repository) -> Result<(), git2::Error> {
    match repo.head() {
        Ok(head) => {
            let target = head.peel(ObjectType::Commit)?;
            repo.reset(&target, git2::ResetType::Mixed, None)
        }
        Err(_) => {
            // No HEAD yet: clearing the index unstages everything.
            let mut index = repo.index()?;
            index.clear()?;
            index.write()
        }
    }
}

/// Discard Working Tree changes for one file.
///
/// An Untracked File has nothing to revert to, so it is deleted from disk; a
/// tracked file is restored from the index (the equivalent of `git checkout`).
fn discard(repo: &Repository, path: &str) -> Result<(), GitError> {
    let status = repo
        .status_file(Path::new(path))
        .map_err(|error| GitError::new("discard", &error))?;

    if status.contains(Status::WT_NEW) {
        delete_from_workdir(repo, path)
    } else {
        let mut checkout = git2::build::CheckoutBuilder::new();
        checkout.path(path).force();
        repo.checkout_index(None, Some(&mut checkout))
            .map_err(|error| GitError::new("discard", &error))
    }
}

/// Discard all Working Tree changes: restore tracked files from the index and
/// delete every Untracked File.
fn discard_all(repo: &Repository) -> Result<(), GitError> {
    let mut checkout = git2::build::CheckoutBuilder::new();
    checkout.force();
    repo.checkout_index(None, Some(&mut checkout))
        .map_err(|error| GitError::new("discard all", &error))?;

    let (unstaged, _) = status(repo).map_err(|error| GitError::new("discard all", &error))?;
    for entry in unstaged {
        if entry.change == ChangeKind::Untracked {
            delete_from_workdir(repo, &entry.path)?;
        }
    }
    Ok(())
}

/// Remove a file from the Working Tree on disk.
fn delete_from_workdir(repo: &Repository, path: &str) -> Result<(), GitError> {
    let workdir = repo
        .workdir()
        .ok_or_else(|| GitError::custom("discard", "the repository has no working tree"))?;
    std::fs::remove_file(workdir.join(path))
        .map_err(|error| GitError::custom("discard", error.to_string()))
}

/// Create a Commit from the current Staging Area, returning its short SHA.
fn commit(repo: &Repository, message: &str) -> Result<String, git2::Error> {
    let mut index = repo.index()?;
    let tree_oid = index.write_tree()?;
    let tree = repo.find_tree(tree_oid)?;
    let signature = repo.signature()?;

    let parents = match repo.head() {
        Ok(head) => vec![head.peel_to_commit()?],
        Err(_) => Vec::new(),
    };
    let parent_refs: Vec<&git2::Commit> = parents.iter().collect();

    let oid = repo.commit(
        Some("HEAD"),
        &signature,
        &signature,
        message,
        &tree,
        &parent_refs,
    )?;

    Ok(short_sha(oid))
}

/// Replace the HEAD Commit with one carrying the current Staging Area as its
/// tree and a new message, keeping the original parents (`git commit --amend`).
fn amend(repo: &Repository, message: &str) -> Result<String, git2::Error> {
    let head = repo.head()?.peel_to_commit()?;
    let mut index = repo.index()?;
    let tree = repo.find_tree(index.write_tree()?)?;
    let signature = repo.signature()?;

    let oid = head.amend(
        Some("HEAD"),
        Some(&signature),
        Some(&signature),
        None,
        Some(message),
        Some(&tree),
    )?;
    Ok(short_sha(oid))
}

fn short_sha(oid: git2::Oid) -> String {
    let full = oid.to_string();
    full[..full.len().min(7)].to_string()
}

/// Build the rendered Diff for a single file.
fn load_diff(repo: &Repository, path: &str, staged: bool) -> Result<Diff, git2::Error> {
    let mut options = DiffOptions::new();
    options.pathspec(path);

    let diff = if staged {
        let head_tree = match repo.head() {
            Ok(head) => Some(head.peel_to_tree()?),
            Err(_) => None,
        };
        repo.diff_tree_to_index(head_tree.as_ref(), None, Some(&mut options))?
    } else {
        // Show new-file content so Untracked Files render their full body.
        // `recurse_untracked_dirs` is essential: without it, an untracked file
        // inside a directory (e.g. `docs/PRD.md`) is reported only as its parent
        // dir, so the per-file pathspec matches nothing and the Diff comes back
        // empty.
        options
            .include_untracked(true)
            .recurse_untracked_dirs(true)
            .show_untracked_content(true);
        repo.diff_index_to_workdir(None, Some(&mut options))?
    };

    let mut lines = Vec::new();
    diff.print(DiffFormat::Patch, |_delta, _hunk, line| {
        let kind = match line.origin() {
            '+' => DiffLineKind::Addition,
            '-' => DiffLineKind::Deletion,
            ' ' => DiffLineKind::Context,
            'H' => DiffLineKind::Header,
            // Skip file headers, binary markers, and end-of-file context.
            _ => return true,
        };
        let content = String::from_utf8_lossy(line.content())
            .trim_end_matches('\n')
            .to_string();
        lines.push(DiffLine {
            kind,
            old_lineno: line.old_lineno(),
            new_lineno: line.new_lineno(),
            content,
        });
        true
    })?;

    Ok(Diff {
        path: path.to_string(),
        staged,
        lines,
    })
}

/// How many recent Commits the History view loads.
const HISTORY_LIMIT: usize = 200;

/// Walk the Commit history from HEAD, newest first, up to `limit` entries.
/// Returns an empty list when the branch is unborn (no commits yet).
fn load_history(repo: &Repository, limit: usize) -> Result<Vec<CommitInfo>, git2::Error> {
    let mut walk = repo.revwalk()?;
    if walk.push_head().is_err() {
        return Ok(Vec::new());
    }
    // Topological ordering (with time as the tie-breaker) guarantees a Commit is
    // always listed before its parents, which the graph layout relies on.
    walk.set_sorting(git2::Sort::TIME | git2::Sort::TOPOLOGICAL)?;

    let mut commits = Vec::new();
    for oid in walk.take(limit) {
        let oid = oid?;
        let commit = repo.find_commit(oid)?;
        commits.push(CommitInfo {
            short_sha: short_sha(oid),
            sha: oid.to_string(),
            summary: commit.summary().ok().flatten().unwrap_or_default().to_string(),
            author: commit.author().name().unwrap_or_default().to_string(),
            time: commit.time().seconds(),
            parents: commit.parent_ids().map(|id| id.to_string()).collect(),
        });
    }
    Ok(commits)
}

/// Load one Commit's metadata and its full Diff against its first parent (or
/// against the empty tree for a root Commit). A header line is injected before
/// each changed file so the combined patch reads clearly in the Diff View.
fn load_commit_detail(repo: &Repository, sha: &str) -> Result<CommitDetail, git2::Error> {
    let oid = git2::Oid::from_str(sha)?;
    let commit = repo.find_commit(oid)?;
    let tree = commit.tree()?;
    let parent_tree = match commit.parent(0) {
        Ok(parent) => Some(parent.tree()?),
        Err(_) => None,
    };

    let diff = repo.diff_tree_to_tree(parent_tree.as_ref(), Some(&tree), None)?;

    let mut lines = Vec::new();
    let mut last_path: Option<String> = None;
    diff.print(DiffFormat::Patch, |delta, _hunk, line| {
        let kind = match line.origin() {
            '+' => DiffLineKind::Addition,
            '-' => DiffLineKind::Deletion,
            ' ' => DiffLineKind::Context,
            'H' => DiffLineKind::Header,
            // Skip libgit2's own file headers, binary markers, etc.
            _ => return true,
        };

        // Inject our own header line whenever the file changes.
        let path = delta
            .new_file()
            .path()
            .or_else(|| delta.old_file().path())
            .map(|path| path.to_string_lossy().to_string());
        if path != last_path {
            if let Some(path) = &path {
                lines.push(DiffLine {
                    kind: DiffLineKind::Header,
                    old_lineno: None,
                    new_lineno: None,
                    content: format!("● {path}"),
                });
            }
            last_path = path;
        }

        let content = String::from_utf8_lossy(line.content())
            .trim_end_matches('\n')
            .to_string();
        lines.push(DiffLine {
            kind,
            old_lineno: line.old_lineno(),
            new_lineno: line.new_lineno(),
            content,
        });
        true
    })?;

    let author = commit.author();
    Ok(CommitDetail {
        short_sha: short_sha(oid),
        sha: oid.to_string(),
        author: author.name().unwrap_or_default().to_string(),
        email: author.email().unwrap_or_default().to_string(),
        time: commit.time().seconds(),
        message: commit.message().unwrap_or_default().to_string(),
        lines,
    })
}

/// Push the current branch to `origin` over SSH. When the branch has no
/// upstream yet, this also configures one (`git push -u`), so subsequent pushes,
/// pulls, and the ahead/behind counts work without further setup.
fn push(repo: &Repository) -> Result<(), GitError> {
    let branch = current_branch(repo)?;
    let mut remote = repo
        .find_remote("origin")
        .map_err(|error| GitError::new("find remote 'origin'", &error))?;

    let mut callbacks = RemoteCallbacks::new();
    callbacks.credentials(ssh_credentials_callback());
    let mut options = PushOptions::new();
    options.remote_callbacks(callbacks);

    let refspec = format!("refs/heads/{branch}:refs/heads/{branch}");
    remote
        .push(&[refspec.as_str()], Some(&mut options))
        .map_err(|error| GitError::new("push", &error))?;

    ensure_upstream(repo, &branch)
}

/// Make `branch` track `origin/<branch>` if it doesn't already. The push above
/// has just landed the commit on the remote, so the remote-tracking ref is
/// synced to the local tip before tracking is configured (libgit2 needs that ref
/// to exist to set up the relationship).
fn ensure_upstream(repo: &Repository, branch: &str) -> Result<(), GitError> {
    let mut local = repo
        .find_branch(branch, git2::BranchType::Local)
        .map_err(|error| GitError::new("find branch", &error))?;
    if local.upstream().is_ok() {
        return Ok(());
    }

    let oid = local
        .get()
        .target()
        .ok_or_else(|| GitError::custom("push", "the branch has no commit to track"))?;
    let tracking = format!("refs/remotes/origin/{branch}");
    repo.reference(&tracking, oid, true, "push: update remote-tracking branch")
        .map_err(|error| GitError::new("update remote-tracking branch", &error))?;
    local
        .set_upstream(Some(&format!("origin/{branch}")))
        .map_err(|error| GitError::new("set upstream", &error))
}

/// Fetch from `origin` and fast-forward the current branch.
///
/// A non-fast-forward (true merge required) is reported as an error rather
/// than attempting an automatic merge — that is out of scope for v1.
fn pull(repo: &Repository) -> Result<(), GitError> {
    let branch = current_branch(repo)?;
    let mut remote = repo
        .find_remote("origin")
        .map_err(|error| GitError::new("find remote 'origin'", &error))?;

    let mut callbacks = RemoteCallbacks::new();
    callbacks.credentials(ssh_credentials_callback());
    let mut options = FetchOptions::new();
    options.remote_callbacks(callbacks);

    remote
        .fetch(&[branch.as_str()], Some(&mut options), None)
        .map_err(|error| GitError::new("fetch", &error))?;

    let fetch_head = repo
        .find_reference("FETCH_HEAD")
        .map_err(|error| GitError::new("read FETCH_HEAD", &error))?;
    let fetched = repo
        .reference_to_annotated_commit(&fetch_head)
        .map_err(|error| GitError::new("resolve FETCH_HEAD", &error))?;

    let (analysis, _) = repo
        .merge_analysis(&[&fetched])
        .map_err(|error| GitError::new("merge analysis", &error))?;

    if analysis.is_up_to_date() {
        return Ok(());
    }
    if !analysis.is_fast_forward() {
        return Err(GitError::custom(
            "pull",
            "remote changes require a merge; resolve manually",
        ));
    }

    let refname = format!("refs/heads/{branch}");
    let mut reference = repo
        .find_reference(&refname)
        .map_err(|error| GitError::new("find branch", &error))?;
    reference
        .set_target(fetched.id(), "pull: fast-forward")
        .map_err(|error| GitError::new("fast-forward", &error))?;
    repo.set_head(&refname)
        .map_err(|error| GitError::new("update HEAD", &error))?;
    repo.checkout_head(Some(git2::build::CheckoutBuilder::default().force()))
        .map_err(|error| GitError::new("checkout", &error))
}

/// Fetch from `origin`, updating every remote-tracking branch (and pruning ones
/// deleted upstream) without touching the Working Tree or merging. Uses the
/// remote's configured refspecs (an empty list), so all `origin/*` refs refresh.
fn fetch(repo: &Repository) -> Result<(), GitError> {
    let mut remote = repo
        .find_remote("origin")
        .map_err(|error| GitError::new("find remote 'origin'", &error))?;

    let mut callbacks = RemoteCallbacks::new();
    callbacks.credentials(ssh_credentials_callback());
    let mut options = FetchOptions::new();
    options.remote_callbacks(callbacks);
    options.prune(git2::FetchPrune::On);

    let refspecs: &[&str] = &[];
    remote
        .fetch(refspecs, Some(&mut options), None)
        .map_err(|error| GitError::new("fetch", &error))
}

/// Read the local branches and emit them, or surface the failure.
fn emit_branches(repo: &Repository, events: &impl EventSink) {
    match load_branches(repo) {
        Ok(branches) => events.emit(GitEvent::BranchesLoaded(branches)),
        Err(error) => events.emit(GitEvent::Error(GitError::new("load branches", &error))),
    }
}

/// List the branches: local ones first (current branch first, then by name),
/// then remote-tracking ones (by name). The `origin/HEAD` alias is skipped.
fn load_branches(repo: &Repository) -> Result<Vec<BranchInfo>, git2::Error> {
    let mut locals = Vec::new();
    for entry in repo.branches(Some(git2::BranchType::Local))? {
        let (branch, _) = entry?;
        let Some(name) = branch.name()?.map(str::to_string) else {
            continue;
        };
        let is_head = branch.is_head();
        let (upstream, ahead, behind) = upstream_divergence(repo, Some(&name));
        locals.push(BranchInfo {
            name,
            is_remote: false,
            is_head,
            upstream,
            ahead,
            behind,
        });
    }
    locals.sort_by(|a, b| b.is_head.cmp(&a.is_head).then_with(|| a.name.cmp(&b.name)));

    let mut remotes = Vec::new();
    for entry in repo.branches(Some(git2::BranchType::Remote))? {
        let (branch, _) = entry?;
        let Some(name) = branch.name()?.map(str::to_string) else {
            continue;
        };
        // `origin/HEAD` is a symbolic alias, not a branch to check out.
        if name.ends_with("/HEAD") {
            continue;
        }
        remotes.push(BranchInfo {
            name,
            is_remote: true,
            is_head: false,
            upstream: None,
            ahead: 0,
            behind: 0,
        });
    }
    remotes.sort_by(|a, b| a.name.cmp(&b.name));

    locals.extend(remotes);
    Ok(locals)
}

/// Switch to a branch by name. A local branch is checked out directly; a remote
/// branch name (`origin/feature`) checks out the matching local branch if one
/// exists, otherwise creates a local tracking branch from it first.
fn checkout_branch(repo: &Repository, name: &str) -> Result<(), GitError> {
    if repo.find_branch(name, git2::BranchType::Local).is_ok() {
        return checkout_local(repo, name);
    }
    if let Ok(remote) = repo.find_branch(name, git2::BranchType::Remote) {
        return checkout_remote(repo, &remote, name);
    }
    // Fall back to a direct checkout; surfaces a clear error if unresolvable.
    checkout_local(repo, name)
}

/// Check out an existing local branch. The checkout is safe: libgit2 refuses it
/// (and we surface the error) if it would overwrite uncommitted local changes.
fn checkout_local(repo: &Repository, name: &str) -> Result<(), GitError> {
    let refname = format!("refs/heads/{name}");
    let object = repo
        .revparse_single(&refname)
        .map_err(|error| GitError::new("find branch", &error))?;

    repo.checkout_tree(&object, None)
        .map_err(|error| GitError::new("checkout", &error))?;
    repo.set_head(&refname)
        .map_err(|error| GitError::new("update HEAD", &error))?;
    Ok(())
}

/// Create a local branch tracking a remote one (`origin/feature` -> `feature`),
/// then check it out. If the local short name is already taken, switch to it.
fn checkout_remote(
    repo: &Repository,
    remote: &git2::Branch,
    remote_name: &str,
) -> Result<(), GitError> {
    // The local name drops the remote prefix: `origin/feature` -> `feature`.
    let local_name = remote_name
        .split_once('/')
        .map(|(_, rest)| rest)
        .unwrap_or(remote_name);

    if repo.find_branch(local_name, git2::BranchType::Local).is_ok() {
        return checkout_local(repo, local_name);
    }

    let commit = remote
        .get()
        .peel_to_commit()
        .map_err(|error| GitError::new("resolve remote branch", &error))?;
    let mut local = repo
        .branch(local_name, &commit, false)
        .map_err(|error| GitError::new("create tracking branch", &error))?;
    // Best-effort: configure tracking. It only fails when the remote isn't in
    // config, which shouldn't happen for a fetched branch; either way, the
    // switch below should still proceed.
    let _ = local.set_upstream(Some(remote_name));

    checkout_local(repo, local_name)
}

/// Create a new local branch at HEAD and switch to it. Fails if the branch
/// already exists or there is no commit yet to branch from.
fn create_branch(repo: &Repository, name: &str) -> Result<(), GitError> {
    let head = repo
        .head()
        .and_then(|head| head.peel_to_commit())
        .map_err(|error| GitError::new("resolve HEAD", &error))?;
    repo.branch(name, &head, false)
        .map_err(|error| GitError::new("create branch", &error))?;
    checkout_branch(repo, name)
}

/// Delete a local branch. Refuses to delete the branch currently checked out.
fn delete_branch(repo: &Repository, name: &str) -> Result<(), GitError> {
    let mut branch = repo
        .find_branch(name, git2::BranchType::Local)
        .map_err(|error| GitError::new("find branch", &error))?;
    if branch.is_head() {
        return Err(GitError::custom(
            "delete branch",
            "cannot delete the current branch",
        ));
    }
    branch
        .delete()
        .map_err(|error| GitError::new("delete branch", &error))
}

/// The short name of the currently checked-out branch.
fn current_branch(repo: &Repository) -> Result<String, GitError> {
    let head = repo
        .head()
        .map_err(|error| GitError::new("resolve HEAD", &error))?;
    head.shorthand()
        .map(str::to_string)
        .map_err(|error| GitError::new("current branch", &error))
}

/// A credentials callback for an SSH remote, hardened against libgit2's retry
/// loop. libgit2 re-invokes the callback every time a credential is rejected; if
/// we kept handing back the same key it would spin forever (a hang in the UI).
/// So this answers a username request, offers the key exactly once, and then
/// returns an error — turning a rejected key into a surfaced failure.
fn ssh_credentials_callback()
-> impl FnMut(&str, Option<&str>, CredentialType) -> Result<Cred, git2::Error> {
    let mut key_attempts = 0;
    move |url, username, allowed| {
        // Some URLs make libgit2 ask for the username on its own first.
        if allowed.contains(CredentialType::USERNAME) {
            return Cred::username(username.unwrap_or("git"));
        }
        key_attempts += 1;
        if key_attempts > 1 {
            return Err(git2::Error::from_str(
                "SSH authentication failed: the agent and ~/.ssh default keys were rejected",
            ));
        }
        ssh_credentials(url, username, allowed)
    }
}

/// Provide SSH credentials: the agent first, then the default key files.
fn ssh_credentials(
    _url: &str,
    username: Option<&str>,
    allowed: CredentialType,
) -> Result<Cred, git2::Error> {
    let user = username.unwrap_or("git");

    if allowed.contains(CredentialType::SSH_KEY) {
        if let Ok(cred) = Cred::ssh_key_from_agent(user) {
            return Ok(cred);
        }
        if let Ok(home) = std::env::var("HOME") {
            for name in ["id_ed25519", "id_rsa"] {
                let key = Path::new(&home).join(".ssh").join(name);
                if key.exists() {
                    return Cred::ssh_key(user, None, &key, None);
                }
            }
        }
    }

    Err(git2::Error::from_str(
        "no SSH credentials available (tried agent and ~/.ssh default keys)",
    ))
}

#[cfg(test)]
mod tests {
    //! Seam 1 (PRD "Testing Decisions"): drive `process` against real
    //! temporary repositories and assert on the emitted events.

    use std::cell::RefCell;
    use std::fs;
    use std::path::Path;

    use git2::Repository;
    use tempfile::TempDir;

    use super::*;

    /// An [`EventSink`] that records every event for later assertions.
    #[derive(Default)]
    struct Collector(RefCell<Vec<GitEvent>>);

    impl EventSink for Collector {
        fn emit(&self, event: GitEvent) {
            self.0.borrow_mut().push(event);
        }
    }

    impl Collector {
        /// The most recent `StatusLoaded` payload.
        fn last_status(&self) -> (Vec<FileEntry>, Vec<FileEntry>) {
            self.0
                .borrow()
                .iter()
                .rev()
                .find_map(|event| match event {
                    GitEvent::StatusLoaded {
                        unstaged, staged, ..
                    } => Some((unstaged.clone(), staged.clone())),
                    _ => None,
                })
                .expect("expected a StatusLoaded event")
        }

        fn events(&self) -> Vec<GitEvent> {
            self.0.borrow().clone()
        }

        /// The HEAD context from the most recent `StatusLoaded`.
        fn last_head(&self) -> HeadInfo {
            self.0
                .borrow()
                .iter()
                .rev()
                .find_map(|event| match event {
                    GitEvent::StatusLoaded { head, .. } => Some(head.clone()),
                    _ => None,
                })
                .expect("expected a StatusLoaded event")
        }

        /// The branch list from the most recent `BranchesLoaded`.
        fn last_branches(&self) -> Vec<BranchInfo> {
            self.0
                .borrow()
                .iter()
                .rev()
                .find_map(|event| match event {
                    GitEvent::BranchesLoaded(branches) => Some(branches.clone()),
                    _ => None,
                })
                .expect("expected a BranchesLoaded event")
        }
    }

    /// Commit a single file, so tests have a HEAD to branch from.
    fn commit_file(dir: &Path, repo: &Repository, events: &Collector, name: &str, contents: &str) {
        write(dir, name, contents);
        process(repo, GitCommand::StageFile(name.into()), events);
        process(repo, GitCommand::Commit(format!("add {name}")), events);
    }

    /// A fresh repository in a temp dir, with a commit identity configured.
    fn temp_repo() -> (TempDir, Repository) {
        let dir = tempfile::tempdir().unwrap();
        let repo = Repository::init(dir.path()).unwrap();
        let mut config = repo.config().unwrap();
        config.set_str("user.name", "Tester").unwrap();
        config.set_str("user.email", "tester@example.com").unwrap();
        (dir, repo)
    }

    fn write(dir: &Path, name: &str, contents: &str) {
        fs::write(dir.join(name), contents).unwrap();
    }

    fn paths(entries: &[FileEntry]) -> Vec<&str> {
        entries.iter().map(|entry| entry.path.as_str()).collect()
    }

    #[test]
    fn refresh_status_on_clean_repo_is_empty() {
        let (_dir, repo) = temp_repo();
        let events = Collector::default();

        process(&repo, GitCommand::RefreshStatus, &events);

        let (unstaged, staged) = events.last_status();
        assert!(unstaged.is_empty());
        assert!(staged.is_empty());
    }

    #[test]
    fn head_info_reports_branch_and_last_commit() {
        let (dir, repo) = temp_repo();

        // Before any commit: the branch is unborn but named, no last commit,
        // no remote, no divergence.
        let events = Collector::default();
        process(&repo, GitCommand::RefreshStatus, &events);
        let head = events.last_head();
        assert!(head.last_commit.is_none());
        assert!(!head.has_remote);
        assert_eq!((head.ahead, head.behind), (0, 0));
        assert!(head.upstream.is_none());

        // After a commit: the last commit summary is surfaced.
        write(dir.path(), "a.txt", "x\n");
        process(&repo, GitCommand::StageFile("a.txt".into()), &events);
        process(&repo, GitCommand::Commit("hello world".into()), &events);
        process(&repo, GitCommand::RefreshStatus, &events);

        let head = events.last_head();
        assert!(head.branch.is_some(), "expected a named branch");
        assert!(!head.detached);
        let commit = head.last_commit.expect("expected a last commit");
        assert_eq!(commit.summary, "hello world");
    }

    #[test]
    fn untracked_file_appears_unstaged() {
        let (dir, repo) = temp_repo();
        write(dir.path(), "notes.txt", "hello\n");
        let events = Collector::default();

        process(&repo, GitCommand::RefreshStatus, &events);

        let (unstaged, staged) = events.last_status();
        assert_eq!(paths(&unstaged), ["notes.txt"]);
        assert_eq!(unstaged[0].change, ChangeKind::Untracked);
        assert!(staged.is_empty());
    }

    #[test]
    fn staging_moves_a_file_into_the_staging_area() {
        let (dir, repo) = temp_repo();
        write(dir.path(), "notes.txt", "hello\n");
        let events = Collector::default();

        process(&repo, GitCommand::StageFile("notes.txt".into()), &events);

        let (unstaged, staged) = events.last_status();
        assert!(unstaged.is_empty());
        assert_eq!(paths(&staged), ["notes.txt"]);
        assert_eq!(staged[0].change, ChangeKind::Added);
    }

    #[test]
    fn unstaging_returns_a_file_to_the_working_tree() {
        let (dir, repo) = temp_repo();
        write(dir.path(), "notes.txt", "hello\n");
        let events = Collector::default();

        process(&repo, GitCommand::StageFile("notes.txt".into()), &events);
        process(&repo, GitCommand::UnstageFile("notes.txt".into()), &events);

        let (unstaged, staged) = events.last_status();
        assert_eq!(paths(&unstaged), ["notes.txt"]);
        assert!(staged.is_empty());
    }

    #[test]
    fn commit_persists_the_staging_area_and_clears_it() {
        let (dir, repo) = temp_repo();
        write(dir.path(), "notes.txt", "hello\n");
        let events = Collector::default();

        process(&repo, GitCommand::StageFile("notes.txt".into()), &events);
        process(&repo, GitCommand::Commit("first commit".into()), &events);

        let committed = events
            .events()
            .iter()
            .any(|event| matches!(event, GitEvent::Committed(_)));
        assert!(committed, "expected a Committed event");

        let (unstaged, staged) = events.last_status();
        assert!(unstaged.is_empty());
        assert!(staged.is_empty());
        // The commit is really in history.
        assert!(repo.head().unwrap().peel_to_commit().is_ok());
    }

    #[test]
    fn amend_replaces_the_last_commit() {
        let (dir, repo) = temp_repo();
        write(dir.path(), "a.txt", "one\n");
        let events = Collector::default();
        process(&repo, GitCommand::StageFile("a.txt".into()), &events);
        process(&repo, GitCommand::Commit("original".into()), &events);
        let original = repo.head().unwrap().peel_to_commit().unwrap().id();

        // Stage a further change and amend with a new message.
        write(dir.path(), "a.txt", "one\ntwo\n");
        process(&repo, GitCommand::StageFile("a.txt".into()), &events);
        process(&repo, GitCommand::Amend("amended".into()), &events);

        let head = repo.head().unwrap().peel_to_commit().unwrap();
        assert_eq!(head.summary().ok().flatten(), Some("amended"));
        // HEAD was replaced, not added to, and the new tree includes the change.
        assert_ne!(head.id(), original);
        assert_eq!(head.parent_count(), 0);

        // History still holds exactly one commit.
        let history = load_history(&repo, 10).unwrap();
        assert_eq!(history.len(), 1);
        assert_eq!(history[0].summary, "amended");
    }

    /// A file whose two edits (near the top and near the bottom) land in two
    /// separate hunks. Returns the path after committing the base and writing
    /// the modified version.
    fn two_hunk_file(dir: &Path, repo: &Repository, events: &Collector) {
        let base: String = (1..=20).map(|n| format!("line {n}\n")).collect();
        write(dir, "a.txt", &base);
        process(repo, GitCommand::StageFile("a.txt".into()), events);
        process(repo, GitCommand::Commit("base".into()), events);

        let mut lines: Vec<String> = (1..=20).map(|n| format!("line {n}")).collect();
        lines[1] = "line 2 CHANGED".to_string();
        lines[18] = "line 19 CHANGED".to_string();
        let modified: String = lines.iter().map(|l| format!("{l}\n")).collect();
        write(dir, "a.txt", &modified);
    }

    fn added_lines(diff: &Diff) -> Vec<String> {
        diff.lines
            .iter()
            .filter(|l| l.kind == DiffLineKind::Addition)
            .map(|l| l.content.clone())
            .collect()
    }

    #[test]
    fn stage_hunk_stages_only_the_targeted_hunk() {
        let (dir, repo) = temp_repo();
        let events = Collector::default();
        two_hunk_file(dir.path(), &repo, &events);

        process(
            &repo,
            GitCommand::StageHunk {
                path: "a.txt".into(),
                hunk: 0,
            },
            &events,
        );

        // The file is now partially staged: it appears on both sides.
        let (unstaged, staged) = events.last_status();
        assert_eq!(paths(&staged), ["a.txt"]);
        assert_eq!(paths(&unstaged), ["a.txt"]);

        // The first change is staged; the second is not.
        let staged_added = added_lines(&load_diff(&repo, "a.txt", true).unwrap());
        assert!(staged_added.iter().any(|l| l.contains("line 2 CHANGED")));
        assert!(!staged_added.iter().any(|l| l.contains("line 19 CHANGED")));

        let unstaged_added = added_lines(&load_diff(&repo, "a.txt", false).unwrap());
        assert!(unstaged_added.iter().any(|l| l.contains("line 19 CHANGED")));
    }

    #[test]
    fn unstage_hunk_unstages_only_the_targeted_hunk() {
        let (dir, repo) = temp_repo();
        let events = Collector::default();
        two_hunk_file(dir.path(), &repo, &events);
        process(&repo, GitCommand::StageAll, &events);

        // Unstage just the first hunk.
        process(
            &repo,
            GitCommand::UnstageHunk {
                path: "a.txt".into(),
                hunk: 0,
            },
            &events,
        );

        // The first change returns to the Working Tree; the second stays staged.
        let staged_added = added_lines(&load_diff(&repo, "a.txt", true).unwrap());
        assert!(!staged_added.iter().any(|l| l.contains("line 2 CHANGED")));
        assert!(staged_added.iter().any(|l| l.contains("line 19 CHANGED")));

        let unstaged_added = added_lines(&load_diff(&repo, "a.txt", false).unwrap());
        assert!(unstaged_added.iter().any(|l| l.contains("line 2 CHANGED")));
    }

    #[test]
    fn load_diff_reports_added_lines() {
        let (dir, repo) = temp_repo();
        write(dir.path(), "a.txt", "line1\n");
        let events = Collector::default();
        process(&repo, GitCommand::StageFile("a.txt".into()), &events);
        process(&repo, GitCommand::Commit("base".into()), &events);

        // Modify the committed file, then read the Working Tree diff.
        write(dir.path(), "a.txt", "line1\nline2\n");
        process(
            &repo,
            GitCommand::LoadDiff {
                path: "a.txt".into(),
                staged: false,
            },
            &events,
        );

        let diff = events
            .events()
            .into_iter()
            .rev()
            .find_map(|event| match event {
                GitEvent::DiffLoaded(diff) => Some(diff),
                _ => None,
            })
            .expect("expected a DiffLoaded event");

        assert_eq!(diff.path, "a.txt");
        assert!(!diff.staged);
        assert!(
            diff.lines
                .iter()
                .any(|line| line.kind == DiffLineKind::Addition && line.content == "line2"),
            "expected an added 'line2' line, got {:?}",
            diff.lines
        );
    }

    #[test]
    fn stage_all_stages_every_file() {
        let (dir, repo) = temp_repo();
        write(dir.path(), "a.txt", "a\n");
        write(dir.path(), "b.txt", "b\n");
        let events = Collector::default();

        process(&repo, GitCommand::StageAll, &events);

        let (unstaged, staged) = events.last_status();
        assert!(unstaged.is_empty());
        assert_eq!(paths(&staged), ["a.txt", "b.txt"]);
    }

    #[test]
    fn unstage_all_empties_the_staging_area() {
        let (dir, repo) = temp_repo();
        write(dir.path(), "a.txt", "a\n");
        write(dir.path(), "b.txt", "b\n");
        let events = Collector::default();
        process(&repo, GitCommand::StageAll, &events);

        process(&repo, GitCommand::UnstageAll, &events);

        let (unstaged, staged) = events.last_status();
        assert!(staged.is_empty());
        assert_eq!(paths(&unstaged), ["a.txt", "b.txt"]);
    }

    #[test]
    fn discard_deletes_an_untracked_file() {
        let (dir, repo) = temp_repo();
        write(dir.path(), "junk.txt", "junk\n");
        let events = Collector::default();

        process(&repo, GitCommand::Discard("junk.txt".into()), &events);

        assert!(!dir.path().join("junk.txt").exists());
        let (unstaged, _) = events.last_status();
        assert!(unstaged.is_empty());
    }

    #[test]
    fn discard_reverts_a_modified_tracked_file() {
        let (dir, repo) = temp_repo();
        write(dir.path(), "a.txt", "original\n");
        let events = Collector::default();
        process(&repo, GitCommand::StageFile("a.txt".into()), &events);
        process(&repo, GitCommand::Commit("base".into()), &events);

        write(dir.path(), "a.txt", "tampered\n");
        process(&repo, GitCommand::Discard("a.txt".into()), &events);

        let restored = fs::read_to_string(dir.path().join("a.txt")).unwrap();
        assert_eq!(restored, "original\n");
        let (unstaged, _) = events.last_status();
        assert!(unstaged.is_empty());
    }

    #[test]
    fn discard_all_reverts_tracked_and_deletes_untracked() {
        let (dir, repo) = temp_repo();
        write(dir.path(), "tracked.txt", "original\n");
        let events = Collector::default();
        process(&repo, GitCommand::StageFile("tracked.txt".into()), &events);
        process(&repo, GitCommand::Commit("base".into()), &events);

        write(dir.path(), "tracked.txt", "tampered\n");
        write(dir.path(), "untracked.txt", "new\n");
        process(&repo, GitCommand::DiscardAll, &events);

        assert_eq!(
            fs::read_to_string(dir.path().join("tracked.txt")).unwrap(),
            "original\n"
        );
        assert!(!dir.path().join("untracked.txt").exists());
        let (unstaged, staged) = events.last_status();
        assert!(unstaged.is_empty());
        assert!(staged.is_empty());
    }

    #[test]
    fn load_diff_shows_content_of_an_untracked_file_in_a_subdirectory() {
        // Regression: untracked files inside a directory must still produce a
        // Diff (they require `recurse_untracked_dirs` on the diff options).
        let (dir, repo) = temp_repo();
        fs::create_dir(dir.path().join("docs")).unwrap();
        write(dir.path(), "docs/notes.txt", "alpha\nbeta\n");
        let events = Collector::default();

        process(
            &repo,
            GitCommand::LoadDiff {
                path: "docs/notes.txt".into(),
                staged: false,
            },
            &events,
        );

        let diff = events
            .events()
            .into_iter()
            .rev()
            .find_map(|event| match event {
                GitEvent::DiffLoaded(diff) => Some(diff),
                _ => None,
            })
            .expect("expected a DiffLoaded event");

        assert_eq!(diff.path, "docs/notes.txt");
        let added: Vec<&str> = diff
            .lines
            .iter()
            .filter(|line| line.kind == DiffLineKind::Addition)
            .map(|line| line.content.as_str())
            .collect();
        assert_eq!(added, ["alpha", "beta"]);
    }

    #[test]
    fn staging_a_deleted_file_records_the_deletion() {
        let (dir, repo) = temp_repo();
        write(dir.path(), "gone.txt", "bye\n");
        let events = Collector::default();
        process(&repo, GitCommand::StageFile("gone.txt".into()), &events);
        process(&repo, GitCommand::Commit("base".into()), &events);

        fs::remove_file(dir.path().join("gone.txt")).unwrap();
        process(&repo, GitCommand::StageFile("gone.txt".into()), &events);

        let (_unstaged, staged) = events.last_status();
        assert_eq!(paths(&staged), ["gone.txt"]);
        assert_eq!(staged[0].change, ChangeKind::Deleted);
    }

    #[test]
    fn load_branches_lists_locals_with_the_current_first() {
        let (dir, repo) = temp_repo();
        let events = Collector::default();
        commit_file(dir.path(), &repo, &events, "a.txt", "x\n");
        let current = events.last_head().branch.expect("a branch");

        process(&repo, GitCommand::CreateBranch("feature".into()), &events);
        // CreateBranch switches to feature; switch back so `current` is head.
        process(&repo, GitCommand::Checkout(current.clone()), &events);
        process(&repo, GitCommand::LoadBranches, &events);

        let branches = events.last_branches();
        let names: Vec<&str> = branches.iter().map(|b| b.name.as_str()).collect();
        assert!(names.contains(&"feature"));
        assert!(names.contains(&current.as_str()));
        // The current branch sorts first and is the only one marked head.
        assert!(branches[0].is_head);
        assert_eq!(branches.iter().filter(|b| b.is_head).count(), 1);
    }

    #[test]
    fn create_branch_switches_to_the_new_branch() {
        let (dir, repo) = temp_repo();
        let events = Collector::default();
        commit_file(dir.path(), &repo, &events, "a.txt", "x\n");

        process(&repo, GitCommand::CreateBranch("feature".into()), &events);

        assert_eq!(events.last_head().branch.as_deref(), Some("feature"));
        assert!(
            events
                .events()
                .iter()
                .any(|e| matches!(e, GitEvent::CheckedOut(name) if name == "feature"))
        );
    }

    #[test]
    fn checkout_switches_between_existing_branches() {
        let (dir, repo) = temp_repo();
        let events = Collector::default();
        commit_file(dir.path(), &repo, &events, "a.txt", "x\n");
        let original = events.last_head().branch.expect("a branch");

        process(&repo, GitCommand::CreateBranch("feature".into()), &events);
        assert_eq!(events.last_head().branch.as_deref(), Some("feature"));

        process(&repo, GitCommand::Checkout(original.clone()), &events);
        assert_eq!(events.last_head().branch, Some(original));
    }

    #[test]
    fn delete_branch_removes_a_non_current_branch() {
        let (dir, repo) = temp_repo();
        let events = Collector::default();
        commit_file(dir.path(), &repo, &events, "a.txt", "x\n");
        let original = events.last_head().branch.expect("a branch");

        process(&repo, GitCommand::CreateBranch("feature".into()), &events);
        process(&repo, GitCommand::Checkout(original), &events);
        process(&repo, GitCommand::DeleteBranch("feature".into()), &events);

        let names: Vec<String> = events.last_branches().into_iter().map(|b| b.name).collect();
        assert!(!names.contains(&"feature".to_string()));
        assert!(
            events
                .events()
                .iter()
                .any(|e| matches!(e, GitEvent::BranchDeleted(name) if name == "feature"))
        );
    }

    #[test]
    fn load_branches_lists_remote_branches() {
        let (dir, repo) = temp_repo();
        let events = Collector::default();
        commit_file(dir.path(), &repo, &events, "a.txt", "x\n");
        let head_oid = repo.head().unwrap().target().unwrap();
        repo.reference("refs/remotes/origin/feature", head_oid, false, "test")
            .unwrap();

        process(&repo, GitCommand::LoadBranches, &events);

        let branches = events.last_branches();
        let remote = branches
            .iter()
            .find(|b| b.name == "origin/feature")
            .expect("expected the remote branch");
        assert!(remote.is_remote);
        assert!(!remote.is_head);
    }

    #[test]
    fn checking_out_a_remote_branch_creates_a_local_tracking_branch() {
        let (dir, repo) = temp_repo();
        let events = Collector::default();
        commit_file(dir.path(), &repo, &events, "a.txt", "x\n");
        // A configured remote plus a fetched branch ref — as after a real fetch.
        repo.remote("origin", "https://example.com/repo.git").unwrap();
        let head_oid = repo.head().unwrap().target().unwrap();
        repo.reference("refs/remotes/origin/feature", head_oid, false, "test")
            .unwrap();

        process(&repo, GitCommand::Checkout("origin/feature".into()), &events);

        // A local `feature` now exists, is checked out, and tracks the remote.
        assert_eq!(events.last_head().branch.as_deref(), Some("feature"));
        let feature = events
            .last_branches()
            .into_iter()
            .find(|b| b.name == "feature" && !b.is_remote)
            .expect("expected a local feature branch");
        assert_eq!(feature.upstream.as_deref(), Some("origin/feature"));
    }

    #[test]
    fn fetch_updates_remote_tracking_branches() {
        // A bare repo standing in for `origin`.
        let remote = tempfile::tempdir().unwrap();
        Repository::init_bare(remote.path()).unwrap();

        let (dir, repo) = temp_repo();
        let events = Collector::default();
        commit_file(dir.path(), &repo, &events, "a.txt", "x\n");
        let head = repo.head().unwrap().peel_to_commit().unwrap();
        repo.branch("feature", &head, false).unwrap();

        // Publish both heads to the remote (local path, so no credentials).
        let current = repo.head().unwrap().shorthand().unwrap().to_string();
        let mut origin = repo.remote("origin", remote.path().to_str().unwrap()).unwrap();
        origin
            .push(
                &[
                    format!("refs/heads/{current}:refs/heads/{current}"),
                    "refs/heads/feature:refs/heads/feature".to_string(),
                ],
                None,
            )
            .unwrap();

        // Drop any tracking refs the push created locally, so the fetch is what
        // restores them.
        let tracking: Vec<String> = repo
            .references_glob("refs/remotes/origin/*")
            .unwrap()
            .names()
            .filter_map(Result::ok)
            .map(str::to_string)
            .collect();
        for name in tracking {
            repo.find_reference(&name).unwrap().delete().unwrap();
        }
        process(&repo, GitCommand::LoadBranches, &events);
        assert!(events.last_branches().iter().all(|b| !b.is_remote));

        process(&repo, GitCommand::Fetch, &events);

        let branches = events.last_branches();
        assert!(branches.iter().any(|b| b.is_remote && b.name == "origin/feature"));
        assert!(events.events().iter().any(|e| matches!(e, GitEvent::Fetched)));
    }

    #[test]
    fn push_sets_upstream_for_a_branch_without_one() {
        let remote = tempfile::tempdir().unwrap();
        Repository::init_bare(remote.path()).unwrap();

        let (dir, repo) = temp_repo();
        let events = Collector::default();
        commit_file(dir.path(), &repo, &events, "a.txt", "x\n");
        repo.remote("origin", remote.path().to_str().unwrap()).unwrap();

        let current = repo.head().unwrap().shorthand().unwrap().to_string();
        // No upstream before the push.
        assert!(
            repo.find_branch(&current, git2::BranchType::Local)
                .unwrap()
                .upstream()
                .is_err()
        );

        process(&repo, GitCommand::Push, &events);

        // The push succeeded and the branch now tracks origin/<branch>.
        assert!(events.events().iter().any(|e| matches!(e, GitEvent::Pushed)));
        let upstream = repo
            .find_branch(&current, git2::BranchType::Local)
            .unwrap()
            .upstream()
            .expect("expected an upstream after push");
        assert_eq!(upstream.name().unwrap(), Some(format!("origin/{current}").as_str()));
        // The branch context now reports the upstream too.
        assert_eq!(events.last_head().upstream.as_deref(), Some(format!("origin/{current}").as_str()));
    }

    #[test]
    fn deleting_the_current_branch_is_refused() {
        let (dir, repo) = temp_repo();
        let events = Collector::default();
        commit_file(dir.path(), &repo, &events, "a.txt", "x\n");
        let current = events.last_head().branch.expect("a branch");

        process(&repo, GitCommand::DeleteBranch(current.clone()), &events);

        // The branch survives and an error was surfaced.
        let names: Vec<String> = events.last_branches().into_iter().map(|b| b.name).collect();
        assert!(names.contains(&current));
        assert!(
            events
                .events()
                .iter()
                .any(|e| matches!(e, GitEvent::Error(_)))
        );
    }
}
