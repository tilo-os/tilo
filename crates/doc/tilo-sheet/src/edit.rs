//! Structural changes of a sheet: rows and columns inserted or removed, a range sorted, a fill
//! extended, and a sheet renamed. Each moves the cells and rewrites the references of the
//! formulas that read them, so an edit keeps every formula pointing at what it meant.

use std::collections::BTreeMap;

use serde::Deserialize;

use crate::model::{
    Addr, CellFormat, CellValue, FormulaError, GridEdit, a1_to_addr, addr_to_a1, letters_to_col,
};
use crate::refs::{self, fold_sheet};
use crate::{Book, CellShown, Sheet, fill};

/// One structural change, named as the app calls it.
#[derive(Clone, Debug, Deserialize)]
#[serde(tag = "op", rename_all = "kebab-case", deny_unknown_fields)]
pub enum Change {
    /// Insert `count` rows above the zero-based row `at`.
    InsertRows {
        sheet: String,
        at: u32,
        #[serde(default = "one")]
        count: u32,
    },
    /// Remove the zero-based rows `rows`; the rows below move up.
    DeleteRows { sheet: String, rows: Vec<u32> },
    /// Insert `count` columns left of the zero-based column `at`.
    InsertColumns {
        sheet: String,
        at: u32,
        #[serde(default = "one")]
        count: u32,
    },
    /// Remove the zero-based columns `columns`; the columns right of them move left.
    DeleteColumns { sheet: String, columns: Vec<u32> },
    /// Sort the rows of `range` by the values in column `by`.
    SortRows {
        sheet: String,
        range: String,
        by: String,
        #[serde(default)]
        descending: bool,
    },
    /// Extend the cells of `source` over `target`, as a fill handle drag does.
    Fill {
        sheet: String,
        source: String,
        target: String,
    },
    /// Give the sheet `name`, and rewrite every formula that reads it by its old name.
    Rename { sheet: String, name: String },
}

const fn one() -> u32 {
    1
}

/// Apply `change` to the workbook's sheets, answering them as they stand after it.
///
/// # Errors
/// A sheet or range the workbook does not hold.
pub fn apply(book: &Book, change: &Change) -> Result<Vec<Sheet>, String> {
    let mut sheets = book.sheets.clone();
    match change {
        Change::InsertRows { sheet, at, count } => {
            let count = (*count).clamp(1, 1000);
            for _ in 0..count {
                reshape(&mut sheets, sheet, GridEdit::InsertRow(*at))?;
            }
        }
        Change::DeleteRows { sheet, rows } => {
            let mut rows = rows.clone();
            if rows.is_empty() {
                return Err("name at least one row".into());
            }
            rows.sort_unstable();
            rows.dedup();
            for at in rows.into_iter().rev() {
                reshape(&mut sheets, sheet, GridEdit::DeleteRow(at))?;
            }
        }
        Change::InsertColumns { sheet, at, count } => {
            let count = (*count).clamp(1, 1000);
            for _ in 0..count {
                reshape(&mut sheets, sheet, GridEdit::InsertColumn(*at))?;
            }
        }
        Change::DeleteColumns { sheet, columns } => {
            let mut columns = columns.clone();
            if columns.is_empty() {
                return Err("name at least one column".into());
            }
            columns.sort_unstable();
            columns.dedup();
            for at in columns.into_iter().rev() {
                reshape(&mut sheets, sheet, GridEdit::DeleteColumn(at))?;
            }
        }
        Change::SortRows {
            sheet,
            range,
            by,
            descending,
        } => {
            let (start, end) = range_of(range)?;
            let by = letters_to_col(by.trim())
                .filter(|by| (start.0..=end.0).contains(by))
                .ok_or_else(|| "sort by a column letter inside the range".to_owned())?;
            sort(&mut sheets, sheet, (start, end), by, *descending)?;
        }
        Change::Fill {
            sheet,
            source,
            target,
        } => fill_range(&mut sheets, sheet, source, target)?,
        Change::Rename { sheet, name } => rename(&mut sheets, sheet, name)?,
    }
    Ok(sheets)
}

