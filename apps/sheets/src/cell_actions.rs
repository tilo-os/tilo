//! What agents and scripts read of a sheet, and the cell work the verbs
//! share: a range's cells as `read-cells` answers them, what each cell
//! shows, and the writes that sort rows.
//!
//! Arguments are A1 references and plain JSON values, and a read returns
//! only the cells a caller asked for, so a whole task fits in a few compact
//! calls.

use std::collections::HashMap;

use crate::model::{Addr, CellFormat, CellValue, addr_to_a1, format_value};
use crate::resource::{CellWrite, range_label};
use crate::workbook::SheetSnapshot;

/// The most cells one read returns; a larger range asks for a smaller one.
pub const MAX_READ_CELLS: usize = 4096;

/// A populated cell with what it shows, for search and sorting.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SearchableCell {
    pub col: u32,
    pub row: u32,
    pub raw: String,
    pub display: String,
}

/// Every populated cell of `snap` with what it shows, row by row: formulas
/// calculated, a formula without a format of its own shown in its
/// operands' (a sum of amounts is an amount).
pub fn shown_cells(snap: &SheetSnapshot) -> Vec<SearchableCell> {
    let values = crate::formula::recalculate(&snap.raw);
    let mut cells = snap
        .raw
        .iter()
        .map(|(&(col, row), raw)| {
            let fmt = snap.fmt.get(&(col, row)).copied().unwrap_or_else(|| {
                raw.strip_prefix('=')
                    .and_then(|body| crate::formula::infer_result_format(body, &snap.fmt))
                    .unwrap_or(CellFormat::General)
            });
            let display = values
                .get(&(col, row))
                .map_or_else(String::new, |value| format_value(value, fmt));
            SearchableCell {
                col,
                row,
                raw: raw.clone(),
                display,
            }
        })
        .collect::<Vec<_>>();
    cells.sort_by_key(|cell| (cell.row, cell.col));
    cells
}

#[derive(Clone, Debug, PartialEq, PartialOrd)]
enum SortKey {
    Number(f64),
    Text(String),
    Blank,
}

fn sort_key(cell: Option<&SearchableCell>) -> SortKey {
    let Some(cell) = cell else {
        return SortKey::Blank;
    };
    let shown = if cell.raw.starts_with('=') {
        &cell.display
    } else {
        &cell.raw
    };
    let shown = shown.trim();
    if shown.is_empty() {
        SortKey::Blank
    } else if let Ok(number) = shown.parse::<f64>() {
        SortKey::Number(number)
    } else {
        SortKey::Text(shown.to_lowercase())
    }
}

fn compare(a: &SortKey, b: &SortKey) -> std::cmp::Ordering {
    use std::cmp::Ordering;
    // Numbers before text, blanks last, as spreadsheets sort.
    match (a, b) {
        (SortKey::Number(x), SortKey::Number(y)) => x.total_cmp(y),
        (SortKey::Text(x), SortKey::Text(y)) => x.cmp(y),
        (SortKey::Blank, SortKey::Blank) => Ordering::Equal,
        (SortKey::Number(_), _) | (SortKey::Text(_), SortKey::Blank) => Ordering::Less,
        _ => Ordering::Greater,
    }
}

/// The writes that reorder the rows of `start..=end` by the column `by`.
/// Formulas move with their row and keep pointing at it.
pub fn sort_writes(
    cells: &[SearchableCell],
    (start, end): (Addr, Addr),
    by: u32,
    descending: bool,
) -> Vec<CellWrite> {
    let at: HashMap<Addr, &SearchableCell> = cells
        .iter()
        .map(|cell| ((cell.col, cell.row), cell))
        .collect();
    let mut rows: Vec<u32> = (start.1..=end.1).collect();
    rows.sort_by(|a, b| {
        let order = compare(
            &sort_key(at.get(&(by, *a)).copied()),
            &sort_key(at.get(&(by, *b)).copied()),
        );
        if descending { order.reverse() } else { order }
    });
    let mut writes = Vec::new();
    for (to, from) in (start.1..).zip(rows) {
        if to == from {
            continue;
        }
        for col in start.0..=end.0 {
            let raw = at.get(&(col, from)).map_or_else(String::new, |cell| {
                let shift = i64::from(to) - i64::from(from);
                if cell.raw.starts_with('=') && shift != 0 {
                    crate::formula::offset_refs(&cell.raw, 0, shift)
                } else {
                    cell.raw.clone()
                }
            });
            writes.push(CellWrite::at((col, to), raw, None));
        }
    }
    writes
}

