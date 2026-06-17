//! The `ui` module: all iced widgets.
//!
//! Every function here is a pure view over `App` state, producing widgets that
//! emit [`Message`]s. It never touches git2 directly — only the message types.
//! All color and surface treatment lives in [`style`].

mod style;

use iced::widget::{button, checkbox, column, container, row, scrollable, text, text_input};
use iced::{Center, Element, Fill, Font, Length};

use crate::app::{App, CommitState, GitMessage, Message, RepoState, Selection, UiMessage};
use crate::git::{ChangeKind, DiffLine, DiffLineKind, FileEntry};

/// The application's custom dark [`iced::Theme`].
pub fn theme() -> iced::Theme {
    style::theme()
}

/// The three-panel layout plus the Status Bar, on the window background.
pub fn root(app: &App) -> Element<'_, Message> {
    let files = container(file_list(app))
        .style(style::panel)
        .width(Length::FillPortion(2))
        .height(Fill);

    let diff = container(diff_view(&app.repo))
        .style(style::panel)
        .width(Fill)
        .height(Fill);

    let commit = container(commit_panel(&app.commit))
        .style(style::panel)
        .width(Fill);

    let right = column![diff, commit]
        .spacing(12)
        .width(Length::FillPortion(3))
        .height(Fill);

    let body = row![files, right].spacing(12).height(Fill);

    container(column![body, status_bar(app)].spacing(12))
        .style(style::app)
        .padding(12)
        .width(Fill)
        .height(Fill)
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
/// on all of them when nothing is checked (the label reflects which).
fn action_toolbar(app: &App) -> Element<'_, Message> {
    let unstaged_checked = app.checked.iter().filter(|s| !s.staged).count();
    let staged_checked = app.checked.iter().filter(|s| s.staged).count();

    let stage = toolbar_button(
        &count_label("Stage", unstaged_checked),
        GitMessage::StageChecked,
        Tone::Normal,
        !app.repo.unstaged.is_empty(),
    );
    let unstage = toolbar_button(
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
    let discard = toolbar_button(
        &discard_label,
        GitMessage::DiscardChecked,
        Tone::Danger,
        !app.repo.unstaged.is_empty(),
    );

    container(row![stage, unstage, discard].spacing(6))
        .padding([2, 4])
        .into()
}

/// "Stage all" when nothing is checked, otherwise e.g. "Stage (3)".
fn count_label(verb: &str, checked: usize) -> String {
    if checked == 0 {
        format!("{verb} all")
    } else {
        format!("{verb} ({checked})")
    }
}

enum Tone {
    Normal,
    Danger,
}

fn toolbar_button<'a>(
    label: &str,
    message: GitMessage,
    tone: Tone,
    enabled: bool,
) -> Element<'a, Message> {
    let style = match tone {
        Tone::Normal => style::ghost as fn(&iced::Theme, button::Status) -> button::Style,
        Tone::Danger => style::ghost_danger,
    };
    button(text(label.to_string()).size(12))
        .on_press_maybe(enabled.then_some(Message::Git(message)))
        .padding([4, 10])
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
        .size(15)
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
        .spacing(8)
        .align_y(Center),
    )
    .padding([6, 4])
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
        .size(15)
        .style(style::check);

    // A small colored status dot reads as an icon and avoids a confusing "?"
    // glyph for untracked files. Color encodes the change (see `badge_color`).
    let dot = container(text(""))
        .width(Length::Fixed(9.0))
        .height(Length::Fixed(9.0))
        .style(style::dot(badge_color(entry.change)));

    let name = button(
        row![dot, text(entry.path.clone()).size(14)]
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

    scrollable(column(rows).padding([8, 4]))
        .height(Fill)
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
fn commit_panel(commit: &CommitState) -> Element<'_, Message> {
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
    let commit_button = button(text(label).size(14))
        .on_press(Message::Git(GitMessage::Commit))
        .padding([8, 18])
        .style(style::primary);

    let hint = text("Ctrl+Enter").size(11).color(style::TEXT_FAINT);

    container(column![input, row![commit_button, hint].spacing(12).align_y(Center)].spacing(10))
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