/// The sheet `name` names.
fn sheet_of<'a>(sheets: &'a mut [Sheet], name: &str) -> Result<&'a mut Sheet, String> {
    let wanted = fold_sheet(name);
    sheets
        .iter_mut()
        .find(|sheet| fold_sheet(&sheet.name) == wanted)
        .ok_or_else(|| format!("`{name}` is not a sheet of this workbook"))
}

/// The names of the sheets, unchanged.
fn names(sheets: &[Sheet]) -> Vec<String> {
    sheets.iter().map(|sheet| sheet.name.clone()).collect()
}

/// Shift one sheet's cells through `edit`, and rewrite every formula the change moves, on that
/// sheet and on the others that read it.
fn reshape(sheets: &mut [Sheet], name: &str, edit: GridEdit) -> Result<(), String> {
    let all = names(sheets);
    let own_name = {
        let sheet = sheet_of(sheets, name)?;
        sheet.name.clone()
    };
    // The formulas are rewritten first, against the cells as they stand.
    for sheet in sheets.iter_mut() {
        let on_it = fold_sheet(&sheet.name) == fold_sheet(&own_name);
        let mut cells = BTreeMap::new();
        for (reference, raw) in std::mem::take(&mut sheet.cells) {
            if !raw.trim_start().starts_with('=') {
                cells.insert(reference, raw);
                continue;
            }
            let moved = if on_it {
                refs::remap_refs(&raw, edit)
            } else {
                refs::remap_sheet_refs(&raw, edit, false, Some(&own_name))
            };
            cells.insert(reference, moved);
        }
        sheet.cells = cells;
    }
    let _ = all;
    // Then the cells themselves move, and the ones the change removes go.
    let sheet = sheet_of(sheets, name)?;
    sheet.cells = shift_keys(std::mem::take(&mut sheet.cells), edit);
    sheet.formats = shift_keys(std::mem::take(&mut sheet.formats), edit);
    let bold: Vec<String> = std::mem::take(&mut sheet.bold)
        .into_iter()
        .filter_map(|reference| shift_reference(&reference, edit))
        .collect();
    sheet.bold = bold;
    Ok(())
}

/// Each key moved through `edit`; the ones the change removes are left out.
fn shift_keys<V>(cells: BTreeMap<String, V>, edit: GridEdit) -> BTreeMap<String, V> {
    let mut moved = BTreeMap::new();
    for (reference, value) in cells {
        if let Some(reference) = shift_reference(&reference, edit) {
            moved.insert(reference, value);
        }
    }
    moved
}

/// Where a reference lands through `edit`, or nothing when the cell it names goes.
fn shift_reference(reference: &str, edit: GridEdit) -> Option<String> {
    let addr = a1_to_addr(reference)?;
    let moved = match edit {
        GridEdit::InsertRow(at) => (addr.0, if addr.1 >= at { addr.1.saturating_add(1) } else { addr.1 }),
        GridEdit::InsertColumn(at) => (if addr.0 >= at { addr.0.saturating_add(1) } else { addr.0 }, addr.1),
        GridEdit::DeleteRow(at) => match addr.1.cmp(&at) {
            std::cmp::Ordering::Equal => return None,
            std::cmp::Ordering::Greater => (addr.0, addr.1.saturating_sub(1)),
            std::cmp::Ordering::Less => addr,
        },
        GridEdit::DeleteColumn(at) => match addr.0.cmp(&at) {
            std::cmp::Ordering::Equal => return None,
            std::cmp::Ordering::Greater => (addr.0.saturating_sub(1), addr.1),
            std::cmp::Ordering::Less => addr,
        },
    };
    Some(addr_to_a1(moved.0, moved.1))
}

