//! The `ui` module: all iced widgets.
//!
//! Every function here is a pure view over `App` state, producing widgets that
//! emit [`Message`]s. It never touches git2 directly — only the message types.
//! All color and surface treatment lives in [`style`].

mod graph;
mod highlight;
mod style;
mod tree;
mod worddiff;

use iced::widget::canvas;
use iced::widget::{
    button, checkbox, column, container, mouse_area, rich_text, row, scrollable, space, span,
    stack, text, text_editor, text_input,
};
use iced::{Center, Element, Fill, Font, Length, Point, Rectangle, Renderer, Theme};

use crate::app::{
    App, ConflictEdit, ContextMenu, GitMessage, HistoryState, MenuTarget, Message, RepoState,
    Selection, UiMessage, ViewMode,
};
use crate::git::{
    BlameFile, BranchInfo, ChangeKind, CommitInfo, Comparison, ConflictFile, ConflictSegment,
    ConflictSide, Diff, DiffLine, DiffLineKind, FileEntry, HeadInfo, ResetKind, StashInfo, TagInfo,
};

/// The application's custom dark [`iced::Theme`].
pub fn theme() -> iced::Theme {
    style::theme()
}

/// The top bar, the two-view body, and the Status Bar, on the window
/// background. The body switches between the Changes and History views.
pub fn root(app: &App) -> Element<'_, Message> {
    let left_body = match app.view {
        ViewMode::Changes => file_list(app),
        ViewMode::History => history_list(&app.history),
        ViewMode::Branches => branches_list(app),
        ViewMode::Stashes => stashes_list(app),
        ViewMode::Tags => tags_list(app),
    };
    let left = container(column![view_tabs(app), left_body])
        .style(style::panel)
        .width(Length::FillPortion(2))
        .height(Fill);

    let right: Element<Message> = match app.view {
        ViewMode::Changes => {
            // A conflicted file under manual edit shows the editor; else the
            // region-by-region resolver; else Blame if toggled on; otherwise the
            // diff.
            let selected_path = app.repo.selected.as_ref().map(|s| s.path.as_str());
            let top: Element<Message> = match (&app.repo.editing, &app.repo.conflict, &app.repo.blame)
            {
                (Some(edit), _, _) if selected_path == Some(edit.path.as_str()) => {
                    conflict_editor_view(edit)
                }
                (_, Some(file), _) if selected_path == Some(file.path.as_str()) => {
                    conflict_view(file)
                }
                (_, _, Some(file)) if selected_path == Some(file.path.as_str()) => {
                    blame_view(file)
                }
                _ => diff_view(&app.repo),
            };
            let diff = container(top)
                .style(style::panel)
                .width(Fill)
                .height(Fill);
            let commit = container(commit_panel(app)).style(style::panel).width(Fill);
            column![diff, commit]
                .spacing(12)
                .width(Length::FillPortion(3))
                .height(Fill)
                .into()
        }
        ViewMode::History => container(commit_detail_view(&app.history))
            .style(style::panel)
            .width(Length::FillPortion(3))
            .height(Fill)
            .into(),
        ViewMode::Branches => {
            let detail: Element<Message> = match &app.branches.comparison {
                Some(comparison) => comparison_view(comparison),
                None => branches_detail(app),
            };
            container(detail)
                .style(style::panel)
                .width(Length::FillPortion(3))
                .height(Fill)
                .into()
        }
        ViewMode::Stashes => container(stashes_detail(app))
            .style(style::panel)
            .width(Length::FillPortion(3))
            .height(Fill)
            .into(),
        ViewMode::Tags => container(tags_detail(app))
            .style(style::panel)
            .width(Length::FillPortion(3))
            .height(Fill)
            .into(),
    };

    let body = row![left, right].spacing(12).height(Fill);

    let base = container(column![top_bar(app), body, status_bar(app)].spacing(12))
        .style(style::app)
        .padding(12)
        .width(Fill)
        .height(Fill);

    // When a context menu is open, float it over everything at the pointer.
    // There is no full-window catcher: clicks elsewhere land on their target
    // and the update loop dismisses the menu in the same press.
    match &app.menu {
        Some(menu) => stack![base, menu_overlay(app, menu)].into(),
        None => base.into(),
    }
}

// ── Context Menu ───────────────────────────────────────────────────────────

/// Position the context menu at its anchor point using spacers, leaving the rest
/// of the layer transparent so clicks there fall through to the content beneath.
fn menu_overlay<'a>(app: &'a App, menu: &'a ContextMenu) -> Element<'a, Message> {
    let positioned = row![
        container(text("")).width(Length::Fixed(menu.at.x)),
        menu_panel(app, menu),
    ];
    column![
        container(text("")).height(Length::Fixed(menu.at.y)),
        positioned,
    ]
    .into()
}

/// One menu action: a borderless full-width button. Destructive items take a
/// confirming second press — the first arms the menu, the second fires.
fn menu_item<'a>(label: &str, message: Message) -> Element<'a, Message> {
    button(text(label.to_string()).size(13))
        .on_press(message)
        .width(Fill)
        .padding([6, 10])
        .style(style::ghost)
        .into()
}

/// A destructive menu action: armed shows "Confirm <label>?" (and fires the real
/// message); unarmed shows the label and only arms the menu.
fn menu_item_danger<'a>(label: &str, armed: bool, confirm: Message) -> Element<'a, Message> {
    let (text_label, message) = if armed {
        (format!("Confirm {}?", label.to_lowercase()), confirm)
    } else {
        (label.to_string(), Message::Ui(UiMessage::ArmMenu))
    };
    button(text(text_label).size(13))
        .on_press(message)
        .width(Fill)
        .padding([6, 10])
        .style(style::ghost_danger)
        .into()
}

/// A faint monospace header naming what the menu acts on.
fn menu_header<'a>(label: String) -> Element<'a, Message> {
    container(
        text(label)
            .size(11)
            .font(Font::MONOSPACE)
            .color(style::TEXT_FAINT),
    )
    .padding([4, 10])
    .into()
}

/// The context menu's contents, chosen by its target. Each action dispatches an
/// existing [`GitMessage`]; destructive ones route through the arm/confirm pair.
fn menu_panel<'a>(app: &'a App, menu: &'a ContextMenu) -> Element<'a, Message> {
    let mut items: Vec<Element<Message>> = Vec::new();

    match &menu.target {
        MenuTarget::Commit { sha, short_sha } => {
            items.push(menu_header(format!("Commit {short_sha}")));
            items.push(menu_item(
                "Cherry-pick",
                Message::Git(GitMessage::CherryPick(sha.clone())),
            ));
            items.push(menu_item(
                "Revert",
                Message::Git(GitMessage::Revert(sha.clone())),
            ));
            items.push(menu_item(
                "Reset (soft)",
                Message::Git(GitMessage::Reset {
                    sha: sha.clone(),
                    kind: ResetKind::Soft,
                }),
            ));
            items.push(menu_item(
                "Reset (mixed)",
                Message::Git(GitMessage::Reset {
                    sha: sha.clone(),
                    kind: ResetKind::Mixed,
                }),
            ));
            items.push(menu_item_danger(
                "Reset (hard)",
                menu.armed,
                Message::Git(GitMessage::Reset {
                    sha: sha.clone(),
                    kind: ResetKind::Hard,
                }),
            ));
        }
        MenuTarget::Branch { name, is_remote } => {
            items.push(menu_header(name.clone()));
            items.push(menu_item(
                "Checkout",
                Message::Git(GitMessage::Checkout(name.clone())),
            ));
            items.push(menu_item(
                "Merge into current",
                Message::Git(GitMessage::Merge(name.clone())),
            ));
            items.push(menu_item(
                "Compare with current",
                Message::Git(GitMessage::CompareWithCurrent(name.clone())),
            ));
            if !is_remote {
                items.push(menu_item_danger(
                    "Delete",
                    menu.armed,
                    Message::Git(GitMessage::DeleteBranch(name.clone())),
                ));
            }
        }
        MenuTarget::Stash { index } => {
            items.push(menu_header(format!("stash@{{{index}}}")));
            items.push(menu_item(
                "Apply",
                Message::Git(GitMessage::StashApply(*index)),
            ));
            items.push(menu_item("Pop", Message::Git(GitMessage::StashPop(*index))));
            items.push(menu_item_danger(
                "Drop",
                menu.armed,
                Message::Git(GitMessage::StashDrop(*index)),
            ));
        }
        MenuTarget::Tag { name } => {
            items.push(menu_header(name.clone()));
            if app.repo.head.has_remote {
                items.push(menu_item(
                    "Push",
                    Message::Git(GitMessage::PushTag(name.clone())),
                ));
            }
            items.push(menu_item_danger(
                "Delete",
                menu.armed,
                Message::Git(GitMessage::DeleteTag(name.clone())),
            ));
        }
    }

    container(column(items).spacing(2))
        .style(style::menu)
        .padding(6)
        .width(Length::Fixed(190.0))
        .into()
}

// ── View Tabs ──────────────────────────────────────────────────────────────

/// The Changes / History switch at the top of the left column.
fn view_tabs(app: &App) -> Element<'_, Message> {
    let changes_count = app.repo.unstaged.len() + app.repo.staged.len();
    let changes_label = if changes_count > 0 {
        format!("Changes ({changes_count})")
    } else {
        "Changes".to_string()
    };
    let stashes_count = app.stashes.stashes.len();
    let stashes_label = if stashes_count > 0 {
        format!("Stashes ({stashes_count})")
    } else {
        "Stashes".to_string()
    };

    let tags_count = app.tags.tags.len();
    let tags_label = if tags_count > 0 {
        format!("Tags ({tags_count})")
    } else {
        "Tags".to_string()
    };

    // Five tabs do not fit one row in the narrow left column, so they wrap onto
    // two rows. Each tab fills its row equally, keeping a tidy grid.
    let row1 = row![
        tab(&changes_label, ViewMode::Changes, app.view),
        tab("History", ViewMode::History, app.view),
        tab("Branches", ViewMode::Branches, app.view),
    ]
    .spacing(4);
    let row2 = row![
        tab(&stashes_label, ViewMode::Stashes, app.view),
        tab(&tags_label, ViewMode::Tags, app.view),
    ]
    .spacing(4);

    container(column![row1, row2].spacing(4))
        .padding([8, 10])
        .into()
}

