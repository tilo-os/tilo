//! The door's answers as the REPL prints them: the reference that starts a line in a column of
//! its own, a title after it, errors, previews and notes in their colours.

use super::style::{Role, highlight, pad, paint};

/// How wide the column of references is.
const REF_COLUMN: usize = 5;

/// Whether `word` is written as a reference is (`n3`, `t91`).
#[must_use]
pub fn is_ref(word: &str) -> bool {
    let mut chars = word.chars();
    chars.next().is_some_and(|c| c.is_ascii_lowercase())
        && chars.clone().next().is_some()
        && chars.all(|c| c.is_ascii_digit())
}

/// The reference a line of an answer starts with, when it starts with one.
#[must_use]
pub fn line_ref(line: &str) -> Option<&str> {
    let first = line.split(' ').next()?;
    is_ref(first).then_some(first)
}

/// A line of the door's `tilo` answer, coloured; `failed` when the answer is an error.
#[must_use]
pub fn door_line(line: &str, failed: bool) -> String {
    for (head, role) in [
        ("error:", Role::Error),
        ("preview:", Role::Keyword),
        ("waiting:", Role::Note),
        ("undone:", Role::Done),
    ] {
        if let Some(rest) = line.strip_prefix(head) {
            return format!("{}{rest}", paint(role, head));
        }
    }
    if line.starts_with("step ") || line.starts_with("nothing ") || line.starts_with("steps of") {
        return paint(Role::Note, line);
    }
    if let Some(shown) = line_ref(line) {
        let rest = line.get(shown.len()..).unwrap_or_default().trim_start();
        let title = rest
            .strip_prefix('"')
            .and_then(|quoted| quoted.strip_suffix('"'))
            .filter(|title| !title.contains('"'));
        let rest = title.map_or_else(|| highlight(rest), |title| paint(Role::Title, title));
        return format!("{}{rest}", pad(&paint(Role::Ref, shown), REF_COLUMN));
    }
    if failed {
        paint(Role::Error, line)
    } else {
        highlight(line)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_line_that_starts_with_a_reference_reads_as_a_row() {
        super::super::style::set(false);
        assert_eq!(door_line("n1 \"Q3 plan\"", false), "n1   Q3 plan");
        assert_eq!(door_line("t12 (true, 3)", false), "t12  (true, 3)");
        assert_eq!(door_line("16", false), "16");
        assert_eq!(line_ref("n1 \"Q3 plan\""), Some("n1"));
        assert_eq!(line_ref("notes 3"), None);
        assert!(!is_ref("n"));
        assert!(!is_ref("N1"));
    }
}
