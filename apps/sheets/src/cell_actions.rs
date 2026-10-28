//! What agents and scripts read of a sheet, and the cell work the verbs
//! share: a range's cells as `read-cells` answers them, what each cell
//! shows, and the writes that sort rows.
//!
//! Arguments are A1 references and plain JSON values, and a read returns
//! only the cells a caller asked for, so a whole task fits in a few compact
//! calls.

use std::collections::HashMap;

use crate::model::{Addr, CellFormat, CellValue, addr_to_a1, format_value, parse_number};
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
    let values = crate::formula::recalculate_snapshot(snap);
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
    } else if let Some(number) = parse_number(shown) {
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

/// `number` to the 15 significant digits a spreadsheet keeps, so a read
/// shows `0.75` for a margin computed as `0.7499999999999999`: the noise of
/// binary arithmetic is not a value anyone entered or a formula meant.
pub(crate) fn significant(number: f64) -> f64 {
    format!("{number:.14e}").parse().unwrap_or(number)
}

/// The ISO text of date serial `serial` ("2026-10-22", with "T09:30" when it
/// holds a time of day): what `set-cells` takes back as the same date, so a
/// read date can be written again and a caller never sees a bare serial.
fn iso_date_text(serial: f64) -> String {
    let date = crate::model::serial_to_date(serial, tilo_locale::DateStyle::Iso);
    let seconds = (serial.fract().abs() * 86_400.0).round();
    // A fraction of a day is below 86 401 seconds, exact as an integer.
    let seconds = format!("{seconds:.0}").parse::<u32>().unwrap_or(0);
    // format: machine — the ISO 8601 text set-cells reads back, not a display.
    match (seconds / 3600 % 24, seconds % 3600 / 60, seconds % 60) {
        (0, 0, 0) => date,
        (hour, minute, 0) => format!("{date}T{hour:02}:{minute:02}"),
        (hour, minute, second) => format!("{date}T{hour:02}:{minute:02}:{second:02}"),
    }
}

/// `value` as a read answers it, shown in `format`: a number in a date cell
/// is its ISO date, the way the sheet shows it rather than the serial behind
/// it; a literal in a text cell is the text typed (`007`, not 7); other
/// numbers keep 15 significant digits.
fn json_value(value: &CellValue, format: CellFormat, raw: Option<&str>) -> serde_json::Value {
    match value {
        CellValue::Empty => serde_json::Value::Null,
        CellValue::Number(serial) if matches!(format, CellFormat::Date(_)) => {
            serde_json::Value::String(iso_date_text(*serial))
        }
        CellValue::Number(_) | CellValue::Bool(_)
            if format == CellFormat::Text && raw.is_some_and(|raw| !raw.starts_with('=')) =>
        {
            serde_json::Value::String(raw.unwrap_or_default().to_owned())
        }
        CellValue::Number(number) => serde_json::Number::from_f64(significant(*number))
            .map_or_else(
                || serde_json::Value::String(number.to_string()),
                serde_json::Value::Number,
            ),
        CellValue::Bool(value) => serde_json::Value::Bool(*value),
        CellValue::Text(_) | CellValue::Error(_) => {
            serde_json::Value::String(value.to_plain_string())
        }
    }
}

/// The format the cell at `addr` of `snap` shows in: its own, else the one
/// its formula takes from its operands (a sum of amounts is an amount).
fn shown_format(snap: &SheetSnapshot, addr: Addr) -> CellFormat {
    snap.fmt.get(&addr).copied().unwrap_or_else(|| {
        snap.raw
            .get(&addr)
            .and_then(|raw| raw.strip_prefix('='))
            .and_then(|body| crate::formula::infer_result_format(body, &snap.fmt))
            .unwrap_or(CellFormat::General)
    })
}

/// `start..=end` of `snap` as `read-cells` answers it: computed rows,
/// formulas, bold cells and the number formats cells have of their own.
fn range_content(snap: &SheetSnapshot, start: Addr, end: Addr) -> serde_json::Value {
    let values = crate::formula::recalculate_snapshot(snap);
    let mut rows = Vec::new();
    let mut formulas = serde_json::Map::new();
    let mut formats = serde_json::Map::new();
    let mut bold = Vec::new();
    for row in start.1..=end.1 {
        let mut line = Vec::new();
        for col in start.0..=end.0 {
            let raw = snap.raw.get(&(col, row));
            line.push(
                values
                    .get(&(col, row))
                    .map_or(serde_json::Value::Null, |value| {
                        json_value(
                            value,
                            shown_format(snap, (col, row)),
                            raw.map(String::as_str),
                        )
                    }),
            );
            if let Some(raw) = raw.filter(|raw| raw.starts_with('=')) {
                formulas.insert(addr_to_a1(col, row), raw.clone().into());
            }
            if let Some(format) = snap.fmt.get(&(col, row)) {
                formats.insert(addr_to_a1(col, row), format_name(*format).into());
            }
            if snap.bold.contains(&(col, row)) {
                bold.push(serde_json::Value::String(addr_to_a1(col, row)));
            }
        }
        rows.push(serde_json::Value::Array(line));
    }
    let mut content = serde_json::json!({
        "sheet": snap.name,
        "range": range_label(start, end),
        "rows": rows,
        "formulas": formulas,
        "bold": bold,
    });
    if !formats.is_empty()
        && let Some(fields) = content.as_object_mut()
    {
        fields.insert("formats".into(), formats.into());
    }
    note_formula_errors(&mut content, snap, start, end);
    content
}

/// A format as `format-cells` and the `formats` of `new-sheet` take it:
/// its category, with the currency when it is money (`currency:EUR`).
fn format_name(format: CellFormat) -> String {
    match format {
        CellFormat::Currency(_) => format.as_str(),
        other => other
            .as_str()
            .split(':')
            .next()
            .unwrap_or_default()
            .to_owned(),
    }
}

/// The input and format a JSON cell value writes, as typing it would: "=…" a
/// formula, a leading apostrophe ("'007") text without the apostrophe, an
/// exact ISO date ("2026-10-22", optionally "2026-10-22T09:30") a real date
/// in the date format, so it calculates and exports as a date; numbers and
/// booleans as they are, other text as text. `None` for null, which leaves
/// the cell.
///
/// # Errors
/// A value that is no string, number, boolean or null.
pub fn cell_input(
    value: &serde_json::Value,
) -> Result<Option<(String, Option<CellFormat>)>, ui::tilo_resource::Refusal> {
    let text = match value {
        serde_json::Value::Null => return Ok(None),
        serde_json::Value::String(text) => text,
        serde_json::Value::Number(number) => return Ok(Some((number.to_string(), None))),
        serde_json::Value::Bool(true) => return Ok(Some(("TRUE".into(), None))),
        serde_json::Value::Bool(false) => return Ok(Some(("FALSE".into(), None))),
        _ => {
            return Err(ui::tilo_resource::Refusal::new(
                "cell values are strings, numbers, booleans or null",
            ));
        }
    };
    if let Some(rest) = text
        .trim()
        .strip_prefix('\'')
        .filter(|rest| !rest.is_empty())
    {
        // A marked `'=x` keeps its apostrophe: without it the input would be
        // a formula, the opposite of what the mark asks for.
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
            Some(CellFormat::Date(tilo_locale::DateStyle::Iso)),
        )));
    }
    Ok(Some((text.clone(), None)))
}