fn tab<'a>(label: &str, target: ViewMode, current: ViewMode) -> Element<'a, Message> {
    button(text(label.to_string()).size(13).center())
        .on_press(Message::Ui(UiMessage::ShowView(target)))
        .width(Fill)
        .padding([6, 12])
        .style(style::tab(target == current))
        .into()
}

// ── Top Bar ──────────────────────────────────────────────────────────────

/// The application bar: the brand on the left, remote actions on the right.
/// Push and Pull live here so they're discoverable (also Ctrl+P / Ctrl+Shift+P).
fn top_bar(app: &App) -> Element<'_, Message> {
    let mark = container(text("⎇").size(16))
        .style(style::brand_mark)
        .width(Length::Fixed(30.0))
        .height(Length::Fixed(30.0))
        .center(Length::Fixed(30.0));

    // The brand's second line carries live repo context: the current branch and
    // its sync state with the Remote.
    let brand = row![
        mark,
        column![
            text("Git Client").size(15).color(style::TEXT),
            branch_summary(&app.repo.head),
        ]
        .spacing(2),
    ]
    .spacing(11)
    .align_y(Center);

    // Repo-wide actions live together on the right: Refresh, then the remote
    // operations. Push/Pull need a Remote and are disabled while one is already
    // in flight; their labels carry the ahead/behind counts. All share the exact
    // pill style of the File List toolbar.
    let head = &app.repo.head;
    let can_remote = head.has_remote && app.operation.is_none();
    let push_label = if head.ahead > 0 {
        format!("Push {}", head.ahead)
    } else {
        "Push".to_string()
    };
    let pull_label = if head.behind > 0 {
        format!("Pull {}", head.behind)
    } else {
        "Pull".to_string()
    };

    let refresh = pill("⟳", 14, "", GitMessage::Refresh, Tone::Normal, true);
    let fetch = pill("↧", 14, "Fetch", GitMessage::Fetch, Tone::Normal, can_remote);
    let pull = pill("↓", 14, &pull_label, GitMessage::Pull, Tone::Normal, can_remote);
    let push = pill("↑", 14, &push_label, GitMessage::Push, Tone::Normal, can_remote);

    container(
        row![brand, space::horizontal(), refresh, fetch, pull, push]
            .spacing(8)
            .align_y(Center),
    )
    .style(style::header)
    .padding([10, 14])
    .width(Fill)
    .into()
}

/// The current branch and its sync state with the Remote, shown under the
/// brand: "⎇ main  ↑2 ↓1", "✓ synced", "no upstream", or "detached HEAD".
fn branch_summary(head: &HeadInfo) -> Element<'static, Message> {
    let name = match &head.branch {
        Some(branch) => format!("⎇ {branch}"),
        None if head.detached => "detached HEAD".to_string(),
        None => "no branch".to_string(),
    };

    let mut parts = row![text(name).size(11).color(style::TEXT_MUTED)]
        .spacing(8)
        .align_y(Center);

    if head.upstream.is_some() {
        if head.ahead > 0 {
            parts = parts.push(text(format!("↑{}", head.ahead)).size(11).color(style::INFO));
        }
        if head.behind > 0 {
            parts = parts.push(
                text(format!("↓{}", head.behind))
                    .size(11)
                    .color(style::YELLOW),
            );
        }
        if head.ahead == 0 && head.behind == 0 {
            parts = parts.push(text("✓ synced").size(11).color(style::GREEN));
        }
    } else if head.branch.is_some() && head.has_remote {
        parts = parts.push(text("no upstream").size(11).color(style::TEXT_FAINT));
    }

    parts.into()
}

// ── File List ────────────────────────────────────────────────────────────

/// The left column: an action toolbar, then Unstaged files and Staged files.
/// Checkboxes pick the action targets; clicking a name shows its Diff.
fn file_list(app: &App) -> Element<'_, Message> {
    let repo = &app.repo;
    let mut items: Vec<Element<Message>> = Vec::new();

    items.push(action_toolbar(app));

    // Merge conflicts come first: they block committing until resolved.
    if !repo.conflicted.is_empty() {
        items.push(conflicts_header(repo.conflicted.len()));
        for entry in &repo.conflicted {
            items.push(conflict_row(app, entry));
        }
        items.push(gap(10.0));
    }

    items.push(section_header(app, "Unstaged", style::YELLOW, false));
    if repo.unstaged.is_empty() {
        items.push(placeholder("Working tree is clean"));
    } else {
        push_tree(app, &repo.unstaged, false, &mut items);
    }

    items.push(gap(10.0));
    items.push(section_header(app, "Staged", style::GREEN, true));
    if repo.staged.is_empty() {
        items.push(placeholder("Nothing staged"));
    } else {
        push_tree(app, &repo.staged, true, &mut items);
    }

    scrollable(column(items).spacing(3).padding(12))
        .height(Fill)
        .into()
}

/// Render a section's files as a collapsible directory tree, appending one row
/// per visible node (directories and the files under expanded ones) to `items`.
fn push_tree<'a>(
    app: &'a App,
    entries: &'a [FileEntry],
    staged: bool,
    items: &mut Vec<Element<'a, Message>>,
) {
    for node in tree::build(entries, |entry| entry.path.as_str()) {
        push_node(app, node, staged, 0, items);
    }
}

fn push_node<'a>(
    app: &'a App,
    node: tree::Node<'a, FileEntry>,
    staged: bool,
    depth: usize,
    items: &mut Vec<Element<'a, Message>>,
) {
    match node {
        tree::Node::Leaf(entry) => items.push(file_row(app, entry, staged, depth)),
        tree::Node::Dir {
            name,
            path,
            children,
        } => {
            let collapsed = app.dir_collapsed(staged, &path);
            let paths = tree::leaf_paths(&children, |entry| entry.path.as_str());
            let all_checked = paths.iter().all(|p| {
                app.checked.contains(&Selection {
                    path: p.clone(),
                    staged,
                })
            });
            items.push(dir_row(&name, &path, staged, depth, collapsed, paths, all_checked));
            if !collapsed {
                for child in children {
                    push_node(app, child, staged, depth + 1, items);
                }
            }
        }
    }
}

/// One directory node: a "select all under here" checkbox, then an indented,
/// clickable row (chevron + folder name + file count) that toggles the directory
/// open or closed. The checkbox sits outside that button so it stages the folder
/// without also collapsing it.
fn dir_row<'a>(
    name: &str,
    path: &str,
    staged: bool,
    depth: usize,
    collapsed: bool,
    paths: Vec<String>,
    all_checked: bool,
) -> Element<'a, Message> {
    let count = paths.len();
    let toggle_paths = paths.clone();
    let check = checkbox(all_checked)
        .on_toggle(move |_| {
            Message::Ui(UiMessage::ToggleDirChecked {
                staged,
                paths: toggle_paths.clone(),
            })
        })
        .size(16)
        .style(style::check);

    let chevron = container(
        text(if collapsed { "▸" } else { "▾" })
            .size(10)
            .color(style::TEXT_FAINT),
    )
    .width(Length::Fixed(16.0))
    .center_x(Length::Fixed(16.0));

    let toggle = button(
        row![
            chevron,
            text(name.to_string()).size(14).color(style::TEXT_MUTED),
            text(count.to_string()).size(11).color(style::TEXT_FAINT),
        ]
        .spacing(8)
        .align_y(Center),
    )
    .on_press(Message::Ui(UiMessage::ToggleDir {
        staged,
        path: path.to_string(),
    }))
    .width(Fill)
    .padding([6, 8])
    .style(style::file_item(false));

    row![tree_indent(depth), check, toggle]
        .spacing(8)
        .align_y(Center)
        .into()
}

/// A fixed-width spacer that indents a tree row to its `depth`.
fn tree_indent<'a>(depth: usize) -> Element<'a, Message> {
    container(text(""))
        .width(Length::Fixed(depth as f32 * 16.0))
        .into()
}

/// The bulk action bar. Each button acts on the checked files of its side, or
/// on all of them when nothing is checked. The buttons share the bordered
/// "pill" look of Pull/Push in the top bar (see [`pill`]).
fn action_toolbar(app: &App) -> Element<'_, Message> {
    let unstaged_checked = app.checked.iter().filter(|s| !s.staged).count();
    let staged_checked = app.checked.iter().filter(|s| s.staged).count();

    let stage = pill(
        "+",
        15,
        &count_label("Stage", unstaged_checked),
        GitMessage::StageChecked,
        Tone::Normal,
        !app.repo.unstaged.is_empty(),
    );
    let unstage = pill(
        "−",
        15,
        &count_label("Unstage", staged_checked),
        GitMessage::UnstageChecked,
        Tone::Normal,
        !app.repo.staged.is_empty(),
    );
    let discard_label = if app.discard_armed {
        "Confirm?".to_string()
    } else {
        count_label("Discard", unstaged_checked)
    };
    let discard = pill(
        "✕",
        14,
        &discard_label,
        GitMessage::DiscardChecked,
        Tone::Danger,
        !app.repo.unstaged.is_empty(),
    );

    // Stash acts on the checked files (deduped across both sides), or on
    // everything when nothing is checked — mirroring the other bulk actions.
    let checked_paths: std::collections::HashSet<&str> =
        app.checked.iter().map(|s| s.path.as_str()).collect();
    let has_changes = !app.repo.unstaged.is_empty() || !app.repo.staged.is_empty();
    let stash_label = match checked_paths.len() {
        0 => "Stash all".to_string(),
        n => format!("Stash ({n})"),
    };
    let stash = pill("", 15, &stash_label, GitMessage::Stash, Tone::Normal, has_changes);

    // Two rows: the staging actions, then Stash — they do not all fit on one.
    let row1 = row![stage, unstage, discard].spacing(6).align_y(Center);
    container(column![row1, row![stash]].spacing(6))
        .padding([2, 0])
        .into()
}

