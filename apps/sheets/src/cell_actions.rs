//! What agents and scripts read of a sheet, and the cell work the verbs
//! share: a range's cells as `read-cells` answers them, what each cell
//! shows, and the writes that sort rows.
//!
//! Arguments are A1 references and plain JSON values, and a read returns
//! only the cells a caller asked for, so a whole task fits in a few compact
//! calls.

use std::collections::HashMap;

use ui::tilo_resource::Id;

use crate::model::{Addr, CellFormat, CellValue, addr_to_a1, format_value, parse_number};
use crate::resource::{CellWrite, range_label};
use crate::workbook::{SheetSnapshot, Workbook};

/// The most cells one part of a read answers, as the record's own read and a file's.
pub const MAX_READ_CELLS: usize = crate::resource::MAX_READ_CELLS;

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
    let values = crate::calc::recalculate_snapshot(snap);
    shown_between(snap, &values, None, false)
}

/// [`shown_cells`], and the cells a formula spills into (`=UNIQUE(A2:A9)`
/// fills the cells below it): they hold no input of their own and show what
/// the formula computed for them.
pub fn shown_cells_and_spills(snap: &SheetSnapshot) -> Vec<SearchableCell> {
    let values = crate::calc::recalculate_snapshot(snap);
    shown_between(snap, &values, None, true)
}

/// [`shown_cells_and_spills`] of the cells of `snap` from `start` to `end` alone, whose cells
/// compute `values`: what a read of a range answers, without reading the rest of the sheet.
pub fn cells_between<S: std::hash::BuildHasher>(
    snap: &SheetSnapshot,
    values: &HashMap<Addr, CellValue, S>,
    start: Addr,
    end: Addr,
) -> Vec<SearchableCell> {
    shown_between(snap, values, Some((start, end)), true)
}

/// The cells of `snap` that compute `values`, within `bounds` when there are any, with what each
/// shows.
fn shown_between<S: std::hash::BuildHasher>(
    snap: &SheetSnapshot,
    values: &HashMap<Addr, CellValue, S>,
    bounds: Option<(Addr, Addr)>,
    spills: bool,
) -> Vec<SearchableCell> {
    let within = |&(col, row): &Addr| {
        bounds.is_none_or(|((left, top), (right, bottom))| {
            (left..=right).contains(&col) && (top..=bottom).contains(&row)
        })
    };
    let mut cells = snap
        .raw
        .iter()
        .filter(|(addr, _)| within(addr))
        .map(|(&(col, row), raw)| {
            let fmt = snap.fmt.get(&(col, row)).copied().unwrap_or_else(|| {
                raw.strip_prefix('=')
                    .and_then(|body| crate::calc::infer_result_format(body, &snap.fmt))
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
    if spills {
        cells.extend(
            values
                .iter()
                .filter(|(addr, value)| {
                    within(addr) && !snap.raw.contains_key(*addr) && **value != CellValue::Empty
                })
                .map(|(&(col, row), value)| {
                    let fmt = snap
                        .fmt
                        .get(&(col, row))
                        .copied()
                        .unwrap_or(CellFormat::General);
                    SearchableCell {
                        col,
                        row,
                        raw: String::new(),
                        display: format_value(value, fmt),
                        value: value.clone(),
                    }
                }),
        );
    }
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
    match (a, b) {
        (SortKey::Number(x), SortKey::Number(y)) => x.total_cmp(y),
        (SortKey::Text(x), SortKey::Text(y)) => x.cmp(y),
        (SortKey::Blank, SortKey::Blank) => Ordering::Equal,
        (SortKey::Number(_), _) | (SortKey::Text(_), SortKey::Blank) => Ordering::Less,
        _ => Ordering::Greater,
    }
}

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
                    crate::refs::offset_refs(&cell.raw, 0, shift)
                } else {
                    cell.raw.clone()
                }
            });
            writes.push(CellWrite::at((col, to), raw, None));
        }
    }
    writes
}

