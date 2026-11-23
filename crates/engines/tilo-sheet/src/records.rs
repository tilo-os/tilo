//! Rows written by column header rather than by position: each record is an object keyed by the
//! sheet's headers, so a value left out leaves its own cell instead of shifting every value after
//! it into the wrong column. In a record's formula `[Header]` is that column's cell in the
//! record's own row and `{row}` that row's number, so a caller never counts rows.

use std::collections::BTreeMap;

use serde_json::Value;

use crate::Sheet;
use crate::model::{Addr, CellFormat, a1_to_addr, addr_to_a1, date_to_serial, serial_to_ymd};

/// The most cells one write changes.
pub const MAX_WRITES: usize = 10_000;

const NAMED_COLUMNS_LISTED: usize = 30;

/// What one record action writes: each cell's input (`None` leaves it), and each format (`None`
/// leaves it).
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Writes {
    /// `(A1, input or None)`.
    pub cells: Vec<(String, Option<String>)>,
    /// `(A1, format code or None)`.
    pub formats: Vec<(String, Option<String>)>,
}

/// The records written into the sheet, and where: what `set-records` asks.
pub struct RecordsRequest<'a> {
    pub records: &'a [Value],
    /// Data row of the first record, 1 = the row under the headers.
    pub row: Option<u32>,
    pub columns: Option<&'a [String]>,
    /// The header that identifies each record's row.
    pub key: Option<&'a str>,
}

struct Columns(Vec<(String, u32)>);

fn fold(name: &str) -> String {
    name.split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .to_lowercase()
}

/// Whether two cell texts name the same key: alike but for case and surrounding space, or the
/// same number (`7` and `7.0`).
fn same_key(a: &str, b: &str) -> bool {
    match (a.trim().parse::<f64>(), b.trim().parse::<f64>()) {
        (Ok(a), Ok(b)) => (a - b).abs() <= f64::EPSILON * a.abs().max(b.abs()),
        _ => fold(a) == fold(b),
    }
}

impl Columns {
    fn from_names(names: &[String]) -> Result<Self, String> {
        let mut columns: Vec<(String, u32)> = Vec::new();
        for (col, name) in (0u32..).zip(names) {
            let name = name.trim();
            if name.is_empty() {
                return Err("a header name in `columns` is empty".into());
            }
            if columns.iter().any(|(other, _)| fold(other) == fold(name)) {
                return Err(format!("`columns` names `{name}` twice"));
            }
            columns.push((name.to_owned(), col));
        }
        if columns.is_empty() {
            return Err("`columns` names no headers".into());
        }
        Ok(Self(columns))
    }

    /// The headers a sheet holds in its first row.
    fn of_sheet(sheet: &Sheet) -> Option<Self> {
        let mut columns: Vec<(String, u32)> = sheet
            .cells
            .iter()
            .filter_map(|(reference, raw)| {
                let (col, row) = a1_to_addr(reference)?;
                (row == 0).then(|| (raw.trim().to_owned(), col))
            })
            .filter(|(name, _)| !name.is_empty())
            .collect();
        columns.sort_by_key(|(_, col)| *col);
        (!columns.is_empty()).then_some(Self(columns))
    }

    fn header_writes(&self) -> Writes {
        Writes {
            cells: self
                .0
                .iter()
                .map(|(name, col)| (addr_to_a1(*col, 0), Some(name.clone())))
                .collect(),
            formats: Vec::new(),
        }
    }

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

    fn first(&self) -> Option<&(String, u32)> {
        self.0.iter().min_by_key(|(_, col)| *col)
    }
}

