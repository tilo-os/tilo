//! The cell-level agent surface on each `sheets.sheet`: read a range, write
//! values and formulas, style a range, sort rows and delete rows.
//!
//! Every mutating action compiles to one ordinary [`SheetsAction`], so it is
//! one undo step and replays like the same edit made on screen. Arguments are
//! A1 references and plain JSON values, and the read returns only the cells a
//! caller asked for, so a whole task fits in a few compact calls.

use std::collections::HashMap;
use std::sync::OnceLock;

use automerge::{AutoCommit, ChangeHash};
use serde::{Deserialize, Serialize};
use tilo_actions::{
    ActionContract, ActionParam, ActionSpec, EncodedAction, ExecutionLocus, OperationKind,
    ParamType, Resource, ResultSpec,
};
use ui::semantic_document_read::{
    MAX_REQUEST_BYTES, ReadPage, RegisteredSemanticReader, SemanticReadError,
};

use crate::actions::{CellWrite, SearchableCell, SheetsAction, encode, sheet_resource_id};
use crate::doc::SheetsDoc;
use crate::model::{Addr, CellFormat, CellValue, a1_to_addr, addr_to_a1, letters_to_col};
use tilo_locale::{Currency, DateStyle, TimeStyle};

pub const READ_OPERATION_TYPE: &str = "sheets.read";
pub static READERS: [RegisteredSemanticReader; 1] = [RegisteredSemanticReader::new(
    READ_OPERATION_TYPE,
    read_result_schema,
    validate_read,
    project_read,
)];

/// The most cells one read returns; a larger range asks for a smaller one.
pub const MAX_READ_CELLS: usize = 4096;
const MAX_WRITES: usize = 16 * 1024;

const FORMATS: [&str; 7] = [
    "general", "number", "currency", "percent", "date", "time", "text",
];

fn invalid(message: impl Into<String>) -> EncodedAction {
    EncodedAction {
        type_id: tilo_actions::resource::INVALID_ARGUMENTS_TYPE.into(),
        payload: message.into().into_bytes(),
    }
}

/// `A1` or `A1:D6`, normalised so the first corner is the top-left one.
pub fn parse_range(text: &str) -> Option<(Addr, Addr)> {
    let (a, b) = text.split_once(':').unwrap_or((text, text));
    let (a, b) = (a1_to_addr(a)?, a1_to_addr(b)?);
    Some(((a.0.min(b.0), a.1.min(b.1)), (a.0.max(b.0), a.1.max(b.1))))
}

fn range_label(start: Addr, end: Addr) -> String {
    if start == end {
        addr_to_a1(start.0, start.1)
    } else {
        format!(
            "{}:{}",
            addr_to_a1(start.0, start.1),
            addr_to_a1(end.0, end.1)
        )
    }
}

/// A JSON cell value as the raw input Sheets stores; `None` leaves the cell.
fn raw_of(value: &serde_json::Value) -> Result<Option<String>, String> {
    Ok(Some(match value {
        serde_json::Value::Null => return Ok(None),
        serde_json::Value::String(text) => text.clone(),
        serde_json::Value::Number(number) => number.to_string(),
        serde_json::Value::Bool(true) => "TRUE".into(),
        serde_json::Value::Bool(false) => "FALSE".into(),
        _ => return Err("cell values are strings, numbers, booleans or null".into()),
    }))
}

pub(crate) fn format_code(name: &str) -> Option<String> {
    Some(
        match name {
            "general" => CellFormat::General,
            "number" => CellFormat::Number,
            "currency" => CellFormat::Currency(Currency::USD),
            "percent" => CellFormat::Percent,
            "date" => CellFormat::Date(DateStyle::Iso),
            "time" => CellFormat::Time(TimeStyle::H24),
            "text" => CellFormat::Text,
            _ => return None,
        }
        .as_str(),
    )
}

/// The cells `rows` write rightward and down from `start`: "=…" a formula,
/// "" a clear, null a skip.
///
/// # Errors
///
/// Why the rows cannot be written: not rows of values, none at all, or
/// more than [`MAX_WRITES`].
pub(crate) fn cells_from_rows(
    start: (u32, u32),
    rows: &[serde_json::Value],
) -> Result<Vec<CellWrite>, String> {
    let mut cells = Vec::new();
    for (dr, row) in (0u32..).zip(rows) {
        let row = row
            .as_array()
            .ok_or("each row must be an array of values")?;
        for (dc, value) in (0u32..).zip(row) {
            if let Some(raw) = raw_of(value)? {
                cells.push(CellWrite {
                    col: start.0.saturating_add(dc),
                    row: start.1.saturating_add(dr),
                    raw,
                    fmt: None,
                });
            }
        }
    }
    if cells.is_empty() {
        return Err("rows hold no values to write".into());
    }
    if cells.len() > MAX_WRITES {
        return Err(format!("at most {MAX_WRITES} cells per call"));
    }
    Ok(cells)
}