/// The Excel serial of an exact ISO date `YYYY-MM-DD`, optionally followed by
/// a time `THH:MM` or ` HH:MM` with optional `:SS` (the serial's fraction).
/// Anything looser (a missing zero, an impossible day such as `2026-02-30`,
/// a zone) is no date here, so text stays text.
pub(crate) fn iso_date_serial(text: &str) -> Option<f64> {
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
    let serial = crate::model::date_to_serial(i64::from(year), month, day);
    // A day past its month's end rolls over; such a date is no date.
    if crate::model::serial_to_ymd(serial) != (i64::from(year), month, day) {
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

/// Adds to `content` the formulas of `start..=end` that do not parse, when
/// there are any.
fn note_formula_errors(
    content: &mut serde_json::Value,
    snap: &SheetSnapshot,
    start: Addr,
    end: Addr,
) {
    let errors = formula_errors(snap, start, end);
    if let Some(fields) = content.as_object_mut() {
        if errors.is_empty() {
            fields.remove("formula_errors");
        } else {
            fields.insert("formula_errors".into(), errors.into());
        }
    }
}

/// The formulas of `start..=end` that do not parse, each with its cell, the
/// error it shows and what is wrong, at most [`MAX_ANSWERED_CELLS`]. The
/// cells keep them, as a spreadsheet does; this tells the writer what to fix.
fn formula_errors(snap: &SheetSnapshot, start: Addr, end: Addr) -> Vec<serde_json::Value> {
    let mut cells: Vec<(Addr, &String)> = snap
        .raw
        .iter()
        .filter(|((col, row), _)| {
            (start.0..=end.0).contains(col) && (start.1..=end.1).contains(row)
        })
        .map(|(addr, raw)| (*addr, raw))
        .collect();
    cells.sort_unstable_by_key(|((col, row), _)| (*row, *col));
    cells
        .into_iter()
        .filter_map(|((col, row), raw)| {
            let error = crate::formula::parse_error(raw)?;
            Some(serde_json::json!({
                "cell": addr_to_a1(col, row),
                "shows": error.error.text(),
                "message": error.problem,
            }))
        })
        .take(MAX_ANSWERED_CELLS as usize)
        .collect()
}

/// The cell edits that answer with the range they changed, so a caller sees
/// the values it produced without reading them back.
pub const ANSWERING_EDITS: [&str; 8] = [
    "new-sheet",
    "set-records",
    "upsert-item",
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
    // An edit run again that wrote nothing new (an upsert of a row as it
    // is) still answers, so its caller sees it held rather than failed.
    if changes.is_empty() {
        return Some(serde_json::json!({ "unchanged": true }));
    }
    let workbook = crate::workbook::Workbook::read(doc).ok()?;
    let touched = touched_cells(changes);
    let sheet_id = touched
        .first()
        .map(|(sheet, _, _)| sheet.clone())
        .or_else(|| changes.iter().find_map(|patch| sheet_of(&props(patch))))?;
    let sheet_key = ui::tilo_resource::Id::from_raw(sheet_id.clone());
    let sheet = workbook.sheet(&sheet_key)?;
    let snap = workbook.snapshot_of(&sheet_key)?;
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
    if let Some(fields) = content.as_object_mut() {
        // The sheet's id, so a sheet just made is addressed without a read.
        fields.insert("item".into(), sheet_id.into());
        if shown_end != end {
            fields.insert("changed".into(), range_label(start, end).into());
        }
    }
    if shown_end != end {
        note_formula_errors(&mut content, &snap, start, end);
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
