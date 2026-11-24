//! A table as a document keeps it: one paragraph whose text is the table's Markdown source
//! (`| a | b |`, a rule row under the head, a row a line). The editor's grid and an app's
//! actions edit a table through these, so a cell written is a splice of that cell's words and a
//! row added is a line spliced in, never the document read back from Markdown.

use std::ops::Range;

/// The attribute that makes a paragraph a table: the paragraph is the table's Markdown source,
/// `true` while an editor shows it as a grid.
pub const ATTR: &str = "table_source";

/// A change to a table's shape, at a cell: rows are counted from the head at 0 and the rule
/// under the head is not a row.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TableEdit {
    /// A blank row under the cell's row.
    AddRow,
    /// A blank column right of the cell's column.
    AddColumn,
    /// The cell's row taken out; a table keeps at least one row.
    DeleteRow,
    /// The cell's column taken out; a table keeps at least one column.
    DeleteColumn,
}

impl TableEdit {
    /// The edit a command names: `add-row`, `add-column`, `delete-row` or `delete-column`, with
    /// or without the editor's `table-` prefix.
    #[must_use]
    pub fn named(name: &str) -> Option<Self> {
        match name.strip_prefix("table-").unwrap_or(name) {
            "add-row" => Some(Self::AddRow),
            "add-column" => Some(Self::AddColumn),
            "delete-row" => Some(Self::DeleteRow),
            "delete-column" => Some(Self::DeleteColumn),
            _ => None,
        }
    }
}

/// A table after a [`TableEdit`]: its new source, its cells' words by row, and the cell the
/// caret moves to (the row or column made, or the one beside what was taken out).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Restructured {
    pub source: String,
    pub rows: Vec<Vec<String>>,
    pub focus: (usize, usize),
}

/// Each line's cells, as byte ranges of `source` with the spaces around each cell left out.
#[must_use]
pub fn ranges(source: &str) -> Vec<Vec<Range<usize>>> {
    let mut offset = 0usize;
    let mut rows = Vec::new();
    for line in source.split_inclusive('\n') {
        let trimmed = line.trim();
        let leading = line.len().saturating_sub(line.trim_start().len());
        let mut delimiters = Vec::new();
        let mut escaped = false;
        for (index, ch) in trimmed.char_indices() {
            if ch == '|' && !escaped {
                delimiters.push(index);
            }
            escaped = ch == '\\' && !escaped;
        }
        if !delimiters.is_empty() {
            let start = usize::from(delimiters.first() == Some(&0));
            let end = if delimiters.last().copied() == trimmed.len().checked_sub(1) {
                trimmed.len().saturating_sub(1)
            } else {
                trimmed.len()
            };
            let mut previous = start;
            let mut cells = Vec::new();
            for boundary in delimiters
                .into_iter()
                .filter(|index| *index >= start && *index < end)
                .chain(std::iter::once(end))
            {
                if let Some(cell) = trimmed.get(previous..boundary) {
                    let left =
                        previous.saturating_add(cell.len().saturating_sub(cell.trim_start().len()));
                    let right = left.saturating_add(cell.trim().len());
                    let base = offset.saturating_add(leading);
                    cells.push(base.saturating_add(left)..base.saturating_add(right));
                }
                previous = boundary.saturating_add(1);
            }
            rows.push(cells);
        }
        offset = offset.saturating_add(line.len());
    }
    rows
}

/// Each line's cells as written, the rule row included.
#[must_use]
pub fn rows(source: &str) -> Vec<Vec<&str>> {
    ranges(source)
        .into_iter()
        .map(|row| {
            row.into_iter()
                .filter_map(|range| source.get(range))
                .collect()
        })
        .collect()
}

/// The rows a person reads, the head first: every line but the rule under the head.
#[must_use]
pub fn data_rows(source: &str) -> Vec<Vec<&str>> {
    rows(source)
        .into_iter()
        .enumerate()
        .filter(|(index, row)| *index != 1 || !is_separator(row))
        .map(|(_, row)| row)
        .collect()
}

/// [`ranges`] of the rows a person reads, the rule under the head left out.
#[must_use]
pub fn data_ranges(source: &str) -> Vec<Vec<Range<usize>>> {
    ranges(source)
        .into_iter()
        .enumerate()
        .filter(|(index, row)| {
            let values: Vec<_> = row
                .iter()
                .filter_map(|range| source.get(range.clone()))
                .collect();
            *index != 1 || !is_separator(&values)
        })
        .map(|(_, row)| row)
        .collect()
}

