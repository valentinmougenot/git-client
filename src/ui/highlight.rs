//! A small, dependency-free syntax highlighter for the Diff View.
//!
//! Diff content arrives line by line and out of order (hunks, context), so this
//! tokenizes each line independently — keywords, strings, numbers, comments,
//! and capitalized type-like identifiers. It is deliberately approximate:
//! constructs that span lines (block comments, multi-line strings) are not
//! tracked across lines. The goal is a readable, on-brand color wash, not a
//! perfect parser.

use iced::Color;

use super::style;

/// The comment/string conventions to apply, picked from a file's extension.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Lang {
    /// `//` line comments, `/* */` blocks, char literals & lifetimes (Rust,
    /// C/C++, JS/TS, Go, Java, …).
    CLike,
    /// `#` line comments (Python, shell, TOML, YAML, Ruby, …).
    Hash,
    /// No comment syntax assumed; still highlights strings and numbers.
    Plain,
}

/// Pick a [`Lang`] from a file path's extension.
pub fn lang_for(path: &str) -> Lang {
    let ext = path.rsplit('.').next().unwrap_or("").to_ascii_lowercase();
    match ext.as_str() {
        "rs" | "c" | "h" | "cpp" | "cc" | "hpp" | "js" | "jsx" | "ts" | "tsx" | "go" | "java"
        | "kt" | "swift" | "scala" | "cs" | "php" | "dart" | "zig" => Lang::CLike,
        "py" | "sh" | "bash" | "zsh" | "rb" | "toml" | "yaml" | "yml" | "ini" | "conf" | "pl"
        | "r" => Lang::Hash,
        _ => Lang::Plain,
    }
}

/// A union of common keywords across the supported languages. Over-matching a
/// keyword that belongs to another language is rare and visually harmless.
const KEYWORDS: &[&str] = &[
    // Rust
    "fn", "let", "mut", "pub", "struct", "enum", "impl", "trait", "use", "mod", "match", "where",
    "ref", "move", "dyn", "crate", "super", "unsafe", "extern", "as", "loop", "self", "Self",
    // Shared control flow / declarations
    "if", "else", "for", "while", "do", "return", "break", "continue", "in", "const", "static",
    "type", "async", "await", "true", "false", "null", "nil", "none", "void",
    // JS / TS / Python / others
    "function", "var", "class", "def", "import", "from", "export", "default", "new", "this",
    "public", "private", "protected", "static", "interface", "extends", "implements", "package",
    "lambda", "pass", "with", "try", "except", "finally", "throw", "catch", "yield", "and", "or",
    "not", "is", "elif",
];

/// Split `content` into colored spans for rendering.
pub fn spans(content: &str, lang: Lang) -> Vec<(String, Color)> {
    let chars: Vec<char> = content.chars().collect();
    let mut out: Vec<(String, Color)> = Vec::new();
    let mut i = 0;

    let push = |out: &mut Vec<(String, Color)>, text: String, color: Color| {
        if !text.is_empty() {
            out.push((text, color));
        }
    };

    while i < chars.len() {
        let c = chars[i];

        // Whitespace run.
        if c.is_whitespace() {
            let start = i;
            while i < chars.len() && chars[i].is_whitespace() {
                i += 1;
            }
            push(&mut out, chars[start..i].iter().collect(), style::TEXT);
            continue;
        }

        // Line comments.
        let line_comment = match lang {
            Lang::CLike => c == '/' && chars.get(i + 1) == Some(&'/'),
            Lang::Hash => c == '#',
            Lang::Plain => false,
        };
        if line_comment {
            push(&mut out, chars[i..].iter().collect(), style::SYN_COMMENT);
            break;
        }

        // Single-line block comment `/* ... */` (or to end of line).
        if lang == Lang::CLike && c == '/' && chars.get(i + 1) == Some(&'*') {
            let start = i;
            i += 2;
            while i < chars.len() && !(chars[i] == '*' && chars.get(i + 1) == Some(&'/')) {
                i += 1;
            }
            i = (i + 2).min(chars.len());
            push(&mut out, chars[start..i].iter().collect(), style::SYN_COMMENT);
            continue;
        }

        // Strings: double and backtick always; single quote unless it reads as
        // a Rust lifetime (`'a`), which is left as punctuation + identifier.
        if c == '"' || c == '`' || (c == '\'' && is_char_literal(&chars, i)) {
            let quote = c;
            let start = i;
            i += 1;
            while i < chars.len() {
                if chars[i] == '\\' {
                    i += 2;
                    continue;
                }
                if chars[i] == quote {
                    i += 1;
                    break;
                }
                i += 1;
            }
            push(&mut out, chars[start..i].iter().collect(), style::SYN_STRING);
            continue;
        }

        // Numbers (with a fractional part, hex, and digit separators/suffixes).
        if c.is_ascii_digit() {
            let start = i;
            while i < chars.len() && (chars[i].is_alphanumeric() || chars[i] == '.' || chars[i] == '_')
            {
                i += 1;
            }
            push(&mut out, chars[start..i].iter().collect(), style::SYN_NUMBER);
            continue;
        }

        // Identifiers / keywords / types.
        if c.is_alphabetic() || c == '_' {
            let start = i;
            while i < chars.len() && (chars[i].is_alphanumeric() || chars[i] == '_') {
                i += 1;
            }
            let word: String = chars[start..i].iter().collect();
            let color = if KEYWORDS.contains(&word.as_str()) {
                style::SYN_KEYWORD
            } else if word.chars().next().is_some_and(|ch| ch.is_uppercase()) {
                style::SYN_TYPE
            } else {
                style::TEXT
            };
            push(&mut out, word, color);
            continue;
        }

        // Anything else (punctuation, operators): a run of symbol characters.
        let start = i;
        while i < chars.len() && is_symbol(chars[i]) {
            i += 1;
        }
        if i == start {
            i += 1; // Always make progress on a lone, unhandled character.
        }
        push(&mut out, chars[start..i].iter().collect(), style::TEXT_MUTED);
    }

    out
}

/// Whether a `'` at `index` opens a char literal (`'a'`, `'\n'`) rather than a
/// Rust lifetime (`'a`). A char literal closes within a few characters.
fn is_char_literal(chars: &[char], index: usize) -> bool {
    match chars.get(index + 1) {
        // Escaped char: `'\n'`, `'\\'`, etc.
        Some('\\') => chars.get(index + 3) == Some(&'\''),
        // Plain char: `'a'` — closes two positions on.
        Some(_) => chars.get(index + 2) == Some(&'\''),
        None => false,
    }
}

/// Symbol characters grouped into a single punctuation span.
fn is_symbol(c: char) -> bool {
    !c.is_alphanumeric() && !c.is_whitespace() && c != '_'
}
