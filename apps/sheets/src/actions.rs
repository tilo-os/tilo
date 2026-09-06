//! Sheets' exported **actions** and the **resource tree** it publishes to Ctrl+K.
//!
//! Every document-mutating operation Sheets performs on the shared workbook is a
//! [`SheetsAction`]: a small serializable value that deterministically mutates
//! the CRDT. Because the workbook already lives in the shared document
//! (`root["wb"]` — see [`crate::doc`]), an action replicates and replays for
//! free, and feeds straight back into the grid on the next `sync`. The exact
//! same [`EncodedAction`] is what the on-screen affordances (a cell edit, the
//! Format menu, the sheet-tab context menu, the Ctrl+K command palette) build
//! and dispatch — one source of truth, one replay path.
//!
//! The resource tree mirrors the data model: the workbook owns its sheets; each
//! sheet exports rename / delete / insert-row / insert-column. Each resource
//! exports the [`ActionSpec`]s Ctrl+K offers for it.
//!
//! ## The split
//!
//! Document-mutating → an action: add/delete/rename/reorder a sheet, insert or
//! delete a row/column, write a cell's value or formula, set a cell's format.
//! Presentation → never an action: the selection (incl. multi-range), scroll,
//! which sheet tab is active, a column-width drag preview until it commits, and
//! the formula-bar edit buffer until Enter. External → never an action:
//! import/export (xlsx/csv/ods) and the formula-graph recompute (derived, not a
//! stored mutation).
//!
//! ## Determinism contract
//!
//! Sheets are addressed by both their current index and a durable sheet id;
//! cells carry the exact coordinates and values, so [`Action::apply`] is a pure
//! function of `(payload, base document)` — no RNG, no wall-clock, no app state.
//! Where an operation restructures a whole sheet (row/column insert + delete) it
//! recomputes the moved cells from the document at the replay's base state and
//! emits its ops in a **sorted** order (a `HashMap`'s iteration order is not
//! stable across replicas), so two replicas at the same heads land on the
//! byte-identical result. A malformed or stale payload is **refused before any
//! CRDT write**, so it cannot partially change a workbook or wedge a peer.

use std::collections::{HashMap, HashSet};

use automerge::AutoCommit;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use qos_actions::{
    Action, ActionAddress, ActionArgs, ActionError, ActionParam, ActionRegistry, ActionSpec, Doc,
    EncodedAction, ExecutionLocus, ExecutionMode, OperationKind, ParamType, Resource,
    ResourceProvider, ResultSpec,
};

use crate::doc::SheetsDoc;
use crate::model::{
    Addr, CellFormat, Currency, DateStyle, TimeStyle, col_to_letters, letters_to_col,
};
use crate::settings::DocSettings;

/// Registry `type_id` for every Sheets action.
pub const SHEETS_ACTION_TYPE: &str = "sheets.action";
pub const SHEETS_OPERATION_TYPE: &str = "sheets.operation";

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum SheetsOperation {
    ImportCsv {
        path: Option<String>,
    },
    ImportXlsx {
        path: Option<String>,
    },
    ImportOds {
        path: Option<String>,
    },
    ExportCsv {
        path: Option<String>,
    },
    ExportXlsx {
        path: Option<String>,
    },
    ExportOds {
        path: Option<String>,
    },
    OpenCommandPalette,
    GoToCell {
        reference: String,
    },
    ToggleFormulaView,
    RevealSheet {
        sheet_id: String,
    },
    RevealCell {
        sheet_id: String,
        col: u32,
        row: u32,
    },
}

pub fn operation_kind(operation: &SheetsOperation) -> OperationKind {
    match operation {
        SheetsOperation::ImportCsv { .. }
        | SheetsOperation::ImportXlsx { .. }
        | SheetsOperation::ImportOds { .. }
        | SheetsOperation::ExportCsv { .. }
        | SheetsOperation::ExportXlsx { .. }
        | SheetsOperation::ExportOds { .. } => OperationKind::Effect,
        SheetsOperation::OpenCommandPalette
        | SheetsOperation::GoToCell { .. }
        | SheetsOperation::ToggleFormulaView
        | SheetsOperation::RevealSheet { .. }
        | SheetsOperation::RevealCell { .. } => OperationKind::View,
    }
}

pub fn sheet_resource_id(sheet_id: &str) -> String {
    format!("sheets.sheet:{sheet_id}")
}

pub fn cell_resource_id(sheet_id: &str, col: u32, row: u32) -> String {
    format!("sheets.cell:{sheet_id}:{col}:{row}")
}

pub fn open_sheet_address(sheet_id: &str) -> ActionAddress {
    ActionAddress::new(sheet_resource_id(sheet_id), "open")
}

pub fn open_cell_address(sheet_id: &str, col: u32, row: u32) -> ActionAddress {
    ActionAddress::new(cell_resource_id(sheet_id, col, row), "open")
}

/// # Errors
///
/// Returns the JSON decoding error when `payload` is not a Sheets operation.
pub fn decode_operation(type_id: &str, payload: &[u8]) -> Result<Option<SheetsOperation>, String> {
    if type_id != SHEETS_OPERATION_TYPE {
        return Ok(None);
    }
    serde_json::from_slice(payload)
        .map(Some)
        .map_err(|error| error.to_string())
}

#[allow(
    clippy::needless_pass_by_value,
    clippy::expect_used,
    reason = "operation values are immediately serialized and every enum variant is serializable"
)]
fn operation(id: &str, label: &str, kind: OperationKind, value: SheetsOperation) -> ActionSpec {
    let spec = ActionSpec::operation(id, label, kind, SHEETS_OPERATION_TYPE, &value)
        .expect("SheetsOperation is always serializable");
    match value {
        SheetsOperation::ExportCsv { .. }
        | SheetsOperation::ExportXlsx { .. }
        | SheetsOperation::ExportOds { .. } => spec.destructive(),
        _ => spec,
    }
}

/// The fixed default currency used when an action reads stored cell formats out
/// of the document. It only seeds a legacy bare `"currency"` format that carries
/// no subtype; every format Sheets writes carries its own. Pinning it keeps
/// [`Action::apply`] independent of the app's live locale, so replay is
/// deterministic.
const APPLY_CURRENCY: Currency = Currency::USD;

/// Excel-compatible grid bounds. Besides matching the visible grid these keep
/// a hostile action from constructing an unrenderable sparse coordinate or
/// overflowing an insert shift at `u32::MAX`.
const MAX_COLUMN: u32 = 16_383;
const MAX_ROW: u32 = 1_048_575;
const MAX_CELL_INPUT_BYTES: usize = 32 * 1024;
const MAX_WRITES_PER_ACTION: usize = 16 * 1024;
const MAX_SHEET_NAME_CHARS: usize = 100;
const TAB_COLOR_COUNT: u32 = 6;

/// One cell's write within a batched [`SheetsAction::SetCells`]: its raw
/// value/formula and, when `fmt` is `Some`, the format to set alongside it. An
/// empty `raw` clears the cell.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CellWrite {
    pub col: u32,
    pub row: u32,
    pub raw: String,
    pub fmt: Option<String>,
}

/// One cell's format within a batched [`SheetsAction::SetFormats`].
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct FormatWrite {
    pub col: u32,
    pub row: u32,
    pub fmt: String,
}

/// Every collaborative operation Sheets exports on the shared workbook.
///
/// Determinism contract: coordinates and values are carried in the payload;
/// [`Action::apply`] never touches the RNG or the wall-clock and emits its ops
/// in a stable order. A malformed or stale payload is refused atomically rather
/// than partially applying its valid prefix.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum SheetsAction {
    /// Write a cell's raw input (a value or a formula). An empty `raw` clears
    /// the cell. When `fmt` is `Some`, the cell's display format is set to it in
    /// the same action (smart-typed entry carries its inferred format here);
    /// `None` leaves any existing format untouched. Sheets cells are discrete,
    /// not a flowing rich-text body, so a whole-cell write is the correct grain.
    SetCell {
        sheet: usize,
        sheet_id: String,
        col: u32,
        row: u32,
        raw: String,
        fmt: Option<String>,
    },
    /// Set a cell's display format only (the value is untouched). `"general"`
    /// clears the format.
    SetFormat {
        sheet: usize,
        sheet_id: String,
        col: u32,
        row: u32,
        fmt: String,
    },
    /// Write a batch of cells in one change — a paste, a fill-down/right, or an
    /// `AutoSum` totals row. One action per bulk operation keeps it a single undo
    /// boundary, a single actor-pinned change, and a byte-identical replay,
    /// rather than a change per cell.
    SetCells {
        sheet: usize,
        sheet_id: String,
        cells: Vec<CellWrite>,
    },
    /// Set the format of a batch of cells in one change — range formatting from
    /// the Format menu or the palette. One undo boundary for the whole range.
    SetFormats {
        sheet: usize,
        sheet_id: String,
        cells: Vec<FormatWrite>,
    },
    /// Append a new blank sheet named `name`.
    AddSheet { name: String, new_sheet_id: String },
    /// Insert a new blank sheet named `name` at the anchored slot `at`.
    InsertSheet {
        at: usize,
        before_sheet_id: Option<String>,
        after_sheet_id: Option<String>,
        name: String,
        new_sheet_id: String,
    },
    /// Delete sheet `idx`. A no-op when it is the workbook's last sheet.
    DeleteSheet { idx: usize, sheet_id: String },
    /// Rename sheet `idx`.
    RenameSheet {
        idx: usize,
        sheet_id: String,
        name: String,
    },
    /// Move sheet `from` so it lands at index `to`, carrying its cells, formats
    /// and colour. Recomputed from the document's base state.
    MoveSheet {
        from: usize,
        sheet_id: String,
        to: usize,
        before_sheet_id: Option<String>,
        after_sheet_id: Option<String>,
    },
    /// Duplicate sheet `idx` into a new sheet inserted immediately after it.
    /// Recomputed from the document's base state.
    DuplicateSheet {
        idx: usize,
        sheet_id: String,
        new_sheet_id: String,
    },
    /// Set (or clear, with `None`) sheet `idx`'s tab colour palette index.
    SetSheetColor {
        idx: usize,
        sheet_id: String,
        color: Option<u32>,
    },
    /// Insert a blank row above row `at` on `sheet`, shifting cells at or below
    /// it down. Recomputed from the document's base state.
    InsertRow {
        sheet: usize,
        sheet_id: String,
        at: u32,
    },
    /// Delete row `at` on `sheet`, shifting the rows below it up. Recomputed
    /// from the document's base state.
    DeleteRow {
        sheet: usize,
        sheet_id: String,
        at: u32,
    },
    /// Insert a blank column left of column `at` on `sheet`, shifting cells at
    /// or after it right. Recomputed from the document's base state.
    InsertColumn {
        sheet: usize,
        sheet_id: String,
        at: u32,
    },
    /// Delete column `at` on `sheet`, shifting the columns after it left.
    /// Recomputed from the document's base state.
    DeleteColumn {
        sheet: usize,
        sheet_id: String,
        at: u32,
    },
    /// Persist the workbook's per-document locale overrides (currency / date /
    /// time). A `None` field clears the stored override so the document falls
    /// back to the global default.
    SetDocSettings {
        currency: Option<Currency>,
        date_format: Option<DateStyle>,
        time_format: Option<TimeStyle>,
    },
}