/// `cells` with the formats `formats` gives their ranges (`{"D2:D5":
/// "percent"}`); a format on a cell no row wrote writes it empty.
///
/// # Errors
///
/// Why the formats cannot apply: not an object of ranges to known format
/// names.
pub(crate) fn with_formats(
    mut cells: Vec<CellWrite>,
    formats: &serde_json::Map<String, serde_json::Value>,
) -> Result<Vec<CellWrite>, String> {
    for (range, name) in formats {
        let ((c0, r0), (c1, r1)) =
            parse_range(range).ok_or_else(|| format!("`{range}` is not a cell or range"))?;
        let fmt = name
            .as_str()
            .and_then(format_code)
            .ok_or_else(|| format!("`{name}` is not one of {}", FORMATS.join(", ")))?;
        for row in r0.min(r1)..=r0.max(r1) {
            for col in c0.min(c1)..=c0.max(c1) {
                match cells
                    .iter_mut()
                    .find(|cell| cell.col == col && cell.row == row)
                {
                    Some(cell) => cell.fmt = Some(fmt.clone()),
                    None => cells.push(CellWrite {
                        col,
                        row,
                        raw: String::new(),
                        fmt: Some(fmt.clone()),
                    }),
                }
            }
        }
    }
    if cells.len() > MAX_WRITES {
        return Err(format!("at most {MAX_WRITES} cells per call"));
    }
    Ok(cells)
}

fn set_cells_spec(sheet: usize, sheet_id: &str) -> ActionSpec {
    let sheet_id = sheet_id.to_string();
    ActionSpec::typed(
        "set-cells",
        "Set cells",
        "start cell and rows of values",
        ExecutionLocus::ClientLocal,
        vec![
            ActionParam::required("start", ParamType::Text, "Top-left cell, e.g. A1"),
            ActionParam::required(
                "rows",
                ParamType::Array,
                "Rows of values written rightward and down from start; \"=…\" is a formula, \"\" clears, null skips",
            ),
        ],
        move |args| {
            let Some(start) = args.text("start").and_then(a1_to_addr) else {
                return invalid("start must be a cell such as A1");
            };
            let Some(rows) = args.array("rows") else {
                return invalid("rows must be an array of rows");
            };
            let cells = match cells_from_rows(start, rows) {
                Ok(cells) => cells,
                Err(message) => return invalid(message),
            };
            encode(&SheetsAction::SetCells {
                sheet,
                sheet_id: sheet_id.clone(),
                cells,
            })
        },
    )
    .reversible()
}

fn format_cells_spec(sheet_id: &str) -> ActionSpec {
    let sheet_id = sheet_id.to_string();
    ActionSpec::typed(
        "format-cells",
        "Format cells",
        "range, bold and/or format",
        ExecutionLocus::ClientLocal,
        vec![
            ActionParam::required("range", ParamType::Text, "Cell or range, e.g. A1:D1"),
            ActionParam::optional("bold", ParamType::Boolean, "Bold text on or off"),
            ActionParam::optional("format", ParamType::Text, "Number format").with_choices(FORMATS),
        ],
        move |args| {
            let Some((start, end)) = args.text("range").and_then(parse_range) else {
                return invalid("range must look like A1 or A1:D1");
            };
            let bold = args.boolean("bold");
            let fmt = args.text("format").and_then(format_code);
            if bold.is_none() && fmt.is_none() {
                return invalid("give bold, format or both");
            }
            encode(&SheetsAction::SetStyle {
                sheet_id: sheet_id.clone(),
                c0: start.0,
                r0: start.1,
                c1: end.0,
                r1: end.1,
                bold,
                fmt,
            })
        },
    )
    .reversible()
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
            writes.push(CellWrite {
                col,
                row: to,
                raw,
                fmt: None,
            });
        }
    }
    writes
}

