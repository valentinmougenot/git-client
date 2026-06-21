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
    button, checkbox, column, container, rich_text, row, scrollable, space, span, stack, text,
    text_input,
};
use iced::{Center, Element, Fill, Font, Length, Point, Rectangle, Renderer, Right, Theme};

use crate::app::{App, GitMessage, HistoryState, Message, RepoState, Selection, UiMessage, ViewMode};
use crate::git::{
    BranchInfo, ChangeKind, CommitInfo, Diff, DiffLine, DiffLineKind, FileEntry, HeadInfo,
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
    };
    let left = container(column![view_tabs(app), left_body])
        .style(style::panel)
        .width(Length::FillPortion(2))
        .height(Fill);

    let right: Element<Message> = match app.view {
        ViewMode::Changes => {
            let diff = container(diff_view(&app.repo))
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
        ViewMode::Branches => container(branches_detail(app))
            .style(style::panel)
            .width(Length::FillPortion(3))
            .height(Fill)
            .into(),
    };

    let body = row![left, right].spacing(12).height(Fill);

    container(column![top_bar(app), body, status_bar(app)].spacing(12))
        .style(style::app)
        .padding(12)
        .width(Fill)
        .height(Fill)
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

    let tabs = row![
        tab(&changes_label, ViewMode::Changes, app.view),
        tab("History", ViewMode::History, app.view),
        tab("Branches", ViewMode::Branches, app.view),
    ]
    .spacing(4);

    container(tabs).padding([8, 10]).into()
}

fn tab<'a>(label: &str, target: ViewMode, current: ViewMode) -> Element<'a, Message> {
    button(text(label.to_string()).size(13))
        .on_press(Message::Ui(UiMessage::ShowView(target)))
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

    container(row![stage, unstage, discard].spacing(6).align_y(Center))
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
        ChangeKind::Deleted => style::RED,
        ChangeKind::Modified | ChangeKind::Renamed | ChangeKind::Typechange => style::INFO,
    }
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

    container(row![cell, body].spacing(6).align_y(Center))
        .height(Length::Fixed(COMMIT_ROW_H))
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

    let armed = app.branches.delete_armed.as_deref();
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
        push_branch_tree(app, &locals, false, armed, &mut items);
    }

    if !remotes.is_empty() {
        items.push(gap(10.0));
        items.push(branch_section_label("REMOTE", remotes.len()));
        push_branch_tree(app, &remotes, true, None, &mut items);
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
    armed: Option<&'a str>,
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
        push_branch_node(app, node, remote, 0, armed, items);
    }
}

fn push_branch_node<'a>(
    app: &'a App,
    node: tree::Node<'_, &'a BranchInfo>,
    remote: bool,
    depth: usize,
    armed: Option<&str>,
    items: &mut Vec<Element<'a, Message>>,
) {
    match node {
        tree::Node::Leaf(branch) => {
            let branch: &'a BranchInfo = branch;
            items.push(branch_row(branch, armed == Some(branch.name.as_str()), depth));
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
                    push_branch_node(app, child, remote, depth + 1, armed, items);
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

/// One branch leaf: a current-branch marker, its (leaf) name, its sync state,
/// and — for any branch other than the current one — a click target to switch to
/// it and a delete button.
fn branch_row<'a>(branch: &BranchInfo, armed: bool, depth: usize) -> Element<'a, Message> {
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

    // The current branch and remote branches have no in-row delete.
    if branch.is_head || branch.is_remote {
        return row![tree_indent(depth), select]
            .spacing(8)
            .align_y(Center)
            .into();
    }

    // A quiet, borderless delete sitting inside the row's rectangle, right-
    // aligned. Stacked over the switch button so its own clicks are caught while
    // the rest of the row still triggers the checkout beneath it.
    let delete = button(text(if armed { "Confirm?" } else { "✕" }).size(if armed { 11 } else { 13 }))
        .on_press(Message::Git(GitMessage::DeleteBranch(branch.name.clone())))
        .padding([4, 8])
        .style(style::ghost_danger);
    let delete = container(delete)
        .width(Fill)
        .height(Fill)
        .align_x(Right)
        .center_y(Fill)
        .padding([0, 6]);

    row![tree_indent(depth), stack![select, delete]]
        .spacing(8)
        .align_y(Center)
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
        text("Click a branch to switch to it · ✕ to delete · Create above")
            .size(12)
            .color(style::TEXT_FAINT),
    );

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

    // The detail spans several files; track the current file (from the injected
    // "● path" header lines) so each line is highlighted for its own language.
    let mut lang = highlight::Lang::Plain;
    let emphasis = intraline_emphasis(&detail.lines);
    let rows: Vec<Element<Message>> = detail
        .lines
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
    let body = scrollable(column(rows).padding([8, 4])).height(Fill).width(Fill);

    column![header, body].spacing(10).padding(12).into()
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