/// The bulk-action verb alone when nothing is checked, otherwise e.g.
/// "Stage (3)". (Acting with nothing checked targets the whole section.)
fn count_label(verb: &str, checked: usize) -> String {
    if checked == 0 {
        verb.to_string()
    } else {
        format!("{verb} ({checked})")
    }
}

enum Tone {
    Normal,
    Danger,
}

/// A bordered command button: an optional leading icon glyph and a label, in
/// the shared pill style. Used by both the File List toolbar and the top bar so
/// every command reads as one consistent control.
fn pill<'a>(
    icon: &str,
    icon_size: u16,
    label: &str,
    message: GitMessage,
    tone: Tone,
    enabled: bool,
) -> Element<'a, Message> {
    let style = match tone {
        Tone::Normal => style::secondary as fn(&iced::Theme, button::Status) -> button::Style,
        Tone::Danger => style::secondary_danger,
    };

    let mut content = row![].spacing(7).align_y(Center);
    if !icon.is_empty() {
        content = content.push(text(icon.to_string()).size(icon_size as f32));
    }
    if !label.is_empty() {
        content = content.push(text(label.to_string()).size(13));
    }

    button(content)
        .on_press_maybe(enabled.then_some(Message::Git(message)))
        .padding([7, 11])
        .style(style)
        .into()
}

/// A section heading: a "select all" checkbox, the label, and a count chip.
fn section_header<'a>(
    app: &App,
    label: &str,
    color: iced::Color,
    staged: bool,
) -> Element<'a, Message> {
    let list = if staged {
        &app.repo.staged
    } else {
        &app.repo.unstaged
    };
    let all_checked = !list.is_empty()
        && list.iter().all(|entry| {
            app.checked.contains(&Selection {
                path: entry.path.clone(),
                staged,
            })
        });

    let select_all = checkbox(all_checked)
        .on_toggle_maybe(
            (!list.is_empty()).then_some(move |_| Message::Ui(UiMessage::ToggleSection { staged })),
        )
        .size(16)
        .style(style::check);

    let chip = container(text(list.len().to_string()).size(11).color(color))
        .style(style::chip(color))
        .padding([1, 7]);

    container(
        row![
            select_all,
            text(label.to_uppercase()).size(11).color(style::TEXT_MUTED),
            chip,
        ]
        .spacing(10)
        .align_y(Center),
    )
    // Zero horizontal padding so the checkbox shares the File List's left edge
    // with the file-row checkboxes below it (they sit at the column's inset).
    .padding([6, 0])
    .into()
}

fn placeholder<'a>(label: &str) -> Element<'a, Message> {
    container(text(label.to_string()).size(13).color(style::TEXT_FAINT))
        .padding([4, 30])
        .into()
}

/// One file: a checkbox (action target), a colored status dot, and the
/// filename (clicking it shows the Diff).
fn file_row<'a>(app: &App, entry: &FileEntry, staged: bool, depth: usize) -> Element<'a, Message> {
    let item = Selection {
        path: entry.path.clone(),
        staged,
    };
    // Show just the file name; its directory is conveyed by the tree.
    let leaf = entry.path.rsplit('/').next().unwrap_or(&entry.path).to_string();
    let active = app.repo.selected.as_ref() == Some(&item);
    let is_checked = app.checked.contains(&item);

    // Own the path so the toggle closure doesn't borrow `entry`.
    let toggle_path = entry.path.clone();
    let check = checkbox(is_checked)
        .on_toggle(move |_| {
            Message::Ui(UiMessage::ToggleChecked {
                path: toggle_path.clone(),
                staged,
            })
        })
        .size(16)
        .style(style::check);

    // A small colored status dot reads as an icon and avoids a confusing "?"
    // glyph for untracked files. Color encodes the change (see `badge_color`).
    let dot = container(text(""))
        .width(Length::Fixed(9.0))
        .height(Length::Fixed(9.0))
        .style(style::dot(badge_color(entry.change)));

    // A thin accent bar on the left edge marks the active row. It keeps the
    // SAME fixed footprint whether active or not (only its color changes), so
    // selecting a file never resizes the row or shifts the ones below it.
    let bar = container(text(""))
        .width(Length::Fixed(3.0))
        .height(Length::Fixed(16.0))
        .style(style::selection_bar(active));

    let name = button(
        row![bar, dot, text(leaf).size(14)]
            .spacing(10)
            .align_y(Center),
    )
    .on_press(Message::Ui(UiMessage::FileSelected {
        path: entry.path.clone(),
        staged,
    }))
    .width(Fill)
    .padding([6, 8])
    .style(style::file_item(active));

    row![tree_indent(depth), check, name]
        .spacing(8)
        .align_y(Center)
        .into()
}

fn badge_color(change: ChangeKind) -> iced::Color {
    match change {
        ChangeKind::Added => style::GREEN,
        ChangeKind::Untracked => style::YELLOW,
        ChangeKind::Deleted | ChangeKind::Conflicted => style::RED,
        ChangeKind::Modified | ChangeKind::Renamed | ChangeKind::Typechange => style::INFO,
    }
}

/// The Conflicts section heading: a count chip and an "Abort merge" escape hatch.
fn conflicts_header<'a>(count: usize) -> Element<'a, Message> {
    let chip = container(text(count.to_string()).size(11).color(style::RED))
        .style(style::chip(style::RED))
        .padding([1, 7]);
    let abort = button(text("Abort merge").size(11))
        .on_press(Message::Git(GitMessage::AbortMerge))
        .padding([2, 9])
        .style(style::secondary_danger);

    container(
        row![
            text("CONFLICTS").size(11).color(style::RED),
            chip,
            space::horizontal(),
            abort,
        ]
        .spacing(10)
        .align_y(Center),
    )
    .padding([6, 0])
    .into()
}

/// One conflicted file: a click target that opens the region-by-region resolver
/// in the right panel.
fn conflict_row<'a>(app: &App, entry: &FileEntry) -> Element<'a, Message> {
    let path = entry.path.clone();
    let active = app.repo.selected.as_ref().is_some_and(|s| !s.staged && s.path == path);
    let leaf = path.rsplit('/').next().unwrap_or(&path).to_string();

    let dot = container(text(""))
        .width(Length::Fixed(9.0))
        .height(Length::Fixed(9.0))
        .style(style::dot(style::RED));
    let name = button(row![dot, text(leaf).size(14)].spacing(10).align_y(Center))
        .on_press(Message::Ui(UiMessage::FileSelected {
            path: path.clone(),
            staged: false,
        }))
        .width(Fill)
        .padding([6, 8])
        .style(style::file_item(active));

    name.into()
}

/// The region-by-region conflict resolver for the selected file: a header with
/// whole-file shortcuts (Ours / Theirs / Both), then each conflict region with
/// its own Ours / Theirs / Both, and the agreed context in between.
fn conflict_view(file: &ConflictFile) -> Element<'_, Message> {
    let path = file.path.clone();
    let region_count = file
        .segments
        .iter()
        .filter(|s| matches!(s, ConflictSegment::Conflict { .. }))
        .count();

    // Whole-file shortcut: resolve every region the same way at once.
    let all = |label: &str, side: ConflictSide| {
        button(text(label.to_string()).size(11))
            .on_press(Message::Git(GitMessage::ResolveConflict {
                path: path.clone(),
                side,
            }))
            .padding([3, 9])
            .style(style::ghost)
    };
    let header = container(
        row![
            text(path.clone())
                .size(13)
                .font(Font::MONOSPACE)
                .color(style::TEXT),
            button(text("Edit").size(11))
                .on_press(Message::Ui(UiMessage::EditConflict))
                .padding([3, 9])
                .style(style::ghost),
            space::horizontal(),
            text("Whole file:").size(11).color(style::TEXT_FAINT),
            all("Ours", ConflictSide::Ours),
            all("Theirs", ConflictSide::Theirs),
            all("Both", ConflictSide::Both),
        ]
        .spacing(8)
        .align_y(Center),
    )
    .style(style::diff_header)
    .padding([7, 12])
    .width(Fill);

    // Body: context blocks plus a card per conflict region (indexed in order).
    let mut rows: Vec<Element<Message>> = Vec::new();
    let mut region = 0;
    for segment in &file.segments {
        match segment {
            ConflictSegment::Context(lines) => {
                for line in lines {
                    rows.push(conflict_line(line, style::TEXT_FAINT, None));
                }
            }
            ConflictSegment::Conflict { ours, theirs } => {
                rows.push(conflict_region(&path, region, region_count, ours, theirs));
                region += 1;
            }
        }
    }
    let body = scrollable(column(rows).spacing(1).padding([8, 4]))
        .height(Fill)
        .width(Fill);

    column![header, body].spacing(10).into()
}