/// Durable, browseable history metadata for a workbook mutation.
pub fn history_context(action: &SheetsAction) -> qos_actions::ActionContext {
    let mut context = qos_actions::ActionContext::for_action("org.qos.Sheets", "Sheets", action);
    let (label, id, kind, resource_label) = history_resource(action);
    context.action_label = label.to_string();
    context.resource = Some(qos_actions::ActionResource {
        id,
        kind: kind.to_string(),
        label: resource_label,
    });
    context
}

type HistoryResource = (&'static str, String, &'static str, String);

fn cell_resource(label: &'static str, sheet_id: &str, col: u32, row: u32) -> HistoryResource {
    (
        label,
        format!("sheets.cell:{sheet_id}:{col}:{row}"),
        "sheets.cell",
        format!("{}{}", column_label(col), row + 1),
    )
}

fn range_resource(label: &'static str, sheet_id: &str, cells: usize) -> HistoryResource {
    (
        label,
        format!("sheets.range:{sheet_id}"),
        "sheets.range",
        format!("{cells} cells"),
    )
}

fn history_resource(action: &SheetsAction) -> HistoryResource {
    match action {
        SheetsAction::SetCell {
            sheet_id, col, row, ..
        } => cell_resource("Edit cell", sheet_id, *col, *row),
        SheetsAction::SetFormat {
            sheet_id, col, row, ..
        } => cell_resource("Format cell", sheet_id, *col, *row),
        SheetsAction::SetCells {
            sheet_id, cells, ..
        } => range_resource("Edit range", sheet_id, cells.len()),
        SheetsAction::SetFormats {
            sheet_id, cells, ..
        } => range_resource("Format range", sheet_id, cells.len()),
        SheetsAction::AddSheet { name, new_sheet_id } => (
            "Add sheet",
            format!("sheets.sheet:{new_sheet_id}"),
            "sheets.sheet",
            name.clone(),
        ),
        SheetsAction::InsertSheet {
            name, new_sheet_id, ..
        } => (
            "Insert sheet",
            format!("sheets.sheet:{new_sheet_id}"),
            "sheets.sheet",
            name.clone(),
        ),
        SheetsAction::DeleteSheet { sheet_id, .. } => (
            "Delete sheet",
            format!("sheets.sheet:{sheet_id}"),
            "sheets.sheet",
            "Sheet".into(),
        ),
        SheetsAction::RenameSheet { sheet_id, name, .. } => (
            "Rename sheet",
            format!("sheets.sheet:{sheet_id}"),
            "sheets.sheet",
            name.clone(),
        ),
        SheetsAction::MoveSheet { sheet_id, .. } => (
            "Move sheet",
            format!("sheets.sheet:{sheet_id}"),
            "sheets.sheet",
            "Sheet".into(),
        ),
        SheetsAction::DuplicateSheet { new_sheet_id, .. } => (
            "Duplicate sheet",
            format!("sheets.sheet:{new_sheet_id}"),
            "sheets.sheet",
            "Sheet".into(),
        ),
        SheetsAction::SetSheetColor { sheet_id, .. } => (
            "Change sheet color",
            format!("sheets.sheet:{sheet_id}"),
            "sheets.sheet",
            "Sheet".into(),
        ),
        SheetsAction::InsertRow { sheet_id, at, .. } => (
            "Insert row",
            format!("sheets.row:{sheet_id}:{at}"),
            "sheets.row",
            format!("Row {}", at + 1),
        ),
        SheetsAction::DeleteRow { sheet_id, at, .. } => (
            "Delete row",
            format!("sheets.row:{sheet_id}:{at}"),
            "sheets.row",
            format!("Row {}", at + 1),
        ),
        SheetsAction::InsertColumn { sheet_id, at, .. } => (
            "Insert column",
            format!("sheets.column:{sheet_id}:{at}"),
            "sheets.column",
            column_label(*at),
        ),
        SheetsAction::DeleteColumn { sheet_id, at, .. } => (
            "Delete column",
            format!("sheets.column:{sheet_id}:{at}"),
            "sheets.column",
            column_label(*at),
        ),
        SheetsAction::SetDocSettings { .. } => (
            "Update workbook settings",
            "sheets.workbook".to_string(),
            "sheets.workbook",
            "Workbook".to_string(),
        ),
    }
}

/// Compatibility wrapper for call sites that already have the document. Every
/// sheet-scoped action now carries its durable identity in the envelope, so
/// history never needs to re-resolve an author-time numeric position.
pub fn history_context_for_doc(
    _doc: &AutoCommit,
    action: &SheetsAction,
) -> qos_actions::ActionContext {
    history_context(action)
}

#[allow(
    clippy::too_many_lines,
    reason = "exhaustive action apply stays adjacent to action enum"
)]
impl Action for SheetsAction {
    fn apply(&self, doc: &mut Doc) -> Result<(), ActionError> {
        // Validate the entire payload before touching the CRDT. In particular,
        // a batched paste must never leave its first few cells committed when a
        // later coordinate/format is malformed or stale.
        validate_action(doc, self)?;
        match self {
            SheetsAction::SetCell {
                sheet: _,
                sheet_id,
                col,
                row,
                raw,
                fmt,
            } => apply_set_cell(
                doc,
                resolve_sheet(doc, sheet_id)?,
                (*col, *row),
                raw,
                fmt.as_deref(),
            ),
            SheetsAction::SetFormat {
                sheet: _,
                sheet_id,
                col,
                row,
                fmt,
            } => {
                SheetsDoc::set_cell_format(
                    doc,
                    resolve_sheet(doc, sheet_id)?,
                    (*col, *row),
                    CellFormat::from_str(fmt),
                );
            }
            SheetsAction::SetCells {
                sheet_id, cells, ..
            } => apply_set_cells(doc, resolve_sheet(doc, sheet_id)?, cells),
            SheetsAction::SetFormats {
                sheet_id, cells, ..
            } => apply_set_formats(doc, resolve_sheet(doc, sheet_id)?, cells),
            SheetsAction::AddSheet { name, new_sheet_id } => {
                SheetsDoc::add_sheet_with_id(doc, name, new_sheet_id);
            }
            SheetsAction::InsertSheet {
                before_sheet_id,
                after_sheet_id,
                name,
                new_sheet_id,
                ..
            } => {
                let at = resolve_slot(
                    doc,
                    before_sheet_id.as_deref(),
                    after_sheet_id.as_deref(),
                    None,
                )?;
                SheetsDoc::insert_sheet_with_id(doc, at, name, new_sheet_id);
            }
            SheetsAction::DeleteSheet { sheet_id, .. } => {
                SheetsDoc::delete_sheet(doc, resolve_sheet(doc, sheet_id)?);
            }
            SheetsAction::RenameSheet { sheet_id, name, .. } => {
                SheetsDoc::rename_sheet(doc, resolve_sheet(doc, sheet_id)?, name);
            }
            SheetsAction::MoveSheet {
                sheet_id,
                before_sheet_id,
                after_sheet_id,
                ..
            } => {
                let from = resolve_sheet(doc, sheet_id)?;
                let to = resolve_slot(
                    doc,
                    before_sheet_id.as_deref(),
                    after_sheet_id.as_deref(),
                    Some(sheet_id),
                )?;
                SheetsDoc::move_sheet(doc, from, to);
            }
            SheetsAction::DuplicateSheet {
                sheet_id,
                new_sheet_id,
                ..
            } => {
                SheetsDoc::duplicate_sheet_with_id(
                    doc,
                    resolve_sheet(doc, sheet_id)?,
                    new_sheet_id,
                );
            }
            SheetsAction::SetSheetColor {
                sheet_id, color, ..
            } => {
                SheetsDoc::set_sheet_color(doc, resolve_sheet(doc, sheet_id)?, *color);
            }
            SheetsAction::InsertRow { sheet_id, at, .. } => {
                restructure(doc, resolve_sheet(doc, sheet_id)?, insert_row_map(*at));
            }
            SheetsAction::DeleteRow { sheet_id, at, .. } => {
                restructure(doc, resolve_sheet(doc, sheet_id)?, delete_row_map(*at));
            }
            SheetsAction::InsertColumn { sheet_id, at, .. } => {
                restructure(doc, resolve_sheet(doc, sheet_id)?, insert_column_map(*at));
            }
            SheetsAction::DeleteColumn { sheet_id, at, .. } => {
                restructure(doc, resolve_sheet(doc, sheet_id)?, delete_column_map(*at));
            }
            SheetsAction::SetDocSettings {
                currency,
                date_format,
                time_format,
            } => {
                SheetsDoc::set_doc_settings(
                    doc,
                    &DocSettings {
                        currency: *currency,
                        date_format: *date_format,
                        time_format: *time_format,
                    },
                );
            }
        }
        Ok(())
    }

