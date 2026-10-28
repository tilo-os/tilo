//! Rows written by column header rather than by position: each record is an
//! object keyed by header names, so a value left out leaves its own cell
//! instead of shifting every value after it into the wrong column.
//!
//! `new-sheet` takes `columns` and `records`; each sheet offers
//! `set-records`, which reads the headers from the sheet's first row. In a
//! record's formula `[Header]` is that column's cell in the record's own row
//! and `{row}` that row's number, so a caller never counts rows to write
//! `=B7*C7`.

use ui::tilo_resource::Refusal;

use crate::cell_actions::{SearchableCell, cell_input};
use crate::model::{Addr, addr_to_a1};
use crate::resource::{CellWrite, MAX_WRITES};
use crate::workbook::SheetSnapshot;

/// How many header names a refusal lists.
const NAMED_COLUMNS_LISTED: usize = 30;

/// The headers records are keyed by, each with its column.
pub(crate) struct Columns(Vec<(String, u32)>);

fn fold(name: &str) -> String {
    name.split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .to_lowercase()
}

/// Whether two cell texts name the same key: alike but for case and
/// surrounding space, or the same number (`7` and `7.0`).
fn same_key(a: &str, b: &str) -> bool {
    match (a.trim().parse::<f64>(), b.trim().parse::<f64>()) {
        (Ok(a), Ok(b)) => (a - b).abs() <= f64::EPSILON * a.abs().max(b.abs()),
        _ => fold(a) == fold(b),
    }
}

impl Columns {
    /// `names`, a new sheet's header names, in columns from A.
    ///
    /// # Errors
    /// A name that is empty or repeats another, or none at all.
    pub(crate) fn from_names(names: &[String]) -> Result<Self, Refusal> {
        let mut columns: Vec<(String, u32)> = Vec::new();
        for (col, name) in (0u32..).zip(names) {
            let name = name.trim();
            if name.is_empty() {
                return Err(Refusal::new("a header name in `columns` is empty"));
            }
            if columns.iter().any(|(other, _)| fold(other) == fold(name)) {
                return Err(Refusal::new(format!("`columns` names `{name}` twice")));
            }
            columns.push((name.to_owned(), col));
        }
        if columns.is_empty() {
            return Err(Refusal::new("`columns` names no headers"));
        }
        Ok(Self(columns))
    }

    /// The headers in the first row of a sheet whose cells show as `shown`;
    /// `None` when that row is empty.
    fn of_sheet(shown: &[SearchableCell]) -> Option<Self> {
        let columns: Vec<(String, u32)> = shown
            .iter()
            .filter(|cell| cell.row == 0)
            .filter_map(|cell| {
                let text = if cell.display.trim().is_empty() {
                    cell.raw.trim()
                } else {
                    cell.display.trim()
                };
                (!text.is_empty()).then(|| (text.to_owned(), cell.col))
            })
            .collect();
        (!columns.is_empty()).then_some(Self(columns))
    }

    /// The header row itself, for a sheet that has none yet.
    fn header_writes(&self) -> Vec<CellWrite> {
        self.0
            .iter()
            .map(|(name, col)| CellWrite::at((*col, 0), name.clone(), None))
            .collect()
    }

    /// The column `key` names: its header exactly, else ignoring case and
    /// surrounding space.
    ///
    /// # Errors
    /// No header matches: the refusal lists the headers.
    fn column(&self, key: &str) -> Result<u32, Refusal> {
        self.0
            .iter()
            .find(|(name, _)| name == key)
            .or_else(|| self.0.iter().find(|(name, _)| fold(name) == fold(key)))
            .map(|(_, col)| *col)
            .ok_or_else(|| {
                let mut names: Vec<String> = self
                    .0
                    .iter()
                    .take(NAMED_COLUMNS_LISTED)
                    .map(|(name, _)| format!("`{name}`"))
                    .collect();
                if self.0.len() > NAMED_COLUMNS_LISTED {
                    names.push(format!("and {} more", self.0.len() - NAMED_COLUMNS_LISTED));
                }
                Refusal::new(format!(
                    "no column `{key}`; the columns are {}",
                    names.join(", ")
                ))
            })
    }

