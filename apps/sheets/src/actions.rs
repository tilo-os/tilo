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
//! Sheets are addressed by index and cells by `(col, row)`; a payload carries
//! the exact coordinates and the exact value, so [`Action::apply`] is a pure
//! function of `(payload, base document)` — no RNG, no wall-clock, no app state.
//! Where an operation restructures a whole sheet (row/column insert + delete) it
//! recomputes the moved cells from the document at the replay's base state and
//! emits its ops in a **sorted** order (a `HashMap`'s iteration order is not
//! stable across replicas), so two replicas at the same heads land on the
//! byte-identical result. A malformed payload is a **no-op**, never an error, so
//! a bad replay can't wedge a peer.

use std::collections::HashMap;

use automerge::AutoCommit;
use serde::{Deserialize, Serialize};

use qos_actions::{
    Action, ActionError, ActionRegistry, ActionSpec, Doc, EncodedAction, ExecutionLocus, Resource,
    ResourceProvider,
};

use crate::doc::SheetsDoc;
use crate::model::{
    Addr, CellFormat, Currency, DateStyle, TimeStyle, col_to_letters, letters_to_col,
};
use crate::settings::DocSettings;

/// Registry `type_id` for every Sheets action.
pub const SHEETS_ACTION_TYPE: &str = "sheets.action";

/// The fixed default currency used when an action reads stored cell formats out
/// of the document. It only seeds a legacy bare `"currency"` format that carries
/// no subtype; every format Sheets writes carries its own. Pinning it keeps
/// [`Action::apply`] independent of the app's live locale, so replay is
/// deterministic.
const APPLY_CURRENCY: Currency = Currency::USD;

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
/// in a stable order. A malformed payload is a **no-op** rather than an error.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum SheetsAction {
    /// Write a cell's raw input (a value or a formula). An empty `raw` clears
    /// the cell. When `fmt` is `Some`, the cell's display format is set to it in
    /// the same action (smart-typed entry carries its inferred format here);
    /// `None` leaves any existing format untouched. Sheets cells are discrete,
    /// not a flowing rich-text body, so a whole-cell write is the correct grain.
    SetCell {
        sheet: usize,
        col: u32,
        row: u32,
        raw: String,
        fmt: Option<String>,
    },
    /// Set a cell's display format only (the value is untouched). `"general"`
    /// clears the format.
    SetFormat {
        sheet: usize,
        col: u32,
        row: u32,
        fmt: String,
    },
    /// Write a batch of cells in one change — a paste, a fill-down/right, or an
    /// `AutoSum` totals row. One action per bulk operation keeps it a single undo
    /// boundary, a single actor-pinned change, and a byte-identical replay,
    /// rather than a change per cell.
    SetCells { sheet: usize, cells: Vec<CellWrite> },
    /// Set the format of a batch of cells in one change — range formatting from
    /// the Format menu or the palette. One undo boundary for the whole range.
    SetFormats {
        sheet: usize,
        cells: Vec<FormatWrite>,
    },
    /// Append a new blank sheet named `name`.
    AddSheet { name: String },
    /// Insert a new blank sheet named `name` at index `at` (clamped to the end).
    InsertSheet { at: usize, name: String },
    /// Delete sheet `idx`. A no-op when it is the workbook's last sheet.
    DeleteSheet { idx: usize },
    /// Rename sheet `idx`.
    RenameSheet { idx: usize, name: String },
    /// Move sheet `from` so it lands at index `to`, carrying its cells, formats
    /// and colour. Recomputed from the document's base state.
    MoveSheet { from: usize, to: usize },
    /// Duplicate sheet `idx` into a new sheet inserted immediately after it.
    /// Recomputed from the document's base state.
    DuplicateSheet { idx: usize },
    /// Set (or clear, with `None`) sheet `idx`'s tab colour palette index.
    SetSheetColor { idx: usize, color: Option<u32> },
    /// Insert a blank row above row `at` on `sheet`, shifting cells at or below
    /// it down. Recomputed from the document's base state.
    InsertRow { sheet: usize, at: u32 },
    /// Delete row `at` on `sheet`, shifting the rows below it up. Recomputed
    /// from the document's base state.
    DeleteRow { sheet: usize, at: u32 },
    /// Insert a blank column left of column `at` on `sheet`, shifting cells at
    /// or after it right. Recomputed from the document's base state.
    InsertColumn { sheet: usize, at: u32 },
    /// Delete column `at` on `sheet`, shifting the columns after it left.
    /// Recomputed from the document's base state.
    DeleteColumn { sheet: usize, at: u32 },
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