/// Sorting reads what the sheet's cells show, calculated when a sort is
/// asked for rather than whenever the sheet is published.
fn sort_rows_spec(
    sheet: usize,
    sheet_id: &str,
    info: std::sync::Arc<crate::resource::SheetInfo>,
) -> ActionSpec {
    let sheet_id = sheet_id.to_string();
    ActionSpec::typed(
        "sort-rows",
        "Sort rows",
        "range and the column to sort by",
        ExecutionLocus::ClientLocal,
        vec![
            ActionParam::required(
                "range",
                ParamType::Text,
                "Rows to reorder, e.g. A2:C9 (leave the header out)",
            ),
            ActionParam::required("by", ParamType::Text, "Column letter to sort by, e.g. C"),
            ActionParam::optional(
                "descending",
                ParamType::Boolean,
                "Largest first; default smallest first",
            ),
        ],
        move |args| {
            let Some(range) = args.text("range").and_then(parse_range) else {
                return invalid("range must look like A2:C9");
            };
            let Some(by) = args.text("by").and_then(|by| letters_to_col(by.trim())) else {
                return invalid("by must be a column letter");
            };
            if by < range.0.0 || by > range.1.0 {
                return invalid("by must be a column inside range");
            }
            let writes = sort_writes(
                info.cells(),
                range,
                by,
                args.boolean("descending").unwrap_or(false),
            );
            if writes.len() > MAX_WRITES {
                return invalid(format!("at most {MAX_WRITES} cells per sort"));
            }
            encode(&SheetsAction::SetCells {
                sheet,
                sheet_id: sheet_id.clone(),
                cells: writes,
            })
        },
    )
    .reversible()
}