    /// The leftmost header, which identifies a row when no `key` is named.
    fn first(&self) -> Option<&(String, u32)> {
        self.0.iter().min_by_key(|(_, col)| *col)
    }
}

/// `formula` in sheet row `row` (0-based) with `{row}` as that row's number
/// and each `[Header]` as that column's cell in it; text in quotes is kept.
///
/// # Errors
/// A `[Header]` names no column, or a `[` is never closed.
fn expand_formula(formula: &str, row: u32, columns: &Columns) -> Result<String, Refusal> {
    /// The placeholder a record's formula names its own row by.
    const ROW: &str = "{row}";
    let number = row.saturating_add(1).to_string();
    let mut out = String::with_capacity(formula.len());
    let mut quote = None;
    let mut rest = formula;
    while let Some(ch) = rest.chars().next() {
        let after = rest.get(ch.len_utf8()..).unwrap_or_default();
        match (quote, ch) {
            (Some(open), _) if ch == open => quote = None,
            (None, '"' | '\'') => quote = Some(ch),
            (None, '{') if rest.starts_with(ROW) => {
                out.push_str(&number);
                rest = rest.get(ROW.len()..).unwrap_or_default();
                continue;
            }
            (None, '[') => {
                let (header, tail) = after.split_once(']').ok_or_else(|| {
                    Refusal::new(format!("`{formula}` opens a `[` it never closes"))
                })?;
                let col = columns
                    .column(header)
                    .map_err(|error| Refusal::new(format!("in `{formula}`: {}", error.0)))?;
                out.push_str(&addr_to_a1(col, row));
                rest = tail;
                continue;
            }
            (Some(_) | None, _) => {}
        }
        out.push(ch);
        rest = after;
    }
    Ok(out)
}

/// The write of JSON `value` into the cell at `addr`, as `set-cells` reads
/// it; `None` for null, which leaves the cell.
fn write_of(addr: Addr, value: &serde_json::Value) -> Result<Option<CellWrite>, Refusal> {
    Ok(cell_input(value)?.map(|(input, format)| CellWrite::at(addr, input, format)))
}

/// The cells `records` write by header from sheet row `first_row` (0-based)
/// down, one record a row: a key left out (or null) leaves its cell.
///
/// # Errors
/// A record that is not an object, a key no header names, a value that is
/// not a cell value, or more than [`MAX_WRITES`] cells.
fn record_writes(
    columns: &Columns,
    rows: impl IntoIterator<Item = u32>,
    records: &[serde_json::Value],
) -> Result<Vec<CellWrite>, Refusal> {
    let mut writes = Vec::new();
    for (row, record) in rows.into_iter().zip(records) {
        let record = object(record)?;
        for (key, value) in record {
            let col = columns.column(key)?;
            let value = match value.as_str() {
                Some(text) if text.starts_with('=') => {
                    serde_json::Value::String(expand_formula(text, row, columns)?)
                }
                _ => value.clone(),
            };
            writes.extend(write_of((col, row), &value)?);
        }
        if writes.len() > MAX_WRITES {
            return Err(Refusal::new(format!("at most {MAX_WRITES} cells at once")));
        }
    }
    Ok(writes)
}

fn object(
    record: &serde_json::Value,
) -> Result<&serde_json::Map<String, serde_json::Value>, Refusal> {
    record
        .as_object()
        .ok_or_else(|| Refusal::new("each record is an object of values keyed by header"))
}

/// A new sheet's first cells: `columns` as its header row and `records`
/// under it.
///
/// # Errors
/// Why the columns or records cannot be written.
pub(crate) fn new_sheet_writes(
    columns: &[String],
    records: &[serde_json::Value],
) -> Result<Vec<CellWrite>, Refusal> {
    let columns = Columns::from_names(columns)?;
    let mut writes = columns.header_writes();
    writes.extend(record_writes(&columns, 1.., records)?);
    Ok(writes)
}