    fn locus(&self) -> ExecutionLocus {
        ExecutionLocus::ClientLocal
    }
}

fn apply_set_cell(doc: &mut Doc, sheet: usize, addr: Addr, raw: &str, fmt: Option<&str>) {
    SheetsDoc::set_cell_raw(doc, sheet, addr, raw);
    if let Some(f) = fmt {
        SheetsDoc::set_cell_format(doc, sheet, addr, CellFormat::from_str(f));
    }
}

fn apply_set_cells(doc: &mut Doc, sheet: usize, cells: &[CellWrite]) {
    // Applied in the payload's given order — the builder emits cells
    // in a stable order, so replay reproduces the identical change.
    for w in cells {
        SheetsDoc::set_cell_raw(doc, sheet, (w.col, w.row), &w.raw);
        if let Some(f) = &w.fmt {
            SheetsDoc::set_cell_format(doc, sheet, (w.col, w.row), CellFormat::from_str(f));
        }
    }
}

fn apply_set_formats(doc: &mut Doc, sheet: usize, cells: &[FormatWrite]) {
    for w in cells {
        SheetsDoc::set_cell_format(doc, sheet, (w.col, w.row), CellFormat::from_str(&w.fmt));
    }
}

fn refuse(message: impl Into<String>) -> Result<(), ActionError> {
    Err(ActionError::Refused(message.into()))
}

fn valid_format(value: &str) -> bool {
    // Actions always carry the canonical serialized spelling. Accepting an
    // unknown string and silently degrading it to General would make an
    // attacker-controlled envelope change formatting in surprising ways.
    CellFormat::from_str(value).as_str() == value
}

fn valid_sheet_name(name: &str) -> bool {
    !name.trim().is_empty()
        && name.chars().count() <= MAX_SHEET_NAME_CHARS
        && !name.chars().any(char::is_control)
}

fn validate_new_sheet_id(doc: &Doc, sheet_id: &str) -> Result<(), ActionError> {
    if Uuid::parse_str(sheet_id).is_err() {
        return refuse("new sheet identity is not a UUID");
    }
    if SheetsDoc::sheet_index_by_resource_id(doc, sheet_id).is_some() {
        return refuse("new sheet identity already exists");
    }
    Ok(())
}

fn valid_addr(col: u32, row: u32) -> bool {
    col <= MAX_COLUMN && row <= MAX_ROW
}

/// Confirm a durable target still exists. The serialized index is author-time
/// context for history and legacy envelopes; resolving the id at apply time is
/// what makes a harmless concurrent tab reorder target the same sheet.
fn validate_sheet(doc: &Doc, _sheet: usize, sheet_id: &str) -> Result<(), ActionError> {
    if sheet_id.is_empty() {
        return refuse("sheet identity is missing");
    }
    if SheetsDoc::sheet_index_by_resource_id(doc, sheet_id).is_some() {
        Ok(())
    } else {
        refuse("sheet target no longer matches its identity")
    }
}

fn resolve_sheet(doc: &Doc, sheet_id: &str) -> Result<usize, ActionError> {
    SheetsDoc::sheet_index_by_resource_id(doc, sheet_id)
        .ok_or_else(|| ActionError::Refused("sheet target no longer exists".into()))
}

fn validate_slot(
    doc: &Doc,
    _at: usize,
    before_sheet_id: Option<&str>,
    after_sheet_id: Option<&str>,
    excluded_sheet_id: Option<&str>,
) -> Result<(), ActionError> {
    let ids: Vec<String> = (0..SheetsDoc::sheet_count_readonly(doc))
        .filter_map(|idx| SheetsDoc::sheet_resource_id(doc, idx))
        .filter(|id| Some(id.as_str()) != excluded_sheet_id)
        .collect();
    let slot = match (before_sheet_id, after_sheet_id) {
        (Some(before), Some(after)) => ids
            .iter()
            .position(|id| id == before)
            .filter(|&idx| idx > 0 && ids.get(idx - 1).is_some_and(|id| id == after)),
        (Some(before), None) => ids.iter().position(|id| id == before),
        (None, Some(after)) => ids.iter().position(|id| id == after).map(|idx| idx + 1),
        (None, None) if ids.is_empty() => Some(0),
        _ => None,
    };
    if slot.is_some() {
        Ok(())
    } else {
        refuse("sheet insertion anchor no longer matches")
    }
}

fn resolve_slot(
    doc: &Doc,
    before_sheet_id: Option<&str>,
    after_sheet_id: Option<&str>,
    excluded_sheet_id: Option<&str>,
) -> Result<usize, ActionError> {
    validate_slot(doc, 0, before_sheet_id, after_sheet_id, excluded_sheet_id)?;
    let ids: Vec<String> = (0..SheetsDoc::sheet_count_readonly(doc))
        .filter_map(|idx| SheetsDoc::sheet_resource_id(doc, idx))
        .filter(|id| Some(id.as_str()) != excluded_sheet_id)
        .collect();
    match (before_sheet_id, after_sheet_id) {
        (Some(before), _) => ids
            .iter()
            .position(|id| id == before)
            .ok_or_else(|| ActionError::Refused("sheet insertion anchor no longer matches".into())),
        (None, Some(after)) => ids
            .iter()
            .position(|id| id == after)
            .map(|idx| idx + 1)
            .ok_or_else(|| ActionError::Refused("sheet insertion anchor no longer matches".into())),
        (None, None) if ids.is_empty() => Ok(0),
        _ => refuse("sheet insertion anchor no longer matches").and(Ok(0)),
    }
}

fn validate_writes<'a>(
    doc: &Doc,
    sheet: usize,
    sheet_id: &str,
    writes: impl Iterator<Item = (u32, u32, &'a str, Option<&'a str>)>,
) -> Result<(), ActionError> {
    validate_sheet(doc, sheet, sheet_id)?;
    let mut seen = HashSet::new();
    for (count, (col, row, raw, fmt)) in writes.enumerate() {
        if count >= MAX_WRITES_PER_ACTION {
            return refuse("too many cells in one action");
        }
        if !valid_addr(col, row) {
            return refuse("cell coordinate is outside the workbook grid");
        }
        if raw.len() > MAX_CELL_INPUT_BYTES {
            return refuse("cell input is too large");
        }
        if fmt.is_some_and(|value| !valid_format(value)) {
            return refuse("cell format is invalid");
        }
        if !seen.insert((col, row)) {
            return refuse("the same cell appears more than once in one action");
        }
    }
    Ok(())
}

/// Preflight every action against the replay base. This is intentionally pure
/// read-only validation: a refusal leaves the document unchanged, while a
/// valid action can then apply as one deterministic transaction.
#[allow(
    clippy::too_many_lines,
    reason = "Keeping exhaustive payload validation next to the action enum makes atomicity auditable."
)]
fn validate_action(doc: &Doc, action: &SheetsAction) -> Result<(), ActionError> {
    match action {
        SheetsAction::SetCell {
            sheet,
            sheet_id,
            col,
            row,
            raw,
            fmt,
        } => validate_writes(
            doc,
            *sheet,
            sheet_id,
            std::iter::once((*col, *row, raw.as_str(), fmt.as_deref())),
        ),
        SheetsAction::SetFormat {
            sheet,
            sheet_id,
            col,
            row,
            fmt,
        } => validate_writes(
            doc,
            *sheet,
            sheet_id,
            std::iter::once((*col, *row, "", Some(fmt.as_str()))),
        ),
        SheetsAction::SetCells {
            sheet,
            sheet_id,
            cells,
        } => validate_writes(
            doc,
            *sheet,
            sheet_id,
            cells
                .iter()
                .map(|cell| (cell.col, cell.row, cell.raw.as_str(), cell.fmt.as_deref())),
        ),
        SheetsAction::SetFormats {
            sheet,
            sheet_id,
            cells,
        } => validate_writes(
            doc,
            *sheet,
            sheet_id,
            cells
                .iter()
                .map(|cell| (cell.col, cell.row, "", Some(cell.fmt.as_str()))),
        ),
        SheetsAction::AddSheet { name, new_sheet_id } => {
            if !valid_sheet_name(name) {
                return refuse("sheet name is empty, too long, or contains control characters");
            }
            validate_new_sheet_id(doc, new_sheet_id)
        }
        SheetsAction::InsertSheet {
            at,
            before_sheet_id,
            after_sheet_id,
            name,
            new_sheet_id,
        } => {
            if !valid_sheet_name(name) {
                return refuse("sheet name is empty, too long, or contains control characters");
            }
            validate_new_sheet_id(doc, new_sheet_id)?;
            validate_slot(
                doc,
                *at,
                before_sheet_id.as_deref(),
                after_sheet_id.as_deref(),
                None,
            )
        }
        SheetsAction::DeleteSheet { idx, sheet_id } => validate_sheet(doc, *idx, sheet_id),
        SheetsAction::DuplicateSheet {
            idx,
            sheet_id,
            new_sheet_id,
        } => {
            validate_sheet(doc, *idx, sheet_id)?;
            validate_new_sheet_id(doc, new_sheet_id)
        }
        SheetsAction::RenameSheet {
            idx,
            sheet_id,
            name,
        } => {
            if !valid_sheet_name(name) {
                return refuse("sheet name is empty, too long, or contains control characters");
            }
            validate_sheet(doc, *idx, sheet_id)
        }
        SheetsAction::MoveSheet {
            from,
            sheet_id,
            to,
            before_sheet_id,
            after_sheet_id,
        } => {
            validate_sheet(doc, *from, sheet_id)?;
            validate_slot(
                doc,
                *to,
                before_sheet_id.as_deref(),
                after_sheet_id.as_deref(),
                Some(sheet_id),
            )
        }
        SheetsAction::SetSheetColor {
            idx,
            sheet_id,
            color,
        } => {
            validate_sheet(doc, *idx, sheet_id)?;
            if color.is_none_or(|color| color < TAB_COLOR_COUNT) {
                Ok(())
            } else {
                refuse("sheet color is invalid")
            }
        }
        SheetsAction::InsertRow {
            sheet,
            sheet_id,
            at,
        } => {
            validate_sheet(doc, *sheet, sheet_id)?;
            if *at >= MAX_ROW {
                refuse("row insertion position is outside the workbook grid")
            } else {
                Ok(())
            }
        }
        SheetsAction::DeleteRow {
            sheet,
            sheet_id,
            at,
        } => {
            validate_sheet(doc, *sheet, sheet_id)?;
            if *at <= MAX_ROW {
                Ok(())
            } else {
                refuse("row is outside the workbook grid")
            }
        }
        SheetsAction::InsertColumn {
            sheet,
            sheet_id,
            at,
        } => {
            validate_sheet(doc, *sheet, sheet_id)?;
            if *at >= MAX_COLUMN {
                refuse("column insertion position is outside the workbook grid")
            } else {
                Ok(())
            }
        }
        SheetsAction::DeleteColumn {
            sheet,
            sheet_id,
            at,
        } => {
            validate_sheet(doc, *sheet, sheet_id)?;
            if *at <= MAX_COLUMN {
                Ok(())
            } else {
                refuse("column is outside the workbook grid")
            }
        }
        SheetsAction::SetDocSettings { .. } => Ok(()),
    }
}