/// The manual conflict editor: a free-form text editor over the conflicted file's
/// raw content (markers and all), for merges that ours/theirs/both can't express.
/// Saving writes the buffer back and, if no markers remain, stages the file.
fn conflict_editor_view(edit: &ConflictEdit) -> Element<'_, Message> {
    let header = container(
        row![
            text(edit.path.clone())
                .size(13)
                .font(Font::MONOSPACE)
                .color(style::TEXT),
            space::horizontal(),
            text("Manual edit").size(11).color(style::TEXT_FAINT),
            button(text("Cancel").size(11))
                .on_press(Message::Ui(UiMessage::CancelConflictEdit))
                .padding([3, 9])
                .style(style::ghost),
            button(text("Save").size(11))
                .on_press(Message::Git(GitMessage::SaveConflictEdit))
                .padding([3, 9])
                .style(style::primary),
        ]
        .spacing(8)
        .align_y(Center),
    )
    .style(style::diff_header)
    .padding([7, 12])
    .width(Fill);

    let editor = text_editor(&edit.content)
        .on_action(|action| Message::Ui(UiMessage::ConflictEdited(action)))
        .font(Font::MONOSPACE)
        .height(Fill)
        .padding(8);

    column![header, editor].spacing(10).height(Fill).into()
}

/// One conflict region as a card: a labelled bar with per-region Ours / Theirs /
/// Both, then the two sides tinted (ours green, theirs blue).
fn conflict_region<'a>(
    path: &str,
    index: usize,
    total: usize,
    ours: &'a [String],
    theirs: &'a [String],
) -> Element<'a, Message> {
    let pick = |label: &str, side: ConflictSide| {
        button(text(label.to_string()).size(11))
            .on_press(Message::Git(GitMessage::ResolveHunk {
                path: path.to_string(),
                index,
                side,
            }))
            .padding([2, 8])
            .style(style::ghost)
    };

    let bar = container(
        row![
            text(format!("Conflict {} of {}", index + 1, total))
                .size(11)
                .color(style::YELLOW),
            space::horizontal(),
            pick("Ours", ConflictSide::Ours),
            pick("Theirs", ConflictSide::Theirs),
            pick("Both", ConflictSide::Both),
        ]
        .spacing(6)
        .align_y(Center),
    )
    .style(style::diff_row(Some(style::INFO_BG)))
    .padding([3, 10])
    .width(Fill);

    let mut lines: Vec<Element<Message>> = vec![bar.into()];
    for line in ours {
        lines.push(conflict_line(line, style::GREEN, Some(style::GREEN_BG)));
    }
    for line in theirs {
        lines.push(conflict_line(line, style::INFO, Some(style::INFO_BG)));
    }
    column(lines).into()
}

/// One rendered line inside the conflict view: monospace content over an optional
/// full-width tint, with a colored side marker.
fn conflict_line<'a>(
    content: &str,
    color: iced::Color,
    tint: Option<iced::Color>,
) -> Element<'a, Message> {
    container(
        text(content.to_string())
            .font(Font::MONOSPACE)
            .size(13)
            .color(color),
    )
    .style(style::diff_row(tint))
    .width(Fill)
    .padding([1, 12])
    .into()
}

// ── History List ─────────────────────────────────────────────────────────

/// Fixed height of a commit row, so the graph cells stack edge-to-edge and their
/// lane lines join across rows.
const COMMIT_ROW_H: f32 = 46.0;
/// Horizontal pitch between graph lanes.
const LANE_W: f32 = 16.0;

/// The left column in the History view: the recent Commits as a graph, newest
/// first. The graph lanes are laid out once, then each row draws its own cell.
fn history_list(history: &HistoryState) -> Element<'_, Message> {
    let search = text_input("Search message, author, file…", &history.query)
        .on_input(|value| Message::Ui(UiMessage::HistoryQueryChanged(value)))
        .on_submit(Message::Ui(UiMessage::SearchHistory))
        .padding([7, 10])
        .size(13)
        .style(style::input);
    let search = container(search).padding([8, 10]);

    // In filtered mode (search or file history) the matches are a flat list — the
    // commit graph only makes sense over the full, unfiltered history.
    let body: Element<Message> = match &history.results {
        Some(results) => {
            history_results(results, history.selected.as_deref(), history.file.as_deref())
        }
        None => history_graph(history),
    };

    column![search, body].into()
}

/// The flat list of filtered Commits, newest first. The header names what the
/// filter is — a file's history (`file` set) or a text search — and offers a
/// "Show all" escape back to the full graph.
fn history_results<'a>(
    results: &'a [CommitInfo],
    selected: Option<&str>,
    file: Option<&str>,
) -> Element<'a, Message> {
    let label: Element<Message> = match file {
        Some(path) => row![
            text("FILE").size(11).color(style::TEXT_FAINT),
            text(path.to_string())
                .size(11)
                .font(Font::MONOSPACE)
                .color(style::TEXT_MUTED),
        ]
        .spacing(8)
        .into(),
        None => branch_section_label("MATCHES", results.len()),
    };
    let header = row![
        label,
        space::horizontal(),
        button(text("Show all").size(11))
            .on_press(Message::Ui(UiMessage::ClearHistoryFilter))
            .padding([2, 8])
            .style(style::ghost),
    ]
    .spacing(8)
    .align_y(Center);

    let empty = if file.is_some() {
        "No commits touch this file"
    } else {
        "No matching commits"
    };
    let list: Element<Message> = if results.is_empty() {
        container(placeholder(empty)).height(Fill).into()
    } else {
        let rows: Vec<Element<Message>> = results
            .iter()
            .map(|c| commit_row_flat(c, selected))
            .collect();
        scrollable(column(rows).spacing(3))
            .height(Fill)
            .width(Fill)
            .into()
    };

    column![container(header).padding([4, 4]), list]
        .spacing(4)
        .padding(12)
        .into()
}

/// The full history as a commit graph, newest first.
fn history_graph(history: &HistoryState) -> Element<'_, Message> {
    if history.commits.is_empty() {
        return container(placeholder("No commits yet")).height(Fill).into();
    }

    let nodes: Vec<graph::Commit> = history
        .commits
        .iter()
        .map(|commit| graph::Commit {
            sha: &commit.sha,
            parents: &commit.parents,
        })
        .collect();
    let layout = graph::layout(&nodes);
    // One width for every cell so the commit text lines up in a single column.
    let lanes = layout.iter().map(|row| row.lanes).max().unwrap_or(1).max(1);

    let rows: Vec<Element<Message>> = history
        .commits
        .iter()
        .zip(layout)
        .map(|(commit, row)| commit_row(commit, history.selected.as_deref(), row, lanes))
        .collect();

    // No inter-row spacing: rows touch so the lane lines are continuous.
    scrollable(column(rows).padding(12)).height(Fill).into()
}

/// One Commit in the History list: its graph cell, then short SHA, summary, and
/// author · time.
fn commit_row<'a>(
    commit: &CommitInfo,
    selected: Option<&str>,
    row: graph::Row,
    lanes: usize,
) -> Element<'a, Message> {
    let active = selected == Some(commit.sha.as_str());

    let cell = canvas(GraphCell { row })
        .width(Length::Fixed(lanes as f32 * LANE_W))
        .height(Length::Fixed(COMMIT_ROW_H));

    let meta = format!("{} · {}", commit.author, relative_time(commit.time));
    let body = column![
        row![
            text(commit.short_sha.clone())
                .size(12)
                .font(Font::MONOSPACE)
                .color(style::INFO),
            text(commit.summary.clone()).size(13).color(style::TEXT),
        ]
        .spacing(10)
        .align_y(Center),
        text(meta).size(11).color(style::TEXT_FAINT),
    ]
    .spacing(2);

    let body = button(body)
        .on_press(Message::Ui(UiMessage::CommitSelected(commit.sha.clone())))
        .width(Fill)
        .padding([7, 8])
        .style(style::file_item(active));

    // Right-click anywhere on the row opens its context menu (Reset / Revert).
    let row = container(row![cell, body].spacing(6).align_y(Center))
        .height(Length::Fixed(COMMIT_ROW_H));
    mouse_area(row)
        .on_right_press(Message::Ui(UiMessage::OpenMenu(MenuTarget::Commit {
            sha: commit.sha.clone(),
            short_sha: commit.short_sha.clone(),
        })))
        .into()
}

/// One search-result Commit: like [`commit_row`] but with no graph cell (a
/// filtered subset has no meaningful graph). Same selection and context menu.
fn commit_row_flat<'a>(commit: &CommitInfo, selected: Option<&str>) -> Element<'a, Message> {
    let active = selected == Some(commit.sha.as_str());

    let meta = format!("{} · {}", commit.author, relative_time(commit.time));
    let body = column![
        row![
            text(commit.short_sha.clone())
                .size(12)
                .font(Font::MONOSPACE)
                .color(style::INFO),
            text(commit.summary.clone()).size(13).color(style::TEXT),
        ]
        .spacing(10)
        .align_y(Center),
        text(meta).size(11).color(style::TEXT_FAINT),
    ]
    .spacing(2);

    let body = button(body)
        .on_press(Message::Ui(UiMessage::CommitSelected(commit.sha.clone())))
        .width(Fill)
        .padding([7, 8])
        .style(style::file_item(active));

    mouse_area(body)
        .on_right_press(Message::Ui(UiMessage::OpenMenu(MenuTarget::Commit {
            sha: commit.sha.clone(),
            short_sha: commit.short_sha.clone(),
        })))
        .into()
}

/// Draws one commit row's graph cell: the lane lines entering from the top and
/// leaving to the bottom, plus the commit's node.
struct GraphCell {
    row: graph::Row,
}

impl canvas::Program<Message> for GraphCell {
    type State = ();