/// `formula` in sheet row `row` (0-based) with `{row}` as that row's number and each `[Header]`
/// as that column's cell in it; text in quotes is kept.
fn expand_formula(formula: &str, row: u32, columns: &Columns) -> Result<String, String> {
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

/// A JSON value as a cell's input and the format it infers, or `None` for null.
fn cell_input(value: &Value) -> Result<Option<(String, Option<CellFormat>)>, String> {
    let text = match value {
        Value::Null => return Ok(None),
        Value::String(text) => text,
        Value::Number(number) => return Ok(Some((number.to_string(), None))),
        Value::Bool(true) => return Ok(Some(("TRUE".into(), None))),
        Value::Bool(false) => return Ok(Some(("FALSE".into(), None))),
        _ => return Err("cell values are strings, numbers, booleans or null".into()),
    };
    if let Some(rest) = text
        .trim()
        .strip_prefix('\'')
        .filter(|rest| !rest.is_empty())
    {
        // A marked `'=x` keeps its apostrophe: without it the input would be a formula, the
        // opposite of what the mark asks for.
        let raw = if rest.starts_with('=') {
            text.trim()
        } else {
            rest
        };
        return Ok(Some((raw.to_owned(), Some(CellFormat::Text))));
    }
    if let Some(serial) = iso_date_serial(text) {
        return Ok(Some((
            serial.to_string(),
            Some(CellFormat::Date(tilo_formats::DateStyle::Iso)),
        )));
    }
    Ok(Some((text.clone(), None)))
}

/// The Excel serial of an exact ISO date `YYYY-MM-DD`, optionally followed by a time.
fn iso_date_serial(text: &str) -> Option<f64> {
    fn digits(part: Option<&str>) -> Option<u32> {
        let part = part?;
        if part.is_empty() || !part.bytes().all(|b| b.is_ascii_digit()) {
            return None;
        }
        part.parse().ok()
    }
    let (date, time) = match text.len() {
        10 => (text, None),
        16 | 19 => {
            let (date, rest) = (text.get(..10)?, text.get(10..)?);
            let time = rest.strip_prefix('T').or_else(|| rest.strip_prefix(' '))?;
            (date, Some(time))
        }
        _ => return None,
    };
    if date.get(4..5) != Some("-") || date.get(7..8) != Some("-") {
        return None;
    }
    let year = digits(date.get(..4))?;
    let month = digits(date.get(5..7))?;
    let day = digits(date.get(8..10))?;
    let serial = date_to_serial(i64::from(year), month, day);
    if serial_to_ymd(serial) != (i64::from(year), month, day) {
        return None;
    }
    let fraction = match time {
        None => 0.0,
        Some(time) => {
            if time.get(2..3) != Some(":") || (time.len() == 8 && time.get(5..6) != Some(":")) {
                return None;
            }
            let hour = digits(time.get(..2))?;
            let minute = digits(time.get(3..5))?;
            let second = if time.len() == 8 {
                digits(time.get(6..8))?
            } else {
                0
            };
            if hour > 23 || minute > 59 || second > 59 {
                return None;
            }
            f64::from(hour * 3600 + minute * 60 + second) / 86_400.0
        }
    };
    Some(serial + fraction)
}

/// The cells `records` write by header from sheet row `first_row` (0-based) down, one record a
/// row: a key left out (or null) leaves its cell.
fn document_writes(
    columns: &Columns,
    rows: impl IntoIterator<Item = u32>,
    records: &[Value],
) -> Result<Writes, String> {
    let mut writes = Writes::default();
    for (row, record) in rows.into_iter().zip(records) {
        let record = object(record)?;
        for (key, value) in record {
            let col = columns.column(key)?;
            let value = match value.as_str() {
                Some(text) if text.starts_with('=') => {
                    Value::String(expand_formula(text, row, columns)?)
                }
                _ => value.clone(),
            };
            let Some((input, format)) = cell_input(&value)? else {
                continue;
            };
            let reference = addr_to_a1(col, row);
            writes.cells.push((reference.clone(), Some(input)));
            writes
                .formats
                .push((reference, format.map(|format| format.as_str())));
        }
        if writes.cells.len() > MAX_WRITES {
            return Err(format!("at most {MAX_WRITES} cells at once"));
        }
    }
    Ok(writes)
}

fn object(record: &Value) -> Result<&serde_json::Map<String, Value>, String> {
    record
        .as_object()
        .ok_or_else(|| "each record is an object of values keyed by header".to_owned())
}

fn new_sheet_writes(columns: &[String], records: &[Value]) -> Result<Writes, String> {
    let columns = Columns::from_names(columns)?;
    let mut writes = columns.header_writes();
    let document = document_writes(&columns, 1.., records)?;
    writes.cells.extend(document.cells);
    writes.formats.extend(document.formats);
    Ok(writes)
}

fn keys_in_order(records: &[Value]) -> Vec<String> {
    let mut keys: Vec<String> = Vec::new();
    for key in records
        .iter()
        .filter_map(Value::as_object)
        .flat_map(serde_json::Map::keys)
    {
        if !keys.contains(key) {
            keys.push(key.clone());
        }
    }
    keys
}

fn value_of<'a>(record: &'a Value, header: &str) -> Option<&'a Value> {
    record
        .as_object()?
        .iter()
        .find(|(name, _)| fold(name) == fold(header))
        .map(|(_, value)| value)
        .filter(|value| !value.is_null())
}