/// The `restructure` shift map for inserting a blank row above `at`.
fn insert_row_map(at: u32) -> impl Fn(Addr) -> Option<Addr> {
    move |(c, r)| Some((c, if r >= at { r + 1 } else { r }))
}

/// The `restructure` shift map for deleting row `at`.
fn delete_row_map(at: u32) -> impl Fn(Addr) -> Option<Addr> {
    move |(c, r)| match r.cmp(&at) {
        std::cmp::Ordering::Equal => None,
        std::cmp::Ordering::Greater => Some((c, r - 1)),
        std::cmp::Ordering::Less => Some((c, r)),
    }
}

/// The `restructure` shift map for inserting a blank column left of `at`.
fn insert_column_map(at: u32) -> impl Fn(Addr) -> Option<Addr> {
    move |(c, r)| Some((if c >= at { c + 1 } else { c }, r))
}

/// The `restructure` shift map for deleting column `at`.
fn delete_column_map(at: u32) -> impl Fn(Addr) -> Option<Addr> {
    move |(c, r)| match c.cmp(&at) {
        std::cmp::Ordering::Equal => None,
        std::cmp::Ordering::Greater => Some((c - 1, r)),
        std::cmp::Ordering::Less => Some((c, r)),
    }
}

/// Rebuild `sheet`'s populated cells through `map`: each cell moves to
/// `map(addr)`, or is dropped when `map` returns `None`. The document's cells
/// are read at the replay's base state and every op is emitted in a **sorted**
/// order so two replicas produce the byte-identical change (a `HashMap`'s
/// iteration order is not stable across replicas). Old cells are cleared before
/// the new ones are written so an overlapping shift is lossless.
fn restructure(doc: &mut Doc, sheet: usize, map: impl Fn(Addr) -> Option<Addr>) {
    let snap = SheetsDoc::snapshot_sheet(doc, sheet, APPLY_CURRENCY);

    let mut new_raw: HashMap<Addr, String> = HashMap::new();
    let mut new_fmt: HashMap<Addr, CellFormat> = HashMap::new();
    for (addr, raw) in &snap.raw {
        if let Some(dst) = map(*addr) {
            // Structural changes move cell *relationships*, not merely the
            // formula-owning cell. `$` anchors only govern copy/fill, so row
            // and column inserts remap both anchored and relative references.
            new_raw.insert(dst, crate::formula::remap_refs(raw, &map));
        }
    }
    for (addr, fmt) in &snap.fmt {
        if let Some(dst) = map(*addr) {
            new_fmt.insert(dst, *fmt);
        }
    }

    // Clear every previously-populated cell first, in a stable order.
    let mut old: Vec<Addr> = snap.raw.keys().chain(snap.fmt.keys()).copied().collect();
    old.sort_unstable();
    old.dedup();
    for addr in old {
        SheetsDoc::set_cell_raw(doc, sheet, addr, "");
        SheetsDoc::set_cell_format(doc, sheet, addr, CellFormat::General);
    }

    // Write the moved cells in a stable order.
    let mut raws: Vec<(Addr, &String)> = new_raw.iter().map(|(a, r)| (*a, r)).collect();
    raws.sort_by_key(|(a, _)| *a);
    for (addr, raw) in raws {
        SheetsDoc::set_cell_raw(doc, sheet, addr, raw);
    }
    let mut fmts: Vec<(Addr, CellFormat)> = new_fmt.iter().map(|(a, f)| (*a, *f)).collect();
    fmts.sort_by_key(|(a, _)| *a);
    for (addr, fmt) in fmts {
        SheetsDoc::set_cell_format(doc, sheet, addr, fmt);
    }
}

/// Register Sheets' action decoder for palette execution + remote replay.
pub fn register(registry: &mut ActionRegistry) {
    registry.register::<SheetsAction>(SHEETS_ACTION_TYPE);
}

/// An inert fallback used only if serializing a real action fails. Every
/// `SheetsAction` variant holds plain owned data, so that cannot happen in
/// practice; the fallback deliberately fails decoding rather than disguising
/// an encoding fault as a document mutation.
fn inert_encoded_action() -> EncodedAction {
    EncodedAction {
        type_id: SHEETS_ACTION_TYPE.to_string(),
        payload: b"{}".to_vec(),
    }
}

/// Encode any [`SheetsAction`] into the wire form the palette and the on-screen
/// affordances both dispatch. The single point that turns a Sheets action into a
/// replayable envelope.
pub fn encode(action: &SheetsAction) -> EncodedAction {
    EncodedAction::encode(SHEETS_ACTION_TYPE, action).unwrap_or_else(|_| inert_encoded_action())
}

// ===========================================================================
// Action builders — the single source of truth shared by the on-screen
// affordances (see `crate::app`) and the Ctrl+K resource tree below.
// ===========================================================================

/// Write a cell's raw value/formula, optionally setting its format in the same
/// action (smart-typed entry carries its inferred format; `None` leaves the
/// existing format untouched). An empty `raw` clears the cell.
pub fn set_cell_action(
    sheet: usize,
    sheet_id: impl Into<String>,
    addr: Addr,
    raw: &str,
    fmt: Option<CellFormat>,
) -> SheetsAction {
    SheetsAction::SetCell {
        sheet,
        sheet_id: sheet_id.into(),
        col: addr.0,
        row: addr.1,
        raw: raw.to_string(),
        fmt: fmt.map(super::model::CellFormat::as_str),
    }
}

/// Set a cell's display format only.
pub fn set_format_action(
    sheet: usize,
    sheet_id: impl Into<String>,
    addr: Addr,
    fmt: CellFormat,
) -> SheetsAction {
    SheetsAction::SetFormat {
        sheet,
        sheet_id: sheet_id.into(),
        col: addr.0,
        row: addr.1,
        fmt: fmt.as_str(),
    }
}

/// Write a batch of cells (paste / fill / `AutoSum` totals) in one change. Each
/// entry is `(addr, raw, optional format)`; an empty `raw` clears the cell.
pub fn set_cells_action(
    sheet: usize,
    sheet_id: impl Into<String>,
    cells: impl IntoIterator<Item = (Addr, String, Option<CellFormat>)>,
) -> SheetsAction {
    SheetsAction::SetCells {
        sheet,
        sheet_id: sheet_id.into(),
        cells: cells
            .into_iter()
            .map(|(addr, raw, fmt)| CellWrite {
                col: addr.0,
                row: addr.1,
                raw,
                fmt: fmt.map(super::model::CellFormat::as_str),
            })
            .collect(),
    }
}

/// Set the format of a batch of cells (range formatting) in one change.
pub fn set_formats_action(
    sheet: usize,
    sheet_id: impl Into<String>,
    cells: impl IntoIterator<Item = (Addr, CellFormat)>,
) -> SheetsAction {
    SheetsAction::SetFormats {
        sheet,
        sheet_id: sheet_id.into(),
        cells: cells
            .into_iter()
            .map(|(addr, fmt)| FormatWrite {
                col: addr.0,
                row: addr.1,
                fmt: fmt.as_str(),
            })
            .collect(),
    }
}

pub fn add_sheet_action(name: &str) -> SheetsAction {
    SheetsAction::AddSheet {
        name: name.to_string(),
        new_sheet_id: Uuid::new_v4().to_string(),
    }
}

pub fn insert_sheet_action(
    at: usize,
    before_sheet_id: Option<String>,
    after_sheet_id: Option<String>,
    name: &str,
) -> SheetsAction {
    SheetsAction::InsertSheet {
        at,
        before_sheet_id,
        after_sheet_id,
        name: name.to_string(),
        new_sheet_id: Uuid::new_v4().to_string(),
    }
}