    fn draw(
        &self,
        _state: &(),
        renderer: &Renderer,
        _theme: &Theme,
        bounds: Rectangle,
        _cursor: iced::mouse::Cursor,
    ) -> Vec<canvas::Geometry> {
        let mut frame = canvas::Frame::new(renderer, bounds.size());
        let center_y = bounds.height / 2.0;
        let lane_x = |lane: usize| lane as f32 * LANE_W + LANE_W / 2.0;
        let stroke = |color: usize| {
            canvas::Stroke::default()
                .with_width(1.6)
                .with_color(lane_color(color))
        };

        for edge in &self.row.top {
            let path = canvas::Path::new(|builder| {
                builder.move_to(Point::new(lane_x(edge.from), 0.0));
                builder.line_to(Point::new(lane_x(edge.to), center_y));
            });
            frame.stroke(&path, stroke(edge.color));
        }
        for edge in &self.row.bottom {
            let path = canvas::Path::new(|builder| {
                builder.move_to(Point::new(lane_x(edge.from), center_y));
                builder.line_to(Point::new(lane_x(edge.to), bounds.height));
            });
            frame.stroke(&path, stroke(edge.color));
        }

        let node = canvas::Path::circle(Point::new(lane_x(self.row.node_lane), center_y), 4.0);
        frame.fill(&node, lane_color(self.row.node_color));

        vec![frame.into_geometry()]
    }
}

/// The color for a graph lane, cycling through the lane palette.
fn lane_color(index: usize) -> iced::Color {
    style::LANE_COLORS[index % style::LANE_COLORS.len()]
}

// ── Branches ─────────────────────────────────────────────────────────────

/// The left column in the Branches view: a new-branch creator, then the local
/// branches with the current one marked.
fn branches_list(app: &App) -> Element<'_, Message> {
    let mut items: Vec<Element<Message>> = Vec::new();

    let can_create = !app.branches.new_name.trim().is_empty();
    let input = text_input("New branch name", &app.branches.new_name)
        .on_input(|value| Message::Ui(UiMessage::NewBranchNameChanged(value)))
        .on_submit(Message::Git(GitMessage::CreateBranch))
        .padding([7, 10])
        .size(13)
        .style(style::input);
    let create = pill("+", 15, "Create", GitMessage::CreateBranch, Tone::Normal, can_create);
    items.push(row![input, create].spacing(6).align_y(Center).into());

    let (locals, remotes): (Vec<_>, Vec<_>) = app
        .branches
        .branches
        .iter()
        .partition(|branch| !branch.is_remote);

    // Branches absent from the Remote (no resolvable upstream and no matching
    // `origin/<name>`) can be pruned in bulk.
    let prunable = locals
        .iter()
        .filter(|branch| {
            !branch.is_head
                && branch.upstream.is_none()
                && !remotes
                    .iter()
                    .any(|remote| remote.name == format!("origin/{}", branch.name))
        })
        .count();

    items.push(gap(8.0));
    items.push(local_header(locals.len(), prunable, app.branches.prune_armed));
    if locals.is_empty() {
        items.push(placeholder("No branches yet"));
    } else {
        push_branch_tree(app, &locals, false, &mut items);
    }

    if !remotes.is_empty() {
        items.push(gap(10.0));
        items.push(branch_section_label("REMOTE", remotes.len()));
        push_branch_tree(app, &remotes, true, &mut items);
    }

    scrollable(column(items).spacing(4).padding(12))
        .height(Fill)
        .into()
}

/// A small uppercase section heading for the Branches list.
fn branch_section_label<'a>(label: &str, count: usize) -> Element<'a, Message> {
    text(format!("{label} ({count})"))
        .size(11)
        .color(style::TEXT_MUTED)
        .into()
}

/// The LOCAL section heading, with a Prune action on the right when there are
/// branches absent from the Remote. The first press arms it ("Delete N?"), the
/// second performs the cleanup.
fn local_header<'a>(count: usize, prunable: usize, armed: bool) -> Element<'a, Message> {
    let mut header = row![branch_section_label("LOCAL", count)]
        .spacing(8)
        .align_y(Center);

    if prunable > 0 {
        let label = if armed {
            format!("Delete {prunable}?")
        } else {
            format!("Prune {prunable}")
        };
        let prune = button(text(label).size(11))
            .on_press(Message::Git(GitMessage::PruneBranches))
            .padding([2, 9])
            .style(style::secondary_danger);
        header = header.push(space::horizontal()).push(prune);
    }

    container(header).width(Fill).into()
}

/// Render a section's branches as a collapsible folder tree, splitting each name
/// on `/` (e.g. `feature/login`). Remote names have their remote prefix
/// (`origin/`) stripped, since the REMOTE heading already conveys it.
fn push_branch_tree<'a>(
    app: &'a App,
    branches: &[&'a BranchInfo],
    remote: bool,
    items: &mut Vec<Element<'a, Message>>,
) {
    let path_of = |branch: &&'a BranchInfo| -> &str {
        let name = branch.name.as_str();
        if remote {
            name.split_once('/').map_or(name, |(_, rest)| rest)
        } else {
            name
        }
    };
    for node in tree::build(branches, path_of) {
        push_branch_node(app, node, remote, 0, items);
    }
}

fn push_branch_node<'a>(
    app: &'a App,
    node: tree::Node<'_, &'a BranchInfo>,
    remote: bool,
    depth: usize,
    items: &mut Vec<Element<'a, Message>>,
) {
    match node {
        tree::Node::Leaf(branch) => {
            let branch: &'a BranchInfo = branch;
            items.push(branch_row(branch, depth));
        }
        tree::Node::Dir {
            name,
            path,
            children,
        } => {
            let collapsed = app.branch_dir_collapsed(remote, &path);
            items.push(branch_dir_row(&name, &path, remote, depth, collapsed));
            if !collapsed {
                for child in children {
                    push_branch_node(app, child, remote, depth + 1, items);
                }
            }
        }
    }
}

/// One folder in the Branch Tree: an indented, clickable row (chevron + folder
/// name) that collapses or expands the branches grouped under it.
fn branch_dir_row<'a>(
    name: &str,
    path: &str,
    remote: bool,
    depth: usize,
    collapsed: bool,
) -> Element<'a, Message> {
    let chevron = container(
        text(if collapsed { "▸" } else { "▾" })
            .size(10)
            .color(style::TEXT_FAINT),
    )
    .width(Length::Fixed(16.0))
    .center_x(Length::Fixed(16.0));

    let toggle = button(
        row![
            chevron,
            text(name.to_string()).size(14).color(style::TEXT_MUTED),
        ]
        .spacing(8)
        .align_y(Center),
    )
    .on_press(Message::Ui(UiMessage::ToggleBranchDir {
        remote,
        path: path.to_string(),
    }))
    .width(Fill)
    .padding([6, 8])
    .style(style::file_item(false));

    row![tree_indent(depth), toggle]
        .spacing(8)
        .align_y(Center)
        .into()
}

/// One branch leaf: a current-branch marker, its (leaf) name, and its sync
/// state. Clicking switches to it (except the current one); right-clicking opens
/// its actions (Checkout / Merge / Delete).
fn branch_row<'a>(branch: &BranchInfo, depth: usize) -> Element<'a, Message> {
    let bar = container(text(""))
        .width(Length::Fixed(3.0))
        .height(Length::Fixed(16.0))
        .style(style::selection_bar(branch.is_head));

    let icon_color = if branch.is_head {
        style::ACCENT
    } else {
        style::TEXT_FAINT
    };
    let name_color = if branch.is_head {
        style::TEXT
    } else {
        style::TEXT_MUTED
    };

    // Show only the leaf segment; the folder rows above convey the path.
    let leaf = branch.name.rsplit('/').next().unwrap_or(&branch.name).to_string();
    let mut label = row![
        bar,
        text("⎇").size(13).color(icon_color),
        text(leaf).size(14).color(name_color),
    ]
    .spacing(10)
    .align_y(Center);

    if branch.ahead > 0 {
        label = label.push(text(format!("↑{}", branch.ahead)).size(11).color(style::GREEN));
    }
    if branch.behind > 0 {
        label = label.push(text(format!("↓{}", branch.behind)).size(11).color(style::YELLOW));
    }
    // A local branch with no upstream is marked "local"; remote branches carry
    // their remote name already, so they need no tag.
    if !branch.is_remote && branch.upstream.is_none() {
        label = label.push(text("local").size(10).color(style::TEXT_FAINT));
    }

    // The whole rectangle switches to the branch (except the current one, which
    // is not a switch target). Switching to a remote branch creates a local
    // tracking branch (handled in the worker).
    let select = button(label)
        .on_press_maybe(
            (!branch.is_head).then(|| Message::Git(GitMessage::Checkout(branch.name.clone()))),
        )
        .width(Fill)
        .padding([6, 8])
        .style(style::file_item(branch.is_head));

    // The current branch is only ever a "you are here" marker — no actions
    // (you can't checkout, merge, or delete the branch you're on).
    if branch.is_head {
        return row![tree_indent(depth), select]
            .spacing(8)
            .align_y(Center)
            .into();
    }

    // Any other branch carries its actions on right-click (Checkout / Merge /
    // Delete) rather than inline buttons.
    let row = row![tree_indent(depth), select].spacing(8).align_y(Center);
    mouse_area(row)
        .on_right_press(Message::Ui(UiMessage::OpenMenu(MenuTarget::Branch {
            name: branch.name.clone(),
            is_remote: branch.is_remote,
        })))
        .into()
}

/// The right panel in the Branches view: a short summary of the current branch
/// and a hint on how to use the list.
fn branches_detail(app: &App) -> Element<'_, Message> {
    let head = &app.repo.head;
    let title = match &head.branch {
        Some(branch) => format!("On branch {branch}"),
        None => "Detached HEAD".to_string(),
    };

    let mut lines = column![text(title).size(16).color(style::TEXT)].spacing(8);
    if let Some(upstream) = &head.upstream {
        lines = lines.push(
            text(format!("Tracking {upstream}"))
                .size(13)
                .color(style::TEXT_MUTED),
        );
    }
    lines = lines.push(
        text("Click a branch to switch · right-click for Merge / Delete")
            .size(12)
            .color(style::TEXT_FAINT),
    );

    container(lines.align_x(Center))
        .center(Fill)
        .padding(12)
        .into()
}

