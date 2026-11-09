//! What a program printed to a terminal, as plain text: the run output a
//! person or an agent reads outside the terminal (History's command views,
//! the parsers that find errors and links in a run) without the escape
//! sequences that drew it.

/// `text` without its escape sequences (CSI, OSC and the other strings,
/// charset designations, other `ESC x`), carriage returns resolved to the
/// last overwrite of each line.
#[must_use]
pub fn plain(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut chars = text.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '\u{1b}' => match chars.next() {
                Some('[') => {
                    for next in chars.by_ref() {
                        if ('\u{40}'..='\u{7e}').contains(&next) {
                            break;
                        }
                    }
                }
                // OSC, and DCS, SOS, PM and APC: a string up to BEL or ST.
                Some(']' | 'P' | 'X' | '^' | '_') => {
                    while let Some(next) = chars.next() {
                        if next == '\u{7}' {
                            break;
                        }
                        if next == '\u{1b}' && chars.peek() == Some(&'\\') {
                            chars.next();
                            break;
                        }
                    }
                }
                // Intermediates, then one final byte: the charset
                // designations `ESC ( B` (which `tput sgr0` prints),
                // `ESC ) 0`, `ESC $ ( B` and the like.
                Some(' '..='/') => {
                    while chars.next_if(|next| (' '..='/').contains(next)).is_some() {}
                    chars.next_if(|next| ('0'..='~').contains(next));
                }
                // Any other `ESC x`, such as `ESC 7` or `ESC =`.
                _ => {}
            },
            '\r' => {
                if chars.peek() == Some(&'\n') {
                    continue;
                }
                let start = out.rfind('\n').map_or(0, |at| at.saturating_add(1));
                out.truncate(start);
            }
            c if c.is_control() && c != '\n' && c != '\t' => {}
            c => out.push(c),
        }
    }
    out
}

/// The last lines of `output`, a run's raw output, as plain text: at most
/// `lines` lines and `bytes` bytes of them, and whether any were left out.
#[must_use]
pub fn tail(output: &[u8], lines: usize, bytes: usize) -> (Vec<String>, bool) {
    let text = plain(&String::from_utf8_lossy(output));
    let mut kept: Vec<String> = Vec::new();
    let mut used = 0_usize;
    let mut all = text.trim_end_matches('\n').split('\n').rev().peekable();
    while let Some(line) = all.next() {
        let line = line.trim_end();
        if kept.len() == lines || used.saturating_add(line.len()) > bytes {
            return (finish(kept), true);
        }
        used = used.saturating_add(line.len());
        kept.push(line.to_owned());
        if all.peek().is_none() {
            break;
        }
    }
    (finish(kept), false)
}

fn finish(mut reversed: Vec<String>) -> Vec<String> {
    reversed.reverse();
    if reversed.len() == 1 && reversed.first().is_some_and(String::is_empty) {
        reversed.clear();
    }
    reversed
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_tail_is_the_last_lines_without_escapes() {
        let output = b"\x1b[1mCompiling\x1b[0m signup\r\nrunning 3 tests\ntest ok\nall passed\n";
        assert_eq!(
            tail(output, 2, 1024),
            (vec!["test ok".to_owned(), "all passed".to_owned()], true)
        );
        assert_eq!(
            tail(output, 10, 1024).0,
            [
                "Compiling signup",
                "running 3 tests",
                "test ok",
                "all passed"
            ]
        );
        assert!(!tail(output, 10, 1024).1);
        assert_eq!(tail(b"", 10, 1024), (Vec::new(), false));
        assert_eq!(
            tail(b"0123456789\nab\n", 10, 4),
            (vec!["ab".to_owned()], true)
        );
    }
}