pub fn delete_sheet_action(idx: usize, sheet_id: impl Into<String>) -> SheetsAction {
    SheetsAction::DeleteSheet {
        idx,
        sheet_id: sheet_id.into(),
    }
}

pub fn rename_sheet_action(idx: usize, sheet_id: impl Into<String>, name: &str) -> SheetsAction {
    SheetsAction::RenameSheet {
        idx,
        sheet_id: sheet_id.into(),
        name: name.to_string(),
    }
}

pub fn move_sheet_action(
    from: usize,
    sheet_id: impl Into<String>,
    to: usize,
    before_sheet_id: Option<String>,
    after_sheet_id: Option<String>,
) -> SheetsAction {
    SheetsAction::MoveSheet {
        from,
        sheet_id: sheet_id.into(),
        to,
        before_sheet_id,
        after_sheet_id,
    }
}

pub fn duplicate_sheet_action(idx: usize, sheet_id: impl Into<String>) -> SheetsAction {
    SheetsAction::DuplicateSheet {
        idx,
        sheet_id: sheet_id.into(),
        new_sheet_id: Uuid::new_v4().to_string(),
    }
}

pub fn set_sheet_color_action(
    idx: usize,
    sheet_id: impl Into<String>,
    color: Option<u32>,
) -> SheetsAction {
    SheetsAction::SetSheetColor {
        idx,
        sheet_id: sheet_id.into(),
        color,
    }
}

pub fn insert_row_action(sheet: usize, sheet_id: impl Into<String>, at: u32) -> SheetsAction {
    SheetsAction::InsertRow {
        sheet,
        sheet_id: sheet_id.into(),
        at,
    }
}

pub fn delete_row_action(sheet: usize, sheet_id: impl Into<String>, at: u32) -> SheetsAction {
    SheetsAction::DeleteRow {
        sheet,
        sheet_id: sheet_id.into(),
        at,
    }
}

pub fn insert_column_action(sheet: usize, sheet_id: impl Into<String>, at: u32) -> SheetsAction {
    SheetsAction::InsertColumn {
        sheet,
        sheet_id: sheet_id.into(),
        at,
    }
}

pub fn delete_column_action(sheet: usize, sheet_id: impl Into<String>, at: u32) -> SheetsAction {
    SheetsAction::DeleteColumn {
        sheet,
        sheet_id: sheet_id.into(),
        at,
    }
}

pub fn set_doc_settings_action(settings: &DocSettings) -> SheetsAction {
    SheetsAction::SetDocSettings {
        currency: settings.currency,
        date_format: settings.date_format,
        time_format: settings.time_format,
    }
}

// ===========================================================================
// Resource tree
// ===========================================================================

/// The presentation context Sheets hands [`provide_resources`]: the sheet names
/// it currently has open, in order. Derived from state the document already
/// holds (the workbook mirrors the shared `sheets` list); nothing is fabricated.
#[derive(Clone)]
pub struct SheetsResourceCtx {
    pub sheet_names: Vec<String>,
    pub searchable_sheets: Vec<SearchableSheet>,
    /// Selection-dependent actions authored by the live grid. Keeping these
    /// as real `ActionSpec`s means Ctrl+K, agents and scripts consume the same
    /// encoded operation instead of a parallel command-id switch.
    pub palette_actions: Vec<ActionSpec>,
    /// The current live selection, published beneath its owning sheet. Its
    /// actions are byte-identical clones of `palette_actions`, so a range can
    /// be addressed directly by agents/Ctrl+K without creating a second
    /// mutation route.
    pub selection: Option<RangeResourceCtx>,
}

/// Presentation-derived identity for the range the user has selected. A range
/// is intentionally ephemeral (selection is not collaborative document state)
/// but its action envelopes remain durable/replayable document mutations.
#[derive(Clone)]
pub struct RangeResourceCtx {
    pub sheet_id: String,
    pub label: String,
    pub actions: Vec<ActionSpec>,
}

/// Immutable search/resource projection rebuilt when workbook heads change.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SearchableSheet {
    pub id: String,
    pub name: String,
    pub cells: Vec<SearchableCell>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SearchableCell {
    pub col: u32,
    pub row: u32,
    pub raw: String,
    pub display: String,
}

fn immediate_spec(id: &str, label: &str, action: &SheetsAction) -> ActionSpec {
    let spec = ActionSpec::immediate(id, label, SHEETS_ACTION_TYPE, action)
        // `ActionSpec`'s fields are private now that it carries typed
        // parameters; build the fallback through the constructor that
        // takes an already-encoded action.
        .unwrap_or_else(|_| {
            ActionSpec::immediate_encoded(
                id,
                label.to_string(),
                ExecutionLocus::ClientLocal,
                inert_encoded_action(),
            )
        })
        .reversible();
    if id.contains("delete") {
        spec.destructive()
    } else {
        spec
    }
}

/// Build the resource tree Sheets currently has open: the workbook (always
/// present, exporting "New sheet") and one `sheets.sheet:<idx>` per sheet, each
/// exporting rename / delete / insert-row / insert-column. Every operation is
/// the same action the on-screen affordance dispatches.
#[allow(
    clippy::too_many_lines,
    clippy::missing_panics_doc,
    clippy::expect_used,
    reason = "resource surface is deliberately co-located and serializes owned enum values"
)]
/// Stable workbook root only. Dynamic sheets and cells are exposed by the
/// retained index/provider rather than being materialized for remote browse.
pub fn workbook_root(ctx: &SheetsResourceCtx) -> Resource {
    let mut workbook = Resource::new("sheets.workbook", "sheets.workbook", "Workbook");
    // Keep the established workbook command ordering for keyboard users while
    // publishing the same envelopes under the selected range as well. The
    // duplicate presentation has no second mutation route: both resources
    // dispatch the exact same encoded action.
    workbook.actions.extend(ctx.palette_actions.clone());

    for (id, label, kind, variant) in [
        ("import-csv", "Import CSV", OperationKind::Effect, 0_u8),
        (
            "import-xlsx",
            "Import Excel workbook",
            OperationKind::Effect,
            1,
        ),
        (
            "import-ods",
            "Import OpenDocument spreadsheet",
            OperationKind::Effect,
            2,
        ),
        ("export-csv", "Export CSV", OperationKind::Effect, 3),
        (
            "export-xlsx",
            "Export Excel workbook",
            OperationKind::Effect,
            4,
        ),
        (
            "export-ods",
            "Export OpenDocument spreadsheet",
            OperationKind::Effect,
            5,
        ),
        ("commands", "Open command palette", OperationKind::View, 6),
    ] {
        if variant == 6 {
            workbook = workbook.with_action(operation(
                id,
                label,
                kind,
                SheetsOperation::OpenCommandPalette,
            ));
            continue;
        }
        workbook = workbook.with_action(
            ActionSpec::typed_operation(
                id,
                label,
                "path (omit to use the system file picker)",
                kind,
                SHEETS_OPERATION_TYPE,
                vec![ActionParam::optional_owned(
                    "path",
                    ParamType::Text,
                    "Direct spreadsheet path",
                )],
                move |args: &ActionArgs| {
                    let path = args
                        .text("path")
                        .filter(|p| !p.trim().is_empty())
                        .map(str::to_string);
                    let value = match variant {
                        0 => SheetsOperation::ImportCsv { path },
                        1 => SheetsOperation::ImportXlsx { path },
                        2 => SheetsOperation::ImportOds { path },
                        3 => SheetsOperation::ExportCsv { path },
                        4 => SheetsOperation::ExportXlsx { path },
                        _ => SheetsOperation::ExportOds { path },
                    };
                    EncodedAction::encode_operation(SHEETS_OPERATION_TYPE, &value)
                        .expect("sheet operation serializes")
                },
            )
            .execution(ExecutionMode::Background { cancellable: false })
            .returns(ResultSpec::Job),
        );
    }

    // Workbook-level: create a new sheet. The default name is resolved from the
    // current sheet count at authoring time, so replay stays deterministic.
    let count = ctx.sheet_names.len();
    workbook = workbook.with_action(ActionSpec::prompted(
        "new-sheet",
        "New sheet",
        "Name",
        "sheet name…",
        ExecutionLocus::ClientLocal,
        move |name: &str| {
            let name = name.trim();
            let name = if name.is_empty() {
                format!("Sheet{}", count + 1)
            } else {
                name.to_string()
            };
            encode(&add_sheet_action(&name))
        },
    ));

    workbook
}