fn cell_resource(label: &'static str, sheet: usize, col: u32, row: u32) -> HistoryResource {
    (
        label,
        format!("sheets.cell:{sheet}:{col}:{row}"),
        "sheets.cell",
        format!("{}{}", column_label(col), row + 1),
    )
}

fn range_resource(label: &'static str, sheet: usize, cells: usize) -> HistoryResource {
    (
        label,
        format!("sheets.range:{sheet}"),
        "sheets.range",
        format!("{cells} cells"),
    )
}

fn history_resource(action: &SheetsAction) -> HistoryResource {
    match action {
        SheetsAction::SetCell {
            sheet, col, row, ..
        } => cell_resource("Edit cell", *sheet, *col, *row),
        SheetsAction::SetFormat {
            sheet, col, row, ..
        } => cell_resource("Format cell", *sheet, *col, *row),
        SheetsAction::SetCells { sheet, cells } => {
            range_resource("Edit range", *sheet, cells.len())
        }
        SheetsAction::SetFormats { sheet, cells } => {
            range_resource("Format range", *sheet, cells.len())
        }
        SheetsAction::AddSheet { name } => (
            "Add sheet",
            format!("sheets.sheet:{name}"),
            "sheets.sheet",
            name.clone(),
        ),
        SheetsAction::InsertSheet { at, name } => (
            "Insert sheet",
            format!("sheets.sheet:{at}"),
            "sheets.sheet",
            name.clone(),
        ),
        SheetsAction::DeleteSheet { idx } => (
            "Delete sheet",
            format!("sheets.sheet:{idx}"),
            "sheets.sheet",
            format!("Sheet {}", idx + 1),
        ),
        SheetsAction::RenameSheet { idx, name } => (
            "Rename sheet",
            format!("sheets.sheet:{idx}"),
            "sheets.sheet",
            name.clone(),
        ),
        SheetsAction::MoveSheet { from, .. } => (
            "Move sheet",
            format!("sheets.sheet:{from}"),
            "sheets.sheet",
            format!("Sheet {}", from + 1),
        ),
        SheetsAction::DuplicateSheet { idx } => (
            "Duplicate sheet",
            format!("sheets.sheet:{idx}"),
            "sheets.sheet",
            format!("Sheet {}", idx + 1),
        ),
        SheetsAction::SetSheetColor { idx, .. } => (
            "Change sheet color",
            format!("sheets.sheet:{idx}"),
            "sheets.sheet",
            format!("Sheet {}", idx + 1),
        ),
        SheetsAction::InsertRow { sheet, at } => (
            "Insert row",
            format!("sheets.row:{sheet}:{at}"),
            "sheets.row",
            format!("Row {}", at + 1),
        ),
        SheetsAction::DeleteRow { sheet, at } => (
            "Delete row",
            format!("sheets.row:{sheet}:{at}"),
            "sheets.row",
            format!("Row {}", at + 1),
        ),
        SheetsAction::InsertColumn { sheet, at } => (
            "Insert column",
            format!("sheets.column:{sheet}:{at}"),
            "sheets.column",
            column_label(*at),
        ),
        SheetsAction::DeleteColumn { sheet, at } => (
            "Delete column",
            format!("sheets.column:{sheet}:{at}"),
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

/// Enrich direct UI actions with the sheet's durable document identity. The
/// action payload still uses the current index for deterministic replay, while
/// History and resource browsing remain attached across rename and move.
pub fn history_context_for_doc(
    doc: &AutoCommit,
    action: &SheetsAction,
) -> qos_actions::ActionContext {
    let mut context = history_context(action);
    let sheet = match action {
        SheetsAction::SetCell { sheet, .. }
        | SheetsAction::SetFormat { sheet, .. }
        | SheetsAction::SetCells { sheet, .. }
        | SheetsAction::SetFormats { sheet, .. }
        | SheetsAction::InsertRow { sheet, .. }
        | SheetsAction::DeleteRow { sheet, .. }
        | SheetsAction::InsertColumn { sheet, .. }
        | SheetsAction::DeleteColumn { sheet, .. } => Some(*sheet),
        SheetsAction::DeleteSheet { idx }
        | SheetsAction::RenameSheet { idx, .. }
        | SheetsAction::DuplicateSheet { idx }
        | SheetsAction::SetSheetColor { idx, .. } => Some(*idx),
        SheetsAction::MoveSheet { from, .. } => Some(*from),
        SheetsAction::AddSheet { .. }
        | SheetsAction::InsertSheet { .. }
        | SheetsAction::SetDocSettings { .. } => None,
    };
    let Some(stable) = sheet.and_then(|idx| SheetsDoc::sheet_resource_id(doc, idx)) else {
        return context;
    };
    let Some(resource) = context.resource.as_mut() else {
        return context;
    };
    resource.id = match action {
        SheetsAction::SetCell { col, row, .. } | SheetsAction::SetFormat { col, row, .. } => {
            format!("sheets.cell:{stable}:{col}:{row}")
        }
        SheetsAction::SetCells { .. } | SheetsAction::SetFormats { .. } => {
            format!("sheets.range:{stable}")
        }
        SheetsAction::InsertRow { at, .. } | SheetsAction::DeleteRow { at, .. } => {
            format!("sheets.row:{stable}:{at}")
        }
        SheetsAction::InsertColumn { at, .. } | SheetsAction::DeleteColumn { at, .. } => {
            format!("sheets.column:{stable}:{at}")
        }
        _ => format!("sheets.sheet:{stable}"),
    };
    context
}

impl Action for SheetsAction {
    fn apply(&self, doc: &mut Doc) -> Result<(), ActionError> {
        match self {
            SheetsAction::SetCell {
                sheet,
                col,
                row,
                raw,
                fmt,
            } => apply_set_cell(doc, *sheet, (*col, *row), raw, fmt.as_deref()),
            SheetsAction::SetFormat {
                sheet,
                col,
                row,
                fmt,
            } => {
                SheetsDoc::set_cell_format(doc, *sheet, (*col, *row), CellFormat::from_str(fmt));
            }
            SheetsAction::SetCells { sheet, cells } => apply_set_cells(doc, *sheet, cells),
            SheetsAction::SetFormats { sheet, cells } => apply_set_formats(doc, *sheet, cells),
            SheetsAction::AddSheet { name } => {
                SheetsDoc::add_sheet(doc, name);
            }
            SheetsAction::InsertSheet { at, name } => {
                SheetsDoc::insert_sheet(doc, *at, name);
            }
            SheetsAction::DeleteSheet { idx } => {
                SheetsDoc::delete_sheet(doc, *idx);
            }
            SheetsAction::RenameSheet { idx, name } => {
                SheetsDoc::rename_sheet(doc, *idx, name);
            }
            SheetsAction::MoveSheet { from, to } => {
                SheetsDoc::move_sheet(doc, *from, *to);
            }
            SheetsAction::DuplicateSheet { idx } => {
                SheetsDoc::duplicate_sheet(doc, *idx);
            }
            SheetsAction::SetSheetColor { idx, color } => {
                SheetsDoc::set_sheet_color(doc, *idx, *color);
            }
            SheetsAction::InsertRow { sheet, at } => {
                restructure(doc, *sheet, insert_row_map(*at));
            }
            SheetsAction::DeleteRow { sheet, at } => {
                restructure(doc, *sheet, delete_row_map(*at));
            }
            SheetsAction::InsertColumn { sheet, at } => {
                restructure(doc, *sheet, insert_column_map(*at));
            }
            SheetsAction::DeleteColumn { sheet, at } => {
                restructure(doc, *sheet, delete_column_map(*at));
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
            new_raw.insert(dst, raw.clone());
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

/// An [`EncodedAction`] that decodes to nothing: an empty payload matches no
/// `SheetsAction` variant, so the executor's own contract (a malformed
/// payload is a no-op, never a crash) absorbs it harmlessly. Used only as the
/// fallback if serializing a real action fails — every `SheetsAction`
/// variant holds plain owned data, so that never happens in practice.
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
    addr: Addr,
    raw: &str,
    fmt: Option<CellFormat>,
) -> SheetsAction {
    SheetsAction::SetCell {
        sheet,
        col: addr.0,
        row: addr.1,
        raw: raw.to_string(),
        fmt: fmt.map(super::model::CellFormat::as_str),
    }
}

/// Set a cell's display format only.
pub fn set_format_action(sheet: usize, addr: Addr, fmt: CellFormat) -> SheetsAction {
    SheetsAction::SetFormat {
        sheet,
        col: addr.0,
        row: addr.1,
        fmt: fmt.as_str(),
    }
}

/// Write a batch of cells (paste / fill / `AutoSum` totals) in one change. Each
/// entry is `(addr, raw, optional format)`; an empty `raw` clears the cell.
pub fn set_cells_action(
    sheet: usize,
    cells: impl IntoIterator<Item = (Addr, String, Option<CellFormat>)>,
) -> SheetsAction {
    SheetsAction::SetCells {
        sheet,
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
    cells: impl IntoIterator<Item = (Addr, CellFormat)>,
) -> SheetsAction {
    SheetsAction::SetFormats {
        sheet,
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
    }
}

pub fn insert_sheet_action(at: usize, name: &str) -> SheetsAction {
    SheetsAction::InsertSheet {
        at,
        name: name.to_string(),
    }
}

pub fn delete_sheet_action(idx: usize) -> SheetsAction {
    SheetsAction::DeleteSheet { idx }
}

pub fn rename_sheet_action(idx: usize, name: &str) -> SheetsAction {
    SheetsAction::RenameSheet {
        idx,
        name: name.to_string(),
    }
}

pub fn move_sheet_action(from: usize, to: usize) -> SheetsAction {
    SheetsAction::MoveSheet { from, to }
}

pub fn duplicate_sheet_action(idx: usize) -> SheetsAction {
    SheetsAction::DuplicateSheet { idx }
}

pub fn set_sheet_color_action(idx: usize, color: Option<u32>) -> SheetsAction {
    SheetsAction::SetSheetColor { idx, color }
}

pub fn insert_row_action(sheet: usize, at: u32) -> SheetsAction {
    SheetsAction::InsertRow { sheet, at }
}

pub fn delete_row_action(sheet: usize, at: u32) -> SheetsAction {
    SheetsAction::DeleteRow { sheet, at }
}

pub fn insert_column_action(sheet: usize, at: u32) -> SheetsAction {
    SheetsAction::InsertColumn { sheet, at }
}

pub fn delete_column_action(sheet: usize, at: u32) -> SheetsAction {
    SheetsAction::DeleteColumn { sheet, at }
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
#[derive(Debug, Clone)]
pub struct SheetsResourceCtx {
    pub sheet_names: Vec<String>,
}

fn immediate_spec(id: &str, label: &str, action: &SheetsAction) -> ActionSpec {
    ActionSpec::immediate(id, label, SHEETS_ACTION_TYPE, action)
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
}

/// Build the resource tree Sheets currently has open: the workbook (always
/// present, exporting "New sheet") and one `sheets.sheet:<idx>` per sheet, each
/// exporting rename / delete / insert-row / insert-column. Every operation is
/// the same action the on-screen affordance dispatches.
pub fn provide_resources(ctx: &SheetsResourceCtx, doc: &AutoCommit) -> Vec<Resource> {
    let mut workbook = Resource::new("sheets.workbook", "sheets.workbook", "Workbook");

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

    for (idx, name) in ctx.sheet_names.iter().enumerate() {
        let label = if name.trim().is_empty() {
            format!("Sheet{}", idx + 1)
        } else {
            name.clone()
        };
        let stable = SheetsDoc::sheet_resource_id(doc, idx).unwrap_or_else(|| idx.to_string());
        let mut sheet = Resource::new(format!("sheets.sheet:{stable}"), "sheets.sheet", label);

        // Rename — a prompted action carrying the new name.
        sheet = sheet.with_action(ActionSpec::prompted(
            "rename",
            "Rename sheet",
            "Name",
            "sheet name…",
            ExecutionLocus::ClientLocal,
            move |new_name: &str| encode(&rename_sheet_action(idx, new_name.trim())),
        ));

        // Delete the whole sheet (a no-op on the workbook's last sheet).
        sheet = sheet.with_action(immediate_spec(
            "delete",
            "Delete sheet",
            &delete_sheet_action(idx),
        ));

        // Insert a blank row above a given 1-based row number (default: the top).
        sheet = sheet.with_action(ActionSpec::prompted(
            "insert-row",
            "Insert row above…",
            "Row",
            "1-based row number (e.g. 5)",
            ExecutionLocus::ClientLocal,
            move |arg: &str| {
                let at = arg
                    .trim()
                    .parse::<u32>()
                    .ok()
                    .map_or(0, |n| n.saturating_sub(1));
                encode(&insert_row_action(idx, at))
            },
        ));

        // Insert a blank column left of a given column letter (default: A).
        sheet = sheet.with_action(ActionSpec::prompted(
            "insert-column",
            "Insert column left…",
            "Column",
            "column letter (e.g. C)",
            ExecutionLocus::ClientLocal,
            move |arg: &str| {
                let at = letters_to_col(arg.trim()).unwrap_or(0);
                encode(&insert_column_action(idx, at))
            },
        ));

        workbook = workbook.with_child(sheet);
    }

    vec![workbook]
}

impl ResourceProvider for crate::app::SheetsApp {
    fn resources(&self, doc: &AutoCommit) -> Vec<Resource> {
        provide_resources(&self.sheets_resource_ctx(), doc)
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

    fn ctx_from(doc: &mut AutoCommit) -> SheetsResourceCtx {
        SheetsResourceCtx {
            sheet_names: SheetsDoc::sheet_names(doc),
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
        let labels = palette.result_labels();
        assert!(labels.iter().any(|l| l.contains("New sheet")), "{labels:?}");
        assert!(
            labels.iter().any(|l| l.contains("Rename sheet")),
            "{labels:?}"
        );
        assert!(
            labels.iter().any(|l| l.contains("Delete sheet")),
            "{labels:?}"
        );
    }

    /// A cell write through the action mutates the document.
    #[test]
    fn set_cell_action_writes_the_value_and_format() {
        let mut doc = seeded();
        dispatch(
            &mut doc,
            &set_cell_action(0, (0, 1), "=SUM(B1:B3)", Some(CellFormat::Number)),
        );
        assert_eq!(SheetsDoc::cell_raw(&mut doc, 0, (0, 1)), "=SUM(B1:B3)");
        let snap = SheetsDoc::snapshot_sheet(&mut doc, 0, Currency::USD);
        assert_eq!(snap.fmt.get(&(0, 1)).copied(), Some(CellFormat::Number));
    }

    /// Deleting a sheet through its action removes it; the last sheet is kept.
    #[test]
    fn delete_sheet_action_removes_a_sheet_but_keeps_the_last() {
        let mut doc = seeded();
        assert_eq!(SheetsDoc::sheet_count(&mut doc), 2);
        dispatch(&mut doc, &delete_sheet_action(1));
        assert_eq!(SheetsDoc::sheet_names(&mut doc), vec!["Budget"]);
        // The last sheet cannot be deleted away.
        dispatch(&mut doc, &delete_sheet_action(0));
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
        // Budget!A1 = "Rent". Insert a row above row 1 (at == 0): it moves to A2.
        dispatch(&mut doc, &insert_row_action(0, 0));
        assert_eq!(SheetsDoc::cell_raw(&mut doc, 0, (0, 0)), "");
        assert_eq!(SheetsDoc::cell_raw(&mut doc, 0, (0, 1)), "Rent");
        // Delete row 1 again: it shifts back up to A1.
        dispatch(&mut doc, &delete_row_action(0, 0));
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

        let action = set_cell_action(0, (2, 4), "converge", None);
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

        let action = insert_row_action(0, 0);
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
        let from_builder = encode(&delete_sheet_action(0));
        assert_eq!(from_spec.type_id, from_builder.type_id);
        assert_eq!(
            from_spec.payload, from_builder.payload,
            "delete is one source of truth"
        );

        // Rename spec == rename builder for the same name.
        let rename_spec = sheet.actions.iter().find(|a| a.id == "rename").unwrap();
        let from_spec = prompted_encode(rename_spec, "Q1");
        let from_builder = encode(&rename_sheet_action(0, "Q1"));
        assert_eq!(
            from_spec.payload, from_builder.payload,
            "rename is one source of truth"
        );

        // Insert-row spec builds an InsertRow at the parsed (1-based → 0-based) row.
        let insert_spec = sheet.actions.iter().find(|a| a.id == "insert-row").unwrap();
        let from_spec = prompted_encode(insert_spec, "5");
        let from_builder = encode(&insert_row_action(0, 4));
        assert_eq!(
            from_spec.payload, from_builder.payload,
            "insert-row is one source of truth"
        );
    }

    #[test]
    fn cell_action_history_has_coordinate_resource() {
        let context = history_context(&SheetsAction::SetCell {
            sheet: 2,
            col: 1,
            row: 4,
            raw: "42".into(),
            fmt: None,
        });
        assert_eq!(context.action_label, "Edit cell");
        let resource = context.resource.expect("cell resource");
        assert_eq!(resource.id, "sheets.cell:2:1:4");
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
}