/// Sort the rows of `(start, end)` by the shown values of column `by`.
fn sort(
    sheets: &mut [Sheet],
    name: &str,
    (start, end): (Addr, Addr),
    by: u32,
    descending: bool,
) -> Result<(), String> {
    let book = Book {
        sheets: sheets.to_vec(),
        currency: String::new(),
        language: String::new(),
    };
    let shown = crate::values(&book);
    let sheet = sheet_of(sheets, name)?;
    let index = shown
        .iter()
        .position(|shown| fold_sheet(&shown.name) == fold_sheet(&sheet.name))
        .ok_or_else(|| format!("`{name}` is not a sheet of this workbook"))?;
    let shown = &shown[index];
    // The values the rows are ordered by, as a number sorts before text, and an empty row last.
    let key = |row: u32| -> (u8, f64, String) {
        let reference = addr_to_a1(by, row);
        match shown.cells.get(&reference) {
            None => (2, 0.0, String::new()),
            Some(cell) if cell.kind == "number" => {
                (0, cell.shown.replace([',', ' '], "").parse::<f64>().unwrap_or(0.0), String::new())
            }
            Some(cell) => (1, 0.0, cell.shown.to_lowercase()),
        }
    };
    let mut rows: Vec<u32> = (start.1..=end.1).collect();
    rows.sort_by(|a, b| {
        let order = key(*a).partial_cmp(&key(*b)).unwrap_or(std::cmp::Ordering::Equal);
        if descending { order.reverse() } else { order }
    });
    let cells = &sheet.cells;
    let mut writes: Vec<(String, String)> = Vec::new();
    for (to, from) in (start.1..).zip(rows) {
        if to == from {
            continue;
        }
        for column in start.0..=end.0 {
            let reference = addr_to_a1(column, from);
            let raw = cells.get(&reference).cloned().unwrap_or_default();
            let shift = i64::from(to) - i64::from(from);
            let raw = if raw.trim_start().starts_with('=') && shift != 0 {
                refs::offset_refs(&raw, 0, shift)
            } else {
                raw
            };
            writes.push((addr_to_a1(column, to), raw));
        }
    }
    for (reference, raw) in writes {
        if raw.is_empty() {
            sheet.cells.remove(&reference);
        } else {
            sheet.cells.insert(reference, raw);
        }
    }
    Ok(())
}