/// What `read-cells` answers for `range` of `snap` (every used cell when
/// none).
///
/// # Errors
/// Names a range that is not one, or one larger than a read returns.
pub fn read(snap: &SheetSnapshot, range: Option<&str>) -> Result<serde_json::Value, String> {
    let used = snap.raw.keys().chain(snap.bold.iter()).copied();
    let (start, end) = match range {
        Some(range) => crate::resource::parse_range(range)
            .ok_or_else(|| format!("`{range}` is not a cell or range such as A1:D6"))?,
        None => used.fold(((0, 0), (0, 0)), |(_, end), (col, row)| {
            ((0, 0), (end.0.max(col), end.1.max(row)))
        }),
    };
    let width = (end.0 - start.0) as usize + 1;
    let height = (end.1 - start.1) as usize + 1;
    if width.saturating_mul(height) > MAX_READ_CELLS {
        return Err(format!("read at most {MAX_READ_CELLS} cells at once"));
    }
    Ok(range_content(snap, start, end))
}

fn json_value(value: &CellValue) -> serde_json::Value {
    match value {
        CellValue::Empty => serde_json::Value::Null,
        CellValue::Number(number) => serde_json::Number::from_f64(*number).map_or_else(
            || serde_json::Value::String(number.to_string()),
            serde_json::Value::Number,
        ),
        CellValue::Bool(value) => serde_json::Value::Bool(*value),
        CellValue::Text(_) | CellValue::Error(_) => {
            serde_json::Value::String(value.to_plain_string())
        }
    }
}

/// `start..=end` of `snap` as `read-cells` answers it.
fn range_content(snap: &SheetSnapshot, start: Addr, end: Addr) -> serde_json::Value {
    let values = crate::formula::recalculate(&snap.raw);
    let mut rows = Vec::new();
    let mut formulas = serde_json::Map::new();
    let mut bold = Vec::new();
    for row in start.1..=end.1 {
        let mut line = Vec::new();
        for col in start.0..=end.0 {
            line.push(
                values
                    .get(&(col, row))
                    .map_or(serde_json::Value::Null, json_value),
            );
            if let Some(raw) = snap.raw.get(&(col, row)).filter(|raw| raw.starts_with('=')) {
                formulas.insert(addr_to_a1(col, row), raw.clone().into());
            }
            if snap.bold.contains(&(col, row)) {
                bold.push(serde_json::Value::String(addr_to_a1(col, row)));
            }
        }
        rows.push(serde_json::Value::Array(line));
    }
    serde_json::json!({
        "sheet": snap.name,
        "range": range_label(start, end),
        "rows": rows,
        "formulas": formulas,
        "bold": bold,
    })
}

/// The schema of what `read-cells` answers.
pub fn read_result_schema() -> serde_json::Value {
    serde_json::json!({"type":"object","required":["sheet","range","rows","formulas","bold"],"additionalProperties":false,"properties":{
        "sheet":{"type":"string"},
        "range":{"type":"string"},
        "rows":{"type":"array","items":{"type":"array"}},
        "formulas":{"type":"object","additionalProperties":true},
        "bold":{"type":"array","items":{"type":"string"}}
    }})
}

#[cfg(test)]
mod tests;