/// Whether a line is the rule that sets the head apart (`| --- | :-: |`).
#[must_use]
pub fn is_separator(row: &[&str]) -> bool {
    !row.is_empty()
        && row.iter().all(|cell| {
            let cell = cell.trim_matches(':').trim();
            cell.len() >= 3 && cell.chars().all(|ch| ch == '-')
        })
}

/// A cell's words as a person reads them: its escapes and line breaks undone.
#[must_use]
pub fn decode_cell(value: &str) -> String {
    let value = value
        .replace("<br>", "\n")
        .replace("<br/>", "\n")
        .replace("<br />", "\n")
        .replace("&#32;", " ")
        .replace("&#9;", "\t")
        .replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&amp;", "&");
    let mut decoded = String::new();
    let mut chars = value.chars().peekable();
    while let Some(ch) = chars.next() {
        if ch == '\\' && chars.peek().is_some_and(|next| matches!(next, '|' | '\\')) {
            if let Some(next) = chars.next() {
                decoded.push(next);
            }
        } else {
            decoded.push(ch);
        }
    }
    decoded
}

/// A cell's words as the source keeps them: a bar, a line break or an edge space escaped so the
/// row stays one row.
#[must_use]
pub fn encode_cell(value: &str) -> String {
    let normalized = value.replace("\r\n", "\n").replace('\r', "\n");
    let first_content = normalized
        .find(|character| character != ' ')
        .unwrap_or(normalized.len());
    let last_content = normalized
        .rfind(|character| character != ' ')
        .map_or(0, |index| {
            index.saturating_add(
                normalized
                    .get(index..)
                    .and_then(|suffix| suffix.chars().next())
                    .map_or(0, char::len_utf8),
            )
        });
    let mut encoded = String::with_capacity(normalized.len());
    for (index, character) in normalized.char_indices() {
        match character {
            '&' => encoded.push_str("&amp;"),
            '<' => encoded.push_str("&lt;"),
            '>' => encoded.push_str("&gt;"),
            ' ' if index < first_content || index >= last_content => encoded.push_str("&#32;"),
            ' ' => encoded.push(' '),
            '\t' => encoded.push_str("&#9;"),
            '\\' => encoded.push_str("\\\\"),
            '|' => encoded.push_str("\\|"),
            '\n' => encoded.push_str("<br>"),
            other => encoded.push(other),
        }
    }
    encoded
}

/// `source` with the cell at `row` (the head 0) and `column` reading `value`: only that cell's
/// words are rewritten. `None` when the table has no such cell or it already reads `value`.
#[must_use]
pub fn with_cell(source: &str, row: usize, column: usize, value: &str) -> Option<String> {
    let rows = data_ranges(source);
    if column >= rows.iter().map(Vec::len).max().unwrap_or(0) {
        return None;
    }
    let ranges = rows.get(row)?;
    if let Some(current) = ranges
        .get(column)
        .and_then(|range| source.get(range.clone()))
        && decode_cell(current) == value
    {
        return None;
    }
    let encoded = encode_cell(value);
    let mut out = source.to_owned();
    if let Some(range) = ranges.get(column) {
        if range.is_empty() {
            // An empty cell is its padding alone: the words take its place between the bars,
            // a space either side of them.
            let bytes = source.as_bytes();
            let mut start = range.start;
            while start > 0 && bytes.get(start.saturating_sub(1)) == Some(&b' ') {
                start = start.saturating_sub(1);
            }
            let mut end = range.end;
            while bytes.get(end) == Some(&b' ') {
                end = end.saturating_add(1);
            }
            out.replace_range(start..end, &format!(" {encoded} "));
        } else {
            out.replace_range(range.clone(), &encoded);
        }
    } else {
        let (first, last) = (ranges.first()?, ranges.last()?);
        let mut cells: Vec<_> = ranges
            .iter()
            .filter_map(|range| source.get(range.clone()).map(str::to_owned))
            .collect();
        cells.resize(column.saturating_add(1), String::new());
        if let Some(cell) = cells.get_mut(column) {
            *cell = encoded;
        }
        out.replace_range(first.start..last.end, &cells.join(" | "));
    }
    (out != source).then_some(out)
}