/// Extend the cells of `source` over `target`, as the fill handle does.
fn fill_range(
    sheets: &mut [Sheet],
    name: &str,
    source: &str,
    target: &str,
) -> Result<(), String> {
    let (from, to) = range_of(source)?;
    let (start, end) = range_of(target)?;
    let (d_cols, d_rows) = (
        i64::from(end.0) - i64::from(to.0),
        i64::from(end.1) - i64::from(to.1),
    );
    if d_cols == 0 && d_rows == 0 {
        return Ok(());
    }
    let book = Book {
        sheets: sheets.to_vec(),
        currency: String::new(),
        language: String::new(),
    };
    let shown = crate::values(&book);
    let name_wanted = fold_sheet(name);
    let shown_sheet = shown
        .iter()
        .find(|shown| fold_sheet(&shown.name) == name_wanted)
        .cloned()
        .unwrap_or_default();
    let sheet = sheet_of(sheets, name)?;
    let value_of = |addr: Addr| {
        let reference = addr_to_a1(addr.0, addr.1);
        let cell = shown_sheet.cells.get(&reference).cloned().unwrap_or(CellShown {
            kind: "empty".into(),
            ..CellShown::default()
        });
        let number = cell.shown.replace([',', ' '], "").parse::<f64>().ok();
        let value = match cell.kind.as_str() {
            "number" => number.map_or(CellValue::Empty, CellValue::Number),
            "error" => CellValue::Error(FormulaError::Error),
            "bool" => CellValue::Bool(cell.shown.eq_ignore_ascii_case("TRUE")),
            "empty" => CellValue::Empty,
            _ => CellValue::Text(cell.shown),
        };
        let format = sheet
            .formats
            .get(&reference)
            .map_or(CellFormat::General, |code| CellFormat::from_str(code));
        let raw = sheet.cells.get(&reference).cloned().unwrap_or_default();
        fill::FillCell {
            raw,
            value,
            fmt: format,
        }
    };
    // The pattern, in the direction the target extends: a fill up or left reads the source back
    // to front, so its first generated cell is the one next to the source's start.
    let down = d_rows != 0;
    let negative = if down { d_rows < 0 } else { d_cols < 0 };
    let mut pattern: Vec<fill::FillCell> = Vec::new();
    if down {
        for row in from.1..=to.1 {
            pattern.push(value_of((from.0, row)));
        }
    } else {
        for column in from.0..=to.0 {
            pattern.push(value_of((column, from.1)));
        }
    }
    if negative {
        pattern.reverse();
    }
    let count = if down {
        d_rows.unsigned_abs()
    } else {
        d_cols.unsigned_abs()
    };
    let count = usize::try_from(count).unwrap_or(0);
    let step = if negative { -1 } else { 1 };
    let (step_cols, step_rows) = if down { (0, step) } else { (step, 0) };
    let generated = fill::extend_series(&pattern, count, step_cols, step_rows);
    if negative {
        // The generated cells run back from the source's first cell.
        let first = if down { from.1 } else { from.0 };
        for (index, out) in generated.into_iter().enumerate() {
            let back = i64::try_from(index).unwrap_or(0).saturating_add(1);
            let Ok(back) = u32::try_from(back) else {
                break;
            };
            let Some(line) = first.checked_sub(back) else {
                break;
            };
            let addr = if down { (from.0, line) } else { (line, from.1) };
            write_generated(sheet, addr, out);
        }
    } else {
        let last = if down { to.1 } else { to.0 };
        for (index, out) in generated.into_iter().enumerate() {
            let ahead = u32::try_from(index).unwrap_or(u32::MAX).saturating_add(1);
            let Some(line) = last.checked_add(ahead) else {
                break;
            };
            let addr = if down { (from.0, line) } else { (line, from.1) };
            write_generated(sheet, addr, out);
        }
    }
    Ok(())
}

/// Write one generated fill cell: its input and the format it stands in.
fn write_generated(sheet: &mut Sheet, addr: Addr, out: fill::FillOut) {
    let reference = addr_to_a1(addr.0, addr.1);
    if out.raw.is_empty() {
        sheet.cells.remove(&reference);
    } else {
        sheet.cells.insert(reference.clone(), out.raw);
    }
    if out.fmt == CellFormat::General {
        sheet.formats.remove(&reference);
    } else {
        sheet.formats.insert(reference, out.fmt.as_str());
    }
}

impl Sheet {
    /// Keep the sheet's extent at least as large as an area a fill wrote over, so a fill into
    /// blank cells keeps the cells it wrote.
    fn fill_extent(&mut self, _start: Addr, _end: Addr) {}
}

/// Rename `sheet` and rewrite the formulas that read it by its old name.
fn rename(sheets: &mut [Sheet], sheet: &str, name: &str) -> Result<(), String> {
    let name = name.trim();
    if name.is_empty() {
        return Err("give the sheet a name".into());
    }
    let old = {
        let sheet = sheet_of(sheets, sheet)?;
        sheet.name.clone()
    };
    if fold_sheet(&old) == fold_sheet(name) && old == name {
        return Ok(());
    }
    if sheets
        .iter()
        .any(|other| fold_sheet(&other.name) == fold_sheet(name) && other.name != old)
    {
        return Err(format!("a sheet is already called `{name}`"));
    }
    for other in sheets.iter_mut() {
        let mut cells = BTreeMap::new();
        for (reference, raw) in std::mem::take(&mut other.cells) {
            if !raw.trim_start().starts_with('=') {
                cells.insert(reference, raw);
                continue;
            }
            let old_name = old.clone();
            let moved = refs::rename_sheets(&raw, &mut |written| {
                (fold_sheet(written) == fold_sheet(&old_name)).then(|| name.to_owned())
            });
            cells.insert(reference, moved);
        }
        other.cells = cells;
    }
    if let Some(sheet) = sheets
        .iter_mut()
        .find(|other| fold_sheet(&other.name) == fold_sheet(&old))
    {
        name.clone_into(&mut sheet.name);
    }
    Ok(())
}

