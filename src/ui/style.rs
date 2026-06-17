//! The design system: one dark palette and the widget styles built from it.
//!
//! Keeping every color and surface treatment here means the rest of `ui` reads
//! as layout, and the look can be retuned in a single place.

use iced::theme::Palette;
use iced::widget::{button, checkbox, container, text_input};
use iced::{Background, Border, Color, Theme};

// ── Palette ──────────────────────────────────────────────────────────────
// A deep, near-black dark scale: layered surfaces, muted text, one indigo
// accent, and the semantic colors (add / remove / info / untracked).

/// The window background, behind every panel.
pub const BG_APP: Color = Color::from_rgb(0.031, 0.035, 0.047);
/// A raised panel surface (File List, Diff View, Commit Panel).
pub const BG_PANEL: Color = Color::from_rgb(0.055, 0.063, 0.078);
/// An inset surface (inputs, action buttons).
pub const BG_ELEVATED: Color = Color::from_rgb(0.086, 0.098, 0.133);
/// The hover wash on list rows.
pub const BG_HOVER: Color = Color::from_rgb(0.114, 0.129, 0.169);
/// Hairline borders and dividers.
pub const BORDER: Color = Color::from_rgb(0.149, 0.169, 0.208);

/// Primary text.
pub const TEXT: Color = Color::from_rgb(0.902, 0.929, 0.953);
/// Secondary text (labels, context lines).
pub const TEXT_MUTED: Color = Color::from_rgb(0.545, 0.580, 0.620);
/// Tertiary text (gutters, hints).
pub const TEXT_FAINT: Color = Color::from_rgb(0.431, 0.463, 0.506);

/// The single accent, used for selection and primary actions.
pub const ACCENT: Color = Color::from_rgb(0.431, 0.482, 0.949);
const ACCENT_HOVER: Color = Color::from_rgb(0.541, 0.584, 0.965);
const ACCENT_SOFT: Color = Color::from_rgba(0.431, 0.482, 0.949, 0.20);
const SELECTION: Color = Color::from_rgba(0.431, 0.482, 0.949, 0.35);

/// Additions.
pub const GREEN: Color = Color::from_rgb(0.247, 0.725, 0.314);
/// Deletions.
pub const RED: Color = Color::from_rgb(0.973, 0.320, 0.286);
/// Hunk headers and modified badges.
pub const INFO: Color = Color::from_rgb(0.345, 0.651, 1.0);
/// Untracked / unstaged accents.
pub const YELLOW: Color = Color::from_rgb(0.824, 0.600, 0.133);

/// Full-row tint behind an added diff line.
pub const GREEN_BG: Color = Color::from_rgba(0.247, 0.725, 0.314, 0.12);
/// Full-row tint behind a removed diff line.
pub const RED_BG: Color = Color::from_rgba(0.973, 0.320, 0.286, 0.12);
/// Full-row tint behind a hunk header.
pub const INFO_BG: Color = Color::from_rgba(0.345, 0.651, 1.0, 0.10);

// ── Helpers ──────────────────────────────────────────────────────────────

fn radius(value: f32) -> Border {
    Border {
        color: Color::TRANSPARENT,
        width: 0.0,
        radius: value.into(),
    }
}

fn with_alpha(color: Color, alpha: f32) -> Color {
    Color { a: alpha, ..color }
}

// ── Theme ────────────────────────────────────────────────────────────────

/// The custom dark [`Theme`]. Driving the whole palette through iced means the
/// default-styled bits (scrollbars, the window base) stay dark too, instead of
/// falling back to iced's grey built-in `Dark`.
pub fn theme() -> Theme {
    Theme::custom(
        "Midnight".to_string(),
        Palette {
            background: BG_APP,
            text: TEXT,
            primary: ACCENT,
            success: GREEN,
            warning: YELLOW,
            danger: RED,
        },
    )
}

// ── Surfaces ─────────────────────────────────────────────────────────────

/// The window background fill.
pub fn app(_: &Theme) -> container::Style {
    container::Style {
        background: Some(Background::Color(BG_APP)),
        text_color: Some(TEXT),
        ..container::Style::default()
    }
}

/// A raised, rounded, bordered panel.
pub fn panel(_: &Theme) -> container::Style {
    container::Style {
        background: Some(Background::Color(BG_PANEL)),
        text_color: Some(TEXT),
        border: Border {
            color: BORDER,
            width: 1.0,
            radius: 12.0.into(),
        },
        ..container::Style::default()
    }
}