/// `source` with `edit` made at the cell at `row` (the head 0) and `column`. `None` when the
/// table has no such cell, or the edit would take its last row or column.
#[must_use]
pub fn restructured(
    source: &str,
    row: usize,
    column: usize,
    edit: TableEdit,
) -> Option<Restructured> {
    let mut rows: Vec<Vec<String>> = data_rows(source)
        .into_iter()
        .map(|row| row.into_iter().map(str::to_owned).collect())
        .collect();
    let columns = rows.iter().map(Vec::len).max().unwrap_or(0);
    if row >= rows.len() || column >= columns {
        return None;
    }
    for row in &mut rows {
        row.resize(columns, String::new());
    }
    let mut divider: Vec<String> = self::rows(source)
        .get(1)
        .filter(|row| is_separator(row))
        .map_or_else(
            || vec!["---".to_owned(); columns],
            |row| row.iter().map(|value| (*value).to_owned()).collect(),
        );
    divider.resize(columns, "---".to_owned());
    let mut focus = (row, column);
    match edit {
        TableEdit::AddRow => {
            focus.0 = row.saturating_add(1);
            rows.insert(focus.0, vec![String::new(); columns]);
        }
        TableEdit::AddColumn => {
            focus.1 = column.saturating_add(1);
            for row in &mut rows {
                row.insert(focus.1, String::new());
            }
            divider.insert(focus.1, "---".into());
        }
        TableEdit::DeleteRow if rows.len() > 1 => {
            rows.remove(row);
            focus.0 = row.min(rows.len().saturating_sub(1));
        }
        TableEdit::DeleteColumn if columns > 1 => {
            for row in &mut rows {
                row.remove(column);
            }
            divider.remove(column);
            focus.1 = column.min(columns.saturating_sub(2));
        }
        TableEdit::DeleteRow | TableEdit::DeleteColumn => return None,
    }
    let mut lines = Vec::new();
    for (index, row) in rows.iter().enumerate() {
        lines.push(format!("| {} |", row.join(" | ")));
        if index == 0 {
            lines.push(format!("| {} |", divider.join(" | ")));
        }
    }
    let mut updated = lines.join("\n");
    if source.ends_with('\n') {
        updated.push('\n');
    }
    Some(Restructured {
        source: updated,
        rows,
        focus,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    const TABLE: &str = "| Name | Age |\n| --- | --- |\n| Ada | 36 |";

    #[test]
    fn a_cell_keeps_readable_internal_spaces_and_exact_edge_spaces() {
        assert_eq!(encode_cell("Mina Chen"), "Mina Chen");
        assert_eq!(decode_cell(&encode_cell(" Mina Chen ")), " Mina Chen ");
        assert_eq!(
            encode_cell("A | B & C"),
            "A \\| B &amp; C",
            "Markdown syntax is escaped without obscuring ordinary spaces"
        );
    }

    #[test]
    fn a_cell_written_rewrites_only_its_own_words() {
        let out = with_cell(TABLE, 1, 1, "37 | almost").unwrap();
        assert_eq!(
            out,
            "| Name | Age |\n| --- | --- |\n| Ada | 37 \\| almost |"
        );
        assert_eq!(decode_cell(data_rows(&out)[1][1]), "37 | almost");
        assert_eq!(
            with_cell(TABLE, 1, 1, "36"),
            None,
            "the same words are no edit"
        );
        assert_eq!(with_cell(TABLE, 5, 0, "x"), None);
        assert_eq!(
            with_cell("| a | b |\n| --- | --- |\n|  |  |", 1, 1, "Ada").unwrap(),
            "| a | b |\n| --- | --- |\n|  | Ada |",
            "an empty cell takes the words between its bars"
        );
    }

    #[test]
    fn rows_and_columns_come_and_go_and_the_rule_follows() {
        let added = restructured(TABLE, 1, 1, TableEdit::AddColumn).unwrap();
        assert_eq!(
            added.source,
            "| Name | Age |  |\n| --- | --- | --- |\n| Ada | 36 |  |"
        );
        assert_eq!(added.focus, (1, 2));
        let row = restructured(&added.source, 1, 0, TableEdit::AddRow).unwrap();
        assert_eq!(data_rows(&row.source).len(), 3);
        let gone = restructured(&row.source, 0, 2, TableEdit::DeleteColumn).unwrap();
        assert_eq!(gone.source.lines().next(), Some("| Name | Age |"));
        let one = restructured(TABLE, 1, 0, TableEdit::DeleteRow).unwrap();
        assert_eq!(restructured(&one.source, 0, 0, TableEdit::DeleteRow), None);
    }
}
