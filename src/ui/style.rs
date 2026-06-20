//! The design system: one dark palette and the widget styles built from it.
//!
//! Keeping every color and surface treatment here means the rest of `ui` reads
//! as layout, and the look can be retuned in a single place.

use iced::theme::Palette;
use iced::widget::{button, checkbox, container, text_input};
use iced::{Background, Border, Color, Theme};

// ── Palette ──────────────────────────────────────────────────────────────
// The "Tokyo Night" scale: a deep, near-black base with a faint blue cast, a
// soft blue accent, and gentle semantic colors (add / remove / info / warn).
//
// IMPORTANT: iced's `Color::from_rgb` here is treated as *linear* light and
// gamma-encoded to sRGB at display time — so a literal like 0.05 renders much
// lighter than #0d0d0d. Every value below is therefore the LINEAR form of the
// sRGB hex named in its doc comment. To change a color, edit the hex and
// re-derive each channel `c` (0..1) with the standard sRGB→linear transfer:
//   c <= 0.04045 ? c/12.92 : ((c + 0.055)/1.055).powf(2.4)
// Don't hand-tweak the floats directly.

/// The window background, behind every panel (sRGB `#08090d`).
pub const BG_APP: Color = Color::from_rgb(0.00243, 0.00273, 0.00402);
/// A raised panel surface — File List, Diff View, Commit Panel (sRGB `#0e1019`).
pub const BG_PANEL: Color = Color::from_rgb(0.00439, 0.00518, 0.00972);
/// An inset surface — inputs, action buttons, headers (sRGB `#171a27`).
pub const BG_ELEVATED: Color = Color::from_rgb(0.00857, 0.01033, 0.02029);
/// The hover wash on list rows (sRGB `#1f2336`).
pub const BG_HOVER: Color = Color::from_rgb(0.01370, 0.01681, 0.03689);
/// Hairline borders and dividers (sRGB `#2a2f44`).
pub const BORDER: Color = Color::from_rgb(0.02315, 0.02843, 0.05781);

/// Primary text (sRGB `#c0caf5`).
pub const TEXT: Color = Color::from_rgb(0.52712, 0.59062, 0.91310);
/// Secondary text — labels, context lines (sRGB `#828bb8`).
pub const TEXT_MUTED: Color = Color::from_rgb(0.22323, 0.25818, 0.47932);
/// Tertiary text — gutters, hints (sRGB `#565f89`).
pub const TEXT_FAINT: Color = Color::from_rgb(0.09306, 0.11444, 0.25016);

/// The single accent, used for selection and primary actions (sRGB `#7aa2f7`).
pub const ACCENT: Color = Color::from_rgb(0.19462, 0.36131, 0.93011);
const ACCENT_HOVER: Color = Color::from_rgb(0.25016, 0.45641, 1.0);
const ACCENT_SOFT: Color = Color::from_rgba(0.19462, 0.36131, 0.93011, 0.16);
const SELECTION: Color = Color::from_rgba(0.19462, 0.36131, 0.93011, 0.30);

/// Additions — green (sRGB `#9ece6a`).
pub const GREEN: Color = Color::from_rgb(0.34191, 0.61721, 0.14413);
/// Deletions — red (sRGB `#f7768e`).
pub const RED: Color = Color::from_rgb(0.93011, 0.18116, 0.27050);
/// Hunk headers and modified badges — cyan (sRGB `#7dcfff`).
pub const INFO: Color = Color::from_rgb(0.20508, 0.62396, 1.0);
/// Untracked / unstaged accents — amber (sRGB `#e0af68`).
pub const YELLOW: Color = Color::from_rgb(0.74540, 0.42869, 0.13843);

// Syntax-highlighting accents (used in the Diff View), Tokyo Night hues.
/// Keywords — purple (sRGB `#bb9af7`).
pub const SYN_KEYWORD: Color = Color::from_rgb(0.49693, 0.32314, 0.93011);
/// Numbers — orange (sRGB `#ff9e64`).
pub const SYN_NUMBER: Color = Color::from_rgb(1.0, 0.34191, 0.12744);
/// Types / capitalized identifiers — cyan (sRGB `#2ac3de`).
pub const SYN_TYPE: Color = Color::from_rgb(0.02315, 0.54572, 0.73046);
/// Strings reuse the additions green; comments reuse the faint gutter color.
pub const SYN_STRING: Color = GREEN;
pub const SYN_COMMENT: Color = TEXT_FAINT;

/// Full-row tint behind an added diff line.
pub const GREEN_BG: Color = Color::from_rgba(0.34191, 0.61721, 0.14413, 0.12);
/// Full-row tint behind a removed diff line.
pub const RED_BG: Color = Color::from_rgba(0.93011, 0.18116, 0.27050, 0.12);
/// Full-row tint behind a hunk header.
pub const INFO_BG: Color = Color::from_rgba(0.20508, 0.62396, 1.0, 0.10);
/// Stronger tint behind the precise words added within an addition line.
pub const GREEN_BG_STRONG: Color = Color::from_rgba(0.34191, 0.61721, 0.14413, 0.30);
/// Stronger tint behind the precise words removed within a deletion line.
pub const RED_BG_STRONG: Color = Color::from_rgba(0.93011, 0.18116, 0.27050, 0.30);

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
        "Tokyo Night".to_string(),
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