pub fn provide_resources(ctx: &SheetsResourceCtx, _doc: &AutoCommit) -> Vec<Resource> {
    let mut workbook = workbook_root(ctx);
    for (idx, searchable) in ctx.searchable_sheets.iter().enumerate() {
        let name = &searchable.name;
        let label = if name.trim().is_empty() {
            format!("Sheet{}", idx + 1)
        } else {
            name.clone()
        };
        // Never publish a positional fallback: after a concurrent reorder it
        // could make an old address operate on a different sheet. Legacy or
        // malformed rows without a durable identity remain usable on-screen,
        // but are deliberately absent from the headless action surface.
        let stable = searchable.id.clone();
        let mut sheet = Resource::new(sheet_resource_id(&stable), "sheets.sheet", label)
            .with_action(operation(
                "open",
                "Open sheet",
                OperationKind::View,
                SheetsOperation::RevealSheet {
                    sheet_id: stable.clone(),
                },
            ));

        if let Some(range_ctx) = ctx
            .selection
            .as_ref()
            .filter(|range| range.sheet_id == stable)
        {
            let mut range = Resource::new(
                format!("sheets.range:{stable}:{}", range_ctx.label),
                "sheets.range",
                range_ctx.label.clone(),
            );
            range.actions.extend(range_ctx.actions.clone());
            sheet = sheet.with_child(range);
        }

        // Rename — a prompted action carrying the new name.
        sheet = sheet.with_action(ActionSpec::prompted(
            "rename",
            "Rename sheet",
            "Name",
            "sheet name…",
            ExecutionLocus::ClientLocal,
            {
                let stable = stable.clone();
                move |new_name: &str| {
                    encode(&rename_sheet_action(idx, stable.clone(), new_name.trim()))
                }
            },
        ));

        // Delete the whole sheet (a final sheet is deliberately a no-op).
        sheet = sheet.with_action(immediate_spec(
            "delete",
            "Delete sheet",
            &delete_sheet_action(idx, stable.clone()),
        ));

        // Insert a blank row above a given 1-based row number (default: the top).
        sheet = sheet.with_action(ActionSpec::prompted(
            "insert-row",
            "Insert row above…",
            "Row",
            "1-based row number (e.g. 5)",
            ExecutionLocus::ClientLocal,
            {
                let stable = stable.clone();
                move |arg: &str| {
                    let at = arg
                        .trim()
                        .parse::<u32>()
                        .ok()
                        .map_or(0, |n| n.saturating_sub(1));
                    encode(&insert_row_action(idx, stable.clone(), at))
                }
            },
        ));

        // Insert a blank column left of a given column letter (default: A).
        sheet = sheet.with_action(ActionSpec::prompted(
            "insert-column",
            "Insert column left…",
            "Column",
            "column letter (e.g. C)",
            ExecutionLocus::ClientLocal,
            {
                let stable = stable.clone();
                move |arg: &str| {
                    let at = letters_to_col(arg.trim()).unwrap_or(0);
                    encode(&insert_column_action(idx, stable.clone(), at))
                }
            },
        ));

        workbook = workbook.with_child(sheet);
    }

    vec![workbook]
}

pub(crate) fn searchable_cell_resource(sheet_id: &str, cell: &SearchableCell) -> Resource {
    let reference = column_label(cell.col) + &(cell.row + 1).to_string();
    Resource::new(
        cell_resource_id(sheet_id, cell.col, cell.row),
        "sheets.cell",
        reference,
    )
    .with_action(operation(
        "open",
        "Open cell",
        OperationKind::View,
        SheetsOperation::RevealCell {
            sheet_id: sheet_id.to_string(),
            col: cell.col,
            row: cell.row,
        },
    ))
}

impl ResourceProvider for crate::app::SheetsApp {
    fn resources(&self, doc: &AutoCommit) -> Vec<Resource> {
        provide_resources(&self.sheets_resource_ctx(), doc)
    }

    fn resource(&self, doc: &AutoCommit, id: &str) -> Option<Resource> {
        let ctx = self.sheets_resource_ctx();
        let eager = provide_resources(&ctx, doc);
        let workbook = eager.first()?;
        if id == workbook.id.0 {
            return Some(workbook.clone());
        }
        if let Some(sheet) = workbook.children.iter().find(|sheet| sheet.id.0 == id) {
            let stable = id.strip_prefix("sheets.sheet:")?;
            return SheetsDoc::sheet_index_by_resource_id(doc, stable).map(|_| sheet.clone());
        }
        if let Some(range) = workbook
            .children
            .iter()
            .flat_map(|sheet| sheet.children.iter())
            .find(|range| range.id.0 == id)
        {
            let tail = id.strip_prefix("sheets.range:")?;
            let (sheet_id, _) = tail.split_once(':')?;
            return SheetsDoc::sheet_index_by_resource_id(doc, sheet_id).map(|_| range.clone());
        }

        // Cells are intentionally not children of a sheet: a 100k-row file
        // must not turn opening Ctrl+K into a resource-tree materialisation.
        // Exact cell addresses remain first-class, however. Resolve one on
        // demand against the current document so a deleted/moved sheet or an
        // invalid coordinate is refused instead of acquiring stale authority.
        let tail = id.strip_prefix("sheets.cell:")?;
        let mut parts = tail.split(':');
        let (Some(sheet_id), Some(col), Some(row), None) =
            (parts.next(), parts.next(), parts.next(), parts.next())
        else {
            return None;
        };
        let (Ok(col), Ok(row)) = (col.parse::<u32>(), row.parse::<u32>()) else {
            return None;
        };
        if col > MAX_COLUMN
            || row > MAX_ROW
            || SheetsDoc::sheet_index_by_resource_id(doc, sheet_id).is_none()
        {
            return None;
        }
        Some(searchable_cell_resource(
            sheet_id,
            &SearchableCell {
                col,
                row,
                raw: String::new(),
                display: String::new(),
            },
        ))
    }
}