fn key_text(value: &Value) -> Option<String> {
    cell_input(value).ok().flatten().map(|(input, _)| input)
}

fn next_free_row(sheet: &Sheet) -> u32 {
    sheet
        .cells
        .keys()
        .filter_map(|reference| a1_to_addr(reference))
        .map(|(_, row)| row.saturating_add(1))
        .max()
        .unwrap_or(1)
        .max(1)
}

/// The writes `set-records` makes on `sheet`.
///
/// # Errors
/// A record that is not an object, a key no header names, a value that is not a cell value, or
/// more than [`MAX_WRITES`] cells.
pub fn set_records(sheet: &Sheet, request: &RecordsRequest<'_>) -> Result<Writes, String> {
    let records = request.records;
    if records.is_empty() {
        return Err("give at least one record".into());
    }
    if request.key.is_some() && request.row.is_some() {
        return Err(
            "give `key` to update rows by their key, or `row` to write at a row, not both".into(),
        );
    }
    let Some(columns) = Columns::of_sheet(sheet) else {
        if !sheet.cells.is_empty() {
            return Err(format!(
                "`{}` has no headers in row 1 to key records by: write them first",
                sheet.name
            ));
        }
        let names = request
            .columns
            .map_or_else(|| keys_in_order(records), <[String]>::to_vec);
        return new_sheet_writes(&names, records);
    };
    let named = |error: String| {
        if error.starts_with("no column") {
            format!(
                "sheet `{}` has {}; write the records to the sheet whose headers they name",
                sheet.name, error
            )
        } else {
            error
        }
    };
    if let Some(key) = request.key {
        let rows = keyed_rows(sheet, &columns, key, records).map_err(named)?;
        return document_writes(&columns, rows, records).map_err(named);
    }
    let first = match request.row {
        Some(row) if row >= 1 => {
            if let Some(refusal) = stale_row(sheet, &columns, row, records) {
                return Err(refusal);
            }
            row
        }
        Some(_) => {
            return Err("`row` is a data row number: 1 is the row under the headers".into());
        }
        None => next_free_row(sheet),
    };
    document_writes(&columns, first.., records).map_err(named)
}

