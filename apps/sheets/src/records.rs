//! Rows written by column header rather than by position: each record is an
//! object keyed by header names, so a value left out leaves its own cell
//! empty instead of shifting every value after it into the wrong column.
//!
//! `new-sheet` takes `columns` and `records`; each sheet (and the workbook,
//! naming the sheet) offers `set-records`, which reads the headers from the
//! sheet's first row. In a record's formula `[Header]` is that column's cell
//! in the record's own row and `{row}` that row's number.

use std::sync::Arc;

use tilo_actions::{ActionParam, ActionSpec, ExecutionLocus, ParamType};

use crate::actions::{CellWrite, SheetsAction, encode};
use crate::cell_actions::{MAX_WRITES, cell_write, invalid};
use crate::model::addr_to_a1;
use crate::resource::SheetInfo;

/// How many header names a refusal lists.
const NAMED_COLUMNS_LISTED: usize = 30;

/// The headers records are keyed by, each with its column.
pub(crate) struct Columns(Vec<(String, u32)>);

impl Columns {
    /// `names`, a new sheet's header names, in columns from A.
    ///
    /// # Errors
    ///
    /// A name that is not text, is empty, or repeats another.
    pub(crate) fn from_names(names: &[serde_json::Value]) -> Result<Self, String> {
        let mut columns = Vec::new();
        for (col, name) in (0u32..).zip(names) {
            let name = name
                .as_str()
                .map(str::trim)
                .ok_or("`columns` are header names: an array of text")?;
            if name.is_empty() {
                return Err("a header name in `columns` is empty".into());
            }
            if columns
                .iter()
                .any(|(other, _): &(String, u32)| fold(other) == fold(name))
            {
                return Err(format!("`columns` names `{name}` twice"));
            }
            columns.push((name.to_owned(), col));
        }
        if columns.is_empty() {
            return Err("`columns` names no headers".into());
        }
        Ok(Self(columns))
    }

    /// The headers in `sheet`'s first row, as it shows them.
    ///
    /// # Errors
    ///
    /// The first row is empty, so there are no headers to key by.
    pub(crate) fn of_sheet(sheet: &SheetInfo) -> Result<Self, String> {
        let columns: Vec<(String, u32)> = sheet
            .cells()
            .iter()
            .filter(|cell| cell.row == 0)
            .filter_map(|cell| {
                let shown = if cell.display.trim().is_empty() {
                    cell.raw.trim()
                } else {
                    cell.display.trim()
                };
                (!shown.is_empty()).then(|| (shown.to_owned(), cell.col))
            })
            .collect();
        if columns.is_empty() {
            return Err(format!(
                "`{}` has no headers in row 1 to key records by: write them first",
                sheet.name
            ));
        }
        Ok(Self(columns))
    }

    /// The header row itself, for a new sheet.
    fn header_cells(&self) -> Vec<CellWrite> {
        self.0
            .iter()
            .map(|(name, col)| CellWrite {
                col: *col,
                row: 0,
                raw: name.clone(),
                fmt: None,
            })
            .collect()
    }

    /// The column `key` names: its header exactly, else ignoring case and
    /// surrounding space.
    ///
    /// # Errors
    ///
    /// No header matches: the refusal lists the headers.
    fn column(&self, key: &str) -> Result<u32, String> {
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
                format!("no column `{key}`; the columns are {}", names.join(", "))
            })
    }
}

fn fold(name: &str) -> String {
    name.trim().to_lowercase()
}