/// The keys of `records` in the order they first come: an empty sheet's
/// headers when none are given.
pub(crate) fn keys_in_order(records: &[serde_json::Value]) -> Vec<String> {
    let mut keys: Vec<String> = Vec::new();
    for key in records
        .iter()
        .filter_map(serde_json::Value::as_object)
        .flat_map(serde_json::Map::keys)
    {
        if !keys.contains(key) {
            keys.push(key.clone());
        }
    }
    keys
}

/// The record value of the header `header`, ignoring case.
fn value_of<'a>(record: &'a serde_json::Value, header: &str) -> Option<&'a serde_json::Value> {
    record
        .as_object()?
        .iter()
        .find(|(name, _)| fold(name) == fold(header))
        .map(|(_, value)| value)
        .filter(|value| !value.is_null())
}

/// The input a key value writes, to compare with what a row holds.
fn key_text(value: &serde_json::Value) -> Option<String> {
    cell_input(value).ok().flatten().map(|(input, _)| input)
}

/// The first sheet row after the last one used, at least the row under the
/// headers.
fn next_free_row(snap: &SheetSnapshot) -> u32 {
    snap.raw
        .keys()
        .map(|&(_, row)| row.saturating_add(1))
        .max()
        .unwrap_or(1)
        .max(1)
}

/// What a `set-records` call asks.
pub(crate) struct RecordsRequest<'a> {
    pub records: &'a [serde_json::Value],
    /// Data row of the first record, 1 = the row under the headers.
    pub row: Option<u32>,
    /// An empty sheet's headers.
    pub columns: Option<&'a [String]>,
    /// The header whose values identify the rows to update.
    pub key: Option<&'a str>,
}

/// The writes of `request` into the sheet `snap`, whose cells show as
/// `shown`.
///
/// # Errors
/// Why the records cannot be written: no headers to key by on a sheet that
/// has cells, a key no header names, a row that changed since it was read.
pub(crate) fn set_records_writes(
    snap: &SheetSnapshot,
    shown: &[SearchableCell],
    request: &RecordsRequest<'_>,
) -> Result<Vec<CellWrite>, Refusal> {
    let records = request.records;
    if records.is_empty() {
        return Err(Refusal::new("give at least one record"));
    }
    if request.key.is_some() && request.row.is_some() {
        return Err(Refusal::new(
            "give `key` to update rows by their key, or `row` to write at a row, not both",
        ));
    }
    let Some(columns) = Columns::of_sheet(shown) else {
        if !snap.raw.is_empty() {
            return Err(Refusal::new(format!(
                "`{}` has no headers in row 1 to key records by: write them first",
                snap.name
            )));
        }
        // An empty sheet takes its header row from the records.
        let names = request
            .columns
            .map_or_else(|| keys_in_order(records), <[String]>::to_vec);
        return new_sheet_writes(&names, records);
    };
    let named = |error: Refusal| {
        if error.0.starts_with("no column") {
            Refusal::new(format!(
                "sheet `{}` has {}; write the records to the sheet whose headers they name",
                snap.name, error.0
            ))
        } else {
            error
        }
    };
    if let Some(key) = request.key {
        let rows = keyed_rows(snap, &columns, key, records).map_err(named)?;
        return record_writes(&columns, rows, records).map_err(named);
    }
    let first = match request.row {
        Some(row) if row >= 1 => {
            if let Some(refusal) = stale_row(snap, &columns, row, records) {
                return Err(refusal);
            }
            row
        }
        Some(_) => {
            return Err(Refusal::new(
                "`row` is a data row number: 1 is the row under the headers",
            ));
        }
        None => next_free_row(snap),
    };
    record_writes(&columns, first.., records).map_err(named)
}