/// The top application bar: a raised strip carrying the brand and remote
/// actions. Bottom-only border so it reads as one continuous surface with the
/// window chrome above it.
pub fn header(_: &Theme) -> container::Style {
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

/// The accent brand mark: a small filled rounded square holding the logo glyph.
pub fn brand_mark(_: &Theme) -> container::Style {
    container::Style {
        background: Some(Background::Color(ACCENT)),
        text_color: Some(Color::WHITE),
        border: radius(8.0),
        ..container::Style::default()
    }
}

/// The header strip atop the Diff View, carrying the file path and counts.
pub fn diff_header(_: &Theme) -> container::Style {
    container::Style {
        background: Some(Background::Color(BG_ELEVATED)),
        text_color: Some(TEXT),
        border: Border {
            color: BORDER,
            width: 1.0,
            radius: 8.0.into(),
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

/// A file-selection checkbox: a soft outlined box that fills with the accent
/// when checked, with a white tick. The border brightens to the accent on hover.
pub fn check(_theme: &Theme, status: checkbox::Status) -> checkbox::Style {
    let checked = matches!(
        status,
        checkbox::Status::Active { is_checked: true }
            | checkbox::Status::Hovered { is_checked: true }
            | checkbox::Status::Disabled { is_checked: true }
    );
    let hovered = matches!(status, checkbox::Status::Hovered { .. });

    let background = if checked {
        ACCENT
    } else if hovered {
        BG_HOVER
    } else {
        BG_ELEVATED
    };

    checkbox::Style {
        background: Background::Color(background),
        icon_color: Color::WHITE,
        border: Border {
            color: if checked || hovered { ACCENT } else { BORDER },
            width: 1.5,
            radius: 6.0.into(),
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

/// A File List row: transparent at rest, washed on hover, a soft accent veil
/// when selected. The crisp selection cue is the accent bar drawn by the row
/// itself (see [`selection_bar`]), so no loud border is needed here.
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
            border: radius(8.0),
            ..button::Style::default()
        }
    }
}

/// A thin vertical bar at the left edge of a File List row. Always occupies the
/// same space (so selecting a file never shifts the layout); only its color
/// changes — accent when the row is active, invisible otherwise.
pub fn selection_bar(active: bool) -> impl Fn(&Theme) -> container::Style {
    move |_| container::Style {
        background: Some(Background::Color(if active { ACCENT } else { Color::TRANSPARENT })),
        border: radius(2.0),
        ..container::Style::default()
    }
}

/// The shared bordered "pill" used by every command button — remote actions
/// (Push / Pull) and the File List toolbar (Stage / Unstage / Refresh). A faint
/// inset surface at rest, accent-tinted border and wash on hover.
pub fn secondary(_theme: &Theme, status: button::Status) -> button::Style {
    let (background, border_color, text_color) = match status {
        button::Status::Hovered | button::Status::Pressed => (ACCENT_SOFT, ACCENT, TEXT),
        button::Status::Disabled => (Color::TRANSPARENT, with_alpha(BORDER, 0.5), TEXT_FAINT),
        _ => (BG_ELEVATED, BORDER, TEXT),
    };

    button::Style {
        background: Some(Background::Color(background)),
        text_color,
        border: Border {
            color: border_color,
            width: 1.0,
            radius: 8.0.into(),
        },
        ..button::Style::default()
    }
}

/// A view tab (Changes / History): the active one is accent-tinted and filled,
/// inactive ones are quiet muted text that wash on hover.
pub fn tab(active: bool) -> impl Fn(&Theme, button::Status) -> button::Style {
    move |_theme, status| {
        let (background, text_color) = if active {
            (Some(Background::Color(ACCENT_SOFT)), ACCENT)
        } else {
            match status {
                button::Status::Hovered | button::Status::Pressed => {
                    (Some(Background::Color(BG_HOVER)), TEXT)
                }
                _ => (None, TEXT_MUTED),
            }
        };

        button::Style {
            background,
            text_color,
            border: radius(7.0),
            ..button::Style::default()
        }
    }
}

/// The danger variant of [`secondary`], for Discard: the same pill shape, in red.
pub fn secondary_danger(_theme: &Theme, status: button::Status) -> button::Style {
    let (background, border_color, text_color) = match status {
        button::Status::Hovered | button::Status::Pressed => {
            (with_alpha(RED, 0.18), RED, RED)
        }
        button::Status::Disabled => (Color::TRANSPARENT, with_alpha(BORDER, 0.5), TEXT_FAINT),
        _ => (BG_ELEVATED, with_alpha(RED, 0.45), with_alpha(RED, 0.9)),
    };

    button::Style {
        background: Some(Background::Color(background)),
        text_color,
        border: Border {
            color: border_color,
            width: 1.0,
            radius: 8.0.into(),
        },
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
