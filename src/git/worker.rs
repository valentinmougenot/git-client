//! The Git Worker: a dedicated thread that owns the [`git2::Repository`] and
//! processes [`GitCommand`]s sequentially, emitting [`GitEvent`]s.
//!
//! See ADR `0003` for why this is a single pinned thread rather than a pool.

use std::path::Path;
use std::sync::mpsc::Receiver;

use git2::{
    ApplyLocation, ApplyOptions, Cred, CredentialType, DiffFormat, DiffOptions, FetchOptions,
    ObjectType, PushOptions, RemoteCallbacks, Repository, StashFlags, Status, StatusOptions,
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
        GitCommand::LoadBlame(path) => match load_blame(repo, &path) {
            Ok(file) => events.emit(GitEvent::BlameLoaded(file)),
            Err(error) => events.emit(GitEvent::Error(error)),
        },
        GitCommand::Reset { sha, kind } => {
            match reset_to(repo, &sha, kind) {
                Ok(short) => events.emit(GitEvent::ResetDone(short)),
                Err(error) => events.emit(GitEvent::Error(error)),
            }
            // A reset moves HEAD and may change the index/Working Tree, so
            // refresh the status and branch list.
            emit_status(repo, events);
            emit_branches(repo, events);
        }
        GitCommand::Revert(sha) => {
            match revert(repo, &sha) {
                Ok(outcome) => events.emit(GitEvent::Reverted { outcome }),
                Err(error) => events.emit(GitEvent::Error(error)),
            }
            emit_status(repo, events);
        }
        GitCommand::CherryPick(sha) => {
            match cherry_pick(repo, &sha) {
                Ok(outcome) => events.emit(GitEvent::CherryPicked { outcome }),
                Err(error) => events.emit(GitEvent::Error(error)),
            }
            emit_status(repo, events);
        }
        GitCommand::LoadBranches => emit_branches(repo, events),
        GitCommand::LoadTags => emit_tags(repo, events),
        GitCommand::CreateTag { name, message } => {
            match create_tag(repo, &name, message.as_deref()) {
                Ok(()) => events.emit(GitEvent::TagCreated(name)),
                Err(error) => events.emit(GitEvent::Error(error)),
            }
            emit_tags(repo, events);
        }
        GitCommand::DeleteTag(name) => {
            match delete_tag(repo, &name) {
                Ok(()) => events.emit(GitEvent::TagDeleted(name)),
                Err(error) => events.emit(GitEvent::Error(error)),
            }
            emit_tags(repo, events);
        }
        GitCommand::PushTag(name) => {
            match push_tag(repo, &name) {
                Ok(()) => events.emit(GitEvent::Pushed),
                Err(error) => events.emit(GitEvent::Error(error)),
            }
        }
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
        GitCommand::PruneBranches => {
            match prune_branches(repo) {
                Ok(pruned) => events.emit(GitEvent::BranchesPruned(pruned)),
                Err(error) => {
                    events.emit(GitEvent::Error(GitError::new("prune branches", &error)))
                }
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
        GitCommand::LoadStashes => emit_stashes(repo, events),
        GitCommand::StashPush { message, paths } => {
            match stash_push(repo, message.as_deref(), &paths) {
                Ok(()) => events.emit(GitEvent::Stashed),
                Err(error) => events.emit(GitEvent::Error(error)),
            }
            // Stashing clears the Working Tree, so refresh the file lists too.
            emit_status(repo, events);
            emit_stashes(repo, events);
        }
        GitCommand::LoadStashDiff(index) => match load_stash_diff(repo, index) {
            Ok(diff) => events.emit(GitEvent::StashDiffLoaded(diff)),
            Err(error) => events.emit(GitEvent::Error(error)),
        },
        GitCommand::Merge(branch) => {
            match merge_branch(repo, &branch) {
                Ok(outcome) => events.emit(GitEvent::Merged { branch, outcome }),
                Err(error) => events.emit(GitEvent::Error(error)),
            }
            // A merge moves HEAD and the Working Tree (and may leave conflicts),
            // so refresh the status, branch list, and any open file lists.
            emit_status(repo, events);
            emit_branches(repo, events);
        }
        GitCommand::ResolveConflict { path, side } => {
            if let Err(error) = resolve_conflict(repo, &path, side) {
                events.emit(GitEvent::Error(error));
            }
            emit_status(repo, events);
        }
        GitCommand::LoadConflict(path) => match load_conflict(repo, &path) {
            Ok(file) => events.emit(GitEvent::ConflictLoaded(file)),
            Err(error) => events.emit(GitEvent::Error(error)),
        },
        GitCommand::ResolveHunk { path, index, side } => {
            match resolve_hunk(repo, &path, index, side) {
                Ok(resolved) => {
                    // The file's conflict status (and the list) may have changed.
                    emit_status(repo, events);
                    // If regions remain, refresh the parsed view; if it is fully
                    // resolved it has left the conflicted list, so nothing to show.
                    if !resolved
                        && let Ok(file) = load_conflict(repo, &path)
                    {
                        events.emit(GitEvent::ConflictLoaded(file));
                    }
                }
                Err(error) => events.emit(GitEvent::Error(error)),
            }
        }
        GitCommand::SaveConflict { path, content } => {
            match save_conflict(repo, &path, &content) {
                Ok(resolved) => {
                    // The file's conflict status (and the list) may have changed.
                    emit_status(repo, events);
                    // Still conflicted: refresh the parsed regions and the editor
                    // seed; fully resolved, it has left the list, so nothing to show.
                    if !resolved
                        && let Ok(file) = load_conflict(repo, &path)
                    {
                        events.emit(GitEvent::ConflictLoaded(file));
                    }
                }
                Err(error) => events.emit(GitEvent::Error(error)),
            }
        }
        GitCommand::AbortMerge => {
            if let Err(error) = abort_merge(repo) {
                events.emit(GitEvent::Error(error));
            }
            emit_status(repo, events);
            emit_branches(repo, events);
        }
        GitCommand::StashApply(index) => {
            match stash_apply(repo, index) {
                Ok(()) => events.emit(GitEvent::StashApplied),
                Err(error) => events.emit(GitEvent::Error(error)),
            }
            // Applying restores changes to the Working Tree (the stash remains).
            emit_status(repo, events);
        }
        GitCommand::StashPop(index) => {
            match stash_pop(repo, index) {
                Ok(()) => events.emit(GitEvent::StashApplied),
                Err(error) => events.emit(GitEvent::Error(error)),
            }
            // A pop both restores the changes and removes the stash.
            emit_status(repo, events);
            emit_stashes(repo, events);
        }
        GitCommand::StashDrop(index) => {
            match stash_drop(repo, index) {
                Ok(()) => events.emit(GitEvent::StashDropped),
                Err(error) => events.emit(GitEvent::Error(error)),
            }
            emit_stashes(repo, events);
        }
    }
}

/// Read the Working Tree, Staging Area, and HEAD context, and emit one
/// `StatusLoaded` event carrying all three as a consistent snapshot.
fn emit_status(repo: &Repository, events: &impl EventSink) {
    match status(repo) {
        Ok((unstaged, staged, conflicted)) => events.emit(GitEvent::StatusLoaded {
            unstaged,
            staged,
            conflicted,
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

/// Collect the Unstaged/Untracked files, the Staged files, and any files left
/// in conflict by an in-progress merge.
type StatusLists = (Vec<FileEntry>, Vec<FileEntry>, Vec<FileEntry>);
fn status(repo: &Repository) -> Result<StatusLists, git2::Error> {
    let mut options = StatusOptions::new();
    options
        .include_untracked(true)
        .recurse_untracked_dirs(true)
        .include_ignored(false)
        .renames_head_to_index(true);

    let statuses = repo.statuses(Some(&mut options))?;
    let mut unstaged = Vec::new();
    let mut staged = Vec::new();
    let mut conflicted = Vec::new();

    for entry in statuses.iter() {
        let status = entry.status();
        let path = match entry.path() {
            Ok(path) => path.to_string(),
            // Non-UTF-8 paths are skipped rather than mangled.
            Err(_) => continue,
        };

        // A conflicted file is its own category — it must be resolved before it
        // can be staged or committed — so it skips the usual classification.
        if status.contains(Status::CONFLICTED) {
            conflicted.push(FileEntry {
                path,
                change: ChangeKind::Conflicted,
            });
            continue;
        }

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

    Ok((unstaged, staged, conflicted))
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

    let (unstaged, _, _) = status(repo).map_err(|error| GitError::new("discard all", &error))?;
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

/// Create a Commit from the current Staging Area, returning its short SHA. When
/// a merge is in progress (`MERGE_HEAD` set, e.g. after resolving conflicts),
/// the merged commit is added as a second parent and the merge state is cleared.
fn commit(repo: &Repository, message: &str) -> Result<String, git2::Error> {
    let mut index = repo.index()?;
    let tree_oid = index.write_tree()?;
    let tree = repo.find_tree(tree_oid)?;
    let signature = repo.signature()?;

    let mut parents = match repo.head() {
        Ok(head) => vec![head.peel_to_commit()?],
        Err(_) => Vec::new(),
    };
    // Finish an in-progress merge by recording its MERGE_HEAD as a parent. A
    // revert in progress has no extra parent (it is a single-parent commit on
    // HEAD), but does leave state to clear below.
    let merge_head = repo
        .revparse_single("MERGE_HEAD")
        .ok()
        .and_then(|object| object.peel_to_commit().ok());
    if let Some(commit) = merge_head {
        parents.push(commit);
    }
    let parent_refs: Vec<&git2::Commit> = parents.iter().collect();

    let oid = repo.commit(
        Some("HEAD"),
        &signature,
        &signature,
        message,
        &tree,
        &parent_refs,
    )?;

    // Clear any in-progress merge/revert/cherry-pick state this commit finished.
    if repo.state() != git2::RepositoryState::Clean {
        repo.cleanup_state()?;
    }

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

/// Render a git2 Diff into our `DiffLine`s, injecting a `● <path>` header line
/// before each changed file so a multi-file patch reads clearly. Shared by the
/// Commit detail and the Stash detail.
fn diff_to_lines(diff: &git2::Diff) -> Result<Vec<DiffLine>, git2::Error> {
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
    Ok(lines)
}

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
    let lines = diff_to_lines(&diff)?;

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

/// Move the current branch to the Commit `sha` with the given reset mode,
/// returning the short SHA it now points at.
fn reset_to(repo: &Repository, sha: &str, kind: ResetKind) -> Result<String, GitError> {
    let oid = git2::Oid::from_str(sha).map_err(|error| GitError::new("reset", &error))?;
    let object = repo
        .find_object(oid, None)
        .map_err(|error| GitError::new("reset", &error))?;
    let reset_type = match kind {
        ResetKind::Soft => git2::ResetType::Soft,
        ResetKind::Mixed => git2::ResetType::Mixed,
        ResetKind::Hard => git2::ResetType::Hard,
    };
    repo.reset(&object, reset_type, None)
        .map_err(|error| GitError::new("reset", &error))?;
    Ok(short_sha(oid))
}

/// Revert the Commit `sha`: apply its inverse on top of HEAD. A clean revert is
/// committed straight away (`git revert --no-edit`); a conflicting one is left
/// in place — with REVERT_HEAD set — for the user to resolve and commit (see
/// [`commit`], which clears the revert state).
fn revert(repo: &Repository, sha: &str) -> Result<RevertOutcome, GitError> {
    let make = |context: &str, error: &git2::Error| GitError::new(context, error);
    let oid = git2::Oid::from_str(sha).map_err(|e| make("revert", &e))?;
    let commit = repo.find_commit(oid).map_err(|e| make("revert", &e))?;

    repo.revert(&commit, None).map_err(|e| make("revert", &e))?;

    let mut index = repo.index().map_err(|e| make("revert", &e))?;
    if index.has_conflicts() {
        let conflicts = index
            .conflicts()
            .map(|iter| iter.count())
            .unwrap_or(0)
            .max(1);
        return Ok(RevertOutcome::Conflicts(conflicts));
    }

    // Clean revert: record it as a single-parent commit on HEAD and clear state.
    let tree = repo
        .find_tree(index.write_tree().map_err(|e| make("revert", &e))?)
        .map_err(|e| make("revert", &e))?;
    let signature = repo.signature().map_err(|e| make("revert", &e))?;
    let head = repo
        .head()
        .and_then(|h| h.peel_to_commit())
        .map_err(|e| make("revert", &e))?;
    let summary = commit.summary().ok().flatten().unwrap_or_default();
    let message = format!("Revert \"{summary}\"\n\nThis reverts commit {oid}.");
    repo.commit(Some("HEAD"), &signature, &signature, &message, &tree, &[&head])
        .map_err(|e| make("revert", &e))?;
    repo.cleanup_state().map_err(|e| make("revert", &e))?;

    Ok(RevertOutcome::Created)
}

/// Cherry-pick the Commit `sha`: apply its changes on top of HEAD. A clean pick
/// is committed straight away (`git cherry-pick --no-edit`), keeping the original
/// message and author; a conflicting one is left in place — with
/// CHERRY_PICK_HEAD set — for the user to resolve and commit (see [`commit`],
/// which clears the cherry-pick state).
fn cherry_pick(repo: &Repository, sha: &str) -> Result<CherryPickOutcome, GitError> {
    let make = |context: &str, error: &git2::Error| GitError::new(context, error);
    let oid = git2::Oid::from_str(sha).map_err(|e| make("cherry-pick", &e))?;
    let commit = repo.find_commit(oid).map_err(|e| make("cherry-pick", &e))?;

    repo.cherrypick(&commit, None)
        .map_err(|e| make("cherry-pick", &e))?;

    let mut index = repo.index().map_err(|e| make("cherry-pick", &e))?;
    if index.has_conflicts() {
        let conflicts = index
            .conflicts()
            .map(|iter| iter.count())
            .unwrap_or(0)
            .max(1);
        return Ok(CherryPickOutcome::Conflicts(conflicts));
    }

    // Clean pick: record a single-parent commit on HEAD, keeping the original
    // commit's message and author (with us as the committer), then clear state.
    let tree = repo
        .find_tree(index.write_tree().map_err(|e| make("cherry-pick", &e))?)
        .map_err(|e| make("cherry-pick", &e))?;
    let committer = repo.signature().map_err(|e| make("cherry-pick", &e))?;
    let head = repo
        .head()
        .and_then(|h| h.peel_to_commit())
        .map_err(|e| make("cherry-pick", &e))?;
    let message = commit.message().unwrap_or_default();
    repo.commit(
        Some("HEAD"),
        &commit.author(),
        &committer,
        message,
        &tree,
        &[&head],
    )
    .map_err(|e| make("cherry-pick", &e))?;
    repo.cleanup_state().map_err(|e| make("cherry-pick", &e))?;

    Ok(CherryPickOutcome::Created)
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

/// Read the tags and emit them, or surface the failure.
fn emit_tags(repo: &Repository, events: &impl EventSink) {
    match load_tags(repo) {
        Ok(tags) => events.emit(GitEvent::TagsLoaded(tags)),
        Err(error) => events.emit(GitEvent::Error(GitError::new("load tags", &error))),
    }
}

/// List the tags, each resolved to the Commit it ultimately points at. An
/// annotated tag (a tag object) carries its message and tagger; a lightweight
/// tag is just a ref to a Commit. Sorted by name.
fn load_tags(repo: &Repository) -> Result<Vec<TagInfo>, git2::Error> {
    let mut tags = Vec::new();
    for name in repo.tag_names(None)?.iter().flatten().flatten() {
        let object = repo.revparse_single(&format!("refs/tags/{name}"))?;
        let (message, is_annotated, commit) = match object.kind() {
            Some(ObjectType::Tag) => {
                let tag = object.into_tag().expect("kind() reported a tag");
                let message = tag
                    .message()
                    .ok()
                    .flatten()
                    .map(|m| m.trim().to_string())
                    .filter(|m| !m.is_empty());
                (message, true, tag.target()?.peel_to_commit()?)
            }
            _ => (None, false, object.peel_to_commit()?),
        };
        tags.push(TagInfo {
            name: name.to_string(),
            target: short_sha(commit.id()),
            summary: commit.summary().ok().flatten().unwrap_or_default().to_string(),
            message,
            is_annotated,
        });
    }
    Ok(tags)
}

/// Create a tag at HEAD. A non-empty `message` makes it annotated (a tag object
/// with the committer as tagger); otherwise it is a lightweight ref.
fn create_tag(repo: &Repository, name: &str, message: Option<&str>) -> Result<(), GitError> {
    let target = repo
        .head()
        .and_then(|head| head.peel(ObjectType::Commit))
        .map_err(|error| GitError::new("resolve HEAD", &error))?;

    match message {
        Some(message) if !message.trim().is_empty() => {
            let signature = repo
                .signature()
                .map_err(|error| GitError::new("create tag", &error))?;
            repo.tag(name, &target, &signature, message.trim(), false)
                .map_err(|error| GitError::new("create tag", &error))?;
        }
        _ => {
            repo.tag_lightweight(name, &target, false)
                .map_err(|error| GitError::new("create tag", &error))?;
        }
    }
    Ok(())
}

/// Delete the named tag.
fn delete_tag(repo: &Repository, name: &str) -> Result<(), GitError> {
    repo.tag_delete(name)
        .map_err(|error| GitError::new("delete tag", &error))
}

/// Push the named tag to `origin` over SSH.
fn push_tag(repo: &Repository, name: &str) -> Result<(), GitError> {
    let mut remote = repo
        .find_remote("origin")
        .map_err(|error| GitError::new("find remote 'origin'", &error))?;

    let mut callbacks = RemoteCallbacks::new();
    callbacks.credentials(ssh_credentials_callback());
    let mut options = PushOptions::new();
    options.remote_callbacks(callbacks);

    let refspec = format!("refs/tags/{name}:refs/tags/{name}");
    remote
        .push(&[refspec.as_str()], Some(&mut options))
        .map_err(|error| GitError::new("push tag", &error))
}

/// Load the stashes and emit them, or surface the failure.
fn emit_stashes(repo: &Repository, events: &impl EventSink) {
    match load_stashes(repo) {
        Ok(stashes) => events.emit(GitEvent::StashesLoaded(stashes)),
        Err(error) => events.emit(GitEvent::Error(error)),
    }
}

// git2's stash operations all need a `&mut Repository`, but the worker holds a
// shared `&Repository`. Opening a fresh handle to the same on-disk repo for the
// duration of one stash call is safe here: the worker is single-threaded and
// processes commands sequentially, so the two handles are never used at once.
fn open_mut(repo: &Repository) -> Result<Repository, GitError> {
    Repository::open(repo.path()).map_err(|error| GitError::new("open repository", &error))
}

/// List the saved stashes, newest (`stash@{0}`) first.
fn load_stashes(repo: &Repository) -> Result<Vec<StashInfo>, GitError> {
    let mut repo = open_mut(repo)?;
    let mut stashes = Vec::new();
    repo.stash_foreach(|index, message, _oid| {
        stashes.push(StashInfo {
            index,
            message: message.to_string(),
        });
        true
    })
    .map_err(|error| GitError::new("load stashes", &error))?;
    Ok(stashes)
}

/// Save the Working Tree and Staging Area as a new stash (including untracked
/// files). When `paths` is empty, stash everything and honour `message`.
///
/// When `paths` are given, stash only those. libgit2's path-limited stash does
/// not work through git2 0.21, so this is done by hand: snapshot the *other*
/// changed files, revert them to HEAD so only the chosen changes remain, stash
/// that, then restore the snapshots. (A selective stash carries no message, and
/// the kept files come back unstaged.)
fn stash_push(repo: &Repository, message: Option<&str>, paths: &[String]) -> Result<(), GitError> {
    let mut repo = open_mut(repo)?;
    let make = |error: &git2::Error| GitError::new("stash", error);

    if paths.is_empty() {
        let signature = repo.signature().map_err(|e| make(&e))?;
        repo.stash_save2(&signature, message, Some(StashFlags::INCLUDE_UNTRACKED))
            .map_err(|e| make(&e))?;
        return Ok(());
    }

    let workdir = repo
        .workdir()
        .ok_or_else(|| GitError::custom("stash", "no working directory"))?
        .to_path_buf();
    let selected: std::collections::HashSet<&str> = paths.iter().map(String::as_str).collect();

    // The other changed files (path, is_untracked) — everything we must keep out
    // of the stash and restore afterwards.
    let (unstaged, staged, _) = status(&repo).map_err(|e| make(&e))?;
    let mut kept: Vec<(String, bool)> = Vec::new();
    let mut seen = std::collections::HashSet::new();
    for entry in unstaged.iter().chain(staged.iter()) {
        if selected.contains(entry.path.as_str()) || !seen.insert(entry.path.clone()) {
            continue;
        }
        let untracked = unstaged
            .iter()
            .any(|e| e.path == entry.path && e.change == ChangeKind::Untracked);
        kept.push((entry.path.clone(), untracked));
    }

    // Snapshot the kept files' working-tree content (None = absent on disk).
    let snapshots: Vec<(String, Option<Vec<u8>>)> = kept
        .iter()
        .map(|(path, _)| (path.clone(), std::fs::read(workdir.join(path)).ok()))
        .collect();

    // Revert the kept files so the Working Tree holds only the selected changes:
    // delete untracked ones, check the rest out from HEAD (index and workdir).
    for (path, untracked) in &kept {
        if *untracked {
            let _ = std::fs::remove_file(workdir.join(path));
        } else {
            let mut checkout = git2::build::CheckoutBuilder::new();
            checkout.force().path(path.as_str());
            repo.checkout_head(Some(&mut checkout)).map_err(|e| make(&e))?;
        }
    }

    // Stash the remaining (selected) changes.
    let result = repo
        .signature()
        .and_then(|sig| repo.stash_save2(&sig, None, Some(StashFlags::INCLUDE_UNTRACKED)));

    // Restore the kept files' content whatever happened, so a failed stash never
    // loses the changes we set aside.
    for (path, content) in snapshots {
        let full = workdir.join(&path);
        match content {
            Some(bytes) => {
                let _ = std::fs::write(full, bytes);
            }
            None => {
                let _ = std::fs::remove_file(full);
            }
        }
    }

    result.map(|_| ()).map_err(|e| make(&e))
}

/// The Diff of the stash at `index`: its changes against its base commit (the
/// stash's first parent), the same view as `git stash show -p`.
fn load_stash_diff(repo: &Repository, index: usize) -> Result<StashDiff, GitError> {
    // Resolve the index to the stash commit's oid via the stash list.
    let mut oid = None;
    {
        let mut repo = open_mut(repo)?;
        repo.stash_foreach(|i, _message, id| {
            if i == index {
                oid = Some(*id);
                false
            } else {
                true
            }
        })
        .map_err(|error| GitError::new("load stash", &error))?;
    }
    let oid = oid.ok_or_else(|| GitError::custom("load stash", "no such stash"))?;

    let make = |error: &git2::Error| GitError::new("load stash", error);
    let commit = repo.find_commit(oid).map_err(|e| make(&e))?;
    let tree = commit.tree().map_err(|e| make(&e))?;
    let base_tree = match commit.parent(0) {
        Ok(parent) => Some(parent.tree().map_err(|e| make(&e))?),
        Err(_) => None,
    };
    let diff = repo
        .diff_tree_to_tree(base_tree.as_ref(), Some(&tree), None)
        .map_err(|e| make(&e))?;
    let lines = diff_to_lines(&diff).map_err(|e| make(&e))?;
    Ok(StashDiff { index, lines })
}

/// Restore the stash at `index` to the Working Tree, leaving it in the list.
fn stash_apply(repo: &Repository, index: usize) -> Result<(), GitError> {
    let mut repo = open_mut(repo)?;
    repo.stash_apply(index, None)
        .map_err(|error| GitError::new("apply stash", &error))
}

/// Restore the stash at `index` and remove it from the list.
fn stash_pop(repo: &Repository, index: usize) -> Result<(), GitError> {
    let mut repo = open_mut(repo)?;
    repo.stash_pop(index, None)
        .map_err(|error| GitError::new("pop stash", &error))
}

/// Remove the stash at `index` without restoring it.
fn stash_drop(repo: &Repository, index: usize) -> Result<(), GitError> {
    let mut repo = open_mut(repo)?;
    repo.stash_drop(index)
        .map_err(|error| GitError::new("drop stash", &error))
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

/// Merge the named branch into the current branch. Handles the three clean
/// outcomes (already up to date, fast-forward, merge commit) and leaves a
/// conflicted merge in place — with `MERGE_HEAD` set — for the user to resolve
/// and commit (see [`commit`], which finishes an in-progress merge).
fn merge_branch(repo: &Repository, name: &str) -> Result<MergeOutcome, GitError> {
    let make = |context: &str, error: &git2::Error| GitError::new(context, error);

    let reference = repo
        .resolve_reference_from_short_name(name)
        .map_err(|e| make("find branch", &e))?;
    let annotated = repo
        .reference_to_annotated_commit(&reference)
        .map_err(|e| make("merge", &e))?;

    let (analysis, _preference) = repo
        .merge_analysis(&[&annotated])
        .map_err(|e| make("merge", &e))?;

    if analysis.is_up_to_date() {
        return Ok(MergeOutcome::UpToDate);
    }

    // A fast-forward just advances HEAD to the target — no merge commit.
    if analysis.is_fast_forward() {
        let target = annotated.id();
        let target_object = repo.find_object(target, None).map_err(|e| make("merge", &e))?;
        repo.checkout_tree(&target_object, None)
            .map_err(|e| make("merge", &e))?;
        let mut head = repo.head().map_err(|e| make("merge", &e))?;
        head.set_target(target, &format!("merge {name}: fast-forward"))
            .map_err(|e| make("merge", &e))?;
        return Ok(MergeOutcome::FastForwarded);
    }

    // A real merge: write the merged result into the index and Working Tree.
    repo.merge(&[&annotated], None, None)
        .map_err(|e| make("merge", &e))?;

    let mut index = repo.index().map_err(|e| make("merge", &e))?;
    if index.has_conflicts() {
        let conflicts = index
            .conflicts()
            .map(|iter| iter.count())
            .unwrap_or(0)
            .max(1);
        // Leave the conflicted state for the user to resolve and commit.
        return Ok(MergeOutcome::Conflicts(conflicts));
    }

    // No conflicts: record the merge commit (HEAD + the merged branch) and clear
    // the in-progress merge state.
    let tree = repo
        .find_tree(index.write_tree().map_err(|e| make("merge", &e))?)
        .map_err(|e| make("merge", &e))?;
    let signature = repo.signature().map_err(|e| make("merge", &e))?;
    let head_commit = repo
        .head()
        .and_then(|h| h.peel_to_commit())
        .map_err(|e| make("merge", &e))?;
    let their_commit = repo
        .find_commit(annotated.id())
        .map_err(|e| make("merge", &e))?;
    repo.commit(
        Some("HEAD"),
        &signature,
        &signature,
        &format!("Merge branch '{name}'"),
        &tree,
        &[&head_commit, &their_commit],
    )
    .map_err(|e| make("merge", &e))?;
    repo.cleanup_state().map_err(|e| make("merge", &e))?;

    Ok(MergeOutcome::Created)
}

/// Resolve a conflicted file by taking one side (ours, theirs, or both), then
/// stage the result so the merge can be committed.
fn resolve_conflict(repo: &Repository, path: &str, side: ConflictSide) -> Result<(), GitError> {
    let make = |error: &git2::Error| GitError::new("resolve conflict", error);
    let workdir = repo
        .workdir()
        .ok_or_else(|| GitError::custom("resolve conflict", "no working directory"))?
        .to_path_buf();

    let mut index = repo.index().map_err(|e| make(&e))?;

    // Find this path's conflict entry and read the blob oids for each side.
    let conflicts = index.conflicts().map_err(|e| make(&e))?;
    let mut our_oid = None;
    let mut their_oid = None;
    for conflict in conflicts {
        let conflict = conflict.map_err(|e| make(&e))?;
        let matches = |entry: &Option<git2::IndexEntry>| {
            entry
                .as_ref()
                .and_then(|e| std::str::from_utf8(&e.path).ok())
                .is_some_and(|p| p == path)
        };
        if matches(&conflict.our) || matches(&conflict.their) || matches(&conflict.ancestor) {
            our_oid = conflict.our.map(|e| e.id);
            their_oid = conflict.their.map(|e| e.id);
            break;
        }
    }

    let blob = |oid: Option<git2::Oid>| -> Result<Vec<u8>, GitError> {
        match oid {
            Some(oid) => Ok(repo.find_blob(oid).map_err(|e| make(&e))?.content().to_vec()),
            None => Ok(Vec::new()),
        }
    };

    let content = match side {
        ConflictSide::Ours => blob(our_oid)?,
        ConflictSide::Theirs => blob(their_oid)?,
        ConflictSide::Both => {
            let mut both = blob(our_oid)?;
            if !both.is_empty() && !both.ends_with(b"\n") {
                both.push(b'\n');
            }
            both.extend_from_slice(&blob(their_oid)?);
            both
        }
    };

    let full = workdir.join(path);
    std::fs::write(&full, &content)
        .map_err(|error| GitError::custom("resolve conflict", error.to_string()))?;

    // Clear the conflict and stage the resolved content.
    let path_ref = Path::new(path);
    let _ = index.conflict_remove(path_ref);
    index.add_path(path_ref).map_err(|e| make(&e))?;
    index.write().map_err(|e| make(&e))?;
    Ok(())
}

/// Read a conflicted file's Working Tree content and parse its conflict markers
/// into ordered segments for region-by-region resolution.
fn load_conflict(repo: &Repository, path: &str) -> Result<ConflictFile, GitError> {
    let workdir = repo
        .workdir()
        .ok_or_else(|| GitError::custom("load conflict", "no working directory"))?;
    let content = std::fs::read_to_string(workdir.join(path))
        .map_err(|error| GitError::custom("load conflict", error.to_string()))?;
    Ok(ConflictFile {
        path: path.to_string(),
        segments: parse_conflict(&content),
        raw: content,
    })
}

/// Blame a file line by line against its committed HEAD version. Each line is
/// tagged with the Commit that last touched it (short SHA, author, time). The
/// HEAD blob supplies the text so line numbers line up with the blame result.
fn load_blame(repo: &Repository, path: &str) -> Result<BlameFile, GitError> {
    let make = |error: &git2::Error| GitError::new("blame", error);
    let path_ref = Path::new(path);

    let blame = repo.blame_file(path_ref, None).map_err(|e| make(&e))?;

    // Read the file's text from the HEAD tree (not the working dir) so its lines
    // match the blame's final line numbering.
    let head = repo
        .head()
        .and_then(|h| h.peel_to_commit())
        .map_err(|e| make(&e))?;
    let blob = head
        .tree()
        .and_then(|t| t.get_path(path_ref).and_then(|e| e.to_object(repo)))
        .and_then(|o| o.peel_to_blob())
        .map_err(|e| make(&e))?;
    let content = std::str::from_utf8(blob.content())
        .map_err(|_| GitError::custom("blame", "cannot blame a binary file"))?;

    let lines = content
        .lines()
        .enumerate()
        .map(|(i, text)| {
            // get_line is 1-based; fall back to empty attribution if absent.
            let hunk = blame.get_line(i + 1);
            let (short_sha, author, time) = match hunk {
                Some(hunk) => {
                    let oid = hunk.final_commit_id();
                    let short_sha = oid.to_string()[..7].to_string();
                    let sig = hunk.final_signature();
                    let author = sig
                        .as_ref()
                        .and_then(|s| s.name().ok())
                        .unwrap_or("unknown")
                        .to_string();
                    let time = sig.as_ref().map(|s| s.when().seconds()).unwrap_or(0);
                    (short_sha, author, time)
                }
                None => (String::new(), String::new(), 0),
            };
            BlameLine {
                short_sha,
                author,
                time,
                content: text.to_string(),
            }
        })
        .collect();

    Ok(BlameFile {
        path: path.to_string(),
        lines,
    })
}

/// Save hand-edited `content` for a conflicted file to the Working Tree. Returns
/// whether the file is now fully resolved (no conflict markers left) — in which
/// case it is also staged, finishing the resolution for that file.
fn save_conflict(repo: &Repository, path: &str, content: &str) -> Result<bool, GitError> {
    let make = |error: &git2::Error| GitError::new("resolve conflict", error);
    let workdir = repo
        .workdir()
        .ok_or_else(|| GitError::custom("resolve conflict", "no working directory"))?
        .to_path_buf();
    std::fs::write(workdir.join(path), content)
        .map_err(|error| GitError::custom("resolve conflict", error.to_string()))?;

    let resolved = !content.contains("<<<<<<<");
    if resolved {
        let mut index = repo.index().map_err(|e| make(&e))?;
        let path_ref = Path::new(path);
        let _ = index.conflict_remove(path_ref);
        index.add_path(path_ref).map_err(|e| make(&e))?;
        index.write().map_err(|e| make(&e))?;
    }
    Ok(resolved)
}

/// Split a file's content into [`ConflictSegment`]s by its conflict markers.
/// Understands both the default style and diff3 (the `|||||||` base block, which
/// is skipped — only our and their sides are surfaced).
fn parse_conflict(content: &str) -> Vec<ConflictSegment> {
    let mut segments = Vec::new();
    let mut context: Vec<String> = Vec::new();
    // Which side we are currently accumulating inside a conflict region.
    enum Side {
        Ours,
        Base,
        Theirs,
    }
    let mut region: Option<(Vec<String>, Vec<String>, Side)> = None;

    for line in content.lines() {
        match &mut region {
            None => {
                if line.starts_with("<<<<<<<") {
                    if !context.is_empty() {
                        segments.push(ConflictSegment::Context(std::mem::take(&mut context)));
                    }
                    region = Some((Vec::new(), Vec::new(), Side::Ours));
                } else {
                    context.push(line.to_string());
                }
            }
            Some((ours, theirs, side)) => {
                if line.starts_with("|||||||") {
                    *side = Side::Base;
                } else if line.starts_with("=======") {
                    *side = Side::Theirs;
                } else if line.starts_with(">>>>>>>") {
                    let (ours, theirs, _) = region.take().unwrap();
                    segments.push(ConflictSegment::Conflict { ours, theirs });
                } else {
                    match side {
                        Side::Ours => ours.push(line.to_string()),
                        Side::Theirs => theirs.push(line.to_string()),
                        // The base block (diff3) is not surfaced.
                        Side::Base => {}
                    }
                }
            }
        }
    }
    if !context.is_empty() {
        segments.push(ConflictSegment::Context(context));
    }
    segments
}

/// Resolve the `index`-th conflict region of `path` by taking one side, rewriting
/// just that region in the Working Tree file. Returns whether the file is now
/// fully resolved (no markers left) — in which case it is also staged.
fn resolve_hunk(
    repo: &Repository,
    path: &str,
    index: usize,
    side: ConflictSide,
) -> Result<bool, GitError> {
    let make = |error: &git2::Error| GitError::new("resolve conflict", error);
    let workdir = repo
        .workdir()
        .ok_or_else(|| GitError::custom("resolve conflict", "no working directory"))?
        .to_path_buf();
    let full = workdir.join(path);
    let content = std::fs::read_to_string(&full)
        .map_err(|error| GitError::custom("resolve conflict", error.to_string()))?;

    // Rebuild the file, replacing the chosen region's marker block with the kept
    // side and leaving every other region (and its markers) untouched.
    let mut out: Vec<String> = Vec::new();
    let mut seen = 0;
    let mut lines = content.lines();
    while let Some(line) = lines.next() {
        if !line.starts_with("<<<<<<<") {
            out.push(line.to_string());
            continue;
        }
        // Collect this region's sides up to its closing marker.
        let (mut ours, mut theirs) = (Vec::new(), Vec::new());
        let mut in_base = false;
        let mut on_theirs = false;
        for inner in lines.by_ref() {
            if inner.starts_with("|||||||") {
                in_base = true;
            } else if inner.starts_with("=======") {
                in_base = false;
                on_theirs = true;
            } else if inner.starts_with(">>>>>>>") {
                break;
            } else if in_base {
                // diff3 base block: ignored.
            } else if on_theirs {
                theirs.push(inner.to_string());
            } else {
                ours.push(inner.to_string());
            }
        }

        if seen == index {
            // Replace just this region with the chosen side.
            match side {
                ConflictSide::Ours => out.extend(ours),
                ConflictSide::Theirs => out.extend(theirs),
                ConflictSide::Both => {
                    out.extend(ours);
                    out.extend(theirs);
                }
            }
        } else {
            // Keep this region conflicted, markers and all.
            out.push("<<<<<<< HEAD".to_string());
            out.extend(ours);
            out.push("=======".to_string());
            out.extend(theirs);
            out.push(">>>>>>> incoming".to_string());
        }
        seen += 1;
    }

    // Preserve a trailing newline if the original had one.
    let mut text = out.join("\n");
    if content.ends_with('\n') {
        text.push('\n');
    }
    std::fs::write(&full, &text)
        .map_err(|error| GitError::custom("resolve conflict", error.to_string()))?;

    // If no conflict markers remain, the file is resolved: stage it.
    let resolved = !text.contains("<<<<<<<");
    if resolved {
        let mut index = repo.index().map_err(|e| make(&e))?;
        let path_ref = Path::new(path);
        let _ = index.conflict_remove(path_ref);
        index.add_path(path_ref).map_err(|e| make(&e))?;
        index.write().map_err(|e| make(&e))?;
    }
    Ok(resolved)
}

/// Abort an in-progress merge: discard the half-merged Working Tree and index by
/// resetting hard to HEAD, then clear the merge state.
fn abort_merge(repo: &Repository) -> Result<(), GitError> {
    let make = |context: &str, error: &git2::Error| GitError::new(context, error);
    let head = repo
        .head()
        .and_then(|h| h.peel(ObjectType::Commit))
        .map_err(|e| make("abort merge", &e))?;
    repo.reset(&head, git2::ResetType::Hard, None)
        .map_err(|e| make("abort merge", &e))?;
    repo.cleanup_state().map_err(|e| make("abort merge", &e))?;
    Ok(())
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

/// Delete every local branch that has no counterpart on the Remote — neither a
/// resolvable upstream nor a same-named `origin/<branch>` — leaving the current
/// branch untouched. Returns the names actually deleted. Best-effort: a branch
/// that fails to delete is skipped rather than aborting the whole cleanup.
///
/// Accuracy depends on the remote-tracking refs being current, so a Fetch (which
/// prunes) beforehand makes this match the Remote's real state.
fn prune_branches(repo: &Repository) -> Result<Vec<String>, git2::Error> {
    // Collect candidates first; deleting while iterating the branch list would
    // invalidate it.
    let mut candidates = Vec::new();
    for entry in repo.branches(Some(git2::BranchType::Local))? {
        let (branch, _) = entry?;
        if branch.is_head() {
            continue;
        }
        let Some(name) = branch.name()?.map(str::to_string) else {
            continue;
        };
        if !is_on_remote(repo, &branch, &name) {
            candidates.push(name);
        }
    }

    let mut deleted = Vec::new();
    for name in candidates {
        if let Ok(mut branch) = repo.find_branch(&name, git2::BranchType::Local)
            && branch.delete().is_ok()
        {
            deleted.push(name);
        }
    }
    Ok(deleted)
}

/// Whether a local branch exists on the Remote: it has a resolvable upstream, or
/// a remote-tracking branch shares its name (`origin/<branch>`).
fn is_on_remote(repo: &Repository, branch: &git2::Branch, name: &str) -> bool {
    branch.upstream().is_ok()
        || repo
            .find_branch(&format!("origin/{name}"), git2::BranchType::Remote)
            .is_ok()
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

        /// The conflicted files from the most recent `StatusLoaded`.
        fn last_conflicted(&self) -> Vec<FileEntry> {
            self.0
                .borrow()
                .iter()
                .rev()
                .find_map(|event| match event {
                    GitEvent::StatusLoaded { conflicted, .. } => Some(conflicted.clone()),
                    _ => None,
                })
                .expect("expected a StatusLoaded event")
        }

        /// The tag list from the most recent `TagsLoaded`.
        fn last_tags(&self) -> Vec<TagInfo> {
            self.0
                .borrow()
                .iter()
                .rev()
                .find_map(|event| match event {
                    GitEvent::TagsLoaded(tags) => Some(tags.clone()),
                    _ => None,
                })
                .expect("expected a TagsLoaded event")
        }

        /// The stash list from the most recent `StashesLoaded`.
        fn last_stashes(&self) -> Vec<StashInfo> {
            self.0
                .borrow()
                .iter()
                .rev()
                .find_map(|event| match event {
                    GitEvent::StashesLoaded(stashes) => Some(stashes.clone()),
                    _ => None,
                })
                .expect("expected a StashesLoaded event")
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
    fn prune_removes_local_branches_absent_from_remote() {
        let (dir, repo) = temp_repo();
        let events = Collector::default();
        commit_file(dir.path(), &repo, &events, "a.txt", "x\n");
        let current = repo.head().unwrap().shorthand().unwrap().to_string();
        let head_oid = repo.head().unwrap().target().unwrap();

        // `gone` exists only locally; `kept` has a matching remote-tracking ref.
        let head = repo.find_commit(head_oid).unwrap();
        repo.branch("gone", &head, false).unwrap();
        repo.branch("kept", &head, false).unwrap();
        repo.reference("refs/remotes/origin/kept", head_oid, false, "test")
            .unwrap();

        process(&repo, GitCommand::PruneBranches, &events);

        let names: Vec<String> = events.last_branches().into_iter().map(|b| b.name).collect();
        assert!(!names.contains(&"gone".to_string()), "gone should be pruned");
        assert!(names.contains(&"kept".to_string()), "kept is on the remote");
        assert!(names.contains(&current), "the current branch is never pruned");
        assert!(
            events
                .events()
                .iter()
                .any(|e| matches!(e, GitEvent::BranchesPruned(pruned) if pruned == &vec!["gone".to_string()]))
        );
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

    #[test]
    fn stash_push_sets_aside_changes_and_pop_restores_them() {
        let (dir, repo) = temp_repo();
        let events = Collector::default();
        commit_file(dir.path(), &repo, &events, "a.txt", "one\n");

        // Modify the tracked file, then stash it.
        write(dir.path(), "a.txt", "two\n");
        process(&repo, GitCommand::StashPush { message: Some("wip".into()), paths: vec![] }, &events);

        // The Working Tree is clean and the stash is listed.
        let (unstaged, staged) = events.last_status();
        assert!(unstaged.is_empty() && staged.is_empty());
        let stashes = events.last_stashes();
        assert_eq!(stashes.len(), 1);
        assert_eq!(stashes[0].index, 0);

        // Popping restores the change and empties the stash list.
        process(&repo, GitCommand::StashPop(0), &events);
        assert_eq!(fs::read_to_string(dir.path().join("a.txt")).unwrap(), "two\n");
        assert!(events.last_stashes().is_empty());
    }

    #[test]
    fn stash_push_includes_untracked_files() {
        let (dir, repo) = temp_repo();
        let events = Collector::default();
        commit_file(dir.path(), &repo, &events, "a.txt", "one\n");

        // A brand-new untracked file is stashed away too.
        write(dir.path(), "new.txt", "fresh\n");
        process(&repo, GitCommand::StashPush { message: None, paths: vec![] }, &events);

        assert!(!dir.path().join("new.txt").exists());
        process(&repo, GitCommand::StashPop(0), &events);
        assert!(dir.path().join("new.txt").exists());
    }

    #[test]
    fn stash_drop_removes_a_stash_without_restoring_it() {
        let (dir, repo) = temp_repo();
        let events = Collector::default();
        commit_file(dir.path(), &repo, &events, "a.txt", "one\n");

        write(dir.path(), "a.txt", "two\n");
        process(&repo, GitCommand::StashPush { message: Some("wip".into()), paths: vec![] }, &events);
        assert_eq!(events.last_stashes().len(), 1);

        // Dropping clears the stash but does not bring the change back.
        process(&repo, GitCommand::StashDrop(0), &events);
        assert!(events.last_stashes().is_empty());
        assert_eq!(fs::read_to_string(dir.path().join("a.txt")).unwrap(), "one\n");
    }

    #[test]
    fn stash_apply_keeps_the_stash_in_the_list() {
        let (dir, repo) = temp_repo();
        let events = Collector::default();
        commit_file(dir.path(), &repo, &events, "a.txt", "one\n");

        write(dir.path(), "a.txt", "two\n");
        process(&repo, GitCommand::StashPush { message: Some("wip".into()), paths: vec![] }, &events);

        // Apply restores the change but leaves the stash present.
        process(&repo, GitCommand::StashApply(0), &events);
        assert_eq!(fs::read_to_string(dir.path().join("a.txt")).unwrap(), "two\n");
        process(&repo, GitCommand::LoadStashes, &events);
        assert_eq!(events.last_stashes().len(), 1);
    }

    #[test]
    fn stash_push_with_paths_stashes_only_the_listed_files() {
        let (dir, repo) = temp_repo();
        let events = Collector::default();
        commit_file(dir.path(), &repo, &events, "a.txt", "one\n");
        commit_file(dir.path(), &repo, &events, "b.txt", "one\n");

        // Both files change, but only a.txt is stashed.
        write(dir.path(), "a.txt", "two\n");
        write(dir.path(), "b.txt", "two\n");
        process(
            &repo,
            GitCommand::StashPush {
                message: None,
                paths: vec!["a.txt".into()],
            },
            &events,
        );

        // a.txt is restored to HEAD (stashed away); b.txt keeps its change.
        assert_eq!(fs::read_to_string(dir.path().join("a.txt")).unwrap(), "one\n");
        assert_eq!(fs::read_to_string(dir.path().join("b.txt")).unwrap(), "two\n");
        assert_eq!(events.last_stashes().len(), 1);
    }

    #[test]
    fn selective_stash_keeps_unselected_modified_and_untracked_files() {
        let (dir, repo) = temp_repo();
        let events = Collector::default();
        commit_file(dir.path(), &repo, &events, "a.txt", "one\n");
        commit_file(dir.path(), &repo, &events, "b.txt", "one\n");

        // a.txt modified (selected), b.txt modified + new.txt untracked (kept).
        write(dir.path(), "a.txt", "two\n");
        write(dir.path(), "b.txt", "two\n");
        write(dir.path(), "new.txt", "fresh\n");
        process(
            &repo,
            GitCommand::StashPush {
                message: None,
                paths: vec!["a.txt".into()],
            },
            &events,
        );

        // Only a.txt was stashed; the kept files survive untouched.
        assert_eq!(fs::read_to_string(dir.path().join("a.txt")).unwrap(), "one\n");
        assert_eq!(fs::read_to_string(dir.path().join("b.txt")).unwrap(), "two\n");
        assert_eq!(fs::read_to_string(dir.path().join("new.txt")).unwrap(), "fresh\n");

        // Popping brings a.txt's change back without disturbing the rest.
        process(&repo, GitCommand::StashPop(0), &events);
        assert_eq!(fs::read_to_string(dir.path().join("a.txt")).unwrap(), "two\n");
        assert_eq!(fs::read_to_string(dir.path().join("b.txt")).unwrap(), "two\n");
        assert_eq!(fs::read_to_string(dir.path().join("new.txt")).unwrap(), "fresh\n");
    }

    #[test]
    fn load_stash_diff_reports_the_stashed_change() {
        let (dir, repo) = temp_repo();
        let events = Collector::default();
        commit_file(dir.path(), &repo, &events, "a.txt", "one\n");

        write(dir.path(), "a.txt", "one\ntwo\n");
        process(
            &repo,
            GitCommand::StashPush { message: Some("wip".into()), paths: vec![] },
            &events,
        );

        process(&repo, GitCommand::LoadStashDiff(0), &events);
        let diff = events
            .events()
            .into_iter()
            .rev()
            .find_map(|event| match event {
                GitEvent::StashDiffLoaded(diff) => Some(diff),
                _ => None,
            })
            .expect("expected a StashDiffLoaded event");

        assert_eq!(diff.index, 0);
        // The added "two" line shows up as an addition, under a file header.
        assert!(
            diff.lines
                .iter()
                .any(|l| l.kind == DiffLineKind::Addition && l.content.contains("two"))
        );
        assert!(
            diff.lines
                .iter()
                .any(|l| l.kind == DiffLineKind::Header && l.content.contains("a.txt"))
        );
    }

    #[test]
    fn create_lightweight_tag_lists_it_against_its_target() {
        let (dir, repo) = temp_repo();
        let events = Collector::default();
        commit_file(dir.path(), &repo, &events, "a.txt", "x\n");
        let head = short_sha(repo.head().unwrap().target().unwrap());

        process(
            &repo,
            GitCommand::CreateTag {
                name: "v1.0".into(),
                message: None,
            },
            &events,
        );

        assert!(
            events
                .events()
                .iter()
                .any(|e| matches!(e, GitEvent::TagCreated(name) if name == "v1.0"))
        );
        let tags = events.last_tags();
        assert_eq!(tags.len(), 1);
        assert_eq!(tags[0].name, "v1.0");
        assert_eq!(tags[0].target, head);
        assert!(!tags[0].is_annotated);
        assert!(tags[0].message.is_none());
    }

    #[test]
    fn create_annotated_tag_carries_its_message() {
        let (dir, repo) = temp_repo();
        let events = Collector::default();
        commit_file(dir.path(), &repo, &events, "a.txt", "x\n");

        process(
            &repo,
            GitCommand::CreateTag {
                name: "v2.0".into(),
                message: Some("release two".into()),
            },
            &events,
        );

        let tags = events.last_tags();
        let tag = tags.iter().find(|t| t.name == "v2.0").expect("the tag");
        assert!(tag.is_annotated);
        assert_eq!(tag.message.as_deref(), Some("release two"));
    }

    #[test]
    fn delete_tag_removes_it_from_the_list() {
        let (dir, repo) = temp_repo();
        let events = Collector::default();
        commit_file(dir.path(), &repo, &events, "a.txt", "x\n");
        process(
            &repo,
            GitCommand::CreateTag {
                name: "v1.0".into(),
                message: None,
            },
            &events,
        );
        assert_eq!(events.last_tags().len(), 1);

        process(&repo, GitCommand::DeleteTag("v1.0".into()), &events);

        assert!(events.last_tags().is_empty());
        assert!(
            events
                .events()
                .iter()
                .any(|e| matches!(e, GitEvent::TagDeleted(name) if name == "v1.0"))
        );
    }

    #[test]
    fn push_tag_publishes_it_to_the_remote() {
        let remote = tempfile::tempdir().unwrap();
        Repository::init_bare(remote.path()).unwrap();

        let (dir, repo) = temp_repo();
        let events = Collector::default();
        commit_file(dir.path(), &repo, &events, "a.txt", "x\n");
        let origin = repo.remote("origin", remote.path().to_str().unwrap()).unwrap();
        drop(origin);
        process(
            &repo,
            GitCommand::CreateTag {
                name: "v1.0".into(),
                message: None,
            },
            &events,
        );

        process(&repo, GitCommand::PushTag("v1.0".into()), &events);

        assert!(events.events().iter().any(|e| matches!(e, GitEvent::Pushed)));
        // The tag now exists on the remote.
        let remote_repo = Repository::open(remote.path()).unwrap();
        assert!(remote_repo.revparse_single("refs/tags/v1.0").is_ok());
    }

    #[test]
    fn reset_hard_moves_head_and_discards_working_changes() {
        let (dir, repo) = temp_repo();
        let events = Collector::default();
        commit_file(dir.path(), &repo, &events, "a.txt", "one\n");
        let first = repo.head().unwrap().target().unwrap().to_string();
        commit_file(dir.path(), &repo, &events, "a.txt", "two\n");

        // An uncommitted edit on top of the second commit.
        write(dir.path(), "a.txt", "scratch\n");
        process(
            &repo,
            GitCommand::Reset {
                sha: first.clone(),
                kind: ResetKind::Hard,
            },
            &events,
        );

        // HEAD is back at the first commit and the file matches it; the scratch
        // edit and the second commit's content are both gone.
        assert_eq!(repo.head().unwrap().target().unwrap().to_string(), first);
        assert_eq!(fs::read_to_string(dir.path().join("a.txt")).unwrap(), "one\n");
        let (unstaged, staged) = events.last_status();
        assert!(unstaged.is_empty() && staged.is_empty());
    }

    #[test]
    fn reset_soft_moves_head_but_keeps_the_changes_staged() {
        let (dir, repo) = temp_repo();
        let events = Collector::default();
        commit_file(dir.path(), &repo, &events, "a.txt", "one\n");
        let first = repo.head().unwrap().target().unwrap().to_string();
        commit_file(dir.path(), &repo, &events, "b.txt", "two\n");

        process(
            &repo,
            GitCommand::Reset {
                sha: first.clone(),
                kind: ResetKind::Soft,
            },
            &events,
        );

        // HEAD moved back, but the second commit's file is now staged, not lost.
        assert_eq!(repo.head().unwrap().target().unwrap().to_string(), first);
        let (_unstaged, staged) = events.last_status();
        assert_eq!(paths(&staged), ["b.txt"]);
    }

    #[test]
    fn revert_creates_an_inverse_commit() {
        let (dir, repo) = temp_repo();
        let events = Collector::default();
        commit_file(dir.path(), &repo, &events, "a.txt", "one\n");
        // A second commit that adds a file, which the revert should remove.
        commit_file(dir.path(), &repo, &events, "b.txt", "two\n");
        let to_revert = repo.head().unwrap().target().unwrap().to_string();

        process(&repo, GitCommand::Revert(to_revert), &events);

        assert!(matches!(
            events.events().iter().rev().find_map(|e| match e {
                GitEvent::Reverted { outcome } => Some(outcome.clone()),
                _ => None,
            }),
            Some(RevertOutcome::Created)
        ));
        // The reverting commit is on top, and b.txt is gone again.
        assert!(!dir.path().join("b.txt").exists());
        let head = repo.head().unwrap().peel_to_commit().unwrap();
        assert!(head.summary().unwrap().unwrap().starts_with("Revert"));
        assert_eq!(head.parent_count(), 1);
        assert_eq!(repo.state(), git2::RepositoryState::Clean);
    }

    #[test]
    fn conflicting_revert_is_reported_and_finished_by_a_commit() {
        let (dir, repo) = temp_repo();
        let events = Collector::default();
        commit_file(dir.path(), &repo, &events, "a.txt", "one\n");
        let base = repo.head().unwrap().target().unwrap().to_string();
        // Two further edits to the same line: reverting the base no longer
        // applies cleanly against the current content.
        commit_file(dir.path(), &repo, &events, "a.txt", "two\n");
        commit_file(dir.path(), &repo, &events, "a.txt", "three\n");

        process(&repo, GitCommand::Revert(base), &events);

        let outcome = events.events().into_iter().rev().find_map(|e| match e {
            GitEvent::Reverted { outcome } => Some(outcome),
            _ => None,
        });
        assert!(matches!(outcome, Some(RevertOutcome::Conflicts(_))));
        assert_ne!(repo.state(), git2::RepositoryState::Clean);

        // Resolving and committing finishes the revert and clears the state.
        write(dir.path(), "a.txt", "resolved\n");
        process(&repo, GitCommand::StageFile("a.txt".into()), &events);
        process(&repo, GitCommand::Commit("finish revert".into()), &events);
        assert_eq!(repo.state(), git2::RepositoryState::Clean);
    }

    #[test]
    fn cherry_pick_applies_a_commit_onto_head() {
        let (dir, repo) = temp_repo();
        let events = Collector::default();
        commit_file(dir.path(), &repo, &events, "a.txt", "one\n");
        // A commit on `feature` adding c.txt — the one to cherry-pick.
        process(&repo, GitCommand::CreateBranch("feature".into()), &events);
        commit_file(dir.path(), &repo, &events, "c.txt", "pick me\n");
        let pick = repo.head().unwrap().target().unwrap().to_string();
        process(&repo, GitCommand::Checkout("master".into()), &events);

        // master has no c.txt yet; cherry-picking brings it over cleanly.
        assert!(!dir.path().join("c.txt").exists());
        process(&repo, GitCommand::CherryPick(pick), &events);

        assert!(matches!(
            events.events().iter().rev().find_map(|e| match e {
                GitEvent::CherryPicked { outcome } => Some(outcome.clone()),
                _ => None,
            }),
            Some(CherryPickOutcome::Created)
        ));
        // The picked change is on top, keeping the original message, single-parent.
        assert_eq!(fs::read_to_string(dir.path().join("c.txt")).unwrap(), "pick me\n");
        let head = repo.head().unwrap().peel_to_commit().unwrap();
        assert_eq!(head.summary().unwrap().unwrap(), "add c.txt");
        assert_eq!(head.parent_count(), 1);
        assert_eq!(repo.state(), git2::RepositoryState::Clean);
    }

    #[test]
    fn conflicting_cherry_pick_is_reported_and_finished_by_a_commit() {
        let (dir, repo) = temp_repo();
        let events = Collector::default();
        commit_file(dir.path(), &repo, &events, "a.txt", "one\n");
        // feature and master change the same line differently.
        process(&repo, GitCommand::CreateBranch("feature".into()), &events);
        commit_file(dir.path(), &repo, &events, "a.txt", "feature\n");
        let pick = repo.head().unwrap().target().unwrap().to_string();
        process(&repo, GitCommand::Checkout("master".into()), &events);
        commit_file(dir.path(), &repo, &events, "a.txt", "master\n");

        process(&repo, GitCommand::CherryPick(pick), &events);

        let outcome = events.events().into_iter().rev().find_map(|e| match e {
            GitEvent::CherryPicked { outcome } => Some(outcome),
            _ => None,
        });
        assert!(matches!(outcome, Some(CherryPickOutcome::Conflicts(_))));
        assert_ne!(repo.state(), git2::RepositoryState::Clean);

        // Resolving and committing finishes the pick and clears the state.
        write(dir.path(), "a.txt", "resolved\n");
        process(&repo, GitCommand::StageFile("a.txt".into()), &events);
        process(&repo, GitCommand::Commit("finish pick".into()), &events);
        assert_eq!(repo.state(), git2::RepositoryState::Clean);
    }

    /// The outcome of the most recent `Merged` event.
    fn last_merge(events: &Collector) -> MergeOutcome {
        events
            .events()
            .into_iter()
            .rev()
            .find_map(|event| match event {
                GitEvent::Merged { outcome, .. } => Some(outcome),
                _ => None,
            })
            .expect("expected a Merged event")
    }

    #[test]
    fn merge_of_an_already_contained_branch_is_up_to_date() {
        let (dir, repo) = temp_repo();
        let events = Collector::default();
        commit_file(dir.path(), &repo, &events, "a.txt", "one\n");
        process(&repo, GitCommand::CreateBranch("feature".into()), &events);
        process(&repo, GitCommand::Checkout("master".into()), &events);

        // feature points at the same commit as master.
        process(&repo, GitCommand::Merge("feature".into()), &events);
        assert_eq!(last_merge(&events), MergeOutcome::UpToDate);
    }

    #[test]
    fn merge_fast_forwards_when_head_has_not_diverged() {
        let (dir, repo) = temp_repo();
        let events = Collector::default();
        commit_file(dir.path(), &repo, &events, "a.txt", "one\n");
        process(&repo, GitCommand::CreateBranch("feature".into()), &events);
        commit_file(dir.path(), &repo, &events, "b.txt", "two\n");
        process(&repo, GitCommand::Checkout("master".into()), &events);

        // master is strictly behind feature, so the merge fast-forwards.
        process(&repo, GitCommand::Merge("feature".into()), &events);
        assert_eq!(last_merge(&events), MergeOutcome::FastForwarded);
        // The fast-forwarded file is now present, with no merge commit.
        assert!(dir.path().join("b.txt").exists());
        let head = repo.head().unwrap().peel_to_commit().unwrap();
        assert_eq!(head.parent_count(), 1);
    }

    #[test]
    fn merge_creates_a_commit_when_both_branches_advanced() {
        let (dir, repo) = temp_repo();
        let events = Collector::default();
        commit_file(dir.path(), &repo, &events, "base.txt", "base\n");
        process(&repo, GitCommand::CreateBranch("feature".into()), &events);
        commit_file(dir.path(), &repo, &events, "feature.txt", "f\n");
        process(&repo, GitCommand::Checkout("master".into()), &events);
        commit_file(dir.path(), &repo, &events, "master.txt", "m\n");

        // Non-conflicting divergent changes produce a merge commit.
        process(&repo, GitCommand::Merge("feature".into()), &events);
        assert_eq!(last_merge(&events), MergeOutcome::Created);
        let head = repo.head().unwrap().peel_to_commit().unwrap();
        assert_eq!(head.parent_count(), 2);
        assert!(dir.path().join("feature.txt").exists());
        assert!(dir.path().join("master.txt").exists());
    }

    #[test]
    fn conflicting_merge_is_reported_and_finished_by_a_commit() {
        let (dir, repo) = temp_repo();
        let events = Collector::default();
        commit_file(dir.path(), &repo, &events, "a.txt", "base\n");
        process(&repo, GitCommand::CreateBranch("feature".into()), &events);
        commit_file(dir.path(), &repo, &events, "a.txt", "feature\n");
        process(&repo, GitCommand::Checkout("master".into()), &events);
        commit_file(dir.path(), &repo, &events, "a.txt", "master\n");

        // Both sides changed the same file: the merge conflicts.
        process(&repo, GitCommand::Merge("feature".into()), &events);
        assert_eq!(last_merge(&events), MergeOutcome::Conflicts(1));
        assert_eq!(repo.state(), git2::RepositoryState::Merge);

        // Resolve the conflict, stage it, and commit: the merge is finished as a
        // two-parent commit and the merge state is cleared.
        write(dir.path(), "a.txt", "resolved\n");
        process(&repo, GitCommand::StageFile("a.txt".into()), &events);
        process(&repo, GitCommand::Commit("merge".into()), &events);

        assert_eq!(repo.state(), git2::RepositoryState::Clean);
        let head = repo.head().unwrap().peel_to_commit().unwrap();
        assert_eq!(head.parent_count(), 2);
    }

    /// Build a conflict on `a.txt`: "feature" on the feature branch, "master" on
    /// master, with the merge left in a conflicted state.
    fn conflicted_repo() -> (TempDir, Repository, Collector) {
        let (dir, repo) = temp_repo();
        let events = Collector::default();
        commit_file(dir.path(), &repo, &events, "a.txt", "base\n");
        process(&repo, GitCommand::CreateBranch("feature".into()), &events);
        commit_file(dir.path(), &repo, &events, "a.txt", "feature\n");
        process(&repo, GitCommand::Checkout("master".into()), &events);
        commit_file(dir.path(), &repo, &events, "a.txt", "master\n");
        process(&repo, GitCommand::Merge("feature".into()), &events);
        assert_eq!(last_merge(&events), MergeOutcome::Conflicts(1));
        (dir, repo, events)
    }

    #[test]
    fn resolving_a_conflict_with_ours_keeps_our_version_and_clears_the_conflict() {
        let (dir, repo, events) = conflicted_repo();

        process(
            &repo,
            GitCommand::ResolveConflict {
                path: "a.txt".into(),
                side: ConflictSide::Ours,
            },
            &events,
        );

        assert_eq!(fs::read_to_string(dir.path().join("a.txt")).unwrap(), "master\n");
        assert!(events.last_conflicted().is_empty());

        // The resolved file is staged, so the merge can be committed.
        process(&repo, GitCommand::Commit("merge".into()), &events);
        assert_eq!(repo.state(), git2::RepositoryState::Clean);
        assert_eq!(repo.head().unwrap().peel_to_commit().unwrap().parent_count(), 2);
    }

    #[test]
    fn resolving_a_conflict_with_theirs_takes_the_merged_in_version() {
        let (dir, repo, events) = conflicted_repo();

        process(
            &repo,
            GitCommand::ResolveConflict {
                path: "a.txt".into(),
                side: ConflictSide::Theirs,
            },
            &events,
        );

        assert_eq!(fs::read_to_string(dir.path().join("a.txt")).unwrap(), "feature\n");
        assert!(events.last_conflicted().is_empty());
    }

    #[test]
    fn resolving_a_conflict_with_both_keeps_each_side() {
        let (dir, repo, events) = conflicted_repo();

        process(
            &repo,
            GitCommand::ResolveConflict {
                path: "a.txt".into(),
                side: ConflictSide::Both,
            },
            &events,
        );

        assert_eq!(
            fs::read_to_string(dir.path().join("a.txt")).unwrap(),
            "master\nfeature\n"
        );
        assert!(events.last_conflicted().is_empty());
    }

    #[test]
    fn aborting_a_merge_restores_head_and_clears_the_conflict() {
        let (dir, repo, events) = conflicted_repo();

        process(&repo, GitCommand::AbortMerge, &events);

        assert_eq!(repo.state(), git2::RepositoryState::Clean);
        assert_eq!(fs::read_to_string(dir.path().join("a.txt")).unwrap(), "master\n");
        assert!(events.last_conflicted().is_empty());
    }

    /// The most recent `ConflictLoaded` payload.
    fn last_conflict(events: &Collector) -> ConflictFile {
        events
            .events()
            .into_iter()
            .rev()
            .find_map(|event| match event {
                GitEvent::ConflictLoaded(file) => Some(file),
                _ => None,
            })
            .expect("expected a ConflictLoaded event")
    }

    #[test]
    fn load_conflict_parses_a_files_regions() {
        let (_dir, repo, events) = conflicted_repo();

        process(&repo, GitCommand::LoadConflict("a.txt".into()), &events);

        let file = last_conflict(&events);
        let regions: Vec<&ConflictSegment> = file
            .segments
            .iter()
            .filter(|s| matches!(s, ConflictSegment::Conflict { .. }))
            .collect();
        assert_eq!(regions.len(), 1);
        let ConflictSegment::Conflict { ours, theirs } = regions[0] else {
            unreachable!()
        };
        // ours is the current branch (master), theirs the merged-in (feature).
        assert_eq!(ours, &["master"]);
        assert_eq!(theirs, &["feature"]);
    }

    #[test]
    fn resolve_hunk_resolves_one_region_and_stages_when_no_markers_remain() {
        let (dir, repo, events) = conflicted_repo();

        process(
            &repo,
            GitCommand::ResolveHunk {
                path: "a.txt".into(),
                index: 0,
                side: ConflictSide::Theirs,
            },
            &events,
        );

        // Single region taken from theirs; file has no markers and is staged.
        assert_eq!(fs::read_to_string(dir.path().join("a.txt")).unwrap(), "feature\n");
        assert!(events.last_conflicted().is_empty());
        let (_unstaged, staged) = events.last_status();
        assert_eq!(paths(&staged), ["a.txt"]);
    }

    #[test]
    fn resolve_hunk_handles_one_region_at_a_time_in_a_multi_conflict_file() {
        let (dir, repo) = temp_repo();
        let events = Collector::default();
        // A file whose first and last lines both conflict, with enough stable
        // middle (> the 3-line diff context) to split into two regions.
        let mid = "1\n2\n3\n4\n5\n6\n7\n8\n";
        commit_file(dir.path(), &repo, &events, "f.txt", &format!("a\n{mid}z\n"));
        process(&repo, GitCommand::CreateBranch("feature".into()), &events);
        commit_file(dir.path(), &repo, &events, "f.txt", &format!("A\n{mid}Z\n"));
        process(&repo, GitCommand::Checkout("master".into()), &events);
        commit_file(dir.path(), &repo, &events, "f.txt", &format!("A2\n{mid}Z2\n"));
        process(&repo, GitCommand::Merge("feature".into()), &events);

        // Two distinct conflict regions, separated by the shared middle.
        process(&repo, GitCommand::LoadConflict("f.txt".into()), &events);
        let regions = last_conflict(&events)
            .segments
            .iter()
            .filter(|s| matches!(s, ConflictSegment::Conflict { .. }))
            .count();
        assert_eq!(regions, 2);

        // Take ours for the first region; the file stays conflicted (one left).
        process(
            &repo,
            GitCommand::ResolveHunk {
                path: "f.txt".into(),
                index: 0,
                side: ConflictSide::Ours,
            },
            &events,
        );
        assert_eq!(paths(&events.last_conflicted()), ["f.txt"]);

        // The remaining region is now index 0; take theirs to finish.
        process(
            &repo,
            GitCommand::ResolveHunk {
                path: "f.txt".into(),
                index: 0,
                side: ConflictSide::Theirs,
            },
            &events,
        );
        assert_eq!(
            fs::read_to_string(dir.path().join("f.txt")).unwrap(),
            format!("A2\n{mid}Z\n")
        );
        assert!(events.last_conflicted().is_empty());
    }

    #[test]
    fn load_conflict_carries_the_raw_content_for_the_editor() {
        let (_dir, repo, events) = conflicted_repo();

        process(&repo, GitCommand::LoadConflict("a.txt".into()), &events);

        // The raw seed keeps the markers, so the editor opens on the real file.
        let raw = last_conflict(&events).raw;
        assert!(raw.contains("<<<<<<<"));
        assert!(raw.contains("master"));
        assert!(raw.contains("feature"));
    }

    #[test]
    fn save_conflict_with_no_markers_writes_and_stages_the_file() {
        let (dir, repo, events) = conflicted_repo();

        // A hand-merged result with no markers left resolves the file.
        process(
            &repo,
            GitCommand::SaveConflict {
                path: "a.txt".into(),
                content: "merged by hand\n".into(),
            },
            &events,
        );

        assert_eq!(
            fs::read_to_string(dir.path().join("a.txt")).unwrap(),
            "merged by hand\n"
        );
        assert!(events.last_conflicted().is_empty());
        let (_unstaged, staged) = events.last_status();
        assert_eq!(paths(&staged), ["a.txt"]);
    }

    #[test]
    fn save_conflict_keeping_markers_leaves_the_file_conflicted() {
        let (dir, repo, events) = conflicted_repo();

        // Saving content that still has markers writes it but keeps the conflict,
        // and re-emits the parsed regions for the resolver.
        let content = "<<<<<<< HEAD\nmaster\n=======\nfeature\n>>>>>>> incoming\ntail\n";
        process(
            &repo,
            GitCommand::SaveConflict {
                path: "a.txt".into(),
                content: content.into(),
            },
            &events,
        );

        assert_eq!(fs::read_to_string(dir.path().join("a.txt")).unwrap(), content);
        assert_eq!(paths(&events.last_conflicted()), ["a.txt"]);
        assert_eq!(last_conflict(&events).raw, content);
    }

    /// The most recent `BlameLoaded` payload.
    fn last_blame(events: &Collector) -> BlameFile {
        events
            .events()
            .into_iter()
            .rev()
            .find_map(|event| match event {
                GitEvent::BlameLoaded(file) => Some(file),
                _ => None,
            })
            .expect("expected a BlameLoaded event")
    }

    #[test]
    fn blame_attributes_each_line_to_its_commit() {
        let (dir, repo) = temp_repo();
        let events = Collector::default();
        commit_file(dir.path(), &repo, &events, "f.txt", "one\ntwo\n");
        // A later commit appends a third line, leaving the first two untouched.
        commit_file(dir.path(), &repo, &events, "f.txt", "one\ntwo\nthree\n");

        process(&repo, GitCommand::LoadBlame("f.txt".into()), &events);
        let blame = last_blame(&events);

        assert_eq!(blame.lines.len(), 3);
        assert_eq!(blame.lines[0].content, "one");
        assert_eq!(blame.lines[2].content, "three");
        // The appended line is attributed to a different (later) commit.
        assert_ne!(blame.lines[0].short_sha, blame.lines[2].short_sha);
        assert!(blame.lines.iter().all(|l| l.author == "Tester"));
        assert!(blame.lines.iter().all(|l| l.time > 0));
    }

    #[test]
    fn blaming_a_missing_file_reports_an_error() {
        let (dir, repo) = temp_repo();
        let events = Collector::default();
        commit_file(dir.path(), &repo, &events, "f.txt", "x\n");

        process(&repo, GitCommand::LoadBlame("nope.txt".into()), &events);

        assert!(
            events
                .events()
                .iter()
                .any(|e| matches!(e, GitEvent::Error(_)))
        );
    }
}

