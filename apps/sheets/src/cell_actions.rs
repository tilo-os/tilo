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
#[derive(Clone, Debug, PartialEq)]
pub struct SearchableCell {
    pub col: u32,
    pub row: u32,
    pub raw: String,
    pub display: String,
    pub value: CellValue,
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
                value: values.get(&(col, row)).cloned().unwrap_or(CellValue::Empty),
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
    // Presentation can add currency signs, separators or rounding. Sort
    // formula results by their value, never by that lossy display string.
    if cell.raw.starts_with('=')
        && let CellValue::Number(number) = &cell.value
    {
        return SortKey::Number(*number);
    }
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

/// The cell edits that answer with the range they changed, so a caller sees
/// the values it produced without reading them back.
pub const ANSWERING_EDITS: [&str; 5] = [
    "set-cells",
    "write-cells",
    "format-cells",
    "sort-rows",
    "delete-rows",
];

/// The most cells an edit answers with; a larger change answers its first
/// rows and names the whole range for `read-cells`.
pub const MAX_ANSWERED_CELLS: u32 = 256;

/// The answer of cell edit `action`, which applied to `doc` as `changes`:
/// the changed range as `read-cells` shows it, clipped to
/// [`MAX_ANSWERED_CELLS`] with `"changed"` naming the whole range when cut.
/// Deleting rows moves everything below, so it answers the used range from
/// the top.
pub fn answer(
    doc: &ui::tilo_resource::Doc,
    action: &str,
    changes: &[automerge::Patch],
) -> Option<serde_json::Value> {
    use ui::tilo_resource::Resource as _;
    if !ANSWERING_EDITS.contains(&action) {
        return None;
    }
    let workbook = crate::workbook::Workbook::read(doc).ok()?;
    let touched = touched_cells(changes);
    let sheet_id = touched
        .first()
        .map(|(sheet, _, _)| sheet.clone())
        .or_else(|| changes.iter().find_map(|patch| sheet_of(&props(patch))))?;
    let sheet = workbook.sheet(&ui::tilo_resource::Id::from_raw(sheet_id.clone()))?;
    let snap = sheet.snapshot();
    let (start, end) = if action == "delete-rows" {
        let used = snap.raw.keys().chain(snap.bold.iter()).copied();
        used.fold(((0, 0), (0, 0)), |(_, end), (col, row)| {
            ((0, 0), (end.0.max(col), end.1.max(row)))
        })
    } else {
        let layout = sheet.layout();
        let cells: Vec<Addr> = touched
            .iter()
            .filter(|(sheet, _, _)| *sheet == sheet_id)
            .filter_map(|(_, row, column)| {
                layout.addr(
                    &ui::tilo_resource::Id::from_raw(row.clone()),
                    &ui::tilo_resource::Id::from_raw(column.clone()),
                )
            })
            .collect();
        let (first, rest) = cells.split_first()?;
        rest.iter()
            .fold((*first, *first), |(start, end), &(col, row)| {
                (
                    (start.0.min(col), start.1.min(row)),
                    (end.0.max(col), end.1.max(row)),
                )
            })
    };
    let width = end.0.saturating_sub(start.0).saturating_add(1);
    let shown_width = width.min(MAX_ANSWERED_CELLS);
    let shown_rows = MAX_ANSWERED_CELLS
        .checked_div(shown_width)
        .unwrap_or(1)
        .max(1);
    let shown_end = (
        start.0.saturating_add(shown_width.saturating_sub(1)),
        end.1
            .min(start.1.saturating_add(shown_rows.saturating_sub(1))),
    );
    let mut content = range_content(&snap, start, shown_end);
    if shown_end != end
        && let Some(fields) = content.as_object_mut()
    {
        fields.insert("changed".into(), range_label(start, end).into());
    }
    Some(content)
}

/// The keys a patch walks from the root, its own key last.
fn props(patch: &automerge::Patch) -> Vec<String> {
    let mut out: Vec<String> = patch
        .path
        .iter()
        .map(|(_, prop)| prop.to_string())
        .collect();
    match &patch.action {
        automerge::PatchAction::PutMap { key, .. } | automerge::PatchAction::DeleteMap { key } => {
            out.push(key.clone());
        }
        _ => {}
    }
    out
}

/// The sheet a patch is in: the item of `sheets` it walks through.
fn sheet_of(props: &[String]) -> Option<String> {
    let at = props.iter().position(|prop| prop == "sheets")?;
    props.get(at + 1).cloned()
}

/// The cells `changes` wrote, as (sheet, row, column) ids: every patch that
/// reaches below a sheet's `cells` grid map at a row and a column.
fn touched_cells(changes: &[automerge::Patch]) -> Vec<(String, String, String)> {
    let mut out = Vec::new();
    for patch in changes {
        let props = props(patch);
        let Some(sheet) = sheet_of(&props) else {
            continue;
        };
        let Some(at) = props
            .windows(2)
            .position(|pair| matches!(pair, [field, map] if field == "cells" && map == "cells"))
        else {
            continue;
        };
        // A grid keeps its cells flat, each under `<row>/<column>`.
        if let Some((row, column)) = props.get(at + 2).and_then(|key| key.split_once('/')) {
            let cell = (sheet, row.to_owned(), column.to_owned());
            if !out.contains(&cell) {
                out.push(cell);
            }
        }
    }
    out
}

#[cfg(test)]
mod tests;