/// The 1-based column letters for `col`, used in action labels/status lines.
#[allow(dead_code)]
pub fn column_label(col: u32) -> String {
    col_to_letters(col)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::CellFormat;
    use qos_actions::{ActionInput, LocalTransport};

    fn registry() -> ActionRegistry {
        let mut r = ActionRegistry::new();
        register(&mut r);
        r
    }

    fn immediate_encode(spec: &ActionSpec) -> EncodedAction {
        match &spec.input {
            ActionInput::Immediate(enc) => enc.clone(),
            ActionInput::Prompted { .. } => panic!("expected an immediate action: {}", spec.id),
        }
    }

    fn prompted_encode(spec: &ActionSpec, arg: &str) -> EncodedAction {
        match &spec.input {
            ActionInput::Prompted { build, .. } => build(arg),
            ActionInput::Immediate(_) => panic!("expected a prompted action: {}", spec.id),
        }
    }

    /// Dispatch an action through the executor, as the on-screen affordance path
    /// does, and return the resulting document heads.
    fn dispatch(doc: &mut AutoCommit, action: &SheetsAction) {
        let encoded = encode(action);
        let decoded = registry().decode(&encoded).unwrap();
        let mut tx = LocalTransport::new();
        qos_actions::dispatch(doc, decoded.as_ref(), encoded, &mut tx).unwrap();
    }

    /// A workbook with two sheets; "Budget" carries a couple of cells.
    fn seeded() -> AutoCommit {
        let mut doc = AutoCommit::new();
        let budget = SheetsDoc::add_sheet(&mut doc, "Budget");
        SheetsDoc::set_cell_raw(&mut doc, budget, (0, 0), "Rent");
        SheetsDoc::set_cell_raw(&mut doc, budget, (1, 0), "1200");
        SheetsDoc::set_cell_format(
            &mut doc,
            budget,
            (1, 0),
            CellFormat::Currency(Currency::USD),
        );
        SheetsDoc::add_sheet(&mut doc, "Sales");
        doc
    }

    fn stable(doc: &AutoCommit, idx: usize) -> String {
        SheetsDoc::sheet_resource_id(doc, idx).expect("seeded sheet identity")
    }

    fn ctx_from(doc: &mut AutoCommit) -> SheetsResourceCtx {
        SheetsResourceCtx {
            sheet_names: SheetsDoc::sheet_names(doc),
            searchable_sheets: (0..SheetsDoc::sheet_count(doc))
                .filter_map(|idx| {
                    Some(SearchableSheet {
                        id: SheetsDoc::sheet_resource_id(doc, idx)?,
                        name: SheetsDoc::sheet_names(doc).get(idx)?.clone(),
                        cells: Vec::new(),
                    })
                })
                .collect(),
            palette_actions: Vec::new(),
            selection: None,
        }
    }

    /// Ctrl+K on Sheets yields the workbook root exporting "New sheet" and a
    /// `sheets.sheet:<idx>` child per sheet, each exporting rename / delete /
    /// insert-row / insert-column. Drives the real resource-builder path.
    #[test]
    fn ctrl_k_publishes_the_workbook_and_sheet_tree() {
        let mut doc = seeded();
        let ctx = ctx_from(&mut doc);
        let resources = provide_resources(&ctx, &doc);
        assert_eq!(resources.len(), 1, "one workbook root");
        let wb = &resources[0];
        assert_eq!(wb.kind, "sheets.workbook");
        let wb_actions: Vec<&str> = wb.actions.iter().map(|a| a.id.as_str()).collect();
        assert!(wb_actions.contains(&"new-sheet"), "{wb_actions:?}");

        assert_eq!(wb.children.len(), 2, "Budget + Sales");
        let budget = &wb.children[0];
        assert_eq!(budget.kind, "sheets.sheet");
        assert_eq!(budget.label, "Budget");
        let s_actions: Vec<&str> = budget.actions.iter().map(|a| a.id.as_str()).collect();
        assert!(s_actions.contains(&"rename"), "{s_actions:?}");
        assert!(s_actions.contains(&"delete"), "{s_actions:?}");
        assert!(s_actions.contains(&"insert-row"), "{s_actions:?}");
        assert!(s_actions.contains(&"insert-column"), "{s_actions:?}");

        // The one palette surfaces the tree's actions as rows.
        let mut palette = ui::ActionPalette::new();
        palette.load_open_resources(&resources);
        palette.open();
        palette.set_query("New sheet");
        let labels = palette.result_labels();
        assert!(labels.iter().any(|l| l.contains("New sheet")), "{labels:?}");
        palette.set_query("Rename sheet");
        let labels = palette.result_labels();
        assert!(
            labels.iter().any(|l| l.contains("Rename sheet")),
            "{labels:?}"
        );
        palette.set_query("Delete sheet");
        let labels = palette.result_labels();
        assert!(
            labels.iter().any(|l| l.contains("Delete sheet")),
            "{labels:?}"
        );
    }

    #[test]
    fn ten_thousand_cells_stay_out_of_the_open_snapshot() {
        let ctx = SheetsResourceCtx {
            sheet_names: vec!["Scale".into()],
            searchable_sheets: vec![SearchableSheet {
                id: "scale-id".into(),
                name: "Scale".into(),
                cells: (0..10_000)
                    .map(|row| SearchableCell {
                        col: 0,
                        row,
                        raw: row.to_string(),
                        display: row.to_string(),
                    })
                    .collect(),
            }],
            palette_actions: Vec::new(),
            selection: None,
        };
        let resources = provide_resources(&ctx, &AutoCommit::new());
        assert!(resources[0].children[0].children.is_empty());
        assert_eq!(
            searchable_cell_resource("scale-id", &ctx.searchable_sheets[0].cells[9_999])
                .id
                .0,
            "sheets.cell:scale-id:0:9999"
        );
    }

    #[test]
    fn omitted_cells_resolve_lazily_but_stale_or_out_of_range_addresses_refuse() {
        use automerge::ROOT;
        use ui::widget::Widget;

        let mut app = crate::app::SheetsApp::new();
        app.shots_mode();
        let mut doc = AutoCommit::new();
        app.sync(&mut doc, &ROOT);
        let sheet_id = SheetsDoc::sheet_resource_id(&doc, 0).expect("seeded sheet identity");
        let id = cell_resource_id(&sheet_id, 10, 100_000);
        let lazy = Widget::resource(&app, &doc, &id).expect("valid omitted cell resolves");
        assert_eq!(lazy.id.0, id);
        assert!(
            qos_actions::door::resolve(&[lazy], &open_cell_address(&sheet_id, 10, 100_000))
                .is_some()
        );
        assert!(
            Widget::resource(&app, &doc, &cell_resource_id(&sheet_id, MAX_COLUMN + 1, 0)).is_none()
        );

        dispatch(&mut doc, &delete_sheet_action(0, sheet_id));
        app.sync(&mut doc, &ROOT);
        assert!(
            Widget::resource(&app, &doc, &id).is_none(),
            "a stale sheet id has no authority"
        );
    }

    #[test]
    fn active_selection_is_a_range_resource_with_canonical_actions() {
        let mut doc = seeded();
        let mut ctx = ctx_from(&mut doc);
        let action = immediate_spec(
            "format-percent",
            "Format as percent",
            &set_formats_action(0, stable(&doc, 0), [((0, 0), CellFormat::Percent)]),
        );
        ctx.selection = Some(RangeResourceCtx {
            sheet_id: String::new(),
            label: "A1:B2".into(),
            actions: vec![action.clone()],
        });
        ctx.selection.as_mut().expect("selection").sheet_id =
            SheetsDoc::sheet_resource_id(&doc, 0).expect("stable sheet identity");

        let resources = provide_resources(&ctx, &doc);
        let range = &resources[0].children[0].children[0];
        assert_eq!(range.kind, "sheets.range");
        assert_eq!(
            range.id.0,
            format!("sheets.range:{}:A1:B2", ctx.selection.unwrap().sheet_id)
        );
        assert_eq!(
            immediate_encode(&range.actions[0]),
            immediate_encode(&action)
        );
    }

    /// A cell write through the action mutates the document.
    #[test]
    fn set_cell_action_writes_the_value_and_format() {
        let mut doc = seeded();
        let sheet_id = stable(&doc, 0);
        dispatch(
            &mut doc,
            &set_cell_action(0, sheet_id, (0, 1), "=SUM(B1:B3)", Some(CellFormat::Number)),
        );
        assert_eq!(SheetsDoc::cell_raw(&mut doc, 0, (0, 1)), "=SUM(B1:B3)");
        let snap = SheetsDoc::snapshot_sheet(&mut doc, 0, Currency::USD);
        assert_eq!(snap.fmt.get(&(0, 1)).copied(), Some(CellFormat::Number));
    }

    #[test]
    fn stable_cell_target_survives_a_concurrent_tab_reorder() {
        let mut doc = seeded();
        let budget_id = stable(&doc, 0);
        let action = set_cell_action(0, budget_id, (7, 7), "on the intended sheet", None);

        // Another collaborator moved Budget after Sales before this envelope
        // replayed. The old positional index now names Sales, but the durable
        // id must still resolve Budget at its new index.
        SheetsDoc::move_sheet(&mut doc, 0, 1);
        dispatch(&mut doc, &action);

        assert_eq!(
            SheetsDoc::cell_raw(&mut doc, 1, (7, 7)),
            "on the intended sheet"
        );
        assert_eq!(SheetsDoc::cell_raw(&mut doc, 0, (7, 7)), "");
    }

    #[test]
    fn authored_new_sheet_identity_converges_across_independent_replays() {
        let mut a = seeded();
        let mut b = seeded();
        let action = SheetsAction::AddSheet {
            name: "Shared".into(),
            new_sheet_id: "10000000-0000-4000-8000-000000000001".into(),
        };
        dispatch(&mut a, &action);
        dispatch(&mut b, &action);
        assert_eq!(
            SheetsDoc::sheet_resource_id(&a, 2).as_deref(),
            Some("10000000-0000-4000-8000-000000000001")
        );
        assert_eq!(
            SheetsDoc::sheet_resource_id(&a, 2),
            SheetsDoc::sheet_resource_id(&b, 2)
        );
    }

    #[test]
    fn new_sheet_identity_must_be_a_fresh_uuid() {
        let mut doc = seeded();
        let existing = "10000000-0000-4000-8000-000000000009".to_string();
        dispatch(
            &mut doc,
            &SheetsAction::AddSheet {
                name: "Existing".into(),
                new_sheet_id: existing.clone(),
            },
        );
        for new_sheet_id in ["not-a-uuid".to_string(), existing] {
            let action = SheetsAction::AddSheet {
                name: "Rejected".into(),
                new_sheet_id,
            };
            let before = doc.save();
            let error = qos_actions::apply_local(&mut doc, &action, encode(&action))
                .expect_err("malformed or duplicate resource identity must be refused");
            assert!(matches!(error, ActionError::Refused(_)));
            assert_eq!(doc.save(), before);
        }
    }

    #[test]
    fn insert_and_duplicate_history_use_envelope_identities() {
        let inserted = SheetsAction::InsertSheet {
            at: 1,
            before_sheet_id: Some("next".into()),
            after_sheet_id: Some("previous".into()),
            name: "Inserted".into(),
            new_sheet_id: "10000000-0000-4000-8000-000000000002".into(),
        };
        assert_eq!(
            history_context(&inserted).resource.unwrap().id,
            "sheets.sheet:10000000-0000-4000-8000-000000000002"
        );
        let duplicate = SheetsAction::DuplicateSheet {
            idx: 0,
            sheet_id: "source-id".into(),
            new_sheet_id: "10000000-0000-4000-8000-000000000003".into(),
        };
        assert_eq!(
            history_context(&duplicate).resource.unwrap().id,
            "sheets.sheet:10000000-0000-4000-8000-000000000003"
        );
        let doc = seeded();
        assert_eq!(
            history_context_for_doc(&doc, &duplicate)
                .resource
                .unwrap()
                .id,
            "sheets.sheet:10000000-0000-4000-8000-000000000003"
        );
    }

    /// Deleting a sheet through its action removes it; the last sheet is kept.
    #[test]
    fn delete_sheet_action_removes_a_sheet_but_keeps_the_last() {
        let mut doc = seeded();
        assert_eq!(SheetsDoc::sheet_count(&mut doc), 2);
        let sheet_id = stable(&doc, 1);
        dispatch(&mut doc, &delete_sheet_action(1, sheet_id));
        assert_eq!(SheetsDoc::sheet_names(&mut doc), vec!["Budget"]);
        // The last sheet cannot be deleted away.
        let sheet_id = stable(&doc, 0);
        dispatch(&mut doc, &delete_sheet_action(0, sheet_id));
        assert_eq!(
            SheetsDoc::sheet_count(&mut doc),
            1,
            "a workbook keeps one sheet"
        );
    }

    /// Inserting a row above the value cell shifts it down; delete shifts back.
    #[test]
    fn insert_and_delete_row_shift_cells() {
        let mut doc = seeded();
        let sheet_id = stable(&doc, 0);
        // Budget!A1 = "Rent". Insert a row above row 1 (at == 0): it moves to A2.
        dispatch(&mut doc, &insert_row_action(0, sheet_id.clone(), 0));
        assert_eq!(SheetsDoc::cell_raw(&mut doc, 0, (0, 0)), "");
        assert_eq!(SheetsDoc::cell_raw(&mut doc, 0, (0, 1)), "Rent");
        // Delete row 1 again: it shifts back up to A1.
        dispatch(&mut doc, &delete_row_action(0, sheet_id, 0));
        assert_eq!(SheetsDoc::cell_raw(&mut doc, 0, (0, 0)), "Rent");
    }

    /// The full undo covenant on one action: dispatched through the executor it
    /// mutates the document and records exactly one undo boundary, and Ctrl+Z
    /// (the host's `undo`) reverts precisely that action.
    #[test]
    fn action_records_one_undo_boundary_and_reverts() {
        let mut state = ui::State::new();
        state.doc = seeded();
        let before_heads = state.doc.get_heads();
        let before = SheetsDoc::sheet_count(&mut state.doc);

        // Add a sheet via the action + host undo recording.
        let encoded = encode(&add_sheet_action("Fresh"));
        let decoded = registry().decode(&encoded).unwrap();
        let mut tx = LocalTransport::new();
        qos_actions::dispatch(&mut state.doc, decoded.as_ref(), encoded, &mut tx).unwrap();
        state.record_committed_action(before_heads, None, None);

        assert_eq!(
            SheetsDoc::sheet_count(&mut state.doc),
            before + 1,
            "the action added a sheet"
        );
        assert!(
            state.peek_last_undo().is_some(),
            "exactly one undo boundary recorded"
        );

        // Ctrl+Z reverts exactly this action.
        state.undo(None, None).expect("an undo entry to pop");
        assert_eq!(
            SheetsDoc::sheet_count(&mut state.doc),
            before,
            "undo reverted the added sheet and nothing else"
        );
    }

    /// The collaboration guarantee: replaying the same encoded action on a second
    /// independent replica (from the same base state) lands on the byte-identical
    /// document — same head hashes, not merely the same values.
    #[test]
    fn action_replays_byte_identical_on_a_peer() {
        let mut a = seeded();
        a.commit();
        let bytes = a.save();
        let mut b = AutoCommit::load(&bytes).unwrap();
        assert_eq!(
            a.get_heads(),
            b.get_heads(),
            "precondition: same base state"
        );

        let action = set_cell_action(0, stable(&a, 0), (2, 4), "converge", None);
        let encoded = encode(&action);
        let env = qos_actions::apply_local(&mut a, &action, encoded).unwrap();
        qos_actions::replay(&mut b, &env, &registry()).unwrap();

        assert_eq!(
            a.get_heads(),
            b.get_heads(),
            "replay reproduced the identical change"
        );
        assert_eq!(SheetsDoc::cell_raw(&mut b, 0, (2, 4)), "converge");
    }

    /// A row restructure also replays byte-identically — the recompute-from-base
    /// path, not just a single-cell write.
    #[test]
    fn row_restructure_replays_byte_identical() {
        let mut a = seeded();
        // Give the sheet a few more cells so the restructure moves several.
        SheetsDoc::set_cell_raw(&mut a, 0, (0, 2), "Food");
        SheetsDoc::set_cell_raw(&mut a, 0, (1, 2), "300");
        a.commit();
        let bytes = a.save();
        let mut b = AutoCommit::load(&bytes).unwrap();
        assert_eq!(
            a.get_heads(),
            b.get_heads(),
            "precondition: same base state"
        );

        let action = insert_row_action(0, stable(&a, 0), 0);
        let encoded = encode(&action);
        let env = qos_actions::apply_local(&mut a, &action, encoded).unwrap();
        qos_actions::replay(&mut b, &env, &registry()).unwrap();

        assert_eq!(
            a.get_heads(),
            b.get_heads(),
            "restructure replayed identically"
        );
        assert_eq!(SheetsDoc::cell_raw(&mut b, 0, (0, 1)), "Rent");
    }

    /// A batched multi-cell write (a paste / fill) is one change and replays
    /// byte-identically on a peer — the whole batch, one actor, equal heads.
    #[test]
    fn batched_set_cells_replays_byte_identical() {
        let mut a = seeded();
        a.commit();
        let bytes = a.save();
        let mut b = AutoCommit::load(&bytes).unwrap();
        assert_eq!(
            a.get_heads(),
            b.get_heads(),
            "precondition: same base state"
        );

        let action = set_cells_action(
            0,
            stable(&a, 0),
            vec![
                ((0, 5), "=A1+1".to_string(), None),
                ((1, 5), "42".to_string(), Some(CellFormat::Number)),
                ((2, 5), "note".to_string(), Some(CellFormat::Text)),
            ],
        );
        let encoded = encode(&action);
        let env = qos_actions::apply_local(&mut a, &action, encoded).unwrap();
        qos_actions::replay(&mut b, &env, &registry()).unwrap();

        assert_eq!(
            a.get_heads(),
            b.get_heads(),
            "the whole batch replayed identically"
        );
        assert_eq!(SheetsDoc::cell_raw(&mut b, 0, (0, 5)), "=A1+1");
        assert_eq!(SheetsDoc::cell_raw(&mut b, 0, (1, 5)), "42");
        let snap = SheetsDoc::snapshot_sheet(&mut b, 0, Currency::USD);
        assert_eq!(snap.fmt.get(&(1, 5)).copied(), Some(CellFormat::Number));
    }

    #[test]
    fn hostile_batch_is_refused_without_a_partial_write() {
        let mut doc = seeded();
        let before = doc.save();
        let action = SheetsAction::SetCells {
            sheet: 0,
            sheet_id: stable(&doc, 0),
            cells: vec![
                CellWrite {
                    col: 0,
                    row: 9,
                    raw: "would be valid alone".into(),
                    fmt: None,
                },
                CellWrite {
                    col: MAX_COLUMN.saturating_add(1),
                    row: 9,
                    raw: "outside the grid".into(),
                    fmt: None,
                },
            ],
        };
        let error = qos_actions::apply_local(&mut doc, &action, encode(&action))
            .expect_err("the full batch must be validated before any write");
        assert!(matches!(error, ActionError::Refused(_)));
        assert_eq!(
            doc.save(),
            before,
            "refusal leaves the CRDT byte-for-byte unchanged"
        );
    }

    #[test]
    fn row_and_column_actions_remap_formula_relationships() {
        let mut doc = seeded();
        SheetsDoc::set_cell_raw(&mut doc, 0, (0, 1), "10");
        SheetsDoc::set_cell_raw(&mut doc, 0, (1, 2), "=SUM($A$1:A2)");
        let sheet_id = stable(&doc, 0);

        dispatch(&mut doc, &insert_row_action(0, sheet_id.clone(), 0));
        assert_eq!(
            SheetsDoc::cell_raw(&mut doc, 0, (1, 3)),
            "=SUM($A$2:A3)",
            "both anchored and relative formula references follow the inserted row"
        );

        dispatch(&mut doc, &delete_column_action(0, sheet_id, 0));
        assert_eq!(
            SheetsDoc::cell_raw(&mut doc, 0, (0, 3)),
            "=SUM(#REF!:#REF!)",
            "a deleted referenced column is represented explicitly, never silently retargeted"
        );
    }

    /// Single source of truth: the palette's delete / rename / insert specs
    /// encode byte-identically to the builders the on-screen affordances call.
    #[test]
    fn palette_and_builder_encode_identically() {
        let mut doc = seeded();
        let ctx = ctx_from(&mut doc);
        let resources = provide_resources(&ctx, &doc);
        let sheet = &resources[0].children[0]; // Budget, idx 0.

        // Delete spec == delete builder.
        let delete_spec = sheet.actions.iter().find(|a| a.id == "delete").unwrap();
        let from_spec = immediate_encode(delete_spec);
        let from_builder = encode(&delete_sheet_action(0, stable(&doc, 0)));
        assert_eq!(from_spec.type_id, from_builder.type_id);
        assert_eq!(
            from_spec.payload, from_builder.payload,
            "delete is one source of truth"
        );

        // Rename spec == rename builder for the same name.
        let rename_spec = sheet.actions.iter().find(|a| a.id == "rename").unwrap();
        let from_spec = prompted_encode(rename_spec, "Q1");
        let from_builder = encode(&rename_sheet_action(0, stable(&doc, 0), "Q1"));
        assert_eq!(
            from_spec.payload, from_builder.payload,
            "rename is one source of truth"
        );

        // Insert-row spec builds an InsertRow at the parsed (1-based → 0-based) row.
        let insert_spec = sheet.actions.iter().find(|a| a.id == "insert-row").unwrap();
        let from_spec = prompted_encode(insert_spec, "5");
        let from_builder = encode(&insert_row_action(0, stable(&doc, 0), 4));
        assert_eq!(
            from_spec.payload, from_builder.payload,
            "insert-row is one source of truth"
        );
    }

    #[test]
    fn cell_action_history_has_coordinate_resource() {
        let context = history_context(&SheetsAction::SetCell {
            sheet: 2,
            sheet_id: "history-sheet".into(),
            col: 1,
            row: 4,
            raw: "42".into(),
            fmt: None,
        });
        assert_eq!(context.action_label, "Edit cell");
        let resource = context.resource.expect("cell resource");
        assert_eq!(resource.id, "sheets.cell:history-sheet:1:4");
        assert_eq!(resource.label, "B5");
    }

    #[test]
    fn document_context_and_palette_keep_sheet_identity_across_move() {
        let mut doc = seeded();
        let stable = SheetsDoc::sheet_resource_id(&doc, 0).expect("stable sheet identity");
        let before = history_context_for_doc(
            &doc,
            &SheetsAction::SetCell {
                sheet: 0,
                sheet_id: stable.clone(),
                col: 1,
                row: 4,
                raw: "42".into(),
                fmt: None,
            },
        );
        assert_eq!(
            before.resource.expect("cell resource").id,
            format!("sheets.cell:{stable}:1:4")
        );

        SheetsDoc::move_sheet(&mut doc, 0, 1);
        let ctx = ctx_from(&mut doc);
        let resources = provide_resources(&ctx, &doc);
        let moved = resources[0]
            .children
            .iter()
            .find(|resource| resource.label == "Budget")
            .expect("moved sheet remains published");
        assert_eq!(moved.id.0, format!("sheets.sheet:{stable}"));
    }

    #[test]
    fn interchange_operations_are_typed_background_jobs() {
        let mut doc = seeded();
        let ctx = ctx_from(&mut doc);
        let resources = provide_resources(&ctx, &doc);
        for id in [
            "import-csv",
            "import-xlsx",
            "import-ods",
            "export-csv",
            "export-xlsx",
            "export-ods",
        ] {
            let action = resources[0]
                .actions
                .iter()
                .find(|action| action.id == id)
                .unwrap();
            assert_eq!(action.params()[0].name.as_ref(), "path");
            assert!(matches!(
                action.contract.execution,
                ExecutionMode::Background { .. }
            ));
            assert_eq!(action.contract.result, ResultSpec::Job);
            assert!(action.contract.validate_declaration().is_empty());
        }
    }
}
