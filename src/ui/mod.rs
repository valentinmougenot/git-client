//! The `ui` module: all iced widgets.
//!
//! Every function here is a pure view over `App` state, producing widgets that
//! emit [`Message`]s. It never touches git2 directly — only the message types.
//! All color and surface treatment lives in [`style`].

mod style;

use iced::widget::{button, checkbox, column, container, row, scrollable, space, text, text_input};
use iced::{Center, Element, Fill, Font, Length};

use crate::app::{App, GitMessage, Message, RepoState, Selection, UiMessage};
use crate::git::{ChangeKind, Diff, DiffLine, DiffLineKind, FileEntry};

/// The application's custom dark [`iced::Theme`].
pub fn theme() -> iced::Theme {
    style::theme()
}

/// The top bar, the three-panel layout, and the Status Bar, on the window
/// background.
pub fn root(app: &App) -> Element<'_, Message> {
    let files = container(file_list(app))
        .style(style::panel)
        .width(Length::FillPortion(2))
        .height(Fill);

    let diff = container(diff_view(&app.repo))
        .style(style::panel)
        .width(Fill)
        .height(Fill);

    let commit = container(commit_panel(app))
        .style(style::panel)
        .width(Fill);

    let right = column![diff, commit]
        .spacing(12)
        .width(Length::FillPortion(3))
        .height(Fill);

    let body = row![files, right].spacing(12).height(Fill);

    container(column![top_bar(app), body, status_bar(app)].spacing(12))
        .style(style::app)
        .padding(12)
        .width(Fill)
        .height(Fill)
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

    let brand = row![
        mark,
        column![
            text("Git Client").size(15).color(style::TEXT),
            text("daily commit loop").size(11).color(style::TEXT_FAINT),
        ]
        .spacing(0),
    ]
    .spacing(11)
    .align_y(Center);

    // Repo-wide actions live together on the right: Refresh, then the remote
    // operations. Push/Pull are disabled while one is already in flight. All
    // share the exact pill style of the File List toolbar.
    let busy = app.operation.is_some();
    let refresh = pill("⟳", 17, "", GitMessage::Refresh, Tone::Normal, true);
    let pull = pill("↓", 14, "Pull", GitMessage::Pull, Tone::Normal, !busy);
    let push = pill("↑", 14, "Push", GitMessage::Push, Tone::Normal, !busy);

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

    let rows: Vec<Element<Message>> = diff.lines.iter().map(diff_line).collect();
    let body = scrollable(column(rows).padding([8, 4]))
        .height(Fill)
        .width(Fill);

    column![diff_header(diff), body].spacing(10).into()
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

/// One diff line: a two-column line-number gutter, a marker, and the content,
/// over a full-width tint that conveys add/remove without tinting the text.
fn diff_line(line: &DiffLine) -> Element<'_, Message> {
    let (marker, marker_color, tint, content_color) = match line.kind {
        DiffLineKind::Addition => ("+", style::GREEN, Some(style::GREEN_BG), style::TEXT),
        DiffLineKind::Deletion => ("-", style::RED, Some(style::RED_BG), style::TEXT),
        DiffLineKind::Context => (" ", style::TEXT_FAINT, None, style::TEXT_MUTED),
        DiffLineKind::Header => ("", style::INFO, Some(style::INFO_BG), style::INFO),
    };

    let gutter = format!(
        "{:>4} {:>4}",
        lineno(line.old_lineno),
        lineno(line.new_lineno)
    );

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
            text(line.content.clone())
                .font(Font::MONOSPACE)
                .size(13)
                .color(content_color),
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

// ── Commit Panel ─────────────────────────────────────────────────────────

/// The bottom-right Commit Panel: message field, Commit button, and a hint.
fn commit_panel(app: &App) -> Element<'_, Message> {
    let commit = &app.commit;
    let staged = app.repo.staged.len();

    let input = text_input("Commit message", &commit.message)
        .on_input(|value| Message::Ui(UiMessage::CommitMessageChanged(value)))
        .on_submit(Message::Git(GitMessage::Commit))
        .padding(10)
        .size(14)
        .style(style::input);

    let label = if commit.committing {
        "Committing…"
    } else {
        "Commit"
    };
    // The Commit can only fire with a non-empty message, something staged, and
    // no commit already running — mirror that in the button's enabled state.
    let ready = !commit.committing && !commit.message.trim().is_empty() && staged > 0;
    let commit_button = button(text(label).size(14))
        .on_press_maybe(ready.then_some(Message::Git(GitMessage::Commit)))
        .padding([8, 18])
        .style(style::primary);

    let summary = match staged {
        0 => "Nothing staged".to_string(),
        1 => "1 file staged".to_string(),
        n => format!("{n} files staged"),
    };
    let summary_color = if staged > 0 {
        style::GREEN
    } else {
        style::TEXT_FAINT
    };

    let footer = row![
        commit_button,
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
/// transient success Notification — in that priority order.
fn status_bar(app: &App) -> Element<'_, Message> {
    let (icon, message, color) = if let Some(operation) = &app.operation {
        ("⟳", operation.clone(), style::INFO)
    } else if let Some(error) = &app.status.message {
        ("⚠", format!("{error}   ·   Esc to dismiss"), style::RED)
    } else if let Some(note) = &app.notification.message {
        ("✓", note.clone(), style::GREEN)
    } else {
        ("", "Ready".to_string(), style::TEXT_FAINT)
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