/// The persistent bottom strip.
pub fn status_bar(_: &Theme) -> container::Style {
    container::Style {
        background: Some(Background::Color(BG_PANEL)),
        text_color: Some(TEXT),
        border: Border {
            color: BORDER,
            width: 1.0,
            radius: 10.0.into(),
        },
        ..container::Style::default()
    }
}

/// A small filled status dot (use on a fixed square container).
pub fn dot(color: Color) -> impl Fn(&Theme) -> container::Style {
    move |_| container::Style {
        background: Some(Background::Color(color)),
        border: radius(5.0),
        ..container::Style::default()
    }
}

/// A file-selection checkbox: accent-filled when checked.
pub fn check(_theme: &Theme, status: checkbox::Status) -> checkbox::Style {
    let checked = matches!(
        status,
        checkbox::Status::Active { is_checked: true }
            | checkbox::Status::Hovered { is_checked: true }
            | checkbox::Status::Disabled { is_checked: true }
    );
    let hovered = matches!(status, checkbox::Status::Hovered { .. });

    checkbox::Style {
        background: Background::Color(if checked { ACCENT } else { BG_ELEVATED }),
        icon_color: Color::WHITE,
        border: Border {
            color: if checked || hovered { ACCENT } else { BORDER },
            width: 1.0,
            radius: 5.0.into(),
        },
        text_color: None,
    }
}

/// A small colored count/letter chip.
pub fn chip(color: Color) -> impl Fn(&Theme) -> container::Style {
    move |_| container::Style {
        text_color: Some(color),
        background: Some(Background::Color(with_alpha(color, 0.16))),
        border: radius(5.0),
        ..container::Style::default()
    }
}

/// The full-width tint behind one diff line (or none, for context lines).
pub fn diff_row(tint: Option<Color>) -> impl Fn(&Theme) -> container::Style {
    move |_| container::Style {
        background: tint.map(Background::Color),
        ..container::Style::default()
    }
}

// ── Buttons ──────────────────────────────────────────────────────────────

/// A File List row: transparent at rest, washed on hover, accent when selected.
pub fn file_item(selected: bool) -> impl Fn(&Theme, button::Status) -> button::Style {
    move |_theme, status| {
        let background = if selected {
            Some(Background::Color(ACCENT_SOFT))
        } else {
            match status {
                button::Status::Hovered | button::Status::Pressed => {
                    Some(Background::Color(BG_HOVER))
                }
                _ => None,
            }
        };

        button::Style {
            background,
            text_color: TEXT,
            border: Border {
                color: if selected { ACCENT } else { Color::TRANSPARENT },
                width: 1.0,
                radius: 8.0.into(),
            },
            ..button::Style::default()
        }
    }
}

/// A borderless "ghost" text button for bulk header actions.
pub fn ghost(_theme: &Theme, status: button::Status) -> button::Style {
    let background = match status {
        button::Status::Hovered | button::Status::Pressed => Some(Background::Color(BG_HOVER)),
        _ => None,
    };

    button::Style {
        background,
        text_color: TEXT_MUTED,
        border: radius(6.0),
        ..button::Style::default()
    }
}

/// A ghost button for a destructive bulk action (red text, red wash on hover).
pub fn ghost_danger(_theme: &Theme, status: button::Status) -> button::Style {
    let (background, text_color) = match status {
        button::Status::Hovered | button::Status::Pressed => {
            (Some(Background::Color(with_alpha(RED, 0.18))), RED)
        }
        _ => (None, with_alpha(RED, 0.85)),
    };

    button::Style {
        background,
        text_color,
        border: radius(6.0),
        ..button::Style::default()
    }
}

/// The filled accent Commit button.
pub fn primary(_theme: &Theme, status: button::Status) -> button::Style {
    let background = match status {
        button::Status::Hovered => ACCENT_HOVER,
        button::Status::Disabled => BG_ELEVATED,
        _ => ACCENT,
    };
    let text_color = if matches!(status, button::Status::Disabled) {
        TEXT_FAINT
    } else {
        Color::WHITE
    };

    button::Style {
        background: Some(Background::Color(background)),
        text_color,
        border: radius(8.0),
        ..button::Style::default()
    }
}

// ── Inputs ───────────────────────────────────────────────────────────────

/// The commit-message field; the border lights up on focus.
pub fn input(_theme: &Theme, status: text_input::Status) -> text_input::Style {
    let border_color = match status {
        text_input::Status::Focused { .. } => ACCENT,
        _ => BORDER,
    };

    text_input::Style {
        background: Background::Color(BG_ELEVATED),
        border: Border {
            color: border_color,
            width: 1.0,
            radius: 8.0.into(),
        },
        icon: TEXT_MUTED,
        placeholder: TEXT_FAINT,
        value: TEXT,
        selection: SELECTION,
    }
}