/// `formula` in sheet row `row` (0-based) with `{row}` as that row's number
/// and each `[Header]` as that column's cell in it; text in quotes is kept.
///
/// # Errors
///
/// A `[Header]` names no column, or a `[` is never closed.
fn expand_formula(formula: &str, row: u32, columns: &Columns) -> Result<String, String> {
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
                let (header, tail) = after
                    .split_once(']')
                    .ok_or_else(|| format!("`{formula}` opens a `[` it never closes"))?;
                let col = columns
                    .column(header)
                    .map_err(|error| format!("in `{formula}`: {error}"))?;
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

/// The cells `records` write by header from sheet row `first_row` (0-based)
/// down, one record a row: a key left out (or null) leaves its cell.
///
/// # Errors
///
/// A record that is not an object, a key no header names, a value that is
/// not a cell value, or more than [`MAX_WRITES`] cells.
pub(crate) fn cells_from_records(
    columns: &Columns,
    first_row: u32,
    records: &[serde_json::Value],
) -> Result<Vec<CellWrite>, String> {
    let mut cells = Vec::new();
    for (row, record) in (first_row..).zip(records) {
        let record = record
            .as_object()
            .ok_or("each record is an object of values keyed by header")?;
        for (key, value) in record {
            let col = columns.column(key)?;
            let value = match value.as_str() {
                Some(text) if text.starts_with('=') => {
                    serde_json::Value::String(expand_formula(text, row, columns)?)
                }
                _ => value.clone(),
            };
            cells.extend(cell_write((col, row), &value)?);
        }
        if cells.len() > MAX_WRITES {
            return Err(format!("at most {MAX_WRITES} cells per call"));
        }
    }
    Ok(cells)
}

/// A new sheet's first cells: `columns` as its header row and `records`
/// under it.
///
/// # Errors
///
/// Why the columns or records cannot be written.
pub(crate) fn new_sheet_cells(
    columns: &[serde_json::Value],
    records: &[serde_json::Value],
) -> Result<Vec<CellWrite>, String> {
    let columns = Columns::from_names(columns)?;
    let mut cells = columns.header_cells();
    cells.extend(cells_from_records(&columns, 1, records)?);
    Ok(cells)
}

/// The keys of `records` in the order they first come: an empty sheet's
/// headers when none are given.
fn keys_in_order(records: &[serde_json::Value]) -> Vec<serde_json::Value> {
    let mut keys: Vec<serde_json::Value> = Vec::new();
    for key in records
        .iter()
        .filter_map(serde_json::Value::as_object)
        .flat_map(serde_json::Map::keys)
    {
        let key = serde_json::Value::String(key.clone());
        if !keys.contains(&key) {
            keys.push(key);
        }
    }
    keys
}

/// The sheet row (0-based) `set-records` writes its first record to: data
/// row `data_row` (1 = the row under the headers), else the row after the
/// last one used.
fn first_record_row(sheet: &SheetInfo, data_row: Option<i64>) -> Result<u32, String> {
    match data_row {
        Some(row) => u32::try_from(row)
            .ok()
            .filter(|row| *row >= 1)
            .ok_or_else(|| "`row` is a data row number: 1 is the row under the headers".into()),
        None => Ok(sheet
            .snapshot
            .raw
            .keys()
            .map(|&(_, row)| row.saturating_add(1))
            .max()
            .unwrap_or(1)
            .max(1)),
    }
}

/// Why `records`, written from sheet row `first` by an explicit `row`, would
/// overwrite a row that is no longer the one the caller read: a record that
/// names the key column (the leftmost header, which identifies a row) with a
/// value other than the one its target row already holds there. Another
/// collaborator may have appended or sorted rows since the caller read the
/// sheet, and a stale row number would then silently replace their row. An
/// empty target row, a matching key, or a record leaving the key out (an
/// update of that row's other cells) is written.
fn stale_row(
    sheet: &SheetInfo,
    columns: &Columns,
    first: u32,
    records: &[serde_json::Value],
) -> Option<String> {
    let (key, key_col) = columns.0.iter().min_by_key(|(_, col)| *col)?;
    for (row, record) in (first..).zip(records) {
        let Some(wanted) = record
            .as_object()
            .and_then(|record| record.iter().find(|(name, _)| fold(name) == fold(key)))
            .map(|(_, value)| value)
        else {
            continue;
        };
        let Ok(Some(written)) = cell_write((*key_col, row), wanted) else {
            continue;
        };
        let held = sheet
            .snapshot
            .raw
            .get(&(*key_col, row))
            .map_or("", |raw| raw.trim());
        if held.is_empty() || fold(held) == fold(&written.raw) {
            continue;
        }
        return Some(format!(
            "data row {} of `{}` holds `{held}` under `{key}`, not `{}`: the sheet changed \
             since it was read. Read it again and write to the row that holds `{}`, or leave \
             `row` out to append (to rename the row's `{key}`, write that cell with set-cells)",
            row, sheet.name, written.raw, written.raw
        ));
    }
    None
}

/// Records written into a sheet by the headers of its first row.
pub(crate) fn set_records_spec(sheet: usize, sheet_id: &str, info: Arc<SheetInfo>) -> ActionSpec {
    let sheet_id = sheet_id.to_owned();
    ActionSpec::typed(
        "set-records",
        "Set records",
        "records keyed by header",
        ExecutionLocus::ClientLocal,
        vec![
            ActionParam::required(
                "records",
                ParamType::Array,
                "Rows as objects keyed by the headers in row 1, e.g. [{\"Item\": \"Venue\", \
                 \"Total\": \"=[Price]*[Qty]\"}]; a key left out or null keeps its cell, \"\" \
                 clears. In a formula [Header] is that column's cell in the same row and {row} \
                 the row's number",
            ),
            ActionParam::optional(
                "row",
                ParamType::Integer,
                "Data row the first record goes to, 1 = the row under the headers (sheet row 2), \
                 the rest below it; omit to append after the last used row. A record whose \
                 first-column value differs from the one its filled row holds is refused: the \
                 rows moved since they were read",
            ),
            ActionParam::optional(
                "columns",
                ParamType::Array,
                "An empty sheet's headers, in order; omitted, they are the records' keys as \
                 they first come",
            ),
        ],
        move |args| {
            let Some(records) = args.array("records") else {
                return invalid("records must be an array of objects keyed by header");
            };
            let explicit = args.integer("row").is_some();
            let cells =
                match Columns::of_sheet(&info) {
                    Ok(columns) => first_record_row(&info, args.integer("row")).and_then(|first| {
                        match stale_row(&info, &columns, first, records).filter(|_| explicit) {
                            Some(refusal) => Err(refusal),
                            None => cells_from_records(&columns, first, records),
                        }
                    }),
                    // An empty sheet takes its header row from the records.
                    Err(_) if info.snapshot.raw.is_empty() => {
                        let names = args
                            .array("columns")
                            .map_or_else(|| keys_in_order(records), <[_]>::to_vec);
                        new_sheet_cells(&names, records)
                    }
                    Err(message) => Err(message),
                };
            match cells {
                Ok(cells) if cells.is_empty() => invalid("records hold no values to write"),
                Ok(cells) => encode(&SheetsAction::SetCells {
                    sheet,
                    sheet_id: sheet_id.clone(),
                    cells,
                }),
                // Which sheet lacks the column: records meant for the sheet
                // beside it name headers this one does not have.
                Err(message) if message.starts_with("no column") => invalid(format!(
                    "sheet `{}` has {message}; write the records to the sheet whose headers \
                     they name",
                    info.name
                )),
                Err(message) => invalid(message),
            }
        },
    )
    .reversible()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn columns(names: &[&str]) -> Columns {
        Columns::from_names(&names.iter().map(|name| json!(name)).collect::<Vec<_>>()).unwrap()
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
            error.contains("no column `Cost`") && error.contains("`People`"),
            "{error}"
        );
        assert!(expand_formula("=[Total", 1, &cols).is_err());
    }

    #[test]
    fn columns_are_named_once_each() {
        for names in [json!(["A", "a "]), json!(["A", ""]), json!([]), json!([1])] {
            assert!(
                Columns::from_names(names.as_array().unwrap()).is_err(),
                "{names}"
            );
        }
    }

    #[test]
    fn a_left_out_key_leaves_its_cell() {
        let cols = columns(&["Item", "Per person", "People", "Total"]);
        let cells = cells_from_records(
            &cols,
            1,
            &[json!({"Item": "Venue", "Per person": 145, "Total": "=[Per person]*[People]"})],
        )
        .unwrap();
        let mut at: Vec<(u32, u32, &str)> = cells
            .iter()
            .map(|cell| (cell.col, cell.row, cell.raw.as_str()))
            .collect();
        at.sort_unstable();
        assert_eq!(at, [(0, 1, "Venue"), (1, 1, "145"), (3, 1, "=B2*C2")]);
    }
}