fn delete_rows_spec(sheet_id: &str) -> ActionSpec {
    let sheet_id = sheet_id.to_string();
    ActionSpec::typed(
        "delete-rows",
        "Delete rows",
        "row numbers",
        ExecutionLocus::ClientLocal,
        vec![ActionParam::required(
            "rows",
            ParamType::Array,
            "Row numbers as shown (1-based) to delete in one step; rows below move up",
        )],
        move |args| {
            let Some(rows) = args.array("rows") else {
                return invalid("rows must be an array of row numbers");
            };
            let mut zero_based = Vec::new();
            for row in rows {
                match row.as_u64().and_then(|row| u32::try_from(row).ok()) {
                    Some(row) if row >= 1 => zero_based.push(row - 1),
                    _ => return invalid("rows are row numbers starting at 1"),
                }
            }
            if zero_based.is_empty() {
                return invalid("name at least one row");
            }
            encode(&SheetsAction::DeleteRows {
                sheet_id: sheet_id.clone(),
                rows: zero_based,
            })
        },
    )
    .reversible()
    .destructive()
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct ReadRequest {
    sheet: String,
    range: Option<String>,
}

fn read_cells_spec(sheet_id: &str) -> ActionSpec {
    let sheet_id = sheet_id.to_string();
    ActionSpec::typed_operation(
        "read-cells",
        "Read cells",
        "range (omit for every used cell)",
        OperationKind::Effect,
        READ_OPERATION_TYPE,
        vec![ActionParam::optional(
            "range",
            ParamType::Text,
            "Cell or range, e.g. A1:D6; omit for the used range",
        )],
        move |args| {
            let request = ReadRequest {
                sheet: sheet_id.clone(),
                range: args.text("range").map(str::to_owned),
            };
            EncodedAction::encode_operation(READ_OPERATION_TYPE, &request)
                .unwrap_or_else(|_| invalid("the read request cannot be encoded"))
        },
    )
    .with_contract(ActionContract {
        result: ResultSpec::Json {
            schema: read_result_schema().clone(),
        },
        ..ActionContract::view()
    })
    .reads_document()
}

/// The cell actions each sheet publishes beside rename/delete/insert.
pub fn sheet_actions(
    sheet: usize,
    sheet_id: &str,
    info: std::sync::Arc<crate::resource::SheetInfo>,
) -> Vec<ActionSpec> {
    let mut specs = vec![
        read_cells_spec(sheet_id),
        set_cells_spec(sheet, sheet_id),
        format_cells_spec(sheet_id),
        sort_rows_spec(sheet, sheet_id, info),
        delete_rows_spec(sheet_id),
    ];
    for spec in &mut specs {
        if ANSWERING_EDITS.contains(&spec.id.as_str()) {
            spec.contract.result = ResultSpec::Json {
                schema: edit_result_schema(),
            };
        }
    }
    specs
}

fn validate_read(resource: &Resource, payload: &[u8]) -> Result<(), SemanticReadError> {
    if payload.len() > MAX_REQUEST_BYTES {
        return Err(SemanticReadError::Limit);
    }
    let request: ReadRequest =
        serde_json::from_slice(payload).map_err(|_| SemanticReadError::InvalidRequest)?;
    if resource.kind != "sheets.sheet" || resource.id.as_str() != sheet_resource_id(&request.sheet)
    {
        return Err(SemanticReadError::InvalidRequest);
    }
    if request
        .range
        .as_deref()
        .is_some_and(|range| parse_range(range).is_none())
    {
        return Err(SemanticReadError::InvalidRequest);
    }
    Ok(())
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

/// `rows` are computed values (numbers stay numbers), `formulas` and `bold`
/// name only the cells that have them.
fn project_read(
    document: &AutoCommit,
    _heads: &[ChangeHash],
    payload: &[u8],
) -> Result<ReadPage, SemanticReadError> {
    let request: ReadRequest =
        serde_json::from_slice(payload).map_err(|_| SemanticReadError::InvalidRequest)?;
    let sheet = SheetsDoc::sheet_index_by_resource_id(document, &request.sheet)
        .ok_or(SemanticReadError::InvalidRequest)?;
    let snap = SheetsDoc::snapshot_sheet(document, sheet, Currency::USD);
    let used = snap.raw.keys().chain(snap.bold.iter()).copied();
    let (start, end) = match request.range.as_deref() {
        Some(range) => parse_range(range).ok_or(SemanticReadError::InvalidRequest)?,
        None => used.fold(((0, 0), (0, 0)), |(_, end), (col, row)| {
            ((0, 0), (end.0.max(col), end.1.max(row)))
        }),
    };
    let width = usize::try_from(end.0 - start.0).map_err(|_| SemanticReadError::Limit)? + 1;
    let height = usize::try_from(end.1 - start.1).map_err(|_| SemanticReadError::Limit)? + 1;
    if width.saturating_mul(height) > MAX_READ_CELLS {
        return Err(SemanticReadError::Limit);
    }
    Ok(ReadPage {
        content: range_content(&snap, start, end),
        next_cursor: None,
    })
}

/// `start..=end` of `snap` as `read-cells` answers it.
fn range_content(snap: &crate::doc::SheetSnapshot, start: Addr, end: Addr) -> serde_json::Value {
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
pub const ANSWERING_EDITS: [&str; 4] = ["set-cells", "format-cells", "sort-rows", "delete-rows"];

/// The most cells an edit answers with; a larger change answers its first
/// rows and names the whole range for `read-cells`.
pub const MAX_ANSWERED_CELLS: u32 = 256;

pub fn edit_result_schema() -> serde_json::Value {
    serde_json::json!({"type": ["object", "null"]})
}

/// The answer of cell edit `action` on `sheet_id`, which applied to `doc` as
/// `changes`: the changed range as `read-cells` shows it, clipped to
/// [`MAX_ANSWERED_CELLS`] with `"changed"` naming the whole range when cut.
pub fn edit_result(
    doc: &AutoCommit,
    sheet_id: &str,
    action: &str,
    changes: &[automerge::Patch],
) -> Option<serde_json::Value> {
    if !ANSWERING_EDITS.contains(&action) {
        return None;
    }
    let sheet = SheetsDoc::sheet_index_by_resource_id(doc, sheet_id)?;
    let touched = SheetsDoc::changed_cells(doc, sheet, changes);
    let (first, rest) = touched.split_first()?;
    let (start, end) = rest
        .iter()
        .fold((*first, *first), |(start, end), &(col, row)| {
            (
                (start.0.min(col), start.1.min(row)),
                (end.0.max(col), end.1.max(row)),
            )
        });
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
    let snap = SheetsDoc::snapshot_sheet(doc, sheet, Currency::USD);
    let mut content = range_content(&snap, start, shown_end);
    if shown_end != end
        && let Some(fields) = content.as_object_mut()
    {
        fields.insert("changed".into(), range_label(start, end).into());
    }
    Some(content)
}

pub fn read_result_schema() -> &'static serde_json::Value {
    static SCHEMA: OnceLock<serde_json::Value> = OnceLock::new();
    SCHEMA.get_or_init(|| {
        let content = serde_json::json!({"type":"object","required":["sheet","range","rows","formulas","bold"],"additionalProperties":false,"properties":{
            "sheet":{"type":"string"},
            "range":{"type":"string"},
            "rows":{"type":"array","items":{"type":"array"}},
            "formulas":{"type":"object","additionalProperties":true},
            "bold":{"type":"array","items":{"type":"string"}}
        }});
        serde_json::json!({"type":"object","required":["version","record","basis","content","nextCursor"],"additionalProperties":false,"properties":{
            "version":{"type":"integer","enum":[1]},
            "record":{"type":"object","required":["roomId","appId","recordId"],"additionalProperties":false,"properties":{
                "roomId":{"type":["string","null"],"format":"uuid"},"appId":{"type":"string","minLength":1,"maxLength":256},"recordId":{"type":"string","maxLength":512}
            }},
            "basis":{"type":"object","required":["heads"],"additionalProperties":false,"properties":{
                "heads":{"type":"array","maxItems":64,"uniqueItems":true,"items":{"type":"string","pattern":"^[0-9a-f]{64}$"}}
            }},
            "content":content,
            "nextCursor":{"type":["string","null"],"maxLength":1024}
        }})
    })
}

#[cfg(test)]
mod tests;
