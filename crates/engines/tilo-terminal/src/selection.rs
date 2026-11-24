//! Selections on a screen: the rows and columns a flow selection covers, and the words it holds.
//!
//! A cell column is not a byte offset: a wide glyph takes two cells, and a copy cuts whole
//! glyphs. Everything here is measured in cells, as the grid draws them.

use unicode_width::UnicodeWidthChar;

use crate::{GridPos, Screen, Selection};

/// A flow selection as per-row cell spans `(row, start, end)`, `end` excluded.
#[must_use]
pub fn row_spans(selection: &Selection, columns: usize) -> Vec<(usize, usize, usize)> {
    let (start, end) = selection.ordered();
    if start == end {
        return Vec::new();
    }
    let mut spans = Vec::new();
    if start.row == end.row {
        spans.push((start.row, start.col, end.col));
    } else {
        spans.push((start.row, start.col, columns));
        for row in (start.row + 1)..end.row {
            spans.push((row, 0, columns));
        }
        spans.push((end.row, 0, end.col));
    }
    spans
}

/// The span a selection takes on `row`, when it covers any of it.
#[must_use]
pub fn span_on(selection: &Selection, row: usize, columns: usize) -> Option<(usize, usize)> {
    let (start, end) = selection.ordered();
    if start == end || row < start.row || row > end.row {
        return None;
    }
    let from = if row == start.row { start.col } else { 0 };
    let to = if row == end.row { end.col } else { columns };
    Some((from, to))
}

/// The byte offset in `line` where the cell `column` begins, honouring wide glyphs.
#[must_use]
pub fn col_to_byte(line: &str, column: usize) -> usize {
    let mut cells = 0usize;
    for (byte, character) in line.char_indices() {
        if cells >= column {
            return byte;
        }
        cells += character.width().unwrap_or(1).max(1);
    }
    line.len()
}

/// The cell column `byte` falls in, honouring wide glyphs.
#[must_use]
pub fn byte_to_col(line: &str, byte: usize) -> usize {
    let mut cells = 0usize;
    for (at, character) in line.char_indices() {
        if at >= byte {
            return cells;
        }
        cells += character.width().unwrap_or(1).max(1);
    }
    cells
}

/// The words a selection holds, each row's trailing blanks cut, as a copy keeps them.
#[must_use]
pub fn text(screen: &Screen, selection: &Selection, columns: usize) -> Option<String> {
    let spans = row_spans(selection, columns);
    if spans.is_empty() {
        return None;
    }
    let lines = screen.active();
    let mut out = String::new();
    for (index, (row, start, end)) in spans.iter().enumerate() {
        if index > 0 {
            out.push('\n');
        }
        let line = lines.get(*row).map_or("", |line| line.text.as_str());
        let mut cells = 0usize;
        let mut taken = String::new();
        for character in line.chars() {
            let width = character.width().unwrap_or(1).max(1);
            if cells >= *end {
                break;
            }
            if cells >= *start {
                taken.push(character);
            }
            cells += width;
        }
        out.push_str(taken.trim_end());
    }
    Some(out)
}

/// The character at cell column `column`, honouring wide glyphs: both cells of a wide glyph
/// report the glyph, so a press on either places the cursor on it.
#[must_use]
pub fn char_at(line: &str, column: usize) -> Option<char> {
    let mut cells = 0usize;
    for character in line.chars() {
        let width = character.width().unwrap_or(1).max(1);
        if column < cells + width {
            return Some(character);
        }
        cells += width;
    }
    None
}

/// The row and column nearest a point given in cells, clamped inside the grid.
#[must_use]
pub fn nearest(rows: usize, columns: usize, row: isize, column: isize) -> GridPos {
    GridPos::new(
        usize::try_from(row).unwrap_or(0).min(rows.saturating_sub(1)),
        usize::try_from(column)
            .unwrap_or(0)
            .min(columns.saturating_sub(1)),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn line(text: &str) -> std::rc::Rc<crate::Line> {
        std::rc::Rc::new(crate::Line {
            text: text.to_owned(),
            attrs: Vec::new(),
        })
    }

    fn screen(lines: &[&str], cursor: (u64, u64)) -> Screen {
        Screen {
            main: lines.iter().map(|text| line(text)).collect(),
            cursor,
            ..Screen::default()
        }
    }

    /// A drag selects in reading order whichever way it went, and its rows are the grid's.
    #[test]
    fn a_selection_spans_reading_order_and_cuts_whole_glyphs() {
        let selection = Selection {
            anchor: GridPos::new(0, 2),
            cursor: GridPos::new(2, 3),
        };
        assert_eq!(
            row_spans(&selection, 10),
            vec![(0, 2, 10), (1, 0, 10), (2, 0, 3)]
        );
        assert_eq!(span_on(&selection, 1, 10), Some((0, 10)));
        assert_eq!(span_on(&selection, 3, 10), None);
        assert!(Selection::default().is_collapsed());

        let backwards = Selection {
            anchor: GridPos::new(2, 3),
            cursor: GridPos::new(0, 2),
        };
        assert_eq!(row_spans(&backwards, 10), row_spans(&selection, 10));

        // A wide glyph takes two cells: a copy at column 1 of "中a" starts after the glyph.
        assert_eq!(col_to_byte("中a", 0), 0);
        assert_eq!(
            col_to_byte("中a", 1),
            3,
            "a wide glyph has no byte of its own past its first cell: a copy there starts after it"
        );
        assert_eq!(col_to_byte("中a", 2), 3);
        assert_eq!(byte_to_col("中a", 3), 2);
        assert_eq!(char_at("中a", 1), Some('中'));
        assert_eq!(char_at("中a", 2), Some('a'));
    }

    /// The words a selection holds are each row's text with its trailing blanks cut.
    #[test]
    fn a_selection_reads_as_the_text_it_covers() {
        let grid = screen(&["hello world  ", "second line", "tail"], (0, 0));
        let selection = Selection {
            anchor: GridPos::new(0, 0),
            cursor: GridPos::new(1, 6),
        };
        assert_eq!(
            text(&grid, &selection, 12).as_deref(),
            Some("hello world\nsecond"),
            "a row's blanks are cut, the row break kept"
        );
        let nothing = Selection {
            anchor: GridPos::new(1, 2),
            cursor: GridPos::new(1, 2),
        };
        assert_eq!(text(&grid, &nothing, 12), None, "a collapsed selection copies nothing");
    }

    /// A hit test clamps to the grid so a drag past its edge still selects its edge.
    #[test]
    fn a_point_outside_the_grid_lands_on_its_edge() {
        assert_eq!(nearest(24, 80, 5, 7), GridPos::new(5, 7));
        assert_eq!(nearest(24, 80, -3, -9), GridPos::new(0, 0));
        assert_eq!(nearest(24, 80, 40, 200), GridPos::new(23, 79));
        assert_eq!(nearest(0, 0, 4, 4), GridPos::new(0, 0));
    }
}
