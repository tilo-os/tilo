//! Drawing the prompt and the line: everything from the prompt's first row
//! down is drawn again after each edit, wrapped at the terminal's width,
//! and the cursor put where the line's cursor is.

use std::fmt::Write as _;

use unicode_width::UnicodeWidthChar as _;

/// Where the cursor was left, relative to the first row of the prompt.
#[derive(Debug, Default, Clone, Copy)]
pub struct Drawn {
    pub cursor_row: usize,
}

/// A position on the screen, relative to the prompt's first row.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
struct At {
    row: usize,
    col: usize,
}

impl At {
    /// Move past `c`, wrapping at `columns`.
    fn advance(&mut self, c: char, columns: usize) {
        if c == '\n' {
            self.row = self.row.saturating_add(1);
            self.col = 0;
            return;
        }
        let width = c.width().unwrap_or(0);
        if width == 0 {
            return;
        }
        if self.col.saturating_add(width) > columns {
            self.row = self.row.saturating_add(1);
            self.col = 0;
        }
        self.col = self.col.saturating_add(width);
        if self.col >= columns {
            self.row = self.row.saturating_add(1);
            self.col = 0;
        }
    }
}

/// The characters of `prompt` the terminal shows: its escape sequences
/// take no room.
fn visible(prompt: &str) -> String {
    let mut out = String::new();
    let mut chars = prompt.chars().peekable();
    while let Some(c) = chars.next() {
        if c != '\x1b' {
            out.push(c);
            continue;
        }
        match chars.next() {
            Some('[') => {
                for c in chars.by_ref() {
                    if ('\x40'..='\x7e').contains(&c) {
                        break;
                    }
                }
            }
            Some(']') => {
                while let Some(c) = chars.next() {
                    if c == '\x07' {
                        break;
                    }
                    if c == '\x1b' && chars.peek() == Some(&'\\') {
                        chars.next();
                        break;
                    }
                }
            }
            _ => {}
        }
    }
    out
}

/// What to write to show `prompt` and `text` (with the cursor `cursor`
/// characters into `text`), replacing what [`Drawn`] says was drawn last.
pub fn draw(
    previous: Drawn,
    prompt: &str,
    text: &str,
    cursor: usize,
    columns: usize,
) -> (String, Drawn) {
    let columns = columns.max(1);
    let mut out = String::new();
    if previous.cursor_row > 0 {
        let _ = write!(out, "\x1b[{}A", previous.cursor_row);
    }
    out.push_str("\r\x1b[J");
    out.push_str(prompt);
    let mut end = At::default();
    for c in visible(prompt).chars() {
        end.advance(c, columns);
    }
    let mut at_cursor = end;
    let mut last = None;
    for (i, c) in text.chars().enumerate() {
        if i == cursor {
            at_cursor = end;
        }
        if c == '\n' {
            out.push_str("\r\n");
        } else {
            out.push(c);
        }
        end.advance(c, columns);
        last = Some(c);
    }
    if cursor >= text.chars().count() {
        at_cursor = end;
    }
    // Text that fills its last row leaves the terminal's cursor on that
    // row, waiting to wrap; put it where the count says it is.
    if end.col == 0 && end.row > 0 && last.is_some_and(|c| c != '\n') {
        out.push_str("\r\n");
    }
    let up = end.row.saturating_sub(at_cursor.row);
    if up > 0 {
        let _ = write!(out, "\x1b[{up}A");
    }
    out.push('\r');
    if at_cursor.col > 0 {
        let _ = write!(out, "\x1b[{}C", at_cursor.col);
    }
    (
        out,
        Drawn {
            cursor_row: at_cursor.row,
        },
    )
}

/// Put the cursor after everything drawn, on a new row: what is shown
/// stays, and output goes below it.
pub fn finish(previous: Drawn, prompt: &str, text: &str, columns: usize) -> String {
    let columns = columns.max(1);
    let mut end = At::default();
    for c in visible(prompt).chars().chain(text.chars()) {
        end.advance(c, columns);
    }
    let mut out = String::new();
    let down = end.row.saturating_sub(previous.cursor_row);
    if down > 0 {
        let _ = write!(out, "\x1b[{down}B");
    }
    out.push_str("\r\n");
    out
}

/// `names` in columns across `columns`, as bash lists completions.
pub fn table(names: &[String], columns: usize) -> String {
    let width = names
        .iter()
        .map(|n| n.chars().map(|c| c.width().unwrap_or(0)).sum::<usize>())
        .max()
        .unwrap_or(0)
        .saturating_add(2);
    let per_row = columns.checked_div(width).unwrap_or(1).max(1);
    let rows = names.len().div_ceil(per_row);
    let mut out = String::new();
    for row in 0..rows {
        for col in 0..per_row {
            let Some(name) = names.get(col.saturating_mul(rows).saturating_add(row)) else {
                continue;
            };
            out.push_str(name);
            let shown: usize = name.chars().map(|c| c.width().unwrap_or(0)).sum();
            let next = col.saturating_add(1);
            if next < per_row
                && names
                    .get(next.saturating_mul(rows).saturating_add(row))
                    .is_some()
            {
                out.push_str(&" ".repeat(width.saturating_sub(shown)));
            }
        }
        out.push_str("\r\n");
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_line_wider_than_the_terminal_wraps_and_the_cursor_follows() {
        let (out, drawn) = draw(Drawn::default(), "\x1b[1m$\x1b[0m ", "abcdefgh", 3, 5);
        // "$ abc" fills the first row; the cursor sits before "d", at the
        // start of the second.
        assert!(out.starts_with("\r\x1b[J\x1b[1m$\x1b[0m abcdefgh"));
        assert_eq!(drawn.cursor_row, 1);
        assert!(out.ends_with("\x1b[1A\r"), "{out:?}");
        let (again, _) = draw(drawn, "$ ", "", 0, 5);
        assert!(again.starts_with("\x1b[1A\r\x1b[J"));
    }
}