/// The writes of an `upsert-item` on the sheet `snap`, whose cells show as
/// `shown`: `value` (by header) into the one data row whose cells under the
/// headers `matching` names hold its values, else into the next free row
/// with `matching`'s values beside it. A sheet with a header row is a keyed
/// collection of its rows, so the generic upsert reaches it as it reaches
/// any collection's items. Returns the writes and, when the row is new, the
/// row it goes to, which the caller inserts afresh.
///
/// # Errors
/// `matching` is empty or names no header, more than one row matches, or a
/// value is not a cell value.
pub(crate) fn upsert_writes(
    snap: &SheetSnapshot,
    shown: &[SearchableCell],
    matching: &serde_json::Map<String, serde_json::Value>,
    value: &serde_json::Map<String, serde_json::Value>,
) -> Result<(Vec<CellWrite>, Option<u32>), Refusal> {
    if matching.is_empty() {
        return Err(Refusal::new(
            "give `match`: the header values that identify the row, e.g. {\"Name\": \"Ana\"}",
        ));
    }
    let mut inserted = value.clone();
    for (key, wanted) in matching {
        if !inserted.keys().any(|named| fold(named) == fold(key)) {
            inserted.insert(key.clone(), wanted.clone());
        }
    }
    let inserted = [serde_json::Value::Object(inserted)];
    let Some(columns) = Columns::of_sheet(shown) else {
        if !snap.raw.is_empty() {
            return Err(Refusal::new(format!(
                "`{}` has no headers in row 1 to match rows by: write them first",
                snap.name
            )));
        }
        return Ok((
            new_sheet_writes(&keys_in_order(&inserted), &inserted)?,
            None,
        ));
    };
    let mut keys = Vec::new();
    for (key, wanted) in matching {
        let text = key_text(wanted)
            .ok_or_else(|| Refusal::new(format!("`match` gives `{key}` no cell value")))?;
        keys.push((key.as_str(), columns.column(key)?, text));
    }
    let mut rows: Vec<u32> = snap
        .raw
        .iter()
        .filter(|((_, row), raw)| *row >= 1 && !raw.trim().is_empty())
        .map(|((_, row), _)| *row)
        .collect();
    rows.sort_unstable();
    rows.dedup();
    let held = |col: u32, row: u32| snap.raw.get(&(col, row)).map_or("", String::as_str);
    let found: Vec<u32> = rows
        .into_iter()
        .filter(|row| {
            keys.iter()
                .all(|(_, col, text)| same_key(held(*col, *row), text))
        })
        .collect();
    // A matched row keeps its key cells as written there.
    let (row, record, appended) = match found.as_slice() {
        [] => {
            let row = next_free_row(snap);
            (row, inserted, Some(row))
        }
        [row] => (*row, [serde_json::Value::Object(value.clone())], None),
        [first, second, ..] => {
            return Err(Refusal::new(format!(
                "`match` fits rows {} and {} of `{}`: name more headers so it fits one",
                first + 1,
                second + 1,
                snap.name
            )));
        }
    };
    Ok((record_writes(&columns, [row], &record)?, appended))
}

/// The sheet row each record goes to when rows are identified by the header
/// `key`: the row whose key cell holds the record's key, else the next
/// free rows in order.
fn keyed_rows(
    snap: &SheetSnapshot,
    columns: &Columns,
    key: &str,
    records: &[serde_json::Value],
) -> Result<Vec<u32>, Refusal> {
    let col = columns.column(key)?;
    let mut held: Vec<(u32, &str)> = snap
        .raw
        .iter()
        .filter(|((c, row), raw)| *c == col && *row >= 1 && !raw.trim().is_empty())
        .map(|((_, row), raw)| (*row, raw.as_str()))
        .collect();
    held.sort_unstable_by_key(|(row, _)| *row);
    let mut next = next_free_row(snap);
    let mut wanted: Vec<String> = Vec::new();
    let mut rows = Vec::new();
    for record in records {
        object(record)?;
        let text = value_of(record, key).and_then(key_text).ok_or_else(|| {
            Refusal::new(format!(
                "every record names its `{key}`, which identifies its row"
            ))
        })?;
        if wanted.iter().any(|other| same_key(other, &text)) {
            return Err(Refusal::new(format!(
                "the records name `{key}` `{text}` twice: give each row once"
            )));
        }
        let matching: Vec<u32> = held
            .iter()
            .filter(|(_, raw)| same_key(raw, &text))
            .map(|(row, _)| *row)
            .collect();
        let row = match matching.as_slice() {
            [] => {
                let row = next;
                next = next.saturating_add(1);
                row
            }
            [row] => *row,
            [first, second, ..] => {
                return Err(Refusal::new(format!(
                    "`{key}` `{text}` is on rows {} and {} of `{}`: which one is meant is \
                     unclear; write the row with set-cells",
                    first + 1,
                    second + 1,
                    snap.name
                )));
            }
        };
        wanted.push(text);
        rows.push(row);
    }
    Ok(rows)
}

