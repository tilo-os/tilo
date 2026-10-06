//! In-buffer find: the match model behind the toolkit's find bar.
//!
//! The bar itself is [`ui::widget::FindBar`]: the query field with its match
//! counter, the Match case and Whole word chips, previous and next, and the
//! replace row. This module is what the editor answers it with: every match
//! of a query in the buffer, in the editor's own `(line, column)` character
//! coordinates, and the same matches as byte ranges for the bar.
//!
//! Matching itself is [`crate::search::line_matches`], the same function the
//! project-wide search uses, so "17 matches" in the bar and 17 rows in the
//! search palette can never come from two different definitions of a match.

use ui::widget::FindMatch;

use crate::search::{Query, line_matches};

/// One match in the active buffer, in the editor's own `(line, column)`
/// character coordinates.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Match {
    pub line: usize,
    pub col: usize,
    pub len: usize,
}

/// Every match of `query` in `text`, in document order.
pub fn matches_in(text: &str, query: &Query) -> Vec<Match> {
    if query.is_empty() {
        return Vec::new();
    }
    let mut out = Vec::new();
    for (line_idx, line) in text.lines().enumerate() {
        for (col, len) in line_matches(line, query) {
            out.push(Match {
                line: line_idx,
                col,
                len,
            });
        }
    }
    out
}

/// `matches` as byte ranges of `text`, the form the find bar speaks.
pub fn byte_ranges(text: &str, matches: &[Match]) -> Vec<FindMatch> {
    let mut line_starts = vec![0usize];
    for (at, ch) in text.char_indices() {
        if ch == '\n' {
            line_starts.push(at + 1);
        }
    }
    matches
        .iter()
        .filter_map(|m| {
            let start_of_line = *line_starts.get(m.line)?;
            let line = text.get(start_of_line..)?;
            let mut chars = line.char_indices();
            let start = chars.nth(m.col).map_or(line.len(), |(at, _)| at);
            let end = if m.len == 0 {
                start
            } else {
                line.char_indices()
                    .nth(m.col + m.len)
                    .map_or(line.len(), |(at, _)| at)
            };
            // A line ends before its newline (and any carriage return).
            let end = end.min(line.find('\n').unwrap_or(line.len()));
            Some(FindMatch::new(start_of_line + start, start_of_line + end))
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn q(n: &str) -> Query {
        Query::new(n)
    }

    #[test]
    fn matches_are_found_across_lines_in_document_order() {
        let text = "let foo = 1;\nfn bar() {}\n// foo again foo\n";
        let m = matches_in(text, &q("foo"));
        assert_eq!(
            m,
            vec![
                Match {
                    line: 0,
                    col: 4,
                    len: 3
                },
                Match {
                    line: 2,
                    col: 3,
                    len: 3
                },
                Match {
                    line: 2,
                    col: 13,
                    len: 3
                },
            ]
        );
    }

    #[test]
    fn an_empty_query_matches_nothing() {
        assert!(matches_in("anything at all", &q("")).is_empty());
    }

    #[test]
    fn byte_ranges_follow_characters_not_bytes() {
        let text = "é foo\nfoo é foo\n";
        let m = matches_in(text, &q("foo"));
        assert_eq!(m.len(), 3);
        let ranges = byte_ranges(text, &m);
        let found: Vec<&str> = ranges
            .iter()
            .map(|r| text.get(r.start..r.end).unwrap_or(""))
            .collect();
        assert_eq!(found, ["foo", "foo", "foo"]);
        // The second `foo` of the file starts after a two-byte `é`.
        assert_eq!(ranges[0].start, "é ".len());
    }
}
