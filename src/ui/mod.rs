//! The `ui` module: all iced widgets.
//!
//! Every function here is a pure view over `App` state, producing widgets that
//! emit [`Message`]s. It never touches git2 directly — only the message types.
//! All color and surface treatment lives in [`style`].

mod highlight;
mod style;
mod worddiff;

use iced::widget::{
    button, checkbox, column, container, rich_text, row, scrollable, space, span, text, text_input,
};
use iced::{Center, Element, Fill, Font, Length};

use crate::app::{App, GitMessage, HistoryState, Message, RepoState, Selection, UiMessage, ViewMode};
use crate::git::{ChangeKind, CommitInfo, Diff, DiffLine, DiffLineKind, FileEntry, HeadInfo};

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

    let refresh = pill("⟳", 17, "", GitMessage::Refresh, Tone::Normal, true);
    let pull = pill("↓", 14, &pull_label, GitMessage::Pull, Tone::Normal, can_remote);
    let push = pill("↑", 14, &push_label, GitMessage::Push, Tone::Normal, can_remote);

    container(
        row![brand, space::horizontal(), refresh, pull, push]
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
        for entry in &repo.unstaged {
            items.push(file_row(app, entry, false));
        }
    }

    items.push(gap(10.0));
    items.push(section_header(app, "Staged", style::GREEN, true));
    if repo.staged.is_empty() {
        items.push(placeholder("Nothing staged"));
    } else {
        for entry in &repo.staged {
            items.push(file_row(app, entry, true));
        }
    }

    scrollable(column(items).spacing(3).padding(12))
        .height(Fill)
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
fn file_row<'a>(app: &App, entry: &FileEntry, staged: bool) -> Element<'a, Message> {
    let item = Selection {
        path: entry.path.clone(),
        staged,
    };
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
        row![bar, dot, text(entry.path.clone()).size(14)]
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

    row![check, name].spacing(8).align_y(Center).into()
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

/// The left column in the History view: the recent Commits, newest first.
fn history_list(history: &HistoryState) -> Element<'_, Message> {
    if history.commits.is_empty() {
        return container(placeholder("No commits yet")).height(Fill).into();
    }

    let rows: Vec<Element<Message>> = history
        .commits
        .iter()
        .map(|commit| commit_row(commit, history.selected.as_deref()))
        .collect();

    scrollable(column(rows).spacing(3).padding(12))
        .height(Fill)
        .into()
}

/// One Commit in the History list: short SHA, summary, and author · time.
fn commit_row<'a>(commit: &CommitInfo, selected: Option<&str>) -> Element<'a, Message> {
    let active = selected == Some(commit.sha.as_str());

    let bar = container(text(""))
        .width(Length::Fixed(3.0))
        .height(Length::Fixed(30.0))
        .style(style::selection_bar(active));

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

    button(row![bar, body].spacing(10).align_y(Center))
        .on_press(Message::Ui(UiMessage::CommitSelected(commit.sha.clone())))
        .width(Fill)
        .padding([7, 8])
        .style(style::file_item(active))
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