/// The branch comparison: a header naming both refs (with add/remove counts and a
/// Close button) over the combined diff `base → target`.
fn comparison_view(comparison: &Comparison) -> Element<'_, Message> {
    let added = comparison
        .lines
        .iter()
        .filter(|l| matches!(l.kind, DiffLineKind::Addition))
        .count();
    let removed = comparison
        .lines
        .iter()
        .filter(|l| matches!(l.kind, DiffLineKind::Deletion))
        .count();

    let header = container(
        row![
            text(comparison.base.clone())
                .size(13)
                .font(Font::MONOSPACE)
                .color(style::YELLOW),
            text("→").size(13).color(style::TEXT_FAINT),
            text(comparison.target.clone())
                .size(13)
                .font(Font::MONOSPACE)
                .color(style::GREEN),
            space::horizontal(),
            text(format!("+{added}")).size(12).color(style::GREEN),
            text(format!("−{removed}")).size(12).color(style::RED),
            button(text("Close").size(11))
                .on_press(Message::Ui(UiMessage::CloseComparison))
                .padding([3, 9])
                .style(style::ghost),
        ]
        .spacing(10)
        .align_y(Center),
    )
    .style(style::diff_header)
    .padding([7, 12])
    .width(Fill);

    if comparison.lines.is_empty() {
        let empty = container(
            text("No differences between these refs")
                .size(14)
                .color(style::TEXT_FAINT),
        )
        .center(Fill);
        return column![header, empty].spacing(10).into();
    }

    column![header, diff_body(&comparison.lines)]
        .spacing(10)
        .padding([12, 0])
        .into()
}

// ── Stashes List ──────────────────────────────────────────────────────────

/// The left column in the Stashes view: a "stash all changes" field and button
/// at the top, then the saved stashes, newest first. (Stashing only selected
/// files is done from the Changes view, where the checkboxes live.)
fn stashes_list(app: &App) -> Element<'_, Message> {
    let mut items: Vec<Element<Message>> = Vec::new();

    let has_changes = !app.repo.unstaged.is_empty() || !app.repo.staged.is_empty();
    let input = text_input("Message — stash all changes", &app.stashes.message)
        .on_input(|value| Message::Ui(UiMessage::StashMessageChanged(value)))
        .on_submit(Message::Git(GitMessage::StashAll))
        .padding([7, 10])
        .size(13)
        .style(style::input);
    let stash = pill("", 15, "Stash all", GitMessage::StashAll, Tone::Normal, has_changes);
    items.push(row![input, stash].spacing(6).align_y(Center).into());

    items.push(gap(8.0));
    items.push(branch_section_label("STASHES", app.stashes.stashes.len()));
    if app.stashes.stashes.is_empty() {
        items.push(placeholder("No stashes"));
    } else {
        let selected = app.stashes.selected;
        for stash in &app.stashes.stashes {
            items.push(stash_row(stash, selected == Some(stash.index)));
        }
    }

    scrollable(column(items).spacing(4).padding(12))
        .height(Fill)
        .into()
}

/// One saved stash: a click target (its description and `stash@{N}` ref) that
/// shows the stash's Diff. Right-clicking opens its actions (Apply / Pop / Drop).
fn stash_row<'a>(stash: &StashInfo, selected: bool) -> Element<'a, Message> {
    let index = stash.index;
    let label = button(
        column![
            text(stash.message.clone()).size(14).color(style::TEXT),
            text(format!("stash@{{{index}}}"))
                .size(11)
                .color(style::TEXT_FAINT),
        ]
        .spacing(2)
        .width(Fill),
    )
    .on_press(Message::Ui(UiMessage::StashSelected(index)))
    .width(Fill)
    .padding([4, 8])
    .style(style::file_item(selected));

    mouse_area(container(label).padding([4, 6]).width(Fill))
        .on_right_press(Message::Ui(UiMessage::OpenMenu(MenuTarget::Stash { index })))
        .into()
}

/// The right panel in the Stashes view: the selected stash's Diff, or — when
/// nothing is selected — a short summary and usage hint.
fn stashes_detail(app: &App) -> Element<'_, Message> {
    let stashes = &app.stashes;

    // A stash is selected: show its Diff (or a loading note while it arrives).
    if let Some(index) = stashes.selected {
        let message = stashes
            .stashes
            .iter()
            .find(|s| s.index == index)
            .map(|s| s.message.clone())
            .unwrap_or_default();
        let header = container(
            column![
                text(message).size(15).color(style::TEXT),
                text(format!("stash@{{{index}}}"))
                    .size(12)
                    .font(Font::MONOSPACE)
                    .color(style::INFO),
            ]
            .spacing(8),
        )
        .style(style::diff_header)
        .padding([10, 12])
        .width(Fill);

        let body: Element<Message> = match &stashes.diff {
            Some(diff) if diff.index == index => diff_body(&diff.lines),
            _ => container(text("Loading…").size(14).color(style::TEXT_FAINT))
                .center(Fill)
                .into(),
        };

        return column![header, body].spacing(10).padding(12).into();
    }

    let title = match stashes.stashes.len() {
        0 => "No stashes".to_string(),
        1 => "1 stash".to_string(),
        n => format!("{n} stashes"),
    };

    let lines = column![
        text(title).size(16).color(style::TEXT),
        text("Click a stash to view its contents")
            .size(12)
            .color(style::TEXT_FAINT),
        text("Right-click for Apply / Pop / Drop")
            .size(12)
            .color(style::TEXT_FAINT),
    ]
    .spacing(8);

    container(lines.align_x(Center))
        .center(Fill)
        .padding(12)
        .into()
}

// ── Tags ──────────────────────────────────────────────────────────────────

/// The left column in the Tags view: a tag creator (name + optional annotation
/// message) at the top, then the tags, each pointing at its target Commit.
fn tags_list(app: &App) -> Element<'_, Message> {
    let mut items: Vec<Element<Message>> = Vec::new();

    let can_create = !app.tags.new_name.trim().is_empty();
    let name = text_input("New tag name", &app.tags.new_name)
        .on_input(|value| Message::Ui(UiMessage::NewTagNameChanged(value)))
        .on_submit(Message::Git(GitMessage::CreateTag))
        .padding([7, 10])
        .size(13)
        .style(style::input);
    let create = pill("+", 15, "Create", GitMessage::CreateTag, Tone::Normal, can_create);
    items.push(row![name, create].spacing(6).align_y(Center).into());

    // An optional annotation message; supplying one makes the tag annotated.
    let message = text_input("Annotation message (optional)", &app.tags.message)
        .on_input(|value| Message::Ui(UiMessage::TagMessageChanged(value)))
        .on_submit(Message::Git(GitMessage::CreateTag))
        .padding([7, 10])
        .size(13)
        .style(style::input);
    items.push(message.into());

    items.push(gap(8.0));
    items.push(branch_section_label("TAGS", app.tags.tags.len()));
    if app.tags.tags.is_empty() {
        items.push(placeholder("No tags yet"));
    } else {
        for tag in &app.tags.tags {
            items.push(tag_row(tag));
        }
    }

    scrollable(column(items).spacing(4).padding(12))
        .height(Fill)
        .into()
}

/// One tag: a tag/annotation marker, its name, and the target Commit it points
/// at. Right-clicking opens its actions (Push / Delete).
fn tag_row<'a>(tag: &TagInfo) -> Element<'a, Message> {
    let marker_color = if tag.is_annotated {
        style::ACCENT
    } else {
        style::TEXT_FAINT
    };

    let label = column![
        row![
            text("🏷").size(12).color(marker_color),
            text(tag.name.clone()).size(14).color(style::TEXT),
        ]
        .spacing(8)
        .align_y(Center),
        text(format!("{}  {}", tag.target, tag.summary))
            .size(11)
            .color(style::TEXT_FAINT),
    ]
    .spacing(2)
    .width(Fill);

    mouse_area(container(label).padding([4, 8]).width(Fill))
        .on_right_press(Message::Ui(UiMessage::OpenMenu(MenuTarget::Tag {
            name: tag.name.clone(),
        })))
        .into()
}

/// The right panel in the Tags view: a short summary and a usage hint.
fn tags_detail(app: &App) -> Element<'_, Message> {
    let title = match app.tags.tags.len() {
        0 => "No tags".to_string(),
        1 => "1 tag".to_string(),
        n => format!("{n} tags"),
    };

    let lines = column![
        text(title).size(16).color(style::TEXT),
        text("Create a tag at the current commit (HEAD)")
            .size(12)
            .color(style::TEXT_FAINT),
        text("A message makes it annotated · right-click for Push / Delete")
            .size(12)
            .color(style::TEXT_FAINT),
    ]
    .spacing(8);

    container(lines.align_x(Center))
        .center(Fill)
        .padding(12)
        .into()
}

// ── Commit Detail ────────────────────────────────────────────────────────

