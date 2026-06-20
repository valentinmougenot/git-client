//! Intra-line ("word level") diffing for the Diff View.
//!
//! A standard line diff shows a changed line as a deletion plus an addition,
//! leaving the reader to spot which words actually changed. This module pairs a
//! removed line with the added line that replaced it and reports the precise
//! character ranges that differ on each side, so the Diff View can tint just
//! those words rather than the whole row.
//!
//! The algorithm tokenizes each line, runs a Longest Common Subsequence over the
//! tokens, and treats every token outside that subsequence as changed. It bails
//! out (returning `None`) when the two lines share too little to make a
//! word-level highlight meaningful — there, the full-row tint reads better.

/// The changed character ranges within a paired deletion/addition, expressed as
/// half-open `[start, end)` ranges over each line's `content` (char indices).
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Emphasis {
    pub old: Vec<(usize, usize)>,
    pub new: Vec<(usize, usize)>,
}

/// A token: a half-open char range over its source line plus its text. Words
/// (alphanumeric/underscore runs) and whitespace runs each form one token;
/// every other character is its own token, so punctuation diffs precisely.
struct Token {
    start: usize,
    end: usize,
    text: String,
}

fn tokenize(line: &str) -> Vec<Token> {
    let chars: Vec<char> = line.chars().collect();
    let mut tokens = Vec::new();
    let mut i = 0;
    while i < chars.len() {
        let c = chars[i];
        let start = i;
        if c.is_alphanumeric() || c == '_' {
            while i < chars.len() && (chars[i].is_alphanumeric() || chars[i] == '_') {
                i += 1;
            }
        } else if c.is_whitespace() {
            while i < chars.len() && chars[i].is_whitespace() {
                i += 1;
            }
        } else {
            i += 1;
        }
        tokens.push(Token {
            start,
            end: i,
            text: chars[start..i].iter().collect(),
        });
    }
    tokens
}

/// Compute the changed ranges between a removed line and the added line that
/// replaced it, or `None` when the lines are too dissimilar to be worth a
/// word-level highlight (the caller should fall back to the full-row tint).
pub fn diff(old: &str, new: &str) -> Option<Emphasis> {
    if old == new {
        return None;
    }
    let old_tokens = tokenize(old);
    let new_tokens = tokenize(new);
    if old_tokens.is_empty() || new_tokens.is_empty() {
        return None;
    }

    // LCS over token text. `matched` marks, for each side, whether a token is
    // part of the common subsequence (unchanged).
    let (old_matched, new_matched) = lcs(&old_tokens, &new_tokens);

    // Skip when too little is shared: highlighting nearly every word is just
    // noise over the existing row tint.
    let common: usize = old_tokens
        .iter()
        .zip(&old_matched)
        .filter(|(_, m)| **m)
        .map(|(t, _)| t.text.chars().count())
        .sum();
    let shorter = old.chars().count().min(new.chars().count());
    if shorter == 0 || common * 4 < shorter {
        return None;
    }

    let emphasis = Emphasis {
        old: ranges(&old_tokens, &old_matched),
        new: ranges(&new_tokens, &new_matched),
    };
    if emphasis.old.is_empty() && emphasis.new.is_empty() {
        return None;
    }
    Some(emphasis)
}

/// Standard LCS DP over token equality, returning a "is in the common
/// subsequence" flag per token on each side.
fn lcs(old: &[Token], new: &[Token]) -> (Vec<bool>, Vec<bool>) {
    let (n, m) = (old.len(), new.len());
    // table[i][j] = LCS length of old[i..] and new[j..].
    let mut table = vec![vec![0u16; m + 1]; n + 1];
    for i in (0..n).rev() {
        for j in (0..m).rev() {
            table[i][j] = if old[i].text == new[j].text {
                table[i + 1][j + 1] + 1
            } else {
                table[i + 1][j].max(table[i][j + 1])
            };
        }
    }

    let mut old_matched = vec![false; n];
    let mut new_matched = vec![false; m];
    let (mut i, mut j) = (0, 0);
    while i < n && j < m {
        if old[i].text == new[j].text {
            old_matched[i] = true;
            new_matched[j] = true;
            i += 1;
            j += 1;
        } else if table[i + 1][j] >= table[i][j + 1] {
            i += 1;
        } else {
            j += 1;
        }
    }
    (old_matched, new_matched)
}

/// Collapse runs of unmatched (changed) tokens into merged char ranges.
fn ranges(tokens: &[Token], matched: &[bool]) -> Vec<(usize, usize)> {
    let mut out: Vec<(usize, usize)> = Vec::new();
    for (token, is_matched) in tokens.iter().zip(matched) {
        if *is_matched {
            continue;
        }
        match out.last_mut() {
            Some(last) if last.1 == token.start => last.1 = token.end,
            _ => out.push((token.start, token.end)),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn highlights_only_the_changed_word() {
        let e = diff("let x = compute(1);", "let x = compute(2);").unwrap();
        // Only the `1` / `2` differ.
        assert_eq!(e.old.len(), 1);
        let (s, end) = e.old[0];
        assert_eq!("let x = compute(1);".chars().skip(s).take(end - s).collect::<String>(), "1");
        let (s, end) = e.new[0];
        assert_eq!("let x = compute(2);".chars().skip(s).take(end - s).collect::<String>(), "2");
    }

    #[test]
    fn merges_adjacent_changed_tokens() {
        let e = diff("foo = bar", "foo = bazqux").unwrap();
        // `bar` -> `bazqux`: one contiguous changed range on each side.
        assert_eq!(e.old.len(), 1);
        assert_eq!(e.new.len(), 1);
    }

    #[test]
    fn identical_lines_have_no_emphasis() {
        assert_eq!(diff("same line", "same line"), None);
    }

    #[test]
    fn wholly_different_lines_bail_out() {
        assert_eq!(diff("alpha beta gamma", "1 + 2 * 3"), None);
    }

    #[test]
    fn ranges_stay_within_bounds() {
        let old = "    return value + 1;";
        let new = "    return value + 2;";
        let e = diff(old, new).unwrap();
        let len = new.chars().count();
        for (s, end) in e.new {
            assert!(s < end && end <= len);
        }
    }
}