/// The cells `start..=end` of sheet `id` as a snapshot of those cells alone, and what each
/// computes: a literal is its own value, and the formula engine is asked only when a formula of
/// the sheet could reach the range, by its own cell or by spilling into one.
///
/// A range therefore costs what its cells do, not what the sheet does: the one pass it makes
/// over the sheet looks for a formula and stops at the first.
pub fn cells_of(
    book: &Workbook,
    id: &Id,
    start: Addr,
    end: Addr,
) -> (SheetSnapshot, HashMap<Addr, CellValue>) {
    let Some(sheet) = book.sheet(id) else {
        return (SheetSnapshot::default(), HashMap::new());
    };
    // A formula's value, and what it spills into, may lie anywhere on the sheet; without one,
    // no cell of the range reads another, so the range is read as it is.
    if !sheet.cells.any(|cell| cell.raw.is_formula()) {
        let mut snap = SheetSnapshot {
            name: sheet.name.0.clone(),
            hidden: sheet.hidden,
            ..SheetSnapshot::default()
        };
        for row in start.1..=end.1 {
            for col in start.0..=end.0 {
                let addr = (col, row);
                let Some(cell) = sheet.cell_at_place((col, row)) else {
                    continue;
                };
                if !cell.raw.is_empty() {
                    snap.raw.insert(addr, cell.raw.text().to_owned());
                }
                if cell.format != CellFormat::General {
                    snap.fmt.insert(addr, cell.format);
                }
                if cell.bold {
                    snap.bold.insert(addr);
                }
            }
        }
        let values = snap
            .raw
            .iter()
            .filter_map(|(&addr, raw)| {
                let value = crate::calc::literal(raw);
                (value != CellValue::Empty).then_some((addr, value))
            })
            .collect();
        return (snap, values);
    }
    let Some(full) = book.snapshot_of(id) else {
        return (SheetSnapshot::default(), HashMap::new());
    };
    let values = crate::calc::recalculate_snapshot(&full);
    (full, values)
}

/// What a range of sheet `id` holds, as values: numbers as numbers and dates as ISO text, with
/// the formulas, formats and bold cells by reference. `range` is every used cell when none, the
/// cells a formula spills into among them. A range of more than [`MAX_READ_CELLS`] cells
/// answers in parts of whole rows: `from` is the row a part starts at, as the sheet numbers it,
/// and the answer's `next` the row to read on from, beside the whole range as `of`. With
/// `shown`, what each cell shows comes beside its value.
///
/// A range reads the cells it names, not the whole sheet: see [`cells_of`].
///
/// # Errors
/// A range that is none, a `from` outside the range, and a sheet the workbook has not.
pub fn read(
    book: &Workbook,
    id: &Id,
    range: Option<&str>,
    from: Option<u32>,
    shown: bool,
) -> Result<serde_json::Value, String> {
    let (start, end, whole) = if let Some(range) = range {
        let (start, end) = crate::resource::parse_range(range)
            .ok_or_else(|| format!("`{range}` is not a cell or range such as A1:D6"))?;
        (start, end, None)
    } else {
        let snap = book
            .snapshot_of(id)
            .ok_or_else(|| "no such sheet".to_owned())?;
        let values = crate::calc::recalculate_snapshot(&snap);
        let spilled = values
            .iter()
            .filter(|(_, value)| **value != CellValue::Empty)
            .map(|(addr, _)| *addr);
        let end = snap
            .raw
            .keys()
            .chain(snap.bold.iter())
            .copied()
            .chain(spilled)
            .fold((0, 0), |end, (col, row)| (end.0.max(col), end.1.max(row)));
        ((0, 0), end, Some((snap, values)))
    };
    let first = if let Some(row) = from {
        row.checked_sub(1)
            .filter(|row| (start.1..=end.1).contains(row))
            .ok_or_else(|| {
                format!(
                    "`from` is a row of {}: {} to {}",
                    range_label(start, end),
                    start.1.saturating_add(1),
                    end.1.saturating_add(1)
                )
            })?
    } else {
        start.1
    };
    let width = usize::try_from(end.0.saturating_sub(start.0))
        .unwrap_or(usize::MAX)
        .saturating_add(1);
    let most_rows = u32::try_from((MAX_READ_CELLS / width).max(1)).unwrap_or(u32::MAX);
    let last = end.1.min(first.saturating_add(most_rows.saturating_sub(1)));
    let (snap, values) =
        whole.unwrap_or_else(|| cells_of(book, id, (start.0, first), (end.0, last)));
    let mut content = range_content_of(&snap, &values, (start.0, first), (end.0, last), shown);
    if (first, last) != (start.1, end.1)
        && let Some(fields) = content.as_object_mut()
    {
        fields.insert("of".into(), range_label(start, end).into());
        if last < end.1 {
            fields.insert("next".into(), last.saturating_add(2).into());
        }
    }
    Ok(content)
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
    match (seconds / 3600 % 24, seconds % 3600 / 60, seconds % 60) {
        (0, 0, 0) => date,
        (hour, minute, 0) => ui::format::machine!(
            "the ISO 8601 text set-cells reads back, not a display",
            format!("{date}T{hour:02}:{minute:02}")
        ),
        (hour, minute, second) => ui::format::machine!(
            "the ISO 8601 text set-cells reads back, not a display",
            format!("{date}T{hour:02}:{minute:02}:{second:02}")
        ),
    }
}