/// The right panel in the History view: the selected Commit's metadata, full
/// message, and Diff. Empty when nothing is selected.
fn commit_detail_view(history: &HistoryState) -> Element<'_, Message> {
    let Some(detail) = &history.detail else {
        let label = if history.selected.is_some() {
            "Loading commit…"
        } else {
            "Select a commit to view it"
        };
        return container(text(label).size(14).color(style::TEXT_FAINT))
            .center(Fill)
            .into();
    };

    let header = container(
        column![
            text(detail.message.trim().to_string()).size(15).color(style::TEXT),
            row![
                text(detail.short_sha.clone())
                    .size(12)
                    .font(Font::MONOSPACE)
                    .color(style::INFO),
                text(format!(
                    "{} <{}> · {}",
                    detail.author,
                    detail.email,
                    relative_time(detail.time)
                ))
                .size(12)
                .color(style::TEXT_MUTED),
            ]
            .spacing(10)
            .align_y(Center),
        ]
        .spacing(8),
    )
    .style(style::diff_header)
    .padding([10, 12])
    .width(Fill);

    column![header, diff_body(&detail.lines)]
        .spacing(10)
        .padding(12)
        .into()
}

/// Render a multi-file patch (`DiffLine`s with injected `● path` headers) as a
/// scrollable, syntax-highlighted body. Shared by the Commit and Stash details.
fn diff_body(lines: &[DiffLine]) -> Element<'_, Message> {
    // Track the current file (from the injected "● path" header lines) so each
    // line is highlighted for its own language.
    let mut lang = highlight::Lang::Plain;
    let emphasis = intraline_emphasis(lines);
    let rows: Vec<Element<Message>> = lines
        .iter()
        .enumerate()
        .map(|(idx, line)| {
            if line.kind == DiffLineKind::Header
                && let Some(path) = line.content.strip_prefix("● ")
            {
                lang = highlight::lang_for(path);
            }
            diff_line(line, lang, emphasis[idx].as_deref())
        })
        .collect();
    scrollable(column(rows).padding([8, 4]))
        .height(Fill)
        .width(Fill)
        .into()
}

/// A coarse "time ago" label for a Unix timestamp (seconds).
fn relative_time(unix_secs: i64) -> String {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0);
    let diff = (now - unix_secs).max(0);

    match diff {
        d if d < 60 => "just now".to_string(),
        d if d < 3600 => format!("{}m ago", d / 60),
        d if d < 86_400 => format!("{}h ago", d / 3600),
        d if d < 86_400 * 30 => format!("{}d ago", d / 86_400),
        d if d < 86_400 * 365 => format!("{}mo ago", d / (86_400 * 30)),
        d => format!("{}y ago", d / (86_400 * 365)),
    }
}

/// Format a Unix timestamp (seconds, UTC) as `YYYY-MM-DD`. Blame dates are often
/// old, where an absolute date reads better than a coarse "2y ago". Uses the
/// civil-from-days algorithm so it needs no date library.
fn ymd(unix_secs: i64) -> String {
    let days = unix_secs.div_euclid(86_400);
    // Howard Hinnant's days-to-civil: shift the epoch so the year starts in March.
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = if m <= 2 { y + 1 } else { y };
    format!("{y:04}-{m:02}-{d:02}")
}

// ── Diff View ────────────────────────────────────────────────────────────

/// The right panel: the Diff of the selected file, GitHub-style.
fn diff_view(repo: &RepoState) -> Element<'_, Message> {
    let Some(diff) = &repo.diff else {
        return container(
            text("Select a file to view its diff")
                .size(14)
                .color(style::TEXT_FAINT),
        )
        .center(Fill)
        .into();
    };

    if diff.lines.is_empty() {
        return container(text("No changes to show").size(14).color(style::TEXT_FAINT))
            .center(Fill)
            .into();
    }

    let lang = highlight::lang_for(&diff.path);
    // Pair removed lines with the additions that replaced them so we can tint
    // just the words that changed within each.
    let emphasis = intraline_emphasis(&diff.lines);
    // Each hunk header (kind Header in a single-file diff) gets a Stage/Unstage
    // action; the index counts hunks in display order so it matches the worker.
    let mut hunk = 0;
    let rows: Vec<Element<Message>> = diff
        .lines
        .iter()
        .enumerate()
        .map(|(idx, line)| {
            if line.kind == DiffLineKind::Header {
                let row = hunk_header_row(&line.content, &diff.path, hunk, diff.staged);
                hunk += 1;
                row
            } else {
                diff_line(line, lang, emphasis[idx].as_deref())
            }
        })
        .collect();
    let body = scrollable(column(rows).padding([8, 4]))
        .height(Fill)
        .width(Fill);

    column![diff_header(diff), body].spacing(10).into()
}

// ── Blame View ───────────────────────────────────────────────────────────

/// The right panel in Blame mode: every line of the file tagged with the Commit
/// that last touched it (short SHA, author, date), with a "Diff" escape back.
fn blame_view(file: &BlameFile) -> Element<'_, Message> {
    let mut bar = row![
        text("Blame").size(11).color(style::INFO),
        text(file.path.clone())
            .size(13)
            .font(Font::MONOSPACE)
            .color(style::TEXT),
    ]
    .spacing(12)
    .align_y(Center);

    // When walking history backwards, show which commit we are blaming "before"
    // and offer a reset back to the HEAD blame.
    if let Some(before) = &file.before {
        let short: String = before.chars().take(7).collect();
        bar = bar.push(
            text(format!("before {short}"))
                .size(11)
                .color(style::YELLOW),
        );
        bar = bar.push(
            button(text("Reset").size(11))
                .on_press(Message::Ui(UiMessage::ResetBlame))
                .padding([3, 9])
                .style(style::ghost),
        );
    }

    bar = bar.push(space::horizontal());
    bar = bar.push(
        button(text("History").size(11))
            .on_press(Message::Ui(UiMessage::ShowFileHistory(file.path.clone())))
            .padding([3, 9])
            .style(style::ghost),
    );
    bar = bar.push(
        button(text("Diff").size(11))
            .on_press(Message::Ui(UiMessage::HideBlame))
            .padding([3, 9])
            .style(style::ghost),
    );

    let header = container(bar)
        .style(style::diff_header)
        .padding([7, 12])
        .width(Fill);

    let lang = highlight::lang_for(&file.path);
    let rows: Vec<Element<Message>> = file
        .lines
        .iter()
        .enumerate()
        .map(|(i, line)| blame_line(i + 1, line, lang))
        .collect();
    let body = scrollable(column(rows).padding([8, 4]))
        .height(Fill)
        .width(Fill);

    column![header, body].spacing(10).into()
}

/// One blamed line: a gutter (short SHA, author, date, line number) then the
/// syntax-highlighted content. The short SHA is a link that jumps to its Commit
/// in the History view.
fn blame_line<'a>(
    lineno: usize,
    line: &'a crate::git::BlameLine,
    lang: highlight::Lang,
) -> Element<'a, Message> {
    // The short SHA links to the Commit in History, and the "⮜" re-blames before
    // it (walking the line's history back). A boundary line (no attribution) gets
    // blank placeholders so the columns stay aligned.
    let (sha_link, before_link): (Element<Message>, Element<Message>) = if line.sha.is_empty() {
        (
            text("       ").font(Font::MONOSPACE).size(11).into(),
            text(" ").size(11).into(),
        )
    } else {
        let short: String = line.sha.chars().take(7).collect();
        let sha = mouse_area(text(short).font(Font::MONOSPACE).size(11).color(style::INFO))
            .on_press(Message::Ui(UiMessage::ShowCommit(line.sha.clone())))
            .interaction(iced::mouse::Interaction::Pointer)
            .into();
        let before = mouse_area(text("⮜").size(11).color(style::TEXT_FAINT))
            .on_press(Message::Ui(UiMessage::ReblameBefore(line.sha.clone())))
            .interaction(iced::mouse::Interaction::Pointer)
            .into();
        (sha, before)
    };

    let meta = format!("{:>10.10} {}", line.author, ymd(line.time));

    let mut spans: Vec<iced::widget::text::Span<()>> = Vec::new();
    for (fragment, color) in highlight::spans(&line.content, lang) {
        spans.push(span(fragment.to_string()).color(color));
    }

    container(
        row![
            sha_link,
            before_link,
            text(meta)
                .font(Font::MONOSPACE)
                .size(11)
                .color(style::TEXT_FAINT),
            text(format!("{lineno:>4}"))
                .font(Font::MONOSPACE)
                .size(12)
                .color(style::TEXT_FAINT),
            rich_text(spans).font(Font::MONOSPACE).size(13),
        ]
        .spacing(10)
        .align_y(Center),
    )
    .width(Fill)
    .padding([1, 10])
    .into()
}

/// A hunk header line, carrying the `@@ … @@` text and a per-hunk action:
/// "Stage" on the Working Tree side, "Unstage" on the Staging Area side.
fn hunk_header_row<'a>(
    content: &str,
    path: &str,
    hunk: usize,
    staged: bool,
) -> Element<'a, Message> {
    let (label, message) = if staged {
        (
            "Unstage",
            GitMessage::UnstageHunk {
                path: path.to_string(),
                hunk,
            },
        )
    } else {
        (
            "Stage",
            GitMessage::StageHunk {
                path: path.to_string(),
                hunk,
            },
        )
    };

    let action = button(text(label.to_string()).size(11))
        .on_press(Message::Git(message))
        .padding([2, 9])
        .style(style::secondary);

    container(
        row![
            text(content.to_string())
                .font(Font::MONOSPACE)
                .size(13)
                .color(style::INFO),
            space::horizontal(),
            action,
        ]
        .spacing(10)
        .align_y(Center),
    )
    .style(style::diff_row(Some(style::INFO_BG)))
    .width(Fill)
    .padding([3, 10])
    .into()
}