/// The writes an `upsert-item` makes: `value` (by header) into the one data row whose cells under
/// the headers `matching` names hold its values, else into the next free row.
///
/// # Errors
/// `matching` empty, names no header, or fits more than one row.
pub fn upsert(
    sheet: &Sheet,
    matching: &serde_json::Map<String, Value>,
    value: &serde_json::Map<String, Value>,
) -> Result<Writes, String> {
    if matching.is_empty() {
        return Err(
            "give `matching`: the header values that identify the row, e.g. {\"Name\": \"Ana\"}"
                .into(),
        );
    }
    let mut inserted = value.clone();
    for (key, wanted) in matching {
        if !inserted.keys().any(|named| fold(named) == fold(key)) {
            inserted.insert(key.clone(), wanted.clone());
        }
    }
    let inserted = [Value::Object(inserted)];
    let Some(columns) = Columns::of_sheet(sheet) else {
        if !sheet.cells.is_empty() {
            return Err(format!(
                "`{}` has no headers in row 1 to match rows by: write them first",
                sheet.name
            ));
        }
        return new_sheet_writes(&keys_in_order(&inserted), &inserted);
    };
    let mut keys = Vec::new();
    for (key, wanted) in matching {
        let text =
            key_text(wanted).ok_or_else(|| format!("`match` gives `{key}` no cell value"))?;
        keys.push((columns.column(key)?, text));
    }
    let mut rows: Vec<u32> = sheet
        .cells
        .iter()
        .filter_map(|(reference, raw)| {
            let (_, row) = a1_to_addr(reference)?;
            (row >= 1 && !raw.trim().is_empty()).then_some(row)
        })
        .collect();
    rows.sort_unstable();
    rows.dedup();
    let held = |col: u32, row: u32| {
        sheet
            .cells
            .get(&addr_to_a1(col, row))
            .map_or("", String::as_str)
    };
    let found: Vec<u32> = rows
        .into_iter()
        .filter(|row| {
            keys.iter()
                .all(|(col, text)| same_key(held(*col, *row), text))
        })
        .collect();
    let (row, record) = match found.as_slice() {
        [] => (next_free_row(sheet), inserted),
        [row] => (*row, [Value::Object(value.clone())]),
        [first, second, ..] => {
            return Err(format!(
                "`match` fits rows {} and {} of `{}`: name more headers so it fits one",
                first + 1,
                second + 1,
                sheet.name
            ));
        }
    };
    document_writes(&columns, [row], &record)
}

/// The sheet row each record goes to when rows are identified by the header `key`.
fn keyed_rows(
    sheet: &Sheet,
    columns: &Columns,
    key: &str,
    records: &[Value],
) -> Result<Vec<u32>, String> {
    let col = columns.column(key)?;
    let mut held: Vec<(u32, &str)> = sheet
        .cells
        .iter()
        .filter_map(|(reference, raw)| {
            let (at, row) = a1_to_addr(reference)?;
            (at == col && row >= 1 && !raw.trim().is_empty()).then_some((row, raw.as_str()))
        })
        .collect();
    held.sort_unstable_by_key(|(row, _)| *row);
    let mut next = next_free_row(sheet);
    let mut wanted: Vec<String> = Vec::new();
    let mut rows = Vec::new();
    for record in records {
        object(record)?;
        let text = value_of(record, key)
            .and_then(key_text)
            .ok_or_else(|| format!("every record names its `{key}`, which identifies its row"))?;
        if wanted.iter().any(|other| same_key(other, &text)) {
            return Err(format!(
                "the records name `{key}` `{text}` twice: give each row once"
            ));
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
                return Err(format!(
                    "`{key}` `{text}` is on rows {} and {} of `{}`: which one is meant is \
                     unclear",
                    first + 1,
                    second + 1,
                    sheet.name
                ));
            }
        };
        wanted.push(text);
        rows.push(row);
    }
    Ok(rows)
}

/// Why `records`, written from data row `first` by an explicit `row`, would overwrite a row that
/// is no longer the one the caller read.
fn stale_row(sheet: &Sheet, columns: &Columns, first: u32, records: &[Value]) -> Option<String> {
    let (key, key_col) = columns.first()?;
    for (row, record) in (first..).zip(records) {
        let Some(written) = value_of(record, key).and_then(key_text) else {
            continue;
        };
        let held = sheet
            .cells
            .get(&addr_to_a1(*key_col, row))
            .map_or("", |raw| raw.trim());
        if held.is_empty() || same_key(held, &written) {
            continue;
        }
        return Some(format!(
            "data row {row} of `{}` holds `{held}` under `{key}`, not `{written}`: the sheet \
             changed since it was read. Read it again and write to the row that holds it",
            sheet.name
        ));
    }
    None
}

/// The cells of a sheet as the app's records see them: each cell's input by address.
#[must_use]
pub fn cell_map(sheet: &Sheet) -> BTreeMap<Addr, String> {
    sheet
        .cells
        .iter()
        .filter_map(|(reference, raw)| Some((a1_to_addr(reference)?, raw.clone())))
        .collect()
}