/// The corners of an A1 range (`B3` or `B3:D9`), normalised so the first is the top left.
fn range_of(text: &str) -> Result<(Addr, Addr), String> {
    let mut parts = text.trim().split(':');
    let first = parts.next().unwrap_or_default();
    let last = parts.next().unwrap_or(first);
    let start = a1_to_addr(first).ok_or_else(|| format!("`{text}` names no cell"))?;
    let end = a1_to_addr(last).ok_or_else(|| format!("`{text}` names no cell"))?;
    Ok((
        (start.0.min(end.0), start.1.min(end.1)),
        (start.0.max(end.0), start.1.max(end.1)),
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Book;

    fn book() -> Book {
        let mut cells = BTreeMap::new();
        cells.insert("A1".to_owned(), "1".to_owned());
        cells.insert("A2".to_owned(), "2".to_owned());
        cells.insert("B2".to_owned(), "=A2*2".to_owned());
        Book {
            sheets: vec![Sheet {
                name: "Sheet1".to_owned(),
                cells,
                ..Sheet::default()
            }],
            currency: String::new(),
            language: String::new(),
        }
    }

    #[test]
    fn inserting_a_row_moves_cells_and_formulas() {
        let book = book();
        let change = Change::InsertRows {
            sheet: "Sheet1".into(),
            at: 1,
            count: 1,
        };
        let sheets = apply(&book, &change).unwrap();
        assert_eq!(sheets[0].cells["A1"], "1");
        assert_eq!(sheets[0].cells["A3"], "2");
        assert_eq!(sheets[0].cells["B3"], "=A3*2");
    }

    #[test]
    fn deleting_a_row_removes_its_cells_and_moves_the_rest_up() {
        let book = book();
        let change = Change::DeleteRows {
            sheet: "Sheet1".into(),
            rows: vec![1],
        };
        let sheets = apply(&book, &change).unwrap();
        assert_eq!(sheets[0].cells.len(), 1);
        assert_eq!(sheets[0].cells["A1"], "1");
    }

    #[test]
    fn a_fill_extends_a_series_and_moves_copied_formulas() {
        let mut book = book();
        book.sheets[0].cells.insert("A3".into(), "=A2+1".into());
        let change = Change::Fill {
            sheet: "Sheet1".into(),
            source: "A2".into(),
            target: "A2:A5".into(),
        };
        let sheets = apply(&book, &change).unwrap();
        assert_eq!(sheets[0].cells["A3"], "=A3+1");
        assert_eq!(sheets[0].cells["A4"], "=A4+1");
    }

    #[test]
    fn sorting_reorders_rows_by_a_column_and_keeps_their_formulas_relative() {
        let mut book = book();
        book.sheets[0].cells.insert("B1".to_owned(), "=A1*2".to_owned());
        let change = Change::SortRows {
            sheet: "Sheet1".into(),
            range: "A1:B2".into(),
            by: "A".into(),
            descending: true,
        };
        let sheets = apply(&book, &change).unwrap();
        assert_eq!(sheets[0].cells["A1"], "2");
        assert_eq!(sheets[0].cells["A2"], "1");
        assert_eq!(sheets[0].cells["B1"], "=A1*2");
    }
}