/// The strip atop the Diff View: the file's side, its path, and the count of
/// added and removed lines.
fn diff_header(diff: &Diff) -> Element<'_, Message> {
    let added = diff
        .lines
        .iter()
        .filter(|l| matches!(l.kind, DiffLineKind::Addition))
        .count();
    let removed = diff
        .lines
        .iter()
        .filter(|l| matches!(l.kind, DiffLineKind::Deletion))
        .count();

    let side = if diff.staged { "Staged" } else { "Unstaged" };
    let side_color = if diff.staged {
        style::GREEN
    } else {
        style::YELLOW
    };

    container(
        row![
            text(side).size(11).color(side_color),
            text(diff.path.clone())
                .size(13)
                .font(Font::MONOSPACE)
                .color(style::TEXT),
            space::horizontal(),
            text(format!("+{added}")).size(12).color(style::GREEN),
            text(format!("−{removed}")).size(12).color(style::RED),
            button(text("Blame").size(11))
                .on_press(Message::Ui(UiMessage::ShowBlame))
                .padding([3, 9])
                .style(style::ghost),
            button(text("History").size(11))
                .on_press(Message::Ui(UiMessage::ShowFileHistory(diff.path.clone())))
                .padding([3, 9])
                .style(style::ghost),
        ]
        .spacing(12)
        .align_y(Center),
    )
    .style(style::diff_header)
    .padding([7, 12])
    .width(Fill)
    .into()
}

/// Walk the diff's lines, pairing each run of removed lines with the run of
/// added lines that follows it, and compute the changed-word ranges for each
/// pair. Returns a slot per line: `Some(ranges)` to emphasize within that line,
/// `None` to leave it whole. Lines outside a paired run, or pairs too dissimilar
/// to be worth it, stay `None`.
fn intraline_emphasis(lines: &[DiffLine]) -> Vec<Option<Vec<(usize, usize)>>> {
    let mut emphasis = vec![None; lines.len()];
    let mut i = 0;
    while i < lines.len() {
        if lines[i].kind != DiffLineKind::Deletion {
            i += 1;
            continue;
        }
        let del_start = i;
        while i < lines.len() && lines[i].kind == DiffLineKind::Deletion {
            i += 1;
        }
        let add_start = i;
        while i < lines.len() && lines[i].kind == DiffLineKind::Addition {
            i += 1;
        }
        let pairs = (add_start - del_start).min(i - add_start);
        for k in 0..pairs {
            let (d, a) = (del_start + k, add_start + k);
            if let Some(e) = worddiff::diff(&lines[d].content, &lines[a].content) {
                emphasis[d] = Some(e.old);
                emphasis[a] = Some(e.new);
            }
        }
    }
    emphasis
}

/// One diff line: a two-column line-number gutter, a marker, and the content,
/// over a full-width tint that conveys add/remove without tinting the text. The
/// content is syntax-highlighted (`lang`), except header lines which stay flat.
/// When `emphasis` is set, the listed char ranges (the words that actually
/// changed) get a stronger background over the row tint.
fn diff_line<'a>(
    line: &'a DiffLine,
    lang: highlight::Lang,
    emphasis: Option<&[(usize, usize)]>,
) -> Element<'a, Message> {
    let (marker, marker_color, tint) = match line.kind {
        DiffLineKind::Addition => ("+", style::GREEN, Some(style::GREEN_BG)),
        DiffLineKind::Deletion => ("-", style::RED, Some(style::RED_BG)),
        DiffLineKind::Context => (" ", style::TEXT_FAINT, None),
        DiffLineKind::Header => ("", style::INFO, Some(style::INFO_BG)),
    };
    let emphasis_bg = match line.kind {
        DiffLineKind::Addition => style::GREEN_BG_STRONG,
        _ => style::RED_BG_STRONG,
    };

    let gutter = format!(
        "{:>4} {:>4}",
        lineno(line.old_lineno),
        lineno(line.new_lineno)
    );

    // Header lines (hunk and per-file markers) are shown flat; code lines get
    // token coloring.
    let content: Element<Message> = if matches!(line.kind, DiffLineKind::Header) {
        text(line.content.clone())
            .font(Font::MONOSPACE)
            .size(13)
            .color(style::INFO)
            .into()
    } else {
        let emphasis = emphasis.unwrap_or(&[]);
        let mut spans: Vec<iced::widget::text::Span<()>> = Vec::new();
        // Highlight fragments tile the content in order; split each one further
        // at emphasis boundaries so changed words get the stronger background
        // while keeping their syntax color.
        let mut pos = 0; // char offset into the content
        for (fragment, color) in highlight::spans(&line.content, lang) {
            let chars: Vec<char> = fragment.chars().collect();
            let mut j = 0;
            while j < chars.len() {
                let emph = in_ranges(pos + j, emphasis);
                let run = j;
                while j < chars.len() && in_ranges(pos + j, emphasis) == emph {
                    j += 1;
                }
                let text: String = chars[run..j].iter().collect();
                let mut sp = span(text).color(color);
                if emph {
                    sp = sp.background(emphasis_bg);
                }
                spans.push(sp);
            }
            pos += chars.len();
        }
        rich_text(spans).font(Font::MONOSPACE).size(13).into()
    };

    container(
        row![
            text(gutter)
                .font(Font::MONOSPACE)
                .size(12)
                .color(style::TEXT_FAINT),
            text(marker)
                .font(Font::MONOSPACE)
                .size(13)
                .color(marker_color)
                .width(Length::Fixed(10.0)),
            content,
        ]
        .spacing(10),
    )
    .style(style::diff_row(tint))
    .width(Fill)
    .padding([1, 10])
    .into()
}

fn lineno(value: Option<u32>) -> String {
    value.map(|n| n.to_string()).unwrap_or_default()
}

/// Whether char offset `pos` falls inside any of the half-open emphasis ranges.
fn in_ranges(pos: usize, ranges: &[(usize, usize)]) -> bool {
    ranges.iter().any(|&(start, end)| pos >= start && pos < end)
}

// ── Commit Panel ─────────────────────────────────────────────────────────

/// The bottom-right Commit Panel: message field, an Amend toggle, the Commit
/// button, and a summary of what will be committed.
fn commit_panel(app: &App) -> Element<'_, Message> {
    let commit = &app.commit;
    let staged = app.repo.staged.len();
    let has_commit = app.repo.head.last_commit.is_some();

    let input = text_input("Commit message", &commit.message)
        .on_input(|value| Message::Ui(UiMessage::CommitMessageChanged(value)))
        .on_submit(Message::Git(GitMessage::Commit))
        .padding(10)
        .size(14)
        .style(style::input);

    // Amend replaces HEAD: it needs an existing Commit but not staged changes.
    // A plain Commit needs something staged.
    let label = if commit.committing {
        "Committing…"
    } else if commit.amend {
        "Amend"
    } else {
        "Commit"
    };
    let ready = !commit.committing
        && !commit.message.trim().is_empty()
        && if commit.amend { has_commit } else { staged > 0 };
    let commit_button = button(text(label).size(14))
        .on_press_maybe(ready.then_some(Message::Git(GitMessage::Commit)))
        .padding([8, 18])
        .style(style::primary);

    // "Commit & Push" chains a Push after the Commit lands; it needs the same
    // staged/message readiness plus a remote to push to and no busy operation.
    let can_push = app.repo.head.has_remote && app.operation.is_none();
    let commit_push_button = button(text("Commit & Push").size(14))
        .on_press_maybe((ready && can_push).then_some(Message::Git(GitMessage::CommitAndPush)))
        .padding([8, 14])
        .style(style::secondary);

    // The Amend toggle is only meaningful once there is a Commit to amend.
    let amend_toggle = checkbox(commit.amend)
        .on_toggle_maybe(has_commit.then_some(|_| Message::Ui(UiMessage::ToggleAmend)))
        .size(16)
        .style(style::check);
    let amend_color = if commit.amend {
        style::ACCENT
    } else {
        style::TEXT_MUTED
    };
    let amend = row![amend_toggle, text("Amend").size(12).color(amend_color)]
        .spacing(8)
        .align_y(Center);

    let summary = if commit.amend {
        "Replaces the last commit".to_string()
    } else {
        match staged {
            0 => "Nothing staged".to_string(),
            1 => "1 file staged".to_string(),
            n => format!("{n} files staged"),
        }
    };
    let summary_color = if commit.amend || staged > 0 {
        style::GREEN
    } else {
        style::TEXT_FAINT
    };

    let footer = row![
        commit_button,
        commit_push_button,
        amend,
        text(summary).size(12).color(summary_color),
        space::horizontal(),
        text("Ctrl+Enter").size(11).color(style::TEXT_FAINT),
    ]
    .spacing(12)
    .align_y(Center);

    container(column![input, footer].spacing(10))
        .padding(14)
        .width(Fill)
        .into()
}

// ── Status Bar ───────────────────────────────────────────────────────────

/// The persistent bottom strip: in-progress remote op, sticky error, or a
/// transient success Notification — in that priority order. When idle, it shows
/// the last Commit as ambient context.
fn status_bar(app: &App) -> Element<'_, Message> {
    let (icon, message, color) = if let Some(operation) = &app.operation {
        ("⟳", operation.clone(), style::INFO)
    } else if let Some(error) = &app.status.message {
        ("⚠", format!("{error}   ·   Esc to dismiss"), style::RED)
    } else if let Some(note) = &app.notification.message {
        ("✓", note.clone(), style::GREEN)
    } else if let Some(commit) = &app.repo.head.last_commit {
        (
            "",
            format!("{}  {}", commit.short_sha, commit.summary),
            style::TEXT_FAINT,
        )
    } else {
        ("", "No commits yet".to_string(), style::TEXT_FAINT)
    };

    let content = row![text(icon).color(color), text(message).size(13).color(color)]
        .spacing(8)
        .align_y(Center);

    container(content)
        .style(style::status_bar)
        .padding([8, 14])
        .width(Fill)
        .into()
}

// ── Helpers ──────────────────────────────────────────────────────────────

/// A fixed vertical gap between sections.
fn gap<'a>(height: f32) -> Element<'a, Message> {
    container(text("")).height(Length::Fixed(height)).into()
}