/// Why `records`, written from data row `first` by an explicit `row`, would
/// overwrite a row that is no longer the one the caller read: a record that
/// names the key column (the leftmost header, which identifies a row) with
/// a value other than the one its target row already holds there. Another
/// collaborator may have appended or sorted rows since the caller read the
/// sheet, and a stale row number would then silently replace their row. An
/// empty target row, a matching key, or a record leaving the key out (an
/// update of that row's other cells) is written.
fn stale_row(
    snap: &SheetSnapshot,
    columns: &Columns,
    first: u32,
    records: &[serde_json::Value],
) -> Option<Refusal> {
    let (key, key_col) = columns.first()?;
    for (row, record) in (first..).zip(records) {
        let Some(written) = value_of(record, key).and_then(key_text) else {
            continue;
        };
        let held = snap.raw.get(&(*key_col, row)).map_or("", |raw| raw.trim());
        if held.is_empty() || same_key(held, &written) {
            continue;
        }
        return Some(Refusal::new(format!(
            "data row {row} of `{}` holds `{held}` under `{key}`, not `{written}`: the sheet \
             changed since it was read. Read it again and write to the row that holds \
             `{written}`, give `key` to find it, or leave `row` out to append (to rename the \
             row's `{key}`, write that cell with set-cells)",
            snap.name
        )));
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn columns(names: &[&str]) -> Columns {
        Columns::from_names(
            &names
                .iter()
                .map(|name| (*name).to_owned())
                .collect::<Vec<_>>(),
        )
        .unwrap()
    }

    #[test]
    fn formulas_name_columns_and_their_row() {
        let cols = columns(&["Item", "Per person", "People", "Total"]);
        assert_eq!(
            expand_formula("=[Per person]*[people]", 4, &cols).unwrap(),
            "=B5*C5"
        );
        assert_eq!(
            expand_formula("=B{row}*C{row}", 1, &cols).unwrap(),
            "=B2*C2"
        );
        assert_eq!(
            expand_formula("=IF([Total]>0,\"[x] {row}\",'[Q]'!A1)", 2, &cols).unwrap(),
            "=IF(D3>0,\"[x] {row}\",'[Q]'!A1)",
            "quoted text is kept"
        );
        let error = expand_formula("=[Cost]*2", 1, &cols).unwrap_err();
        assert!(
            error.0.contains("no column `Cost`") && error.0.contains("`People`"),
            "{}",
            error.0
        );
        assert!(expand_formula("=[Total", 1, &cols).is_err());
    }

    #[test]
    fn columns_are_named_once_each() {
        for names in [vec!["A", "a "], vec!["A", ""], vec![]] {
            let names: Vec<String> = names.into_iter().map(str::to_owned).collect();
            assert!(Columns::from_names(&names).is_err(), "{names:?}");
        }
    }

    #[test]
    fn a_left_out_key_leaves_its_cell() {
        let cols = columns(&["Item", "Per person", "People", "Total"]);
        let writes = record_writes(
            &cols,
            1..,
            &[json!({"Item": "Venue", "Per person": 145, "Total": "=[Per person]*[People]"})],
        )
        .unwrap();
        let mut at: Vec<(String, String)> = writes
            .into_iter()
            .map(|write| (write.cell, write.input.unwrap_or_default()))
            .collect();
        at.sort_unstable();
        assert_eq!(
            at,
            [
                ("A2".to_owned(), "Venue".to_owned()),
                ("B2".to_owned(), "145".to_owned()),
                ("D2".to_owned(), "=B2*C2".to_owned()),
            ]
        );
    }

    #[test]
    fn keys_match_ignoring_case_and_number_spelling() {
        assert!(same_key(" Venue", "venue"));
        assert!(same_key("7", "7.0"));
        assert!(!same_key("7", "8"));
    }
}
