//! The `ui` module: all iced widgets.
//!
//! Every function here is a pure view over `App` state, producing widgets that
//! emit [`Message`]s. It never touches git2 directly — only the message types.
//! All color and surface treatment lives in [`style`].

mod style;

use iced::widget::{button, column, container, row, scrollable, text, text_input};
use iced::{Center, Element, Fill, Font, Length};

use crate::app::{App, CommitState, GitMessage, Message, RepoState, UiMessage};
use crate::git::{ChangeKind, DiffLine, DiffLineKind, FileEntry};

/// The application's custom dark [`iced::Theme`].
pub fn theme() -> iced::Theme {
    style::theme()
}

/// The three-panel layout plus the Status Bar, on the window background.
pub fn root(app: &App) -> Element<'_, Message> {
    let files = container(file_list(&app.repo))
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

/// The left column: Unstaged/Untracked files on top, Staged files below.
fn file_list(repo: &RepoState) -> Element<'_, Message> {
    let mut items: Vec<Element<Message>> = Vec::new();

    items.push(list_header("Unstaged", repo.unstaged.len(), style::YELLOW));
    if repo.unstaged.is_empty() {
        items.push(placeholder("Working tree is clean"));
    } else {
        for entry in &repo.unstaged {
            items.push(file_row(repo, entry, false));
        }
    }

    items.push(gap(10.0));
    items.push(list_header("Staged", repo.staged.len(), style::GREEN));
    if repo.staged.is_empty() {
        items.push(placeholder("Nothing staged"));
    } else {
        for entry in &repo.staged {
            items.push(file_row(repo, entry, true));
        }
    }

    scrollable(column(items).spacing(3).padding(12))
        .height(Fill)
        .into()
}

/// A section heading with a colored count chip.
fn list_header<'a>(label: &str, count: usize, color: iced::Color) -> Element<'a, Message> {
    let chip = container(text(count.to_string()).size(11).color(color))
        .style(style::chip(color))
        .padding([1, 7]);

    row![
        text(label.to_uppercase()).size(11).color(style::TEXT_MUTED),
        chip,
    ]
    .spacing(8)
    .align_y(Center)
    .padding([6, 4])
    .into()
}

fn placeholder<'a>(label: &str) -> Element<'a, Message> {
    container(text(label.to_string()).size(13).color(style::TEXT_FAINT))
        .padding([4, 12])
        .into()
}

/// One file: a change badge + filename (the selectable area) and a
/// Stage/Unstage action button.
fn file_row<'a>(repo: &RepoState, entry: &FileEntry, staged: bool) -> Element<'a, Message> {
    let selected = repo
        .selected
        .as_ref()
        .is_some_and(|s| s.path == entry.path && s.staged == staged);

    // A small colored status dot reads as an icon and avoids a confusing "?"
    // glyph for untracked files. Color encodes the change (see `badge_color`).
    let dot = container(text(""))
        .width(Length::Fixed(9.0))
        .height(Length::Fixed(9.0))
        .style(style::dot(badge_color(entry.change)));

    let label = button(
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
    .style(style::file_item(selected));

    let action = button(
        text(if staged { "−" } else { "+" })
            .size(15)
            .font(Font::MONOSPACE),
    )
    .on_press(Message::Git(if staged {
        GitMessage::Unstage(entry.path.clone())
    } else {
        GitMessage::Stage(entry.path.clone())
    }))
    .padding([4, 10])
    .style(style::action);

    row![label, action].spacing(6).align_y(Center).into()
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