/// `value` as a read answers it, shown in `format`: a number in a date cell
/// is its ISO date, the way the sheet shows it rather than the serial behind
/// it; a literal in a text cell is the text typed (`007`, not 7); other
/// numbers keep 15 significant digits.
pub(crate) fn json_value(
    value: &CellValue,
    format: CellFormat,
    raw: Option<&str>,
) -> serde_json::Value {
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
            .and_then(|body| crate::calc::infer_result_format(body, &snap.fmt))
            .unwrap_or(CellFormat::General)
    })
}

/// The cells `start..=end` of `snap`, whose cells compute `values`, as a read answers them;
/// with `shown`, also what each shows.
fn range_content_of(
    snap: &SheetSnapshot,
    values: &HashMap<Addr, CellValue>,
    start: Addr,
    end: Addr,
    shown: bool,
) -> serde_json::Value {
    let mut rows = Vec::new();
    let mut words = Vec::new();
    let mut formulas = serde_json::Map::new();
    let mut formats = serde_json::Map::new();
    let mut bold = Vec::new();
    for row in start.1..=end.1 {
        let mut line = Vec::new();
        let mut line_words = Vec::new();
        for col in start.0..=end.0 {
            let raw = snap.raw.get(&(col, row));
            let held = values
                .get(&(col, row))
                .map(|value| (value, shown_format(snap, (col, row))));
            line.push(held.map_or(serde_json::Value::Null, |(value, format)| {
                json_value(value, format, raw.map(String::as_str))
            }));
            if shown {
                line_words.push(serde_json::Value::String(
                    held.map_or_else(String::new, |(value, format)| format_value(value, format)),
                ));
            }
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
        words.push(serde_json::Value::Array(line_words));
    }
    let mut content = serde_json::json!({
        "sheet": snap.name,
        "range": range_label(start, end),
        "rows": rows,
        "formulas": formulas,
        "bold": bold,
    });
    if let Some(fields) = content.as_object_mut() {
        if !formats.is_empty() {
            fields.insert("formats".into(), formats.into());
        }
        if shown {
            fields.insert("shown".into(), words.into());
        }
    }
    note_formula_errors(&mut content, snap, start, end);
    content
}

/// A format as `format-cells` and the `formats` of `new-sheet` take it:
/// its category, with the currency when it is money (`currency:EUR`).
pub(crate) fn format_name(format: CellFormat) -> String {
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
            let error = crate::calc::parse_error(raw)?;
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

pub const MAX_ANSWERED_CELLS: u32 = 256;

pub fn answer(
    doc: &ui::tilo_resource::Doc,
    action: &str,
    changes: &[automerge::Patch],
) -> Option<serde_json::Value> {
    use ui::tilo_resource::Resource as _;
    if !ANSWERING_EDITS.contains(&action) {
        return None;
    }
    if changes.is_empty() {
        return Some(serde_json::json!({ "unchanged": true }));
    }
    let workbook = crate::workbook::Workbook::read(doc).ok()?;
    let touched = touched_cells(changes);
    // The answer is about the sheet the rows left, whichever cells the delete also wrote.
    let sheet_id = reshaped_sheet(changes)
        .filter(|_| action == "delete-rows")
        .or_else(|| touched.first().map(|(sheet, _, _)| sheet.clone()))
        .or_else(|| changes.iter().find_map(|patch| sheet_of(&props(patch))))?;
    let sheet_key = ui::tilo_resource::Id::from_raw(sheet_id.clone());
    let sheet = workbook.sheet(&sheet_key)?;
    let (start, end) = if action == "delete-rows" {
        let snap = workbook.snapshot_of(&sheet_key)?;
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
        let Some((first, rest)) = cells.split_first() else {
            // A change that wrote no cell, as a sheet made with none: the sheet itself, so
            // its maker has its id.
            return Some(serde_json::json!({
                "sheet": sheet.name.0,
                "item": sheet_id,
                "rows": [],
                "seen": workbook.seen(),
            }));
        };
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
    // The answer reads the cells the change wrote, not the sheet around them.
    let (snap, values) = cells_of(&workbook, &sheet_key, start, shown_end);
    let mut content = range_content_of(&snap, &values, start, shown_end, false);
    if let Some(fields) = content.as_object_mut() {
        // The sheet's id, so a sheet just made is addressed without a read.
        fields.insert("item".into(), sheet_id.into());
        // And where rows and columns are now, for the caller's next write to name.
        fields.insert("seen".into(), workbook.seen().into());
        if shown_end != end {
            fields.insert("changed".into(), range_label(start, end).into());
        }
    }
    if shown_end != end {
        note_formula_errors(&mut content, &snap, start, end);
    }
    let note =
        match action {
            "delete-rows" => lost_references(&workbook, &sheet_key, changes)
                .map(|lost| ("reference_errors", lost)),
            "sort-rows" => unmoved_references(&workbook, &sheet_key, start, end)
                .map(|kept| ("references", kept)),
            _ => None,
        };
    if let (Some((key, note)), Some(fields)) = (note, content.as_object_mut()) {
        fields.insert(key.into(), note);
    }
    Some(content)
}

/// The most cells an answer names when it lists the formulas a change concerns.
const MAX_NAMED_CELLS: usize = 20;

/// `cells`, each named with its sheet, as an answer lists them: the first few and how many more.
fn named_cells(cells: Vec<String>, how: &str) -> Option<serde_json::Value> {
    if cells.is_empty() {
        return None;
    }
    let more = cells.len().saturating_sub(MAX_NAMED_CELLS);
    let mut note = serde_json::json!({
        "cells": cells.into_iter().take(MAX_NAMED_CELLS).collect::<Vec<_>>(),
        "how": how,
    });
    if more > 0
        && let Some(fields) = note.as_object_mut()
    {
        fields.insert("more".into(), more.into());
    }
    Some(note)
}

/// The rows and the columns `changes` deleted, by id.
fn deleted_lines(changes: &[automerge::Patch]) -> (Vec<String>, Vec<String>) {
    let (mut rows, mut columns) = (Vec::new(), Vec::new());
    for patch in changes {
        if !matches!(patch.action, automerge::PatchAction::DeleteMap { .. }) {
            continue;
        }
        if let [.., field, lines, id] = props(patch).as_slice()
            && field == "cells"
        {
            match lines.as_str() {
                "rows" => rows.push(id.clone()),
                "columns" => columns.push(id.clone()),
                _ => {}
            }
        }
    }
    (rows, columns)
}

/// The formulas, on every sheet, that pointed at the rows or columns `changes` deleted from
/// sheet `sheet` and read `#REF!` now: what pointed at the deleted cells says so rather than
/// reading the cells that slid into their place. A range that only began or ended there shrank
/// and is left out.
fn lost_references(
    workbook: &crate::workbook::Workbook,
    sheet: &ui::tilo_resource::Id,
    changes: &[automerge::Patch],
) -> Option<serde_json::Value> {
    let (rows, columns) = deleted_lines(changes);
    let mut cells = Vec::new();
    for (id, held) in workbook.sheets.iter() {
        let layout = held.layout();
        let mut lost: Vec<Addr> = held
            .cells
            .cells()
            .filter(|(_, _, cell)| {
                let raw = &cell.raw;
                (!rows.is_empty() && raw.reads_line(id, sheet, true, &rows))
                    || (!columns.is_empty() && raw.reads_line(id, sheet, false, &columns))
            })
            .filter_map(|(row, column, _)| layout.addr(&row, &column))
            .collect();
        lost.sort_unstable_by_key(|(col, row)| (*row, *col));
        cells.extend(
            lost.into_iter()
                .map(|(col, row)| format!("{}!{}", held.name.0, addr_to_a1(col, row))),
        );
    }
    named_cells(
        cells,
        "these formulas pointed at deleted cells and hold #REF! now; write them again to point them elsewhere",
    )
}

/// The formulas outside the sorted cells `start..=end` of sheet `sorted` that point at some of
/// its rows: a sort moves what cells hold and no reference follows it, so these read other
/// rows' values now. One that reads every sorted row reads the same values as before and is
/// left out.
fn unmoved_references(
    workbook: &crate::workbook::Workbook,
    sorted: &ui::tilo_resource::Id,
    start: Addr,
    end: Addr,
) -> Option<serde_json::Value> {
    let named = workbook.name_reaching(sorted);
    let lines = crate::bound::Lines::of(workbook);
    let mut cells = Vec::new();
    for (id, sheet) in workbook.sheets.iter() {
        let own = id == sorted;
        if !own && named.is_none() {
            continue;
        }
        let Some(layout) = lines.layout(id) else {
            continue;
        };
        let mut pointing: Vec<Addr> = sheet
            .cells
            .cells()
            .filter(|(_, _, cell)| cell.raw.is_formula() && (own || cell.raw.text().contains('!')))
            .filter_map(|(row, column, cell)| {
                let at = layout.addr(&row, &column)?;
                let moved_itself =
                    own && (start.0..=end.0).contains(&at.0) && (start.1..=end.1).contains(&at.1);
                if moved_itself {
                    return None;
                }
                let mut points = false;
                let shown = cell.raw.shown(&layout, &lines);
                crate::refs::each_reference(&shown, |sheet, first, last| {
                    let reads_sorted = match sheet {
                        None => own,
                        Some(written) => {
                            named.as_deref() == Some(crate::refs::fold_sheet(written).as_str())
                        }
                    };
                    let meets = first.0 <= end.0
                        && last.0 >= start.0
                        && first.1 <= end.1
                        && last.1 >= start.1;
                    let every_row = first.1 <= start.1 && last.1 >= end.1;
                    points |= reads_sorted && meets && !every_row;
                });
                points.then_some(at)
            })
            .collect();
        pointing.sort_unstable_by_key(|(col, row)| (*row, *col));
        cells.extend(
            pointing
                .into_iter()
                .map(|(col, row)| format!("{}!{}", sheet.name.0, addr_to_a1(col, row))),
        );
    }
    named_cells(
        cells,
        "a sort moves what cells hold, not the formulas pointing at them: these keep their addresses and read what the sort put there",
    )
}

/// The sheet whose rows or columns `changes` added or removed.
fn reshaped_sheet(changes: &[automerge::Patch]) -> Option<String> {
    changes.iter().find_map(|patch| {
        let props = props(patch);
        props
            .windows(2)
            .any(|pair| matches!(pair, [field, lines] if field == "cells" && (lines == "rows" || lines == "columns")))
            .then(|| sheet_of(&props))
            .flatten()
    })
}

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

fn sheet_of(props: &[String]) -> Option<String> {
    let at = props.iter().position(|prop| prop == "sheets")?;
    props.get(at + 1).cloned()
}

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
